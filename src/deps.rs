//! `forge deps`: each Rust repo's Cargo.lock moved to the newest versions
//! its Cargo.toml allows, in one PR, opened once clippy and the tests pass
//! on this machine. It stands in for a bot's PR per crate, each running CI,
//! on Actions minutes that may be billed.
//!
//! Repos are cloned into forge's cache directory and kept there, build cache
//! and all, for the next run. A path dependency leading out of the repo, like
//! `../../libs/x`, is cloned from the owner's repo named like its last part,
//! to where the path expects it.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;
use toml::Table;

use crate::gh::{Gh, NewPr};
use crate::manifest::{Manifest, Repo};
use crate::progress::Progress;

/// The branch forge's PR comes from.
pub const BRANCH: &str = "forge/deps";
const TITLE: &str = "chore(deps): update dependencies";

/// What the updated Cargo.lock must pass, as `rust-ci.yml` runs it.
const CHECKS: [&[&str]; 2] = [
    &[
        "clippy",
        "--workspace",
        "--all-targets",
        "--all-features",
        "--locked",
        "--",
        "-D",
        "warnings",
    ],
    &["test", "--workspace", "--all-features", "--locked"],
];

/// `[deps]`: which repos `forge deps` leaves alone.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deps {
    /// Repos with any of these tags are only updated when they have open
    /// Dependabot alerts about their Rust crates (or when named).
    #[serde(default)]
    pub alerts_only: Vec<String>,
}

/// A repo to update.
struct Target {
    /// `owner/name`.
    full: String,
    branch: String,
    auto_merge: bool,
}

enum Plan {
    Update(Target),
    /// Left alone, and why.
    Skip(String),
    /// No Cargo.lock at the root, so nothing to update.
    NoLock,
}

/// What `cargo update` changed, and what it couldn't.
#[derive(Default, Debug, PartialEq, Eq)]
struct Update {
    changes: Vec<Change>,
    held: Vec<Held>,
}

/// A crate whose locked version changed: `None` on one side when it was
/// added or removed.
#[derive(Debug, PartialEq, Eq)]
struct Change {
    name: String,
    from: Option<String>,
    to: Option<String>,
}

/// A crate with a newer version than its Cargo.toml requirement allows.
#[derive(Debug, PartialEq, Eq)]
struct Held {
    name: String,
    locked: String,
    available: String,
}

enum Done {
    UpToDate,
    Opened(String),
    WouldOpen,
    /// A check failed: its command, and the log of the checks.
    Failed(String, PathBuf),
}

