//! The shared workflows, and the repos calling them.
//!
//! A repo opts into one by calling it from its own workflow file, and the
//! inputs it passes are where it configures it (for the release workflow,
//! what it ships). forge only reads the call, treating each input turned on
//! as a tag (so `winget: true` brings the winget tag's secret), and checks it
//! against the shared workflow itself.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::gh::Gh;

pub struct SharedWorkflow {
    /// The file's stem, which is also the tag of every repo calling it.
    pub name: String,
    /// Where repos call it from, like `.github/workflows/release.yml`.
    caller: String,
    /// `owner/repo/.github/workflows/file.yml`, as callers reference it.
    path: String,
    /// The ref callers must use.
    git_ref: String,
    inputs: Vec<String>,
    /// Targets built per input, from the workflow's `{INPUT}_TARGETS` env.
    targets: BTreeMap<String, Vec<String>>,
}

/// A repo's call of a shared workflow.
pub struct Call {
    git_ref: String,
    pub inputs: BTreeMap<String, Value>,
}

impl Call {
    /// Inputs set to `true`.
    pub fn enabled(&self) -> impl Iterator<Item = &str> {
        self.inputs
            .iter()
            .filter(|(_, v)| **v == Value::Bool(true))
            .map(|(k, _)| k.as_str())
    }

