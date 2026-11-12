//! The shared workflows, and the repos calling them.
//!
//! A repo opts into one by calling it from its own workflow file, and the
//! inputs it passes are where it configures it (for the release workflow,
//! what it ships). forge only reads the call, treating each input turned on
//! as a tag (so `winget: true` brings the winget tag's secret), and checks it
//! against the shared workflow itself.
//!
//! Its triggers are checked against `callers/{name}.yml`, an example caller
//! kept next to the shared workflow, at the same ref: a caller may leave
//! triggers out, but add none. And a repo's workflow files must each be a
//! call, or listed as the repo's own (`own_workflows`), so a workflow can't
//! be added without the manifest saying so.
//!
//! GitHub won't start a shared workflow at all when one of its jobs asks for
//! a permission the call doesn't grant, even a job that would be skipped, so
//! a call must grant every permission the workflow's jobs ask for.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

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
    /// The `on:` of `callers/{name}.yml`: every trigger a caller may have.
    /// `None` without that file, and then any trigger goes.
    triggers: Option<Value>,
    /// The most each permission is asked for, by the workflow or any job.
    permissions: Permissions,
}

/// A repo's call of a shared workflow.
pub struct Call {
    git_ref: String,
    pub inputs: BTreeMap<String, Value>,
    /// The caller's `on:`.
    on: Value,
    /// Jobs in the caller's file other than the call.
    other_jobs: Vec<String>,
    /// What the call grants: its job's `permissions`, or else the file's.
    /// `None` leaves the repo's default token.
    grant: Option<Permissions>,
}

/// How much of a permission a token has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    None,
    Read,
    Write,
}

impl Level {
    fn parse(value: &Value) -> Self {
        match value.as_str() {
            Some("write") => Self::Write,
            Some("read") => Self::Read,
            _ => Self::None,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

/// A `permissions:` value: one level for every permission (`read-all`,
/// `write-all`), and one per permission listed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Permissions {
    all: Option<Level>,
    each: BTreeMap<String, Level>,
}

impl Permissions {
    /// `None` for a missing `permissions:`.
    fn parse(value: &Value) -> Option<Self> {
        match value {
            Value::Null => None,
            Value::String(all) => Some(Self {
                all: Some(match all.as_str() {
                    "write-all" => Level::Write,
                    "read-all" => Level::Read,
                    _ => Level::None,
                }),
                each: BTreeMap::new(),
            }),
            Value::Object(each) => Some(Self {
                all: None,
                each: each
                    .iter()
                    .map(|(name, level)| (name.clone(), Level::parse(level)))
                    .collect(),
            }),
            _ => Some(Self::default()),
        }
    }

    /// The token workflows get without a `permissions:` of their own, from
    /// the repo's `default_workflow_permissions`: `write` for everything, or
    /// else (`read`, GitHub's default) reading the code and packages.
    pub fn default_token(setting: Option<&str>) -> Self {
        if setting == Some("write") {
            return Self {
                all: Some(Level::Write),
                each: BTreeMap::new(),
            };
        }
        Self {
            all: None,
            each: [("contents", Level::Read), ("packages", Level::Read)]
                .into_iter()
                .map(|(name, level)| (name.to_owned(), level))
                .collect(),
        }
    }

    fn level(&self, name: &str) -> Level {
        self.each
            .get(name)
            .copied()
            .or(self.all)
            .unwrap_or(Level::None)
    }

    /// Raises each permission to at least what `other` has.
    fn add(&mut self, other: &Self) {
        self.all = self.all.max(other.all);
        for (name, &level) in &other.each {
            let entry = self.each.entry(name.clone()).or_insert(Level::None);
            *entry = (*entry).max(level);
        }
    }

    /// What `self` asks for that `grant` doesn't give, as `name: level`.
    fn missing_from(&self, grant: &Self) -> Vec<String> {
        let mut missing: Vec<String> = self
            .each
            .iter()
            .filter(|&(name, &level)| grant.level(name) < level)
            .map(|(name, level)| format!("{name}: {}", level.name()))
            .collect();
        if let Some(all) = self.all
            && grant.all.is_none_or(|granted| granted < all)
        {
            missing.push(format!("{}-all", all.name()));
        }
        missing
    }
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

/// Where a shared workflow lives, from the `uses:` reference callers write,
/// like `owner/repo/.github/workflows/file.yml@v1`.
pub struct Reference {
    /// `owner/repo`.
    pub repo: String,
    /// The file's path in that repo.
    pub file: String,
    pub git_ref: String,
}

impl Reference {
    pub fn parse(uses: &str) -> Result<Self> {
        let (path, git_ref) = uses
            .split_once('@')
            .with_context(|| format!("workflow {uses} needs an @ref"))?;
        let mut parts = path.splitn(3, '/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(owner), Some(repo), Some(file)) if !owner.is_empty() && !repo.is_empty() => {
                Ok(Self {
                    repo: format!("{owner}/{repo}"),
                    file: file.to_owned(),
                    git_ref: git_ref.to_owned(),
                })
            }
            _ => bail!("workflow {uses} isn't owner/repo/path@ref"),
        }
    }

    /// The file's stem, which names the workflow.
    pub fn name(&self) -> &str {
        Path::new(&self.file)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&self.file)
    }
}

impl SharedWorkflow {
    /// Reads the workflow `uses` names from GitHub, at the ref callers use,
    /// with its example caller. Repos call it from `.github/workflows/{caller}`.
    pub fn fetch(gh: &Gh, caller: &str, uses: &str) -> Result<Self> {
        let reference = Reference::parse(uses)?;
        let text = gh
            .file(&reference.repo, &reference.file, Some(&reference.git_ref))?
            .with_context(|| format!("{uses} doesn't exist"))?;
        let example = gh.file(
            &reference.repo,
            &format!("callers/{}.yml", reference.name()),
            Some(&reference.git_ref),
        )?;
        Self::parse(caller, uses, &text, example.as_deref())
    }