/// Updates `repos`, one at a time, since the checks build them. `named` when
/// they were named on the command line: then `[deps] alerts_only` doesn't
/// apply, and a repo without a Cargo.lock is reported. Clones live under
/// `cache/repos`, check logs under `cache/logs`.
pub fn run(
    gh: &Gh,
    manifest: &Manifest,
    repos: &[&Repo],
    named: bool,
    dry_run: bool,
    cache: &Path,
) -> ExitCode {
    let mut plans: Vec<Option<Result<Plan>>> = repos.iter().map(|_| None).collect();
    crate::in_parallel(
        repos,
        |_, repo| plan(gh, manifest, repo, named),
        |i, plan| plans[i] = Some(plan),
    );

    let mut failed = false;
    let mut targets = Vec::new();
    for (repo, plan) in repos.iter().zip(plans) {
        match plan.expect("every repo is planned") {
            Ok(Plan::Update(target)) => targets.push((repo.name.as_str(), target)),
            Ok(Plan::Skip(why)) => println!("- {}: {why}", repo.name),
            Ok(Plan::NoLock) if named => println!("- {}: no Cargo.lock at the root", repo.name),
            Ok(Plan::NoLock) => {}
            Err(err) => {
                failed = true;
                println!("! {}\n    error: {err:#}", repo.name);
            }
        }
    }

    let root = cache.join("repos");
    let logs = cache.join("logs");
    let lines: Vec<&str> = targets.iter().map(|(name, _)| *name).collect();
    let progress = Progress::new(&lines);
    for (i, (name, target)) in targets.iter().enumerate() {
        progress.start(i);
        let log = logs.join(format!("{name}.log"));
        let result = update(gh, target, &root, &log, dry_run);
        let (mark, text) = match &result {
            Ok((update, done)) => report(update, done),
            Err(err) => ("!", format!("\n    error: {err:#}")),
        };
        failed |= matches!(mark, "!" | "✗");
        progress.finish(i, mark);
        progress.report(i, &format!("{mark} {name}{text}"));
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Whether and how to update `repo`, from what the API says of it.
fn plan(gh: &Gh, manifest: &Manifest, repo: &Repo, named: bool) -> Result<Plan> {
    let full = format!("{}/{}", manifest.owner, repo.name);
    if gh.file(&full, "Cargo.lock", None)?.is_none() {
        return Ok(Plan::NoLock);
    }
    if let Some(url) = gh.open_pr_from(&full, BRANCH)? {
        return Ok(Plan::Skip(format!("{url} is still open")));
    }
    if !named
        && let Some(tag) = repo
            .tags
            .iter()
            .find(|t| manifest.deps.alerts_only.contains(t))
        && !has_alerts(gh, &full)?
    {
        return Ok(Plan::Skip(format!(
            "tagged {tag}, and no open Dependabot alerts"
        )));
    }
    let info = gh.get(&format!("repos/{full}"))?;
    Ok(Plan::Update(Target {
        branch: info["default_branch"]
            .as_str()
            .context("no default branch")?
            .to_owned(),
        auto_merge: info["allow_auto_merge"] == true,
        full,
    }))
}

/// Whether `repo` has open Dependabot alerts about Rust crates.
fn has_alerts(gh: &Gh, repo: &str) -> Result<bool> {
    let path = format!("repos/{repo}/dependabot/alerts?state=open&ecosystem=rust&per_page=1");
    match gh.get(&path) {
        Err(err) if err.to_string().contains("alerts are disabled") => Ok(false),
        result => Ok(result?.as_array().is_some_and(|a| !a.is_empty())),
    }
}

/// Clones or refreshes the repo, updates Cargo.lock, checks it, and opens the
/// PR.
fn update(
    gh: &Gh,
    target: &Target,
    root: &Path,
    log: &Path,
    dry_run: bool,
) -> Result<(Update, Done)> {
    let dir = root.join(&target.full);
    let head = checkout(gh, &target.full, &target.branch, &dir)?;
    let owner = target.full.split('/').next().unwrap_or_default();
    fetch_path_dependencies(gh, owner, &dir, root)?;

    let lock = dir.join("Cargo.lock");
    let before = fs::read(&lock).context("reading Cargo.lock")?;
    let out = cargo(&dir, &["update", "--verbose"])
        .output()
        .context("running cargo")?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        bail!("cargo update: {}", stderr.trim());
    }
    let mut update = parse_update(&stderr);
    let direct = direct_dependencies(&dir)?;
    update.held.retain(|h| direct.contains(&h.name));

    let after = fs::read_to_string(&lock).context("reading Cargo.lock")?;
    if before == after.as_bytes() {
        return Ok((update, Done::UpToDate));
    }

    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent)?;
    }
    File::create(log).with_context(|| format!("creating {}", log.display()))?;
    for check in CHECKS {
        if !run_logged(&dir, check, log)? {
            let command = format!("cargo {}", check[0]);
            return Ok((update, Done::Failed(command, log.to_owned())));
        }
    }
    if dry_run {
        return Ok((update, Done::WouldOpen));
    }

    let url = gh.open_pr(&NewPr {
        repo: &target.full,
        base: &target.branch,
        parent: &head,
        branch: BRANCH,
        files: &[("Cargo.lock", &after)],
        title: TITLE,
        body: &body(&update),
    })?;
    if target.auto_merge {
        gh.auto_merge(&url)
            .with_context(|| format!("opened {url}"))?;
    }
    Ok((update, Done::Opened(url)))
}

/// A repo's mark and the lines after its name.
fn report(update: &Update, done: &Done) -> (&'static str, String) {
    let n = update.changes.len();
    let crates = if n == 1 { "crate" } else { "crates" };
    let (mark, mut text) = match done {
        Done::UpToDate => ("✓", ": up to date".to_owned()),
        Done::Opened(url) => ("↻", format!(": {n} {crates} updated in {url}")),
        Done::WouldOpen => ("↻", format!(": {n} {crates} to update; would open a PR")),
        Done::Failed(command, log) => {
            let tail = fs::read_to_string(log).unwrap_or_default();
            let lines: Vec<&str> = tail.lines().filter(|l| !l.trim().is_empty()).collect();
            let mut text = format!(
                ": {n} {crates} to update, but {command} failed (log: {})",
                log.display()
            );
            for line in &lines[lines.len().saturating_sub(12)..] {
                write!(text, "\n    | {line}").expect("writing to a String");
            }
            ("✗", text)
        }
    };
    for held in &update.held {
        write!(
            text,
            "\n    Cargo.toml holds back {} {} ({} is out)",
            held.name, held.locked, held.available
        )
        .expect("writing to a String");
    }
    (mark, text)
}

