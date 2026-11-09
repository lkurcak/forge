//! Workflow inputs that pin a release of another repo (`[pins]`), like a
//! T repo's `t-version` and `t-sha256` for the t-lang it's checked with.
//!
//! `forge check` reports a repo whose pin is behind that repo's latest
//! release; `forge bump` opens a PR moving it there, and turns on
//! auto-merge, so the PR merges once the repo's required checks pass.

use std::cmp::Ordering;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

use crate::gh::{Gh, NewPr};

/// What stands for the release's tag in a pin's `asset`.
const TAG: &str = "{tag}";

/// `[pins."file.yml"]`: the workflow file (in `.github/workflows/`) whose
/// shared-workflow call has inputs pinning another repo's release.
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    /// `owner/repo` whose latest release the inputs should name.
    pub release: String,
    /// The input holding the release's tag, like `t-version`.
    pub version: String,
    /// The input holding the sha256 of one of the release's assets, if
    /// there's one: `asset` says which.
    #[serde(default)]
    pub sha256: Option<String>,
    /// The asset's name, `{tag}` standing for the release's tag, like
    /// `t-lang-{tag}-x86_64-unknown-linux-gnu.tar.gz`.
    #[serde(default)]
    pub asset: Option<String>,
}

/// A release a pin should name.
#[derive(Clone)]
pub struct Latest {
    pub tag: String,
    /// The sha256 of the pin's asset in it, if the pin has one.
    pub sha256: Option<String>,
}

/// A repo whose pin is behind.
pub struct Behind {
    pub repo: String,
    /// The workflow file, like `t.yml`.
    pub file: String,
    pub pinned: String,
    pub latest: Latest,
}

/// What `forge check` says of a pin that's behind.
pub fn describe(file: &str, pin: &Pin, pinned: &str, latest: &Latest) -> String {
    format!(
        "{file}: {} is {pinned}, {}'s latest release is {}",
        pin.version, pin.release, latest.tag
    )
}

impl Pin {
    /// The latest release of `release`, and its asset's sha256 from the
    /// digest GitHub keeps of it.
    pub fn latest(&self, gh: &Gh) -> Result<Latest> {
        let release = gh.get(&format!("repos/{}/releases/latest", self.release))?;
        let tag = release["tag_name"]
            .as_str()
            .with_context(|| format!("{}'s latest release has no tag", self.release))?
            .to_owned();
        let sha256 = match (&self.sha256, &self.asset) {
            (Some(_), Some(asset)) => {
                let name = asset.replace(TAG, &tag);
                let digest = release["assets"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|a| a["name"] == name.as_str())
                    .and_then(|a| a["digest"].as_str())
                    .with_context(|| {
                        format!("{} {tag} has no asset {name} with a digest", self.release)
                    })?;
                let Some(hex) = digest.strip_prefix("sha256:") else {
                    bail!("{name}'s digest isn't a sha256: {digest}");
                };
                Some(hex.to_owned())
            }
            (Some(input), None) => bail!("pin input {input} needs an `asset`"),
            (None, _) => None,
        };
        Ok(Latest { tag, sha256 })
    }
}

/// Repo `repo`'s (of `owner`) pin in workflow `file`, if it has one and
/// it's behind `latest`.
pub fn behind(
    gh: &Gh,
    owner: &str,
    repo: &str,
    file: &str,
    pin: &Pin,
    latest: &Latest,
) -> Result<Option<Behind>> {
    let path = format!(".github/workflows/{file}");
    let Some(text) = gh.file(&format!("{owner}/{repo}"), &path, None)? else {
        return Ok(None);
    };
    let Some(pinned) = input(&text, &pin.version)? else {
        return Ok(None);
    };
    Ok(older(&pinned, &latest.tag).then(|| Behind {
        repo: repo.to_owned(),
        file: file.to_owned(),
        pinned,
        latest: latest.clone(),
    }))
}

/// The value of input `name` of the shared-workflow call in workflow
/// `text`, if it has one.
pub fn input(text: &str, name: &str) -> Result<Option<String>> {
    let yaml: Value = serde_yaml_ng::from_str(text)?;
    let mut values = yaml["jobs"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, job)| job["with"][name].as_str().map(str::to_owned));
    match (values.next(), values.next()) {
        (one, None) => Ok(one),
        _ => bail!("input {name} is set in more than one job"),
    }
}

/// Is version `a` older than `b`? Both `vX.Y.Z` (the `v` optional); one
/// that isn't is older if it differs.
pub fn older(a: &str, b: &str) -> bool {
    fn parse(v: &str) -> Option<Vec<u64>> {
        v.strip_prefix('v')
            .unwrap_or(v)
            .split('.')
            .map(|n| n.parse().ok())
            .collect()
    }
    match (parse(a), parse(b)) {
        (Some(a), Some(b)) => a.cmp(&b) == Ordering::Less,
        _ => a != b,
    }
}

