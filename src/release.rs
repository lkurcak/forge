//! Checks on what a repository has published, rather than how it's configured.
//!
//! These only report: fixing them means cutting a new release, or merging the
//! PRs a release opened.

use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::cargo::Workspace;
use crate::gh::Gh;
use crate::manifest::{Needs, Repo};

/// The repo holding the Homebrew formulas, under the manifest's owner.
const TAP: &str = "homebrew-tap";

#[derive(Default)]
pub struct Findings {
    pub problems: Vec<String>,
    /// Releases still on their way somewhere, like a PR awaiting review.
    pub pending: Vec<String>,
}

/// Checks the latest release's tag and binary archives, and that it reached
/// the package managers the repo's tags name.
///
/// Archives are expected as `{repo}-{tag}-{target}.tar.gz`, or `.zip` on Windows.
pub fn check(gh: &Gh, owner: &str, repo: &Repo, needs: &Needs) -> Result<Findings> {
    let base = format!("repos/{owner}/{}", repo.name);
    let mut findings = Findings::default();
    let release = match gh.get(&format!("{base}/releases/latest")) {
        Ok(release) => release,
        // Nothing published yet: only worth mentioning if binaries are expected.
        Err(err) if err.to_string().contains("(HTTP 404)") => {
            if !needs.release_targets.is_empty() {
                findings.problems.push("release: none published yet".into());
            }
            return Ok(findings);
        }
        Err(err) => return Err(err),
    };

    let tag = release["tag_name"].as_str().unwrap_or_default();
    let problems = &mut findings.problems;

    if let Some(prefix) = &repo.release_tag_prefix {
        let version_ok = tag.strip_prefix(prefix.as_str()).is_some_and(is_version);
        if !version_ok {
            problems.push(format!("release {tag}: tag should look like {prefix}X.Y.Z"));
        }
    }

    // HEAD is the default branch. A tag ahead of it, like one pushed before
    // its PR was squash merged, points at a commit the branch doesn't have.
    let compare = gh.get(&format!("{base}/compare/HEAD...{tag}?per_page=1"))?;
    if matches!(compare["status"].as_str(), Some("ahead" | "diverged")) {
        problems.push(format!("release {tag}: tag isn't on the default branch"));
    }

    let assets: Vec<&str> = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["name"].as_str())
        .collect();
    for target in &needs.release_targets {
        let stem = format!("{}-{tag}-{target}", repo.name);
        let found = assets.iter().any(|name| {
            [".tar.gz", ".zip"].iter().any(|ext| {
                name.strip_suffix(ext)
                    .is_some_and(|name| glob_match(&stem, name))
            })
        });
        if !found {
            problems.push(format!("release {tag}: no archive for {target}"));
        }
    }

    let Some(version) = repo
        .release_tag_prefix
        .as_deref()
        .and_then(|prefix| tag.strip_prefix(prefix))
        .filter(|v| is_version(v))
    else {
        return Ok(findings);
    };
    let has = |tag: &str| needs.tags.iter().any(|t| t == tag);
    if has("crates-io") {
        for name in published_crates(gh, &format!("{owner}/{}", repo.name))? {
            crates_io(&name, version, &mut findings)?;
        }
    }
    if has("homebrew") {
        homebrew(gh, owner, &repo.name, version, &mut findings)?;
    }
    if has("winget") {
        winget(gh, owner, &repo.name, version, &mut findings)?;
    }
    Ok(findings)
}