    /// Reads the workflow `uses` names from its text, and the triggers
    /// callers may have from its example caller's.
    pub fn parse(caller: &str, uses: &str, text: &str, example: Option<&str>) -> Result<Self> {
        let reference = Reference::parse(uses)?;
        let path = uses.split_once('@').map_or(uses, |(path, _)| path);
        let yaml: Value =
            serde_yaml_ng::from_str(text).with_context(|| format!("parsing {uses}"))?;
        let mut permissions = Permissions::parse(&yaml["permissions"]).unwrap_or_default();
        for job in yaml["jobs"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(_, j)| j)
        {
            if let Some(asked) = Permissions::parse(&job["permissions"]) {
                permissions.add(&asked);
            }
        }
        let triggers = example
            .map(|example| {
                let yaml: Value = serde_yaml_ng::from_str(example)
                    .with_context(|| format!("parsing callers/{}.yml", reference.name()))?;
                anyhow::Ok(yaml["on"].clone())
            })
            .transpose()?;

        let workflow_call = yaml["on"]
            .get("workflow_call")
            .with_context(|| format!("{uses} has no workflow_call trigger"))?;
        let inputs = match workflow_call.get("inputs") {
            None => Vec::new(),
            Some(inputs) => inputs
                .as_object()
                .with_context(|| format!("{uses} has invalid workflow_call inputs"))?
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
            name: reference.name().to_owned(),
            caller: format!(".github/workflows/{caller}"),
            path: path.to_owned(),
            git_ref: reference.git_ref,
            inputs,
            targets,
            triggers,
            permissions,
        })
    }

    /// The file name repos call it from, like `ci.yml`.
    pub fn caller_file(&self) -> &str {
        self.caller.trim_start_matches(".github/workflows/")
    }

