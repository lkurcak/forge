use std::fs;

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use crate::diff::subset;
use crate::gh::Gh;
use crate::manifest::{Repo, Ruleset};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Check,
    Apply,
}

/// What reconciling one repository found.
#[derive(Default)]
pub struct Outcome {
    /// A line per difference found (and fixed, when applying).
    pub drift: Vec<String>,
    /// Sections that couldn't be checked or fixed.
    pub errors: Vec<String>,
    /// Sections skipped because the repository can't have them.
    pub skipped: Vec<String>,
    /// Differences forge can't fix itself, like secrets whose values it never sees.
    pub manual: Vec<String>,
}

/// Compares one repository against the manifest and, in apply mode, fixes it.
///
/// Each section is handled on its own, so one failing doesn't hide drift in the others.
pub fn reconcile(gh: &Gh, owner: &str, repo: &Repo, mode: Mode) -> Outcome {
    let base = format!("repos/{owner}/{}", repo.name);
    let mut outcome = Outcome::default();

    if !repo.settings.is_empty() {
        let result = gh.get(&base).and_then(|actual| {
            object_drift(
                gh,
                mode,
                "settings",
                &repo.settings,
                &actual,
                "PATCH",
                &base,
            )
        });
        outcome.record(result);
    }

    if !repo.actions.is_empty() {
        let path = format!("{base}/actions/permissions/workflow");
        let result = gh.get(&path).and_then(|actual| {
            object_drift(gh, mode, "actions", &repo.actions, &actual, "PUT", &path)
        });
        outcome.record(result);
    }

    if !repo.rulesets.is_empty() {
        match gh.get(&format!(
            "{base}/rulesets?includes_parents=false&per_page=100"
        )) {
            // Free accounts only get rulesets on public repositories.
            Err(err) if err.to_string().contains("Upgrade to GitHub Pro") => {
                outcome
                    .skipped
                    .push("rulesets (private repos need GitHub Pro)".into());
            }
            Err(err) => outcome.errors.push(format!("{err:#}")),
            Ok(existing) => {
                for ruleset in &repo.rulesets {
                    outcome.record(ruleset_drift(gh, mode, &base, ruleset, &existing));
                }
            }
        }
    }

    if !repo.variables.is_empty() {
        let result = variables_drift(gh, mode, &base, repo);
        outcome.record(result);
    }

    if !repo.secrets.is_empty() || !repo.forbidden_secrets.is_empty() {
        let result = gh
            .get(&format!("{base}/actions/secrets?per_page=100"))
            .and_then(|actual| secrets_drift(gh, mode, owner, repo, &actual, &mut outcome.manual));
        outcome.record(result);
    }

    outcome
}

impl Outcome {
    fn record(&mut self, result: Result<Vec<String>>) {
        match result {
            Ok(drift) => self.drift.extend(drift),
            Err(err) => self.errors.push(format!("{err:#}")),
        }
    }
}

fn object_drift(
    gh: &Gh,
    mode: Mode,
    label: &str,
    expected: &Map<String, Value>,
    actual: &Value,
    method: &str,
    path: &str,
) -> Result<Vec<String>> {
    let expected = Value::Object(expected.clone());
    let drift: Vec<String> = subset(label, &expected, actual)
        .iter()
        .map(ToString::to_string)
        .collect();
    if mode == Mode::Apply && !drift.is_empty() {
        gh.api(method, path, Some(&expected))?;
    }
    Ok(drift)
}

fn ruleset_drift(
    gh: &Gh,
    mode: Mode,
    base: &str,
    ruleset: &Ruleset,
    existing: &Value,
) -> Result<Vec<String>> {
    let label = format!("ruleset {}", ruleset.name);
    let id = existing
        .as_array()
        .context("unexpected rulesets response")?
        .iter()
        .find(|r| r["name"] == ruleset.name.as_str())
        .and_then(|r| r["id"].as_u64());

    let Some(id) = id else {
        if mode == Mode::Apply {
            gh.api("POST", &format!("{base}/rulesets"), Some(&ruleset.body))?;
        }
        return Ok(vec![format!("{label}: missing")]);
    };

    let path = format!("{base}/rulesets/{id}");
    let actual = gh.get(&path)?;
    let drift: Vec<String> = subset(&label, &ruleset.body, &actual)
        .iter()
        .map(ToString::to_string)
        .collect();
    if mode == Mode::Apply && !drift.is_empty() {
        gh.api("PUT", &path, Some(&ruleset.body))?;
    }
    Ok(drift)
}

fn variables_drift(gh: &Gh, mode: Mode, base: &str, repo: &Repo) -> Result<Vec<String>> {
    let actual = gh.get(&format!("{base}/actions/variables?per_page=100"))?;
    let actual = actual["variables"]
        .as_array()
        .context("unexpected variables response")?;
    let mut drift = Vec::new();

    for (name, value) in &repo.variables {
        let body = serde_json::json!({ "name": name, "value": value });
        match actual.iter().find(|v| v["name"] == name.as_str()) {
            None => {
                drift.push(format!("variable {name}: missing"));
                if mode == Mode::Apply {
                    gh.api("POST", &format!("{base}/actions/variables"), Some(&body))?;
                }
            }
            Some(v) if v["value"] != value.as_str() => {
                drift.push(format!(
                    "variable {name}: expected {value:?}, found {}",
                    v["value"]
                ));
                if mode == Mode::Apply {
                    gh.api(
                        "PATCH",
                        &format!("{base}/actions/variables/{name}"),
                        Some(&body),
                    )?;
                }
            }
            Some(_) => {}
        }
    }

    Ok(drift)
}

/// Secrets are write-only, so only their presence is checked: missing ones are
/// set from their local files, unwanted ones are left for a human to remove
/// once nothing uses them.
fn secrets_drift(
    gh: &Gh,
    mode: Mode,
    owner: &str,
    repo: &Repo,
    actual: &Value,
    manual: &mut Vec<String>,
) -> Result<Vec<String>> {
    let names: Vec<&str> = actual["secrets"]
        .as_array()
        .context("unexpected secrets response")?
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    let mut drift = Vec::new();

    for (name, file) in &repo.secrets {
        if names.contains(&name.as_str()) {
            continue;
        }
        if !file.exists() {
            manual.push(format!(
                "secret {name}: missing, and there's no value at {} to set it from",
                file.display()
            ));
            continue;
        }
        drift.push(format!("secret {name}: missing"));
        if mode == Mode::Apply {
            let value = fs::read(file)
                .with_context(|| format!("reading secret {name} from {}", file.display()))?;
            gh.set_secret(&format!("{owner}/{}", repo.name), name, &value)?;
        }
    }

    manual.extend(
        repo.forbidden_secrets
            .iter()
            .filter(|s| names.contains(&s.as_str()))
            .map(|s| format!("secret {s}: should be removed")),
    );

    Ok(drift)
}
