//! `forge leaks`: secrets committed to the owner's repos, as gitleaks finds
//! them in every commit a branch, tag or PR reaches. GitHub's secret
//! scanning and push protection cover public repos, but not a user's private
//! ones, so those are what's scanned unless asked otherwise. It only
//! reports: a secret in a repo's history is out wherever the repo was
//! cloned, so it has to be revoked where it was issued, not just deleted.
//!
//! Each repo is fetched into a bare clone in forge's cache. Its whole history
//! is scanned the first time; after that, only commits the refs scanned last
//! didn't reach. Those refs are kept in the clone as
//! `refs/forge/scanned/SHA`, which also keeps their commits from being
//! collected, and the findings beside them, so a finding is reported on
//! every run until the manifest's `[leaks] ignore` lists it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::{ErrorKind, IsTerminal, Write as _, stdout};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use serde::{Deserialize, Serialize};

use crate::gh::Gh;

/// `[leaks]`: findings `forge leaks` no longer reports.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Leaks {
    /// Findings that aren't secrets, or whose secret has been revoked, each
    /// as the id forge prints: `repo:commit:file:rule:line`.
    #[serde(default)]
    pub ignore: Vec<String>,
}

/// What's fetched: every branch and tag, and every PR's head, since a PR's
/// commits stay on GitHub after its branch is deleted or squash-merged.
const REFSPECS: [&str; 3] = [
    "+refs/heads/*:refs/heads/*",
    "+refs/tags/*:refs/tags/*",
    "+refs/pull/*/head:refs/pull/*/head",
];
const REFS: [&str; 3] = ["refs/heads", "refs/tags", "refs/pull"];
/// Where the refs scanned last are kept, one per commit (or tag object).
const SCANNED: &str = "refs/forge/scanned";
/// The commits to scan, as gitleaks' own `git log` options pick them, but
/// only from fetched refs.
const LOG_OPTS: &str = "--full-history --diff-filter=tuxdb --branches --tags --glob=refs/pull/*";
/// Leaves out what an earlier run scanned.
const NOT_SCANNED: &str = "--not --glob=refs/forge/scanned/*";
/// The findings so far, in the bare clone.
const FOUND: &str = "forge-leaks.json";

const INSTALL: &str = "forge leaks scans with gitleaks, which isn't installed: \
     see https://github.com/gitleaks/gitleaks#installing \
     (it's `gitleaks` in Homebrew and most package managers)";

/// A possible secret gitleaks found: where, but not what.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Finding {
    #[serde(rename = "RuleID")]
    rule: String,
    #[serde(rename = "File")]
    file: String,
    #[serde(rename = "StartLine")]
    line: u64,
    #[serde(rename = "Commit")]
    commit: String,
    #[serde(rename = "Date")]
    date: String,
    /// `commit:file:rule:line`.
    #[serde(rename = "Fingerprint")]
    fingerprint: String,
}

