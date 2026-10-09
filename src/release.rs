//! Checks on what a repository has published, rather than how it's configured.
//!
//! These only report: fixing them means cutting a new release.

use anyhow::Result;

use crate::gh::Gh;
use crate::manifest::Repo;

/// Checks the latest release's tag and binary archives.
///
/// Archives are expected as `{repo}-{tag}-{target}.tar.gz`, or `.zip` on Windows.
pub fn check(gh: &Gh, base: &str, repo: &Repo) -> Result<Vec<String>> {
    let release = match gh.get(&format!("{base}/releases/latest")) {
        Ok(release) => release,
        // Nothing published yet: only worth mentioning if binaries are expected.
        Err(err) if err.to_string().contains("(HTTP 404)") => {
            return Ok(if repo.release_targets.is_empty() {
                Vec::new()
            } else {
                vec!["release: none published yet".into()]
            });
        }
        Err(err) => return Err(err),
    };

    let tag = release["tag_name"].as_str().unwrap_or_default();
    let mut problems = Vec::new();

    if let Some(prefix) = &repo.release_tag_prefix {
        let version_ok = tag.strip_prefix(prefix.as_str()).is_some_and(is_version);
        if !version_ok {
            problems.push(format!("release {tag}: tag should look like {prefix}X.Y.Z"));
        }
    }

    let assets: Vec<&str> = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["name"].as_str())
        .collect();
    for target in &repo.release_targets {
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

    Ok(problems)
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