/// The PR's description: what changed, and what Cargo.toml holds back.
fn body(update: &Update) -> String {
    let mut body = String::from(
        "Cargo.lock at the newest versions Cargo.toml allows.\n\n\
         | Crate | From | To |\n| --- | --- | --- |\n",
    );
    for change in &update.changes {
        writeln!(
            body,
            "| {} | {} | {} |",
            change.name,
            change.from.as_deref().unwrap_or("(new)"),
            change.to.as_deref().unwrap_or("(removed)")
        )
        .expect("writing to a String");
    }
    if !update.held.is_empty() {
        body.push_str("\nCargo.toml would have to change for these:\n\n");
        for held in &update.held {
            writeln!(body, "- {} {} → {}", held.name, held.locked, held.available)
                .expect("writing to a String");
        }
    }
    body.push_str(
        "\nOpened by `forge deps`, after clippy and the tests passed with these versions.\n",
    );
    body
}

/// `cargo` in `dir`, with plain output. The repo's rust-toolchain.toml picks
/// the toolchain, as in CI, even when forge runs under `cargo run`, which
/// sets `RUSTUP_TOOLCHAIN` to its own.
fn cargo(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.args(args)
        .current_dir(dir)
        .env("CARGO_TERM_COLOR", "never")
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("RUSTUP_TOOLCHAIN_SOURCE");
    cmd
}

/// Runs cargo with `args` in `dir`, appending its output to `log`. Gives
/// whether it passed.
fn run_logged(dir: &Path, args: &[&str], log: &Path) -> Result<bool> {
    let mut file = OpenOptions::new()
        .append(true)
        .open(log)
        .with_context(|| format!("opening {}", log.display()))?;
    writeln!(file, "$ cargo {}", args.join(" "))?;
    let status = cargo(dir, args)
        .stdout(file.try_clone()?)
        .stderr(file)
        .status()
        .context("running cargo")?;
    Ok(status.success())
}

/// Puts the tip of `branch` of `repo` (`owner/name`) in `dir`, cloning it
/// there the first time, and gives its commit. Files git ignores, like
/// `target/`, stay, so builds there stay incremental.
fn checkout(gh: &Gh, repo: &str, branch: &str, dir: &Path) -> Result<String> {
    if !dir.join(".git").exists() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        gh.git(dir, &["init", "--quiet"])?;
    }
    let url = format!("https://github.com/{repo}.git");
    let refspec = format!("refs/heads/{branch}");
    gh.git(
        dir,
        &[
            "fetch",
            "--quiet",
            "--depth",
            "1",
            "--no-tags",
            &url,
            &refspec,
        ],
    )?;
    gh.git(
        dir,
        &["checkout", "--quiet", "--force", "--detach", "FETCH_HEAD"],
    )?;
    gh.git(dir, &["clean", "--quiet", "--force", "-d"])?;
    Ok(gh.git(dir, &["rev-parse", "HEAD"])?.trim().to_owned())
}

/// Clones the path dependencies leading out of `dir`, and out of those in
/// turn, from `owner`'s repos named like each path's last part. They must
/// land inside `root`, forge's own directory.
fn fetch_path_dependencies(gh: &Gh, owner: &str, dir: &Path, root: &Path) -> Result<()> {
    let mut queue = vec![dir.to_owned()];
    let mut seen = BTreeSet::new();
    while let Some(dir) = queue.pop() {
        for (manifest, path) in outside_paths(&dir)? {
            if !seen.insert(path.clone()) {
                continue;
            }
            let from = || format!("{}: path dependency {}", manifest.display(), path.display());
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .with_context(from)?;
            if !path.starts_with(root) {
                bail!("{}, which leads out of {}", from(), root.display());
            }
            let repo = format!("{owner}/{name}");
            let info = gh
                .get(&format!("repos/{repo}"))
                .with_context(|| format!("{}, from {repo}", from()))?;
            let branch = info["default_branch"]
                .as_str()
                .context("no default branch")?;
            checkout(gh, &repo, branch, &path)?;
            queue.push(path);
        }
    }
    Ok(())
}