/// Scans `names`, or with none every private repo of `owner` (every one,
/// with `public`), forks and archived ones aside. `full` forgets earlier
/// runs and scans all history again. Fails when something is found.
pub fn run(
    gh: &Gh,
    owner: &str,
    names: &[String],
    public: bool,
    full: bool,
    ignore: &[String],
    cache: &Path,
) -> Result<ExitCode> {
    check_gitleaks()?;
    let all = gh.source_repo_summaries(owner)?;
    let repos: Vec<String> = if names.is_empty() {
        all.into_iter()
            .filter(|r| public || !r.public)
            .map(|r| r.name)
            .collect()
    } else {
        for name in names {
            if !all.iter().any(|r| &r.name == name) {
                bail!("{owner} has no repo {name} (forks and archived ones aren't scanned)");
            }
        }
        names.to_vec()
    };

    let root = cache.join("leaks").join(owner);
    let bar = stdout().is_terminal().then(|| {
        let total = u64::try_from(repos.len()).unwrap_or(u64::MAX);
        let bar = ProgressBar::with_draw_target(Some(total), ProgressDrawTarget::stdout())
            .with_style(
                ProgressStyle::with_template("{spinner} scanning {pos}/{len} repos")
                    .expect("valid template"),
            );
        bar.enable_steady_tick(Duration::from_millis(80));
        bar
    });
    let print = |text: &str| match &bar {
        Some(bar) => bar.println(text),
        None => println!("{text}"),
    };

    let (mut found, mut failed) = (0, 0);
    crate::in_parallel(
        &repos,
        |_, name| {
            let dir = root.join(format!("{name}.git"));
            let result =
                fetch(gh, &format!("{owner}/{name}"), &dir).and_then(|()| scan(&dir, full));
            if let Some(bar) = &bar {
                bar.inc(1);
            }
            result
        },
        |i, result| {
            let name = &repos[i];
            match result {
                Ok(findings) => {
                    let shown = unignored(name, &findings, ignore);
                    if !shown.is_empty() {
                        found += 1;
                        print(&report(owner, name, &shown));
                    }
                }
                Err(err) => {
                    failed += 1;
                    print(&format!("! {name}\n    error: {err:#}"));
                }
            }
        },
    );
    if let Some(bar) = bar {
        bar.finish_and_clear();
    }

    let n = repos.len();
    let mut summary = format!("scanned {n} {}", if n == 1 { "repo" } else { "repos" });
    match found {
        0 => summary.push_str(": no secrets found"),
        _ => write!(summary, ": {found} with findings")?,
    }
    if failed > 0 {
        write!(summary, ", {failed} failed")?;
    }
    println!("{summary}");
    if found > 0 {
        println!(
            "Revoke each real secret where it was issued, since every clone has it, then add \
             its id to [leaks] ignore in the manifest, as for a finding that isn't a secret."
        );
    }
    Ok(if found + failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn check_gitleaks() -> Result<()> {
    match Command::new("gitleaks").arg("version").output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => bail!(
            "gitleaks version: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(err) if err.kind() == ErrorKind::NotFound => bail!(INSTALL),
        Err(err) => Err(err).context("running gitleaks"),
    }
}

/// Brings `dir`, a bare clone of `repo` (`owner/name`), up to date, making it
/// the first time. Refs gone from GitHub go here too.
fn fetch(gh: &Gh, repo: &str, dir: &Path) -> Result<()> {
    if !dir.join("HEAD").exists() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        git(dir, &["init", "--bare", "--quiet"], None)?;
    }
    let url = format!("https://github.com/{repo}.git");
    let mut args = vec!["fetch", "--quiet", "--prune", &url];
    args.extend(REFSPECS);
    gh.git(dir, &args)?;
    Ok(())
}

/// Scans the bare clone in `dir` for what's new since the last scan, or all
/// of it with `full`, and gives everything found so far.
fn scan(dir: &Path, full: bool) -> Result<Vec<Finding>> {
    let path = dir.join(FOUND);
    let mut found = BTreeMap::new();
    if !full && path.exists() {
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let known: Vec<Finding> =
            serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
        found.extend(known.into_iter().map(|f| (f.fingerprint.clone(), f)));
    }

    let tips = refs(dir, &REFS, "%(objectname)")?;
    if !tips.is_empty() {
        let log_opts = if full {
            LOG_OPTS.to_owned()
        } else {
            format!("{LOG_OPTS} {NOT_SCANNED}")
        };
        let new = gitleaks(dir, &log_opts)?;
        found.extend(new.into_iter().map(|f| (f.fingerprint.clone(), f)));
    }

    let found: Vec<Finding> = found.into_values().collect();
    fs::write(&path, serde_json::to_string_pretty(&found)?)
        .with_context(|| format!("writing {}", path.display()))?;
    mark_scanned(dir, &tips)?;
    Ok(found)
}

/// Runs gitleaks on the bare clone in `dir`, with secrets redacted and the
/// report read from its stdout, so it's never written anywhere. The clone
/// is the working directory, so a `.gitleaksignore` wherever forge runs
/// isn't picked up.
fn gitleaks(dir: &Path, log_opts: &str) -> Result<Vec<Finding>> {
    let out = Command::new("gitleaks")
        .args(["git", ".", "--redact", "--no-banner", "--no-color"])
        .args(["--log-level", "error", "--exit-code", "0"])
        .args(["--report-format", "json", "--report-path", "-"])
        .args(["--log-opts", log_opts])
        .current_dir(dir)
        .output()
        .context("running gitleaks")?;
    if !out.status.success() {
        bail!("gitleaks: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    serde_json::from_slice(&out.stdout).context("reading gitleaks' report")
}

/// Points `refs/forge/scanned/` at `tips` alone.
fn mark_scanned(dir: &Path, tips: &BTreeSet<String>) -> Result<()> {
    let old: BTreeSet<String> = refs(dir, &[SCANNED], "%(refname:lstrip=3)")?;
    let mut commands = String::new();
    for sha in tips.difference(&old) {
        writeln!(commands, "create {SCANNED}/{sha} {sha}")?;
    }
    for sha in old.difference(tips) {
        writeln!(commands, "delete {SCANNED}/{sha}")?;
    }
    if !commands.is_empty() {
        git(dir, &["update-ref", "--stdin"], Some(&commands))?;
    }
    Ok(())
}

/// Each ref under `prefixes`, as `format` gives it.
fn refs(dir: &Path, prefixes: &[&str], format: &str) -> Result<BTreeSet<String>> {
    let format = format!("--format={format}");
    let mut args = vec!["for-each-ref", &format];
    args.extend(prefixes);
    Ok(git(dir, &args, None)?.lines().map(str::to_owned).collect())
}

/// Runs git in `dir` (no network), with `input` on stdin. Gives its stdout.
fn git(dir: &Path, args: &[&str], input: Option<&str>) -> Result<String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running git")?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    if let Some(input) = input {
        stdin.write_all(input.as_bytes())?;
    }
    drop(stdin);
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The findings in `repo` that `ignore` doesn't list, oldest first.
fn unignored<'a>(repo: &str, findings: &'a [Finding], ignore: &[String]) -> Vec<&'a Finding> {
    let mut shown: Vec<&Finding> = findings
        .iter()
        .filter(|f| !ignore.contains(&id(repo, f)))
        .collect();
    shown.sort_by(|a, b| (&a.date, &a.file, a.line).cmp(&(&b.date, &b.file, b.line)));
    shown
}

