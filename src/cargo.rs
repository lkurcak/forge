//! A Rust repository's Cargo manifests, read through the API.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use toml::{Table, Value};

use crate::gh::Gh;

/// Lint group (`rust`, `clippy`, …) to lint name to level, as in `[lints]`.
pub type Lints = BTreeMap<String, BTreeMap<String, String>>;

pub struct Workspace {
    /// The root `Cargo.toml`.
    root: Table,
    /// Every package's manifest path and contents: the root's, if it is a
    /// package, then the workspace members'.
    pub packages: Vec<(String, Table)>,
}

impl Workspace {
    /// Reads the default branch's manifests, or `None` without a root
    /// `Cargo.toml`. Members may end in `/*`.
    pub fn read(gh: &Gh, repo: &str) -> Result<Option<Self>> {
        let manifest = |path: &str| -> Result<Option<Table>> {
            gh.file(repo, path, None)?
                .map(|text| {
                    text.parse()
                        .with_context(|| format!("parsing {repo}'s {path}"))
                })
                .transpose()
        };

        let Some(root) = manifest("Cargo.toml")? else {
            return Ok(None);
        };
        let mut packages = Vec::new();
        if root.contains_key("package") {
            packages.push(("Cargo.toml".to_owned(), root.clone()));
        }
        let members = root
            .get("workspace")
            .and_then(|w| w.get("members"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str);
        for member in members {
            let dirs = match member.strip_suffix("/*") {
                Some(parent) => gh
                    .get(&format!("repos/{repo}/contents/{parent}"))?
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|e| e["type"] == "dir")
                    .filter_map(|e| e["path"].as_str().map(str::to_owned))
                    .collect(),
                None => vec![member.to_owned()],
            };
            for dir in dirs {
                // Members like "." or "./crates/a" name the same files as "" and "crates/a".
                let dir = dir.trim_start_matches("./").trim_end_matches('/');
                let path = match dir {
                    "" | "." => "Cargo.toml".to_owned(),
                    dir => format!("{dir}/Cargo.toml"),
                };
                if packages.iter().any(|(p, _)| *p == path) {
                    continue;
                }
                if let Some(table) = manifest(&path)? {
                    packages.push((path, table));
                }
            }
        }
        Ok(Some(Self { root, packages }))
    }

    /// Where each package's lints don't set `expected`, or set it laxer.
    /// Packages inheriting `[workspace.lints]` are checked there, once.
    pub fn lint_problems(&self, expected: &Lints) -> Vec<String> {
        let mut checked: Vec<(String, Option<&Table>)> = Vec::new();
        for (path, package) in &self.packages {
            let lints = package.get("lints").and_then(Value::as_table);
            let source = if lints.and_then(|l| l.get("workspace")) == Some(&Value::Boolean(true)) {
                let lints = self
                    .root
                    .get("workspace")
                    .and_then(|w| w.get("lints"))
                    .and_then(Value::as_table);
                ("Cargo.toml [workspace.lints]".to_owned(), lints)
            } else {
                (format!("{path} [lints]"), lints)
            };
            if !checked.iter().any(|(label, _)| *label == source.0) {
                checked.push(source);
            }
        }

        let mut problems = Vec::new();
        for (label, lints) in checked {
            for (group, names) in expected {
                for (name, level) in names {
                    let actual = lints
                        .and_then(|l| l.get(group))
                        .and_then(|g| g.get(name))
                        .and_then(|l| l.as_str().or_else(|| l.get("level")?.as_str()));
                    if actual.is_some_and(|actual| strictness(actual) >= strictness(level)) {
                        continue;
                    }
                    problems.push(format!(
                        "{label}: {group}.{name} should be {level}, is {}",
                        actual.unwrap_or("unset")
                    ));
                }
            }
        }
        problems
    }
}

/// Orders lint levels, so a lint denied where a warning is asked for passes.
fn strictness(level: &str) -> u8 {
    match level {
        "warn" => 1,
        "deny" => 2,
        "forbid" => 3,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(manifests: &[(&str, &str)]) -> Workspace {
        let mut tables = manifests
            .iter()
            .map(|(path, text)| ((*path).to_owned(), text.parse::<Table>().unwrap()));
        let (_, root) = tables.next().unwrap();
        let mut packages: Vec<_> = tables.collect();
        if root.contains_key("package") {
            packages.insert(0, ("Cargo.toml".to_owned(), root.clone()));
        }
        Workspace { root, packages }
    }

    fn expected() -> Lints {
        toml::from_str("clippy.mod_module_files = 'warn'").unwrap()
    }

    #[test]
    fn package_lints() {
        let set = "[package]\nname = 'a'\n[lints.clippy]\nmod_module_files = 'warn'";
        assert_eq!(
            workspace(&[("", set)]).lint_problems(&expected()),
            Vec::<String>::new()
        );

        let stricter = "[package]\nname = 'a'\n[lints.clippy]\nmod_module_files = { level = 'deny', priority = 1 }";
        assert_eq!(
            workspace(&[("", stricter)]).lint_problems(&expected()),
            Vec::<String>::new()
        );

        let laxer = "[package]\nname = 'a'\n[lints.clippy]\nmod_module_files = 'allow'";
        assert_eq!(
            workspace(&[("", laxer)]).lint_problems(&expected()),
            ["Cargo.toml [lints]: clippy.mod_module_files should be warn, is allow"]
        );

        let unset = "[package]\nname = 'a'";
        assert_eq!(
            workspace(&[("", unset)]).lint_problems(&expected()),
            ["Cargo.toml [lints]: clippy.mod_module_files should be warn, is unset"]
        );
    }

    #[test]
    fn workspace_lints_are_checked_once() {
        let root = "[workspace]\nmembers = ['a', 'b']\n[workspace.lints.clippy]\nmod_module_files = 'warn'";
        let inherits = "[package]\nname = 'x'\n[lints]\nworkspace = true";
        let own = "[package]\nname = 'y'";
        assert_eq!(
            workspace(&[
                ("", root),
                ("a/Cargo.toml", inherits),
                ("b/Cargo.toml", own)
            ])
            .lint_problems(&expected()),
            ["b/Cargo.toml [lints]: clippy.mod_module_files should be warn, is unset"]
        );

        let root = "[workspace]\nmembers = ['a', 'b']";
        assert_eq!(
            workspace(&[
                ("", root),
                ("a/Cargo.toml", inherits),
                ("b/Cargo.toml", inherits)
            ])
            .lint_problems(&expected()),
            ["Cargo.toml [workspace.lints]: clippy.mod_module_files should be warn, is unset"]
        );
    }
}