/// The path dependencies of `dir`'s Cargo workspace that lead out of it,
/// resolved, with the manifest naming each. Follows workspace members and
/// path dependencies inside `dir`.
fn outside_paths(dir: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut queue = vec![dir.join("Cargo.toml")];
    let mut seen = BTreeSet::new();
    let mut found = Vec::new();
    while let Some(manifest) = queue.pop() {
        if !seen.insert(manifest.clone()) {
            continue;
        }
        // A missing one is cargo's to report.
        let Ok(text) = fs::read_to_string(&manifest) else {
            continue;
        };
        let table: Table = text
            .parse()
            .with_context(|| format!("parsing {}", manifest.display()))?;
        let here = manifest.parent().unwrap_or(dir);
        for member in members(&table, here)? {
            queue.push(member.join("Cargo.toml"));
        }
        for path in dependency_paths(&table) {
            let path = normalize(&here.join(path));
            if path.starts_with(dir) {
                queue.push(path.join("Cargo.toml"));
            } else {
                found.push((manifest.clone(), path));
            }
        }
    }
    Ok(found)
}

/// The directories of a workspace's members, in `here`. A member ending in
/// `/*` is every directory in its parent with a Cargo.toml.
fn members(manifest: &Table, here: &Path) -> Result<Vec<PathBuf>> {
    let members = manifest
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str);
    let mut dirs = Vec::new();
    for member in members {
        match member.strip_suffix("/*") {
            Some(parent) => {
                let parent = here.join(parent);
                for entry in fs::read_dir(&parent)
                    .with_context(|| format!("listing {}", parent.display()))?
                {
                    let path = entry?.path();
                    if path.join("Cargo.toml").exists() {
                        dirs.push(path);
                    }
                }
            }
            None => dirs.push(here.join(member)),
        }
    }
    Ok(dirs)
}

/// The `path` of every dependency a manifest declares: for any target, in
/// `[workspace.dependencies]`, or patching another source.
fn dependency_paths(manifest: &Table) -> Vec<&str> {
    const KINDS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
    fn table(value: Option<&toml::Value>) -> Option<&Table> {
        value.and_then(toml::Value::as_table)
    }
    let mut tables: Vec<&Table> = KINDS
        .iter()
        .filter_map(|k| table(manifest.get(*k)))
        .collect();
    for target in table(manifest.get("target"))
        .into_iter()
        .flat_map(Table::values)
    {
        tables.extend(KINDS.iter().filter_map(|k| table(target.get(*k))));
    }
    tables.extend(table(
        manifest
            .get("workspace")
            .and_then(|w| w.get("dependencies")),
    ));
    for patches in table(manifest.get("patch"))
        .into_iter()
        .flat_map(Table::values)
    {
        tables.extend(patches.as_table());
    }
    tables
        .iter()
        .flat_map(|t| t.values())
        .filter_map(|dep| dep.get("path")?.as_str())
        .collect()
}

/// `path` with `.` and `..` resolved without asking the file system, since
/// it may not exist yet.
fn normalize(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normal.pop();
            }
            Component::CurDir => {}
            component => normal.push(component),
        }
    }
    normal
}