/// The crates the repo publishes: its package and workspace members, minus
/// those with `publish = false`.
fn published_crates(gh: &Gh, repo: &str) -> Result<Vec<String>> {
    let published = |table: &toml::Table| -> Option<String> {
        let package = table.get("package")?;
        let publish = package.get("publish");
        let off = publish
            .is_some_and(|p| p.as_bool() == Some(false) || p.as_array().is_some_and(Vec::is_empty));
        if off {
            return None;
        }
        package.get("name")?.as_str().map(str::to_owned)
    };

    let mut names: Vec<String> = Vec::new();
    for (_, package) in Workspace::read(gh, repo)?.map_or_default(|w| w.packages) {
        if let Some(name) = published(&package)
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    Ok(names)
}

fn crates_io(name: &str, version: &str, findings: &mut Findings) -> Result<()> {
    let url = format!("https://crates.io/api/v1/crates/{name}");
    let out = Command::new("curl")
        .args([
            "-sS",
            "--user-agent",
            "forge (github.com/lkurcak/forge)",
            &url,
        ])
        .output()
        .context("running curl")?;
    if !out.status.success() {
        bail!("GET {url}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let info: Value =
        serde_json::from_slice(&out.stdout).with_context(|| format!("GET {url}: not JSON"))?;
    match info["crate"]["max_version"].as_str() {
        None => findings
            .problems
            .push(format!("crates.io {name}: not published")),
        Some(v) if v != version => findings.problems.push(format!(
            "crates.io {name}: has {v}, latest release is {version}"
        )),
        Some(_) => {}
    }
    Ok(())
}

/// Every URL in the formula (one per platform, for some) has to mention the
/// version; the release opens a PR bumping them.
fn homebrew(
    gh: &Gh,
    owner: &str,
    name: &str,
    version: &str,
    findings: &mut Findings,
) -> Result<()> {
    let tap = format!("{owner}/{TAP}");
    let Some(formula) = gh.file(&tap, &format!("Formula/{name}.rb"), None)? else {
        findings
            .problems
            .push(format!("homebrew: no Formula/{name}.rb in {tap}"));
        return Ok(());
    };
    let mut urls = formula
        .lines()
        .filter_map(|l| l.trim().strip_prefix("url \""))
        .peekable();
    if urls.peek().is_some() && urls.all(|url| url.contains(version)) {
        return Ok(());
    }
    let title = format!("chore({name}): update to {version}");
    let legacy_title = format!("{name} {version}");
    let pr = gh
        .get(&format!("repos/{tap}/pulls?state=open&per_page=100"))?
        .as_array()
        .into_iter()
        .flatten()
        .find(|pr| pr["title"] == title.as_str() || pr["title"] == legacy_title.as_str())
        .and_then(|pr| pr["number"].as_u64());
    match pr {
        Some(n) => findings
            .pending
            .push(format!("homebrew: {version} waiting in {tap}#{n}")),
        None => findings.problems.push(format!(
            "homebrew: formula isn't at {version}, and no PR bumps it"
        )),
    }
    Ok(())
}

/// The package is `{owner}.{name}`; versions show up in winget-pkgs once a
/// moderator merges their PR.
fn winget(gh: &Gh, owner: &str, name: &str, version: &str, findings: &mut Findings) -> Result<()> {
    let id = format!("{owner}.{name}");
    let initial = owner
        .chars()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let dir = format!("repos/microsoft/winget-pkgs/contents/manifests/{initial}/{owner}/{name}");
    let versions: Vec<String> = match gh.get(&dir) {
        Ok(entries) => entries
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e["name"].as_str().map(str::to_owned))
            .collect(),
        Err(err) if err.to_string().contains("(HTTP 404)") => Vec::new(),
        Err(err) => return Err(err),
    };
    if versions.iter().any(|v| v == version) {
        return Ok(());
    }

    let query = format!("repo:microsoft/winget-pkgs+is:pr+is:open+in:title+{id}+{version}");
    let pr = gh.get(&format!("search/issues?q={query}"))?["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|pr| {
            pr["title"]
                .as_str()
                .is_some_and(|t| t.contains(&id) && t.ends_with(&format!("version {version}")))
        })
        .and_then(|pr| pr["number"].as_u64());
    match (pr, versions.is_empty()) {
        (Some(n), _) => findings.pending.push(format!(
            "winget: {version} waiting in microsoft/winget-pkgs#{n}"
        )),
        (None, true) => findings.problems.push(format!(
            "winget: {id} isn't in winget-pkgs; submit the first version with komac"
        )),
        (None, false) => findings.problems.push(format!(
            "winget: has {}, latest release is {version}, and no PR adds it",
            versions.last().map_or("nothing", String::as_str)
        )),
    }
    Ok(())
}

/// `X.Y.Z` with numeric parts, optionally followed by a `-prerelease` suffix.
fn is_version(s: &str) -> bool {
    let core = s.split_once('-').map_or(s, |(core, _)| core);
    let parts: Vec<_> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Matches `text` against `pattern`, where `*` matches any run of characters.
fn glob_match(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((head, rest)) => {
            let Some(text) = text.strip_prefix(head) else {
                return false;
            };
            (0..=text.len())
                .filter(|&i| text.is_char_boundary(i))
                .any(|i| glob_match(rest, &text[i..]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_version("1.2.3"));
        assert!(is_version("0.1.0-rc.1"));
        assert!(!is_version("1.2"));
        assert!(!is_version("1.2.x"));
        assert!(!is_version(""));
    }

    #[test]
    fn globs() {
        assert!(glob_match(
            "ocalc-v1-x86_64-pc-windows-*",
            "ocalc-v1-x86_64-pc-windows-gnu"
        ));
        assert!(glob_match("a*c*e", "abcde"));
        assert!(!glob_match(
            "ocalc-v1-x86_64-pc-windows-*",
            "ocalc-v1-aarch64-pc-windows-gnu"
        ));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("exact", "exactly"));
    }
}