    /// The inputs, for listing.
    pub fn describe(&self) -> String {
        self.inputs
            .iter()
            .map(|(k, v)| match v {
                Value::Bool(true) => k.clone(),
                _ => format!("{k}={v}"),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl SharedWorkflow {
    /// Reads the workflow `uses` names from the repo `dir` holds, which must
    /// be the one it lives in. Repos call it from `.github/workflows/{caller}`.
    pub fn load(dir: &Path, caller: &str, uses: &str) -> Result<Self> {
        let (path, git_ref) = uses
            .split_once('@')
            .with_context(|| format!("workflow {uses} needs an @ref"))?;
        let local = path
            .splitn(3, '/')
            .nth(2)
            .with_context(|| format!("workflow {uses} isn't owner/repo/path@ref"))?;
        let file = dir.join(local);
        let text =
            fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
        let yaml: Value = serde_yaml_ng::from_str(&text)
            .with_context(|| format!("parsing {}", file.display()))?;

        let workflow_call = yaml["on"]
            .get("workflow_call")
            .with_context(|| format!("{} has no workflow_call trigger", file.display()))?;
        let inputs = match workflow_call.get("inputs") {
            None => Vec::new(),
            Some(inputs) => inputs
                .as_object()
                .with_context(|| format!("{} has invalid workflow_call inputs", file.display()))?
                .keys()
                .cloned()
                .collect(),
        };
        let targets = yaml["env"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(key, value)| {
                let input = key.strip_suffix("_TARGETS")?;
                let targets = value.as_str()?.split_whitespace().map(str::to_owned);
                Some((input.to_lowercase().replace('_', "-"), targets.collect()))
            })
            .collect();

        Ok(Self {
            name: Path::new(local)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(local)
                .to_owned(),
            caller: format!(".github/workflows/{caller}"),
            path: path.to_owned(),
            git_ref: git_ref.to_owned(),
            inputs,
            targets,
        })
    }

    /// The repo's call of this workflow, if it has one.
    pub fn call(&self, gh: &Gh, owner: &str, repo: &str) -> Result<Option<Call>> {
        let Some(text) = gh.file(&format!("{owner}/{repo}"), &self.caller, None)? else {
            return Ok(None);
        };
        self.parse_call(&text)
            .with_context(|| format!("reading {repo}'s {}", self.caller))
    }

    fn parse_call(&self, text: &str) -> Result<Option<Call>> {
        let yaml: Value = serde_yaml_ng::from_str(text)?;
        let calls: Vec<(&Value, &str)> = yaml["jobs"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(_, job)| {
                let git_ref = job["uses"]
                    .as_str()?
                    .strip_prefix(self.path.as_str())?
                    .strip_prefix('@')?;
                Some((job, git_ref))
            })
            .collect();
        let (job, git_ref) = match calls.as_slice() {
            [] => return Ok(None),
            [call] => *call,
            _ => bail!("calls {} more than once", self.path),
        };
        Ok(Some(Call {
            git_ref: git_ref.to_owned(),
            inputs: job["with"]
                .as_object()
                .map_or_default(|w| w.clone().into_iter().collect()),
        }))
    }

    /// What to tell a repo that should call this workflow but doesn't.
    pub fn missing(&self) -> String {
        format!(
            "{}: should call {}@{}",
            self.caller, self.path, self.git_ref
        )
    }

    /// What's wrong with a repo's call; the repo has to fix these itself.
    pub fn problems(&self, call: &Call) -> Vec<String> {
        let mut problems = Vec::new();
        if call.git_ref != self.git_ref {
            problems.push(format!(
                "{}: calls {}@{}, should be @{}",
                self.caller, self.name, call.git_ref, self.git_ref
            ));
        }
        for input in call.inputs.keys() {
            if !self.inputs.contains(input) {
                problems.push(format!(
                    "{}: {} has no input {input}",
                    self.caller, self.name
                ));
            }
        }
        problems
    }

    /// Targets the call builds release archives for, in the workflow's order.
    pub fn targets(&self, call: &Call) -> Vec<String> {
        let mut targets: Vec<String> = Vec::new();
        for input in call.enabled() {
            for target in self.targets.get(input).into_iter().flatten() {
                if !targets.contains(target) {
                    targets.push(target.clone());
                }
            }
        }
        targets
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> SharedWorkflow {
        SharedWorkflow::load(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            "release.yml",
            "lkurcak/forge/.github/workflows/rust-release.yml@v1",
        )
        .unwrap()
    }

    const CALL: &str = r#"
name: Release
on:
  push:
    tags: ["v[0-9]+.[0-9]+.[0-9]+"]
jobs:
  release:
    uses: lkurcak/forge/.github/workflows/rust-release.yml@v1
    secrets: inherit
    with:
      binaries: true
      winget: true
      homebrew: false
"#;

    #[test]
    fn reads_the_shared_workflow() {
        let shared = shared();
        assert_eq!(shared.name, "rust-release");
        assert!(shared.inputs.contains(&"winget".to_owned()));
        assert!(shared.targets["binaries"].contains(&"x86_64-pc-windows-msvc".to_owned()));
        assert!(shared.targets["winget"].contains(&"aarch64-pc-windows-msvc".to_owned()));
    }

    #[test]
    fn reads_a_workflow_without_inputs() {
        let shared = SharedWorkflow::load(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            "pr.yml",
            "lkurcak/forge/.github/workflows/pr-title.yml@v1",
        )
        .unwrap();
        let call = shared
            .parse_call(include_str!("../.github/workflows/pr.yml"))
            .unwrap()
            .unwrap();
        assert_eq!(shared.inputs, Vec::<String>::new());
        assert_eq!(shared.targets(&call), Vec::<String>::new());
        assert_eq!(shared.problems(&call), Vec::<String>::new());
    }

    #[test]
    fn reads_a_call() {
        let shared = shared();
        let release = shared.parse_call(CALL).unwrap().unwrap();
        assert_eq!(
            release.enabled().collect::<Vec<_>>(),
            ["binaries", "winget"]
        );
        assert_eq!(shared.problems(&release), Vec::<String>::new());

        let targets = shared.targets(&release);
        let windows = targets.iter().filter(|t| t.contains("windows")).count();
        assert_eq!(windows, 2, "x86_64 msvc is listed once: {targets:?}");
    }

    #[test]
    fn flags_bad_calls() {
        let shared = shared();
        let call = CALL.replace("@v1", "@main").replace("winget:", "wignet:");
        let release = shared.parse_call(&call).unwrap().unwrap();
        assert_eq!(shared.problems(&release).len(), 2);
    }

    #[test]
    fn ignores_other_workflows() {
        let call = "jobs:\n  build:\n    runs-on: ubuntu-latest\n";
        assert!(shared().parse_call(call).unwrap().is_none());
    }
}