/// The packages the workspace's members depend on by name.
fn direct_dependencies(dir: &Path) -> Result<BTreeSet<String>> {
    let out = cargo(dir, &["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .context("running cargo")?;
    if !out.status.success() {
        bail!(
            "cargo metadata: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let metadata: Value = serde_json::from_slice(&out.stdout).context("reading cargo metadata")?;
    Ok(metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|p| p["dependencies"].as_array().into_iter().flatten())
        .filter_map(|d| d["name"].as_str().map(str::to_owned))
        .collect())
}

/// What `cargo update --verbose` reports on stderr: `Updating a v1 -> v2`,
/// `Adding`, `Removing`, `Downgrading`, and `Unchanged a v1 (available: v2)`
/// for one its requirement holds back.
fn parse_update(output: &str) -> Update {
    let version = |words: &[&str]| -> Option<String> {
        let words = words.join(" ");
        let words = words.strip_prefix('v').unwrap_or(&words);
        (!words.is_empty()).then(|| words.to_owned())
    };
    let mut update = Update::default();
    for line in output.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let [verb, name, rest @ ..] = words.as_slice() else {
            continue;
        };
        let name = (*name).to_owned();
        match *verb {
            // `Updating crates.io index` has no arrow.
            "Updating" | "Downgrading" => {
                if let Some(arrow) = rest.iter().position(|w| *w == "->") {
                    update.changes.push(Change {
                        name,
                        from: version(&rest[..arrow]),
                        to: version(&rest[arrow + 1..]),
                    });
                }
            }
            "Adding" => update.changes.push(Change {
                name,
                from: None,
                to: version(&rest[..rest.len().min(1)]),
            }),
            "Removing" => update.changes.push(Change {
                name,
                from: version(&rest[..rest.len().min(1)]),
                to: None,
            }),
            "Unchanged" => {
                if let [locked, "(available:", available, ..] = rest {
                    update.held.push(Held {
                        name,
                        locked: locked.trim_start_matches('v').to_owned(),
                        available: available
                            .trim_start_matches('v')
                            .trim_end_matches([')', ','])
                            .to_owned(),
                    });
                }
            }
            _ => {}
        }
    }
    update
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cargo_update() {
        let output = "    Updating crates.io index
     Locking 3 packages to latest compatible versions
    Updating anyhow v1.0.90 -> v1.0.104
    Updating toml v1.1.7+spec-1.1.0 -> v1.2.1+spec-1.1.0
      Adding foldhash v0.2.0
    Removing once_cell v1.21.4
   Unchanged itoa v1.0.18 (available: v1.0.19)
   Unchanged semver v0.11.0 (available: v1.0.28, requires Rust 1.90)
note: to see how you depend on a package, run `cargo tree --invert <dep>@<ver>`
";
        let change = |name: &str, from: Option<&str>, to: Option<&str>| Change {
            name: name.to_owned(),
            from: from.map(str::to_owned),
            to: to.map(str::to_owned),
        };
        let held = |name: &str, locked: &str, available: &str| Held {
            name: name.to_owned(),
            locked: locked.to_owned(),
            available: available.to_owned(),
        };
        assert_eq!(
            parse_update(output),
            Update {
                changes: vec![
                    change("anyhow", Some("1.0.90"), Some("1.0.104")),
                    change("toml", Some("1.1.7+spec-1.1.0"), Some("1.2.1+spec-1.1.0")),
                    change("foldhash", None, Some("0.2.0")),
                    change("once_cell", Some("1.21.4"), None),
                ],
                held: vec![
                    held("itoa", "1.0.18", "1.0.19"),
                    held("semver", "0.11.0", "1.0.28"),
                ],
            }
        );
    }

    #[test]
    fn finds_dependency_paths() {
        let manifest: Table = r#"
[dependencies]
a = { path = "../a" }
serde = "1"
[dev-dependencies]
b = { path = "b", version = "1" }
[target.'cfg(unix)'.build-dependencies]
c = { path = "../../c" }
[workspace.dependencies]
d = { path = "crates/d" }
[patch.crates-io]
e = { path = "../e" }
"#
        .parse()
        .unwrap();
        let mut paths = dependency_paths(&manifest);
        paths.sort_unstable();
        assert_eq!(paths, ["../../c", "../a", "../e", "b", "crates/d"]);
    }

    #[test]
    fn finds_paths_leading_out_of_the_workspace() {
        let dir = std::env::temp_dir().join(format!("forge-deps-{}", std::process::id()));
        let repo = dir.join("me/game");
        fs::create_dir_all(repo.join("crates/client")).unwrap();
        fs::create_dir_all(repo.join("crates/core")).unwrap();
        fs::create_dir_all(repo.join("crates/notes")).unwrap();
        fs::write(
            repo.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        fs::write(
            repo.join("crates/client/Cargo.toml"),
            "[dependencies]\ncore = { path = \"../core\" }\ncmd = { path = \"../../../../libs/cmd\" }\n",
        )
        .unwrap();
        fs::write(
            repo.join("crates/core/Cargo.toml"),
            "[dependencies]\nserde = \"1\"\n",
        )
        .unwrap();
        let found = outside_paths(&repo).unwrap();
        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(
            found,
            [(repo.join("crates/client/Cargo.toml"), dir.join("libs/cmd"))]
        );
    }

    #[test]
    fn describes_the_update() {
        let update = Update {
            changes: vec![Change {
                name: "anyhow".to_owned(),
                from: Some("1.0.90".to_owned()),
                to: Some("1.0.104".to_owned()),
            }],
            held: vec![Held {
                name: "semver".to_owned(),
                locked: "0.11.0".to_owned(),
                available: "1.0.28".to_owned(),
            }],
        };
        let body = body(&update);
        assert!(body.contains("| anyhow | 1.0.90 | 1.0.104 |"), "{body}");
        assert!(body.contains("- semver 0.11.0 → 1.0.28"), "{body}");
    }
}