    /// Whether `file` in `repo`'s (`owner/name`) .github/workflows is this
    /// workflow itself, in the repo sharing it.
    pub fn is(&self, repo: &str, file: &str) -> bool {
        self.path == format!("{repo}/.github/workflows/{file}")
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
        let other_jobs = yaml["jobs"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, other)| !std::ptr::eq(*other, job))
            .map(|(name, _)| name.clone())
            .collect();
        Ok(Some(Call {
            git_ref: git_ref.to_owned(),
            inputs: job["with"]
                .as_object()
                .map_or_default(|w| w.clone().into_iter().collect()),
            on: yaml["on"].clone(),
            other_jobs,
            grant: Permissions::parse(&job["permissions"])
                .or_else(|| Permissions::parse(&yaml["permissions"])),
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
    /// `default_branch` is what `$default-branch` in the example caller's
    /// triggers stands for, and `default_token` what a call granting no
    /// permissions gets.
    pub fn problems(
        &self,
        call: &Call,
        default_branch: &str,
        default_token: &Permissions,
    ) -> Vec<String> {
        let mut problems = Vec::new();
        let missing = self
            .permissions
            .missing_from(call.grant.as_ref().unwrap_or(default_token));
        if !missing.is_empty() {
            problems.push(format!(
                "{}: {} asks for {}, which the call doesn't grant, so GitHub won't start it",
                self.caller,
                self.name,
                missing.join(", ")
            ));
        }
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
        for job in &call.other_jobs {
            problems.push(format!(
                "{}: job {job} isn't the {} call: move it to a workflow of its own, in own_workflows",
                self.caller, self.name
            ));
        }
        if let Some(triggers) = &self.triggers {
            for extra in extra_triggers(&call.on, triggers, default_branch) {
                problems.push(format!(
                    "{}: {extra} isn't among callers/{}.yml's triggers",
                    self.caller, self.name
                ));
            }
        }
        problems
    }

    pub fn has_input(&self, input: &str) -> bool {
        self.inputs.iter().any(|i| i == input)
    }

    /// With `private_ci = "{input}"`, what's wrong with a call: a private
    /// repo must turn the input on, a public one leave it off. Calls of a
    /// workflow without the input are fine either way.
    pub fn private_ci_problem(&self, call: &Call, input: &str, public: bool) -> Option<String> {
        if !self.has_input(input) {
            return None;
        }
        match (public, call.enabled().any(|i| i == input)) {
            (false, false) => Some(format!(
                "{}: private repo, so {} needs `{input}: true`",
                self.caller, self.name
            )),
            (true, true) => Some(format!(
                "{}: public repo, so {} shouldn't have `{input}: true`",
                self.caller, self.name
            )),
            _ => None,
        }
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

/// What's wrong with the files in a repo's .github/workflows, `files`. Each
/// must call a shared workflow from the file `[workflows]` names for it, be
/// listed in the repo's `own_workflows`, or be one of the shared workflows,
/// in the repo sharing them. `calls` pairs with `workflows`; `repo` is
/// `owner/name`.
pub fn file_problems(
    repo: &str,
    files: &[String],
    workflows: &[SharedWorkflow],
    calls: &[Option<Call>],
    own: &[String],
) -> Vec<String> {
    let mut problems = Vec::new();
    for file in files {
        let path = Path::new(file);
        if !matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("yml" | "yaml")
        ) {
            continue;
        }
        let caller = workflows
            .iter()
            .zip(calls)
            .find(|(w, _)| w.caller_file() == file);
        problems.extend(match caller {
            Some((workflow, Some(_))) => own.contains(file).then(|| {
                format!(
                    "own_workflows lists {file}, which calls {}: take it off, calls are checked as calls",
                    workflow.name
                )
            }),
            _ if own.contains(file) || workflows.iter().any(|w| w.is(repo, file)) => None,
            Some((workflow, None)) => Some(format!(
                ".github/workflows/{file}: doesn't call {}: make it the call, or list it in own_workflows",
                workflow.name
            )),
            None => Some(format!(
                ".github/workflows/{file}: calls no shared workflow: move it into one, or list it in own_workflows"
            )),
        });
    }
    for file in own {
        if !files.contains(file) {
            problems.push(format!(
                "own_workflows lists {file}, which .github/workflows doesn't have"
            ));
        }
    }
    problems
}

/// `on:` as a map of event to filters (null for none), whichever of YAML's
/// forms it's written in: one event, a list of them, or a map.
fn events(on: &Value) -> Map<String, Value> {
    match on {
        Value::String(event) => Map::from_iter([(event.clone(), Value::Null)]),
        Value::Array(events) => events
            .iter()
            .filter_map(|e| Some((e.as_str()?.to_owned(), Value::Null)))
            .collect(),
        Value::Object(events) => events.clone(),
        _ => Map::new(),
    }
}

/// A filter's values: one string, or a list of them. A schedule's are its
/// cron expressions.
fn values(filter: &Value) -> Vec<String> {
    match filter {
        Value::String(value) => vec![value.clone()],
        Value::Array(values) => values
            .iter()
            .filter_map(|v| v.as_str().or_else(|| v["cron"].as_str()))
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// The triggers in `on` that `allowed` doesn't have, described: an event it
/// lacks, a filter value it doesn't list, a filter it doesn't use, or an
/// event without the filters it narrows that event with.
fn extra_triggers(on: &Value, allowed: &Value, default_branch: &str) -> Vec<String> {
    let allowed = events(allowed);
    let mut extra = Vec::new();
    for (event, filters) in events(on) {
        let Some(allowed_filters) = allowed.get(&event) else {
            extra.push(event);
            continue;
        };
        if event == "schedule" {
            let allowed = values(allowed_filters);
            for cron in values(&filters) {
                if !allowed.contains(&cron) {
                    extra.push(format!("schedule {cron}"));
                }
            }
            continue;
        }
        let empty = Map::new();
        let filters = filters.as_object().unwrap_or(&empty);
        let allowed_filters = allowed_filters.as_object().unwrap_or(&empty);
        if filters.is_empty() && !allowed_filters.is_empty() {
            extra.push(format!("{event} without filters"));
            continue;
        }
        for (filter, values_) in filters {
            let Some(allowed_values) = allowed_filters.get(filter) else {
                extra.push(format!("{event}.{filter}"));
                continue;
            };
            let branch = |v: String| {
                if v == "$default-branch" {
                    default_branch.to_owned()
                } else {
                    v
                }
            };
            let allowed_values: Vec<String> =
                values(allowed_values).into_iter().map(branch).collect();
            for value in values(values_) {
                if !allowed_values.contains(&branch(value.clone())) {
                    extra.push(format!("{event}.{filter} {value}"));
                }
            }
        }
    }
    extra
}

/// One of this repo's own shared workflows, read from disk rather than GitHub,
/// with its example caller from callers/.
#[cfg(test)]
pub fn local(caller: &str, uses: &str) -> Result<SharedWorkflow> {
    let reference = Reference::parse(uses)?;
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let example = dir.join(format!("callers/{}.yml", reference.name()));
    SharedWorkflow::parse(
        caller,
        uses,
        &std::fs::read_to_string(dir.join(&reference.file))?,
        std::fs::read_to_string(example).ok().as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GitHub's default token: reading the code and packages.
    fn token() -> Permissions {
        Permissions::default_token(Some("read"))
    }

    fn shared() -> SharedWorkflow {
        local(
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
    permissions:
      contents: write
      id-token: write
      actions: write
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
        let shared = local("pr.yml", "lkurcak/forge/.github/workflows/pr-title.yml@v1").unwrap();
        let call = shared
            .parse_call(include_str!("../.github/workflows/pr.yml"))
            .unwrap()
            .unwrap();
        assert_eq!(shared.inputs, Vec::<String>::new());
        assert_eq!(shared.targets(&call), Vec::<String>::new());
        assert_eq!(
            shared.problems(&call, "main", &token()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn reads_a_call() {
        let shared = shared();
        let release = shared.parse_call(CALL).unwrap().unwrap();
        assert_eq!(
            release.enabled().collect::<Vec<_>>(),
            ["binaries", "winget"]
        );
        assert_eq!(
            shared.problems(&release, "main", &token()),
            Vec::<String>::new()
        );

        let targets = shared.targets(&release);
        let windows = targets.iter().filter(|t| t.contains("windows")).count();
        assert_eq!(windows, 2, "x86_64 msvc is listed once: {targets:?}");
    }

    #[test]
    fn flags_bad_calls() {
        let shared = shared();
        let call = CALL.replace("@v1", "@main").replace("winget:", "wignet:");
        let release = shared.parse_call(&call).unwrap().unwrap();
        assert_eq!(shared.problems(&release, "main", &token()).len(), 2);
    }

    #[test]
    fn private_ci_wants_the_input_on_private_repos_only() {
        let ci = local("ci.yml", "lkurcak/forge/.github/workflows/rust-ci.yml@v1").unwrap();
        let call = |with: &str| {
            let text = format!(
                "jobs:\n  ci:\n    uses: lkurcak/forge/.github/workflows/rust-ci.yml@v1\n{with}"
            );
            ci.parse_call(&text).unwrap().unwrap()
        };
        let light = call("    with:\n      light: true\n");
        let full = call("    with:\n      light: false\n");
        let plain = call("");

        assert_eq!(ci.private_ci_problem(&light, "light", false), None);
        assert_eq!(ci.private_ci_problem(&full, "light", true), None);
        assert_eq!(ci.private_ci_problem(&plain, "light", true), None);
        for (call, public) in [(&light, true), (&full, false), (&plain, false)] {
            assert!(ci.private_ci_problem(call, "light", public).is_some());
        }

        let release = shared();
        let release_call = release.parse_call(CALL).unwrap().unwrap();
        assert!(!release.has_input("light"));
        assert_eq!(
            release.private_ci_problem(&release_call, "light", false),
            None
        );
    }

    #[test]
    fn parses_references() {
        let reference = Reference::parse("a/b/.github/workflows/ci.yml@v2").unwrap();
        assert_eq!(reference.repo, "a/b");
        assert_eq!(reference.file, ".github/workflows/ci.yml");
        assert_eq!(reference.git_ref, "v2");
        assert_eq!(reference.name(), "ci");
        assert!(Reference::parse("a/b/ci.yml").is_err());
        assert!(Reference::parse("a/ci.yml@v1").is_err());
    }

    /// Every shared workflow in this repo, with its caller's file name and
    /// this repo's own call of it, if it has one.
    fn all() -> Vec<(SharedWorkflow, Option<&'static str>)> {
        let uses = |name: &str| format!("lkurcak/forge/.github/workflows/{name}.yml@v1");
        vec![
            (
                local("ci.yml", &uses("rust-ci")).unwrap(),
                Some(include_str!("../.github/workflows/ci.yml")),
            ),
            (
                local("release.yml", &uses("rust-release")).unwrap(),
                Some(include_str!("../.github/workflows/release.yml")),
            ),
            (
                local("pr.yml", &uses("pr-title")).unwrap(),
                Some(include_str!("../.github/workflows/pr.yml")),
            ),
            (local("t.yml", &uses("t-ci")).unwrap(), None),
        ]
    }

    #[test]
    fn examples_and_this_repos_calls_pass() {
        for (shared, own) in all() {
            assert!(shared.triggers.is_some(), "callers/{}.yml", shared.name);
            let example = std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("callers/{}.yml", shared.name)),
            )
            .unwrap();
            for text in [Some(example.as_str()), own].into_iter().flatten() {
                let call = shared.parse_call(text).unwrap().unwrap();
                assert_eq!(
                    shared.problems(&call, "main", &token()),
                    Vec::<String>::new()
                );
            }
        }
    }

    #[test]
    fn callers_may_leave_triggers_out_but_add_none() {
        let ci = local("ci.yml", "lkurcak/forge/.github/workflows/rust-ci.yml@v1").unwrap();
        let problems = |on: &str| {
            let text = format!(
                "on:\n{on}\njobs:\n  ci:\n    uses: lkurcak/forge/.github/workflows/rust-ci.yml@v1\n"
            );
            ci.problems(&ci.parse_call(&text).unwrap().unwrap(), "trunk", &token())
        };
        assert_eq!(
            problems("  push:\n    branches: [trunk]"),
            Vec::<String>::new()
        );
        assert_eq!(
            problems("  - pull_request\n  - workflow_dispatch"),
            Vec::<String>::new()
        );
        assert_eq!(
            problems("  push:\n    branches: [trunk]\n    tags: [\"v*\"]"),
            [".github/workflows/ci.yml: push.tags isn't among callers/rust-ci.yml's triggers"]
        );
        assert_eq!(
            problems("  push:\n    branches: [main]"),
            [
                ".github/workflows/ci.yml: push.branches main isn't among callers/rust-ci.yml's triggers"
            ]
        );
        assert_eq!(
            problems("  push:"),
            [
                ".github/workflows/ci.yml: push without filters isn't among callers/rust-ci.yml's triggers"
            ]
        );
        assert_eq!(
            problems("  schedule:\n    - cron: \"0 0 * * *\""),
            [
                ".github/workflows/ci.yml: schedule 0 0 * * * isn't among callers/rust-ci.yml's triggers"
            ]
        );
        assert_eq!(
            problems("  release:"),
            [".github/workflows/ci.yml: release isn't among callers/rust-ci.yml's triggers"]
        );

        let pr = local("pr.yml", "lkurcak/forge/.github/workflows/pr-title.yml@v1").unwrap();
        let call = |types: &str| {
            let text = format!(
                "on:\n  pull_request:\n    types: [{types}]\njobs:\n  t:\n    uses: lkurcak/forge/.github/workflows/pr-title.yml@v1\n"
            );
            pr.problems(&pr.parse_call(&text).unwrap().unwrap(), "main", &token())
        };
        assert_eq!(call("opened, edited, reopened"), Vec::<String>::new());
        assert_eq!(call("opened, closed").len(), 1);
    }

    #[test]
    fn a_caller_holds_the_call_alone() {
        let ci = local("ci.yml", "lkurcak/forge/.github/workflows/rust-ci.yml@v1").unwrap();
        let text = "on: pull_request\njobs:\n  ci:\n    uses: lkurcak/forge/.github/workflows/rust-ci.yml@v1\n  extra:\n    runs-on: ubuntu-latest\n";
        let call = ci.parse_call(text).unwrap().unwrap();
        assert_eq!(
            ci.problems(&call, "main", &token()),
            [
                ".github/workflows/ci.yml: job extra isn't the rust-ci call: move it to a workflow of its own, in own_workflows"
            ]
        );
    }

    #[test]
    fn workflow_files_are_calls_or_listed() {
        let workflows: Vec<SharedWorkflow> = all().into_iter().map(|(w, _)| w).collect();
        let ci = include_str!("../.github/workflows/ci.yml");
        let calls: Vec<Option<Call>> = workflows
            .iter()
            .map(|w| (w.name == "rust-ci").then(|| w.parse_call(ci).unwrap().unwrap()))
            .collect();
        let files = |names: &[&str]| names.iter().map(|&n| n.to_owned()).collect::<Vec<_>>();
        let problems = |repo: &str, present: &[&str], own: &[&str]| {
            file_problems(repo, &files(present), &workflows, &calls, &files(own))
        };

        assert_eq!(
            problems(
                "me/app",
                &["ci.yml", "build.yml", "notes.md"],
                &["build.yml"]
            ),
            Vec::<String>::new()
        );
        assert_eq!(
            problems("me/app", &["ci.yml", "build.yml"], &[]),
            [
                ".github/workflows/build.yml: calls no shared workflow: move it into one, or list it in own_workflows"
            ]
        );
        assert_eq!(
            problems("me/app", &["release.yml"], &[]),
            [
                ".github/workflows/release.yml: doesn't call rust-release: make it the call, or list it in own_workflows"
            ]
        );
        assert_eq!(
            problems("me/app", &["release.yml"], &["release.yml"]),
            Vec::<String>::new()
        );
        assert_eq!(
            problems("me/app", &["ci.yml"], &["ci.yml", "gone.yml"]).len(),
            2
        );
        // The shared workflows themselves, where they're shared from.
        assert_eq!(
            problems("lkurcak/forge", &["rust-ci.yml", "t-ci.yml"], &[]),
            Vec::<String>::new()
        );
        assert_eq!(problems("me/app", &["rust-ci.yml"], &[]).len(), 1);
    }

    #[test]
    fn a_call_grants_what_the_workflow_asks_for() {
        let release = shared();
        assert_eq!(
            release.permissions.missing_from(&token()),
            ["actions: write", "contents: write", "id-token: write"]
        );
        // The release.yml forge's rust-release gained the tag job under.
        let before = CALL.replace("      actions: write\n", "");
        let call = release.parse_call(&before).unwrap().unwrap();
        assert_eq!(
            release.problems(&call, "main", &token()),
            [
                ".github/workflows/release.yml: rust-release asks for actions: write, which the call doesn't grant, so GitHub won't start it"
            ]
        );

        // A file-wide grant counts, and so does the default token.
        let ci = local("ci.yml", "lkurcak/forge/.github/workflows/rust-ci.yml@v1").unwrap();
        let call = |text: &str| ci.parse_call(text).unwrap().unwrap();
        let uses = "jobs:\n  ci:\n    uses: lkurcak/forge/.github/workflows/rust-ci.yml@v1\n";
        let none = call(&format!("permissions: {{}}\n{uses}"));
        assert_eq!(ci.problems(&none, "main", &token()).len(), 1);
        assert_eq!(
            ci.problems(&call(uses), "main", &token()),
            Vec::<String>::new()
        );
        let read_all = call(&format!("permissions: read-all\n{uses}"));
        assert_eq!(
            ci.problems(&read_all, "main", &token()),
            Vec::<String>::new()
        );
        let nothing = Permissions::default_token(None).missing_from(&token());
        assert_eq!(nothing, Vec::<String>::new());
    }

    #[test]
    fn ignores_other_workflows() {
        let call = "jobs:\n  build:\n    runs-on: ubuntu-latest\n";
        assert!(shared().parse_call(call).unwrap().is_none());
    }
}
