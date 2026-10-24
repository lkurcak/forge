use std::fs;

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

use crate::cargo::Workspace;
use crate::diff::subset;
use crate::gh::Gh;
use crate::manifest::{Actions, Manifest, Needs, Repo, Ruleset};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Check,
    Apply {
        /// Also overwrite secrets that already exist, e.g. after renewing a token.
        update_secrets: bool,
    },
}

impl Mode {
    pub const fn applies(self) -> bool {
        matches!(self, Self::Apply { .. })
    }
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
    /// Things on their way to being right, like a release waiting on review.
    pub pending: Vec<String>,
}

/// Compares one repository against the manifest and, in apply mode, fixes it.
///
/// Each section is handled on its own, so one failing doesn't hide drift in the others.
pub fn reconcile(gh: &Gh, manifest: &Manifest, repo: &Repo, mode: Mode) -> Outcome {
    let owner = manifest.owner.as_str();
    let base = format!("repos/{owner}/{}", repo.name);
    let mut outcome = Outcome::default();

    let mut calls = Vec::new();
    for workflow in &manifest.workflows {
        let call = match workflow.call(gh, owner, &repo.name) {
            Ok(call) => call,
            Err(err) => {
                outcome.errors.push(format!("{err:#}"));
                None
            }
        };
        if let Some(call) = &call {
            outcome.manual.extend(workflow.problems(call));
        }
        calls.push(call);
    }
    // Everything below needs to know whether the repo is public.
    let actual = match gh.get(&base) {
        Ok(actual) => actual,
        Err(err) => {
            outcome.errors.push(format!("{err:#}"));
            return outcome;
        }
    };
    let public = actual["private"] == false;

    let needs = manifest.needs(repo, &calls, public);
    outcome
        .manual
        .extend(needs.visibility_problems.iter().cloned());
    for (workflow, call) in manifest.workflows.iter().zip(&calls) {
        if call.is_none() && needs.calls.contains(&workflow.name) {
            outcome.manual.push(workflow.missing());
        }
    }

    if !needs.settings.is_empty() {
        let result = settings_drift(gh, mode, &base, &needs.settings, &actual);
        outcome.record(result);
    }

    actions_drift(gh, mode, &base, &repo.actions, &mut outcome);

    if !needs.rulesets.is_empty() {
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
                for ruleset in &needs.rulesets {
                    outcome.record(ruleset_drift(gh, mode, &base, ruleset, &existing));
                }
            }
        }
    }

    if !needs.variables.is_empty() {
        let result = variables_drift(gh, mode, &base, &needs);
        outcome.record(result);
    }

    if !needs.secrets.is_empty() || !needs.forbidden_secrets.is_empty() {
        let result = gh
            .get(&format!("{base}/actions/secrets?per_page=100"))
            .and_then(|actual| {
                secrets_drift(gh, mode, owner, repo, &needs, &actual, &mut outcome.manual)
            });
        outcome.record(result);
    }

    if !needs.environments.is_empty() {
        let result = environments_drift(gh, mode, &base, &needs);
        outcome.record(result);
    }

    if !needs.lints.is_empty() {
        match Workspace::read(gh, &format!("{owner}/{}", repo.name)) {
            Ok(Some(workspace)) => outcome.manual.extend(workspace.lint_problems(&needs.lints)),
            Ok(None) => outcome
                .skipped
                .push("lints (no Cargo.toml at the root)".into()),
            Err(err) => outcome.errors.push(format!("{err:#}")),
        }
    }

    if repo.release_tag_prefix.is_some() {
        match crate::release::check(gh, owner, repo, &needs) {
            Ok(findings) => {
                outcome.manual.extend(findings.problems);
                outcome.pending.extend(findings.pending);
            }
            Err(err) => outcome.errors.push(format!("{err:#}")),
        }
    }

    match crate::dependabot::check(gh, &base) {
        Ok(findings) => {
            outcome.manual.extend(findings.problems);
            outcome.pending.extend(findings.pending);
        }
        Err(err) => outcome.errors.push(format!("{err:#}")),
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

/// Settings for `PATCH /repos/{owner}/{repo}`. `security_and_analysis` goes
/// in a request of its own: GitHub ignores it when other settings change in
/// the same one. Its `dependabot_security_updates` is ignored even there, so
/// it goes through `/automated-security-fixes`.
fn settings_drift(
    gh: &Gh,
    mode: Mode,
    base: &str,
    settings: &Map<String, Value>,
    actual: &Value,
) -> Result<Vec<String>> {
    let mut settings = settings.clone();
    let security = settings.remove("security_and_analysis");
    let mut drift = object_drift(gh, mode, "settings", &settings, actual, "PATCH", base)?;
    if let Some(mut security) = security {
        // Ignored in the PATCH too, though its own endpoint works.
        let updates = security
            .as_object_mut()
            .and_then(|s| s.remove("dependabot_security_updates"));
        if let Some(updates) = updates {
            let expected = Map::from_iter([(
                "security_and_analysis".into(),
                json!({ "dependabot_security_updates": updates }),
            )]);
            let found = object_drift(gh, Mode::Check, "settings", &expected, actual, "", "")?;
            if mode.applies() && !found.is_empty() {
                let fixes = format!("{base}/automated-security-fixes");
                if updates["status"] == "enabled" {
                    // They act on Dependabot alerts, which must be on first.
                    gh.api("PUT", &format!("{base}/vulnerability-alerts"), None)?;
                    gh.api("PUT", &fixes, None)?;
                } else {
                    gh.api("DELETE", &fixes, None)?;
                }
            }
            drift.extend(found);
        }
        let security = Map::from_iter([("security_and_analysis".into(), security)]);
        drift.extend(object_drift(
            gh, mode, "settings", &security, actual, "PATCH", base,
        )?);
    }
    Ok(drift)
}

/// The three Actions permission endpoints, each recorded on its own. The
/// allowlist only exists while `allowed_actions` is `selected` (GitHub answers
/// 409 otherwise), so it's set after the permissions that make it so.
fn actions_drift(gh: &Gh, mode: Mode, base: &str, actions: &Actions, outcome: &mut Outcome) {
    let path = format!("{base}/actions/permissions");
    let mut allowed = None;
    if !actions.permissions.is_empty() {
        let result = gh.get(&path).and_then(|actual| {
            allowed = actual.get("allowed_actions").cloned();
            object_drift(
                gh,
                mode,
                "actions",
                &actions.permissions,
                &actual,
                "PUT",
                &path,
            )
        });
        if mode.applies() && result.is_ok() {
            allowed = actions
                .permissions
                .get("allowed_actions")
                .cloned()
                .or(allowed);
        }
        outcome.record(result);
    }

    if !actions.workflow.is_empty() {
        let path = format!("{path}/workflow");
        let result = gh.get(&path).and_then(|actual| {
            object_drift(
                gh,
                mode,
                "actions.workflow",
                &actions.workflow,
                &actual,
                "PUT",
                &path,
            )
        });
        outcome.record(result);
    }

    if !actions.allowlist.is_empty() {
        let selected = |value: Option<&Value>| value.and_then(Value::as_str) == Some("selected");
        if selected(allowed.as_ref()) {
            let path = format!("{path}/selected-actions");
            let result = gh.get(&path).and_then(|actual| {
                let label = "actions.selected-actions";
                object_drift(gh, mode, label, &actions.allowlist, &actual, "PUT", &path)
            });
            outcome.record(result);
        } else if !selected(actions.permissions.get("allowed_actions")) {
            // When the manifest does ask for it, that's already reported.
            outcome
                .skipped
                .push("actions.selected-actions (allowed_actions isn't selected)".into());
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
    if mode.applies() && !drift.is_empty() {
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
        if mode.applies() {
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
    if mode.applies() && !drift.is_empty() {
        gh.api("PUT", &path, Some(&ruleset.body))?;
    }
    Ok(drift)
}

fn variables_drift(gh: &Gh, mode: Mode, base: &str, needs: &Needs) -> Result<Vec<String>> {
    let actual = gh.get(&format!("{base}/actions/variables?per_page=100"))?;
    let actual = actual["variables"]
        .as_array()
        .context("unexpected variables response")?;
    let mut drift = Vec::new();

    for (name, value) in &needs.variables {
        let body = serde_json::json!({ "name": name, "value": value });
        match actual.iter().find(|v| v["name"] == name.as_str()) {
            None => {
                drift.push(format!("variable {name}: missing"));
                if mode.applies() {
                    gh.api("POST", &format!("{base}/actions/variables"), Some(&body))?;
                }
            }
            Some(v) if v["value"] != value.as_str() => {
                drift.push(format!(
                    "variable {name}: expected {value:?}, found {}",
                    v["value"]
                ));
                if mode.applies() {
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

/// Missing environments are created without protection rules, so the jobs
/// using them run without waiting. Those with `deploy_tags` only accept
/// deployments from refs matching them, and from no branch.
fn environments_drift(gh: &Gh, mode: Mode, base: &str, needs: &Needs) -> Result<Vec<String>> {
    let actual = gh.get(&format!("{base}/environments?per_page=100"))?;
    let actual = actual["environments"]
        .as_array()
        .context("unexpected environments response")?;
    let mut drift = Vec::new();
    for (name, environment) in &needs.environments {
        let path = format!("{base}/environments/{name}");
        let label = format!("environment {name}");
        let tags_only = json!({ "protected_branches": false, "custom_branch_policies": true });
        let existing = actual.iter().find(|e| e["name"] == name.as_str());
        if existing.is_none() {
            drift.push(format!("{label}: missing"));
        }
        if environment.deploy_tags.is_empty() {
            if existing.is_none() && mode.applies() {
                gh.api("PUT", &path, None)?;
            }
            continue;
        }
        let policy = existing.map_or(&Value::Null, |e| &e["deployment_branch_policy"]);

        if *policy != tags_only {
            drift.push(format!(
                "{label}: deployment policy expected tags only, found {policy}"
            ));
            if mode.applies() {
                let body = json!({ "deployment_branch_policy": tags_only });
                gh.api("PUT", &path, Some(&body))?;
            }
        }

        // There's no policy list until the policy above allows custom ones.
        let policies = if *policy == tags_only {
            gh.get(&format!("{path}/deployment-branch-policies?per_page=100"))?
        } else {
            json!({ "branch_policies": [] })
        };
        let policies = policies["branch_policies"]
            .as_array()
            .context("unexpected deployment policies response")?;
        for pattern in &environment.deploy_tags {
            let wanted = json!({ "name": pattern, "type": "tag" });
            if policies.iter().any(|p| subset("", &wanted, p).is_empty()) {
                continue;
            }
            drift.push(format!("{label}: tag {pattern} can't deploy"));
            if mode.applies() {
                gh.api(
                    "POST",
                    &format!("{path}/deployment-branch-policies"),
                    Some(&wanted),
                )?;
            }
        }
        for p in policies {
            let allowed = p["type"] == "tag"
                && environment
                    .deploy_tags
                    .iter()
                    .any(|t| p["name"] == t.as_str());
            if allowed {
                continue;
            }
            drift.push(format!("{label}: {} {} can deploy", p["type"], p["name"]));
            if mode.applies() {
                let id = p["id"]
                    .as_u64()
                    .context("deployment policy without an id")?;
                gh.api(
                    "DELETE",
                    &format!("{path}/deployment-branch-policies/{id}"),
                    None,
                )?;
            }
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
    needs: &Needs,
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

    let update_existing = matches!(
        mode,
        Mode::Apply {
            update_secrets: true
        }
    );
    for (name, file) in &needs.secrets {
        let exists = names.contains(&name.as_str());
        if exists && !update_existing {
            continue;
        }
        if !file.exists() {
            manual.push(format!(
                "secret {name}: {}, and there's no value at {} to set it from",
                if exists { "can't update" } else { "missing" },
                file.display()
            ));
            continue;
        }
        drift.push(format!(
            "secret {name}: {}",
            if exists { "updated" } else { "missing" }
        ));
        if mode.applies() {
            let value = fs::read(file)
                .with_context(|| format!("reading secret {name} from {}", file.display()))?;
            // Files usually end in a newline, which would break a token.
            let value = value.trim_ascii_end();
            gh.set_secret(&format!("{owner}/{}", repo.name), name, value)?;
        }
    }

    manual.extend(
        needs
            .forbidden_secrets
            .iter()
            .filter(|s| names.contains(&s.as_str()))
            .map(|s| format!("secret {s}: should be removed")),
    );

    Ok(drift)
}
