//! Open issues and PRs that nobody has touched in a while, across every repo
//! the owner has, managed or not. Only listed: deciding what to do about them
//! is up to whoever reads the list.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::Result;
use serde_json::Value;

use crate::days;
use crate::gh::Gh;

/// The search API stops at 1000 results.
const PAGES: u32 = 10;

/// Lines per repo, each issue or PR quiet for at least `min_days` and
/// carrying none of `ignore_labels`.
pub fn find(
    gh: &Gh,
    owner: &str,
    min_days: i64,
    ignore_labels: &[String],
) -> Result<BTreeMap<String, Vec<String>>> {
    let today = days::today();
    let cutoff = days::format(today - min_days);
    let mut query = format!("user:{owner}+is:open+archived:false+updated:%3C%3D{cutoff}");
    for label in ignore_labels {
        write!(query, "+-label:%22{}%22", label.replace(' ', "+"))?;
    }

    let mut items = Vec::new();
    for page in 1..=PAGES {
        let found = gh.get(&format!(
            "search/issues?q={query}&sort=updated&order=asc&per_page=100&page={page}"
        ))?;
        let batch = found["items"].as_array().cloned().unwrap_or_default();
        let last = batch.len() < 100;
        items.extend(batch);
        if last {
            break;
        }
    }

    let mut repos: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for item in &items {
        let repo = item["repository_url"]
            .as_str()
            .and_then(|url| url.rsplit('/').next())
            .unwrap_or_default();
        let quiet = item["updated_at"]
            .as_str()
            .and_then(days::parse)
            .map_or(0, |updated| today - updated);
        let kind = if item["pull_request"].is_null() {
            "issue"
        } else {
            "PR"
        };
        let mut line = format!(
            "#{} {kind}: {} (quiet {quiet} days",
            item["number"],
            item["title"].as_str().unwrap_or_default()
        );
        if let Some(author) = author(item, owner) {
            write!(line, ", by {author}")?;
        }
        line.push(')');
        repos.entry(repo.to_owned()).or_default().push(line);
    }
    Ok(repos)
}

/// Who opened it, unless it was the owner.
fn author<'a>(item: &'a Value, owner: &str) -> Option<&'a str> {
    item["user"]["login"]
        .as_str()
        .filter(|login| *login != owner)
}
