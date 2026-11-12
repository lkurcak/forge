//! Workflow runs that built the same commit more than once.
//!
//! A branch and a tag pushed together each start a run of a workflow that
//! both trigger, so a release can build twice, side by side, and the second
//! run is billed like the first. Only checked on private repos, whose
//! Actions minutes are billed, and only with `private_ci` set.

use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::Value;

use crate::days;
use crate::gh::Gh;

/// How many days of pushes are looked at.
const DAYS: i64 = 7;

/// A push's run of a workflow.
struct Run {
    id: u64,
    /// The branch or tag pushed.
    git_ref: String,
    created_at: String,
}

/// A line per workflow file that more than one push run did work on (gave
/// a job a runner) for the same commit, in the last week. `base` is
/// `repos/{owner}/{name}`.
pub fn duplicates(gh: &Gh, base: &str) -> Result<Vec<String>> {
    let since = days::format(days::today() - DAYS);
    let runs = gh.get(&format!(
        "{base}/actions/runs?event=push&created=%3E%3D{since}&per_page=100"
    ))?;
    let mut found: BTreeMap<String, Vec<(String, Vec<Run>)>> = BTreeMap::new();
    for ((file, sha), runs) in by_commit(&runs) {
        if runs.len() < 2 {
            continue;
        }
        let mut worked = Vec::new();
        for run in runs {
            let jobs = gh.get(&format!("{base}/actions/runs/{}/jobs?per_page=100", run.id))?;
            if got_a_runner(&jobs) {
                worked.push(run);
            }
        }
        if worked.len() > 1 {
            found.entry(file).or_default().push((sha, worked));
        }
    }
    Ok(found
        .into_iter()
        .map(|(file, commits)| describe(&file, &commits))
        .collect())
}

/// Push runs by workflow file and commit.
fn by_commit(runs: &Value) -> BTreeMap<(String, String), Vec<Run>> {
    let mut grouped: BTreeMap<(String, String), Vec<Run>> = BTreeMap::new();
    for run in runs["workflow_runs"].as_array().into_iter().flatten() {
        let (Some(path), Some(sha), Some(id)) = (
            run["path"].as_str(),
            run["head_sha"].as_str(),
            run["id"].as_u64(),
        ) else {
            continue;
        };
        let file = path.to_owned();
        grouped
            .entry((file, sha.to_owned()))
            .or_default()
            .push(Run {
                id,
                git_ref: run["head_branch"].as_str().unwrap_or("?").to_owned(),
                created_at: run["created_at"].as_str().unwrap_or_default().to_owned(),
            });
    }
    grouped
}

/// Whether any of a run's jobs ran: skipped ones never get a runner, and
/// cost nothing.
fn got_a_runner(jobs: &Value) -> bool {
    jobs["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|job| !job["runner_name"].is_null())
}

fn describe(file: &str, commits: &[(String, Vec<Run>)]) -> String {
    let latest = commits
        .iter()
        .max_by_key(|(_, runs)| runs.iter().map(|r| r.created_at.as_str()).max())
        .expect("only workflows with duplicates are described");
    let (sha, runs) = latest;
    let mut refs: Vec<&str> = runs.iter().map(|r| r.git_ref.as_str()).collect();
    refs.sort_unstable();
    let count = commits.len();
    let commits = if count == 1 { "commit" } else { "commits" };
    format!(
        "{file}: built {count} {commits} more than once in the last {DAYS} days (latest {}, pushed as {}): \
         each ref pushed starts its own run, so keep one trigger",
        sha.get(..7).unwrap_or(sha),
        refs.join(" and "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(id: u64, sha: &str, git_ref: &str, created_at: &str) -> Value {
        json!({
            "id": id,
            "path": ".github/workflows/publish.yml",
            "head_sha": sha,
            "head_branch": git_ref,
            "created_at": created_at,
        })
    }

    #[test]
    fn groups_runs_by_workflow_and_commit() {
        let runs = json!({ "workflow_runs": [
            run(4, "bbbbbbbbb", "0.2.0", "2026-11-12T02:35:14Z"),
            run(3, "bbbbbbbbb", "main", "2026-11-12T02:35:04Z"),
            run(2, "aaaaaaaaa", "main", "2026-11-11T10:00:00Z"),
        ]});
        let grouped = by_commit(&runs);
        assert_eq!(grouped.len(), 2);
        let twice = &grouped[&(
            ".github/workflows/publish.yml".to_owned(),
            "bbbbbbbbb".to_owned(),
        )];
        assert_eq!(twice.iter().map(|r| r.id).collect::<Vec<_>>(), [4, 3]);
    }

    #[test]
    fn only_jobs_given_a_runner_count() {
        let skipped = json!({ "jobs": [{ "conclusion": "skipped", "runner_name": null }] });
        let ran =
            json!({ "jobs": [{ "conclusion": "success", "runner_name": "GitHub Actions 1" }] });
        assert!(!got_a_runner(&skipped));
        assert!(got_a_runner(&ran));
    }

    #[test]
    fn describes_the_latest_commit() {
        let runs = |sha: &str, day: &str| {
            let runs = json!({ "workflow_runs": [
                run(1, sha, "main", &format!("2026-11-{day}T00:00:00Z")),
                run(2, sha, "0.1.0", &format!("2026-11-{day}T00:00:10Z")),
            ]});
            by_commit(&runs).into_values().next().unwrap()
        };
        let line = describe(
            ".github/workflows/publish.yml",
            &[
                ("aaaaaaaaa".to_owned(), runs("aaaaaaaaa", "10")),
                ("bbbbbbbbb".to_owned(), runs("bbbbbbbbb", "12")),
            ],
        );
        assert!(
            line.starts_with(".github/workflows/publish.yml: built 2 commits more than once"),
            "{line}"
        );
        assert!(
            line.contains("latest bbbbbbb, pushed as 0.1.0 and main"),
            "{line}"
        );
    }
}
