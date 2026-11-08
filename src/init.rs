//! `forge init`: a starter manifest covering every repository an account owns.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::gh::{Gh, RepoSummary};

/// The manifest's fixed part; the repos are added after it.
const TEMPLATE: &str = include_str!("init.toml");

pub fn run(path: &Path, owner: Option<String>) -> Result<()> {
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    let owner = match owner {
        Some(owner) => owner,
        None => Gh::active_user()?,
    };
    let repos = Gh::new(&owner)?.source_repo_summaries(&owner)?;
    fs::write(path, manifest(&owner, &repos))
        .with_context(|| format!("writing {}", path.display()))?;
    println!(
        "wrote {} with {} repos; review it, then run `forge check`",
        path.display(),
        repos.len()
    );
    Ok(())
}

fn manifest(owner: &str, repos: &[RepoSummary]) -> String {
    let mut text = TEMPLATE.replace("@OWNER@", owner);
    let mut repos: Vec<_> = repos.iter().collect();
    repos.sort_by_key(|repo| repo.name.to_lowercase());
    for repo in repos {
        let mut tags = Vec::new();
        if repo.language.as_deref() == Some("Rust") {
            tags.push("\"rust\"");
        }
        if repo.public {
            tags.push("\"security\"");
        }
        write!(
            text,
            "\n[repos.{}]\ntags = [{}]\n",
            key(&repo.name),
            tags.join(", ")
        )
        .expect("writing to a String");
    }
    text
}

/// `name` as a TOML key, quoted unless it's a valid bare key.
fn key(name: &str) -> String {
    if !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        name.to_owned()
    } else {
        format!("{name:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    fn summary(name: &str, public: bool, language: Option<&str>) -> RepoSummary {
        RepoSummary {
            name: name.to_owned(),
            public,
            language: language.map(str::to_owned),
        }
    }

    #[test]
    fn writes_a_manifest_forge_can_load() {
        let text = manifest(
            "someone",
            &[
                summary("zeta", false, None),
                summary("tool", true, Some("Rust")),
                summary("site.nvim", true, Some("Lua")),
            ],
        );
        let dir = std::env::temp_dir().join(format!("forge-init-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("forge.toml");
        fs::write(&path, &text).unwrap();
        let mut manifest = Manifest::load(&path).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        manifest.fetch_workflows(crate::workflow::local).unwrap();

        assert_eq!(manifest.owner, "someone");
        let names: Vec<_> = manifest.repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["site.nvim", "tool", "zeta"]);
        let tool = &manifest.repos[1];
        assert_eq!(tool.tags, ["rust", "security"]);
        let calls: Vec<_> = manifest.workflows.iter().map(|_| None).collect();
        let needs = manifest.needs(tool, &calls, true);
        assert!(needs.calls.contains(&"rust-ci".to_owned()));
        assert_eq!(needs.visibility_problems, Vec::<String>::new());
    }

    #[test]
    fn quotes_keys_only_when_needed() {
        assert_eq!(key("t-lang_2"), "t-lang_2");
        assert_eq!(key("t-lang.nvim"), "\"t-lang.nvim\"");
    }
}