/// `text` with input `name`'s value `old` replaced by `new`, on the one
/// line that sets it: `  name: old`, maybe with a comment after.
pub fn rewrite(text: &str, name: &str, old: &str, new: &str) -> Result<String> {
    let mut found = 0;
    let lines: Vec<String> = text
        .split_inclusive('\n')
        .map(|line| {
            let trimmed = line.trim_start();
            let Some(rest) = trimmed.strip_prefix(name).and_then(|r| r.strip_prefix(':')) else {
                return line.to_owned();
            };
            let value = rest.split('#').next().unwrap_or("").trim();
            let unquoted = value.trim_matches(|c| c == '"' || c == '\'');
            if unquoted != old {
                return line.to_owned();
            }
            found += 1;
            let indent = &line[..line.len() - trimmed.len()];
            let after = &rest[rest.find(value).expect("value is in rest") + value.len()..];
            let quoted = value.replace(old, new);
            format!("{indent}{name}: {quoted}{after}")
        })
        .collect();
    match found {
        1 => Ok(lines.concat()),
        0 => bail!("no line sets {name} to {old}"),
        n => bail!("{n} lines set {name} to {old}"),
    }
}

/// Opens a PR moving `b`'s pin to its latest release, on a branch of its
/// own, and turns on auto-merge (squash). Gives the PR's URL, or `None` if
/// a PR for that branch is open already.
pub fn bump(gh: &Gh, owner: &str, pin: &Pin, b: &Behind) -> Result<Option<String>> {
    let repo = format!("{owner}/{}", b.repo);
    let path = format!(".github/workflows/{}", b.file);
    let branch = format!("forge/{}-{}", pin.version, b.latest.tag);
    if gh.open_pr_from(&repo, &branch)?.is_some() {
        return Ok(None);
    }

    let base = gh.get(&format!("repos/{repo}"))?["default_branch"]
        .as_str()
        .context("no default branch")?
        .to_owned();
    let head = gh.get(&format!("repos/{repo}/git/ref/heads/{base}"))?["object"]["sha"]
        .as_str()
        .context("no head commit")?
        .to_owned();
    let text = gh
        .file(&repo, &path, Some(&head))?
        .with_context(|| format!("{path} is gone"))?;
    let mut new = rewrite(&text, &pin.version, &b.pinned, &b.latest.tag)?;
    if let (Some(input), Some(sha)) = (&pin.sha256, &b.latest.sha256) {
        let old = self::input(&text, input)?
            .with_context(|| format!("{path} sets {} but not {input}", pin.version))?;
        new = rewrite(&new, input, &old, sha)?;
    }

    let name = pin.release.rsplit('/').next().unwrap_or(&pin.release);
    let title = format!("chore: {name} {}", b.latest.tag);
    let body = format!(
        "Moves `{}` to [{tag}](https://github.com/{rel}/releases/tag/{tag}), {rel}'s latest release.\n\nOpened by `forge bump`; it merges itself once the required checks pass.",
        pin.version,
        tag = b.latest.tag,
        rel = pin.release,
    );
    let url = gh.open_pr(&NewPr {
        repo: &repo,
        base: &base,
        parent: &head,
        branch: &branch,
        files: &[(&path, &new)],
        title: &title,
        body: &body,
    })?;
    gh.auto_merge(&url)?;
    Ok(Some(url))
}

#[cfg(test)]
mod tests {
    use super::*;

    const T_YML: &str = "\
jobs:
  ci:
    uses: lkurcak/forge/.github/workflows/t-ci.yml@v1
    with:
      # Bump both together.
      t-version: v0.74.0
      t-sha256: abc123 # the digest
";

    #[test]
    fn reads_inputs() {
        assert_eq!(
            input(T_YML, "t-version").unwrap().as_deref(),
            Some("v0.74.0")
        );
        assert_eq!(input(T_YML, "t-sha256").unwrap().as_deref(), Some("abc123"));
        assert_eq!(input(T_YML, "run").unwrap(), None);
    }

    #[test]
    fn compares_versions() {
        assert!(older("v0.74.0", "v0.75.0"));
        assert!(older("v0.9.0", "v0.10.0"));
        assert!(!older("v0.75.0", "v0.75.0"));
        assert!(!older("v1.0.0", "v0.75.0"));
        assert!(older("main", "v0.75.0"));
    }

    #[test]
    fn rewrites_one_line() {
        let new = rewrite(T_YML, "t-version", "v0.74.0", "v0.75.0").unwrap();
        let new = rewrite(&new, "t-sha256", "abc123", "def456").unwrap();
        assert_eq!(
            new,
            T_YML
                .replace("v0.74.0", "v0.75.0")
                .replace("abc123", "def456")
        );
        assert!(rewrite(T_YML, "t-version", "v0.73.0", "v0.75.0").is_err());
    }

    #[test]
    fn keeps_quotes() {
        let text = "      t-version: \"v0.74.0\"\n";
        assert_eq!(
            rewrite(text, "t-version", "v0.74.0", "v0.75.0").unwrap(),
            "      t-version: \"v0.75.0\"\n"
        );
    }
}
