//! Dependabot alerts, and PRs bots opened that nobody merged.
//!
//! These only report: fixing them means merging, bumping a dependency, or
//! dismissing an alert that doesn't apply.

use std::collections::BTreeSet;

use anyhow::Result;
use serde_json::Value;

use crate::days;
use crate::gh::Gh;
use crate::release::Findings;

/// How many days a bot's PR may stay open before it's worth a look.
const BOT_PR_DAYS: i64 = 7;

/// Open alerts, each pending while a young bot PR fixes it, and bot PRs
/// open for longer than [`BOT_PR_DAYS`]. Repos with alerts turned off have none.
pub fn check(gh: &Gh, base: &str) -> Result<Findings> {
    let alerts = match gh.get(&format!("{base}/dependabot/alerts?state=open&per_page=100")) {
        Err(err) if err.to_string().contains("alerts are disabled") => Value::Null,
        result => result?,
    };
    let pulls = gh.get(&format!("{base}/pulls?state=open&per_page=100"))?;
    let bot_prs: Vec<&Value> = pulls
        .as_array()
        .into_iter()
        .flatten()
        .filter(|pr| pr["user"]["type"] == "Bot")
        .collect();
    let today = days::today();
    let open_for = |pr: &Value| {
        pr["created_at"]
            .as_str()
            .and_then(days::parse)
            .map_or(0, |created| today - created)
    };

    let mut findings = Findings::default();
    let mut fixes = BTreeSet::new();
    for alert in alerts.as_array().into_iter().flatten() {
        let package = alert["dependency"]["package"]["name"]
            .as_str()
            .unwrap_or_default();
        let what = format!(
            "dependabot alert #{}: {package} ({})",
            alert["number"],
            alert["security_advisory"]["severity"]
                .as_str()
                .unwrap_or("unknown")
        );
        // Dependabot's PRs link each package they bump as `[name](url)`.
        let fix = bot_prs.iter().find(|pr| {
            pr["body"]
                .as_str()
                .is_some_and(|body| body.contains(&format!("[{package}]")))
        });
        let patched =
            alert["security_vulnerability"]["first_patched_version"]["identifier"].as_str();
        match (fix, patched) {
            (Some(pr), _) => {
                fixes.insert(pr["number"].as_u64());
                let days = open_for(pr);
                if days > BOT_PR_DAYS {
                    findings.problems.push(format!(
                        "{what}: fix in #{} open for {days} days",
                        pr["number"]
                    ));
                } else {
                    findings
                        .pending
                        .push(format!("{what}: fix in #{}", pr["number"]));
                }
            }
            (None, Some(version)) => findings
                .problems
                .push(format!("{what}: fixed in {version}, but no PR bumps it")),
            (None, None) => findings.problems.push(format!(
                "{what}: no fixed version yet; dismiss it if it doesn't affect you"
            )),
        }
    }

    for pr in bot_prs {
        let days = open_for(pr);
        if days > BOT_PR_DAYS && !fixes.contains(&pr["number"].as_u64()) {
            findings.problems.push(format!(
                "#{} by {} open for {days} days: {}",
                pr["number"],
                pr["user"]["login"].as_str().unwrap_or("a bot"),
                pr["title"].as_str().unwrap_or_default()
            ));
        }
    }
    Ok(findings)
}