/// What `[leaks] ignore` lists a finding as.
fn id(repo: &str, finding: &Finding) -> String {
    format!("{repo}:{}", finding.fingerprint)
}

/// A repo's findings: the rule each matched, when, where on GitHub, and its
/// id. Never the secret.
fn report(owner: &str, repo: &str, findings: &[&Finding]) -> String {
    let n = findings.len();
    let mut text = format!(
        "✗ {repo}: {n} {}",
        if n == 1 { "finding" } else { "findings" }
    );
    for f in findings {
        let date = f.date.get(..10).unwrap_or(&f.date);
        let file = f
            .file
            .replace('%', "%25")
            .replace(' ', "%20")
            .replace('#', "%23");
        write!(
            text,
            "\n    {}, committed {date}: https://github.com/{owner}/{repo}/blob/{}/{file}#L{}\n      id: {}",
            f.rule,
            f.commit,
            f.line,
            id(repo, f)
        )
        .expect("writing to a String");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(file: &str, line: u64, date: &str) -> Finding {
        let commit = "919757064a26c761ec01335230fac587868000c0";
        Finding {
            rule: "github-pat".to_owned(),
            file: file.to_owned(),
            line,
            commit: commit.to_owned(),
            date: date.to_owned(),
            fingerprint: format!("{commit}:{file}:github-pat:{line}"),
        }
    }

    #[test]
    fn reads_gitleaks_report_without_the_secret() {
        let report = r#"[
 {
  "RuleID": "github-pat",
  "Description": "Uncovered a GitHub Personal Access Token",
  "StartLine": 2,
  "EndLine": 2,
  "StartColumn": 11,
  "EndColumn": 50,
  "Match": "REDACTED",
  "Secret": "REDACTED",
  "File": "config.py",
  "SymlinkFile": "",
  "Commit": "919757064a26c761ec01335230fac587868000c0",
  "Entropy": 5.1,
  "Author": "t",
  "Email": "t@t",
  "Date": "2026-11-10T11:10:12Z",
  "Message": "two",
  "Tags": [],
  "Fingerprint": "919757064a26c761ec01335230fac587868000c0:config.py:github-pat:2"
 }
]"#;
        let found: Vec<Finding> = serde_json::from_str(report).unwrap();
        assert_eq!(found, [finding("config.py", 2, "2026-11-10T11:10:12Z")]);
        let stored = serde_json::to_string(&found).unwrap();
        assert!(!stored.contains("REDACTED"), "{stored}");
    }

    #[test]
    fn reports_what_isnt_ignored() {
        let findings = [
            finding("b.env", 1, "2026-02-01T00:00:00Z"),
            finding("a b.py", 7, "2026-01-01T00:00:00Z"),
        ];
        let ignore = [format!("app:{}", findings[0].fingerprint)];
        let shown = unignored("app", &findings, &ignore);
        assert_eq!(shown, [&findings[1]]);
        assert_eq!(
            report("me", "app", &shown),
            "✗ app: 1 finding\n    github-pat, committed 2026-01-01: \
             https://github.com/me/app/blob/919757064a26c761ec01335230fac587868000c0/a%20b.py#L7\n      \
             id: app:919757064a26c761ec01335230fac587868000c0:a b.py:github-pat:7"
        );
    }

    /// Runs gitleaks for real, so only where it's installed.
    #[test]
    fn scans_only_new_commits_but_keeps_reporting() {
        if Command::new("gitleaks").arg("version").output().is_err() {
            eprintln!("gitleaks isn't installed; skipping");
            return;
        }
        let tmp = std::env::temp_dir().join(format!("forge-leaks-{}", std::process::id()));
        let (src, bare) = (tmp.join("src"), tmp.join("bare.git"));
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&bare).unwrap();
        let commit = |file: &str, text: &str| {
            fs::write(src.join(file), text).unwrap();
            git(&src, &["add", "."], None).unwrap();
            git(
                &src,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "-qm",
                    file,
                ],
                None,
            )
            .unwrap();
        };
        let sync = || {
            let mut args = vec!["fetch", "--quiet", "--prune", "../src"];
            args.extend(REFSPECS);
            git(&bare, &args, None).unwrap();
        };
        git(&src, &["init", "--quiet"], None).unwrap();
        git(&bare, &["init", "--bare", "--quiet"], None).unwrap();

        // Split, so nothing scanning this file finds it.
        let key = ["AKIA", "IMNOJVGFDXXXE4OA"].concat();
        commit("readme", "hi\n");
        commit("config.py", &format!("aws_key = \"{key}\"\n"));
        sync();
        let first = scan(&bare, false).unwrap();
        assert_eq!(first.len(), 1, "{first:?}");
        assert_eq!(first[0].file, "config.py");
        let stored = fs::read_to_string(bare.join(FOUND)).unwrap();
        assert!(!stored.contains(&key), "{stored}");

        // Only the new commit is scanned, but what was found is still
        // reported.
        commit("config.py", "aws_key = None\n");
        sync();
        let unscanned = gitleaks(&bare, &format!("{LOG_OPTS} {NOT_SCANNED}")).unwrap();
        assert!(unscanned.is_empty(), "{unscanned:?}");
        assert_eq!(gitleaks(&bare, LOG_OPTS).unwrap(), first);
        assert_eq!(scan(&bare, false).unwrap(), first);
        let scanned = refs(&bare, &[SCANNED], "%(refname:lstrip=3)").unwrap();
        assert_eq!(scanned, refs(&bare, &REFS, "%(objectname)").unwrap());

        assert_eq!(scan(&bare, true).unwrap(), first);
        fs::remove_dir_all(&tmp).unwrap();
    }
}
