use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::cargo::Lints;
use crate::deps::Deps;
use crate::gh::Gh;
use crate::leaks::Leaks;
use crate::pins::{Latest, Pin};
use crate::workflow::{Call, Reference, SharedWorkflow};

/// Rulesets anyone can use by name. A file of the same name in `rulesets/`
/// next to the manifest takes their place.
const BUILT_IN_RULESETS: &[(&str, &str)] = &[
    (
        "protect-default-branch",
        include_str!("../rulesets/protect-default-branch.json"),
    ),
    ("require-pr", include_str!("../rulesets/require-pr.json")),
    (
        "require-pr-title",
        include_str!("../rulesets/require-pr-title.json"),
    ),
    (
        "protect-release-tags",
        include_str!("../rulesets/protect-release-tags.json"),
    ),
    ("require-ci", include_str!("../rulesets/require-ci.json")),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    owner: String,
    #[serde(default)]
    rulesets: Vec<String>,
    #[serde(default)]
    settings: Map<String, Value>,
    #[serde(default)]
    actions: Map<String, Value>,
    release_tag_prefix: Option<String>,
    /// Apply rulesets and environments to private repos too, which needs
    /// GitHub Pro (or an organization on Team) for them to protect anything;
    /// without it forge leaves both out there.
    #[serde(default)]
    private_protection: bool,
    /// An input of the shared workflows that private repos must turn on and
    /// public ones leave off, like rust-ci's `light`: only private repos'
    /// Actions minutes are billed.
    private_ci: Option<String>,
    /// Workflow file repos call a shared workflow from, to its `uses:`
    /// reference. forge reads the shared workflow at that reference.
    #[serde(default)]
    workflows: BTreeMap<String, String>,
    #[serde(default)]
    tags: BTreeMap<String, RawTag>,
    #[serde(default)]
    repos: BTreeMap<String, RawRepo>,
    #[serde(default)]
    stale: Stale,
    #[serde(default)]
    deps: Deps,
    #[serde(default)]
    leaks: Leaks,
    /// Workflow file to the inputs in it that pin another repo's release.
    #[serde(default)]
    pins: BTreeMap<String, Pin>,
}

/// `[stale]`: what `forge stale` leaves out.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stale {
    /// Issues and PRs with any of these labels, like a backlog of feature ideas.
    #[serde(default)]
    pub ignore_labels: Vec<String>,
}

/// Requirements for every repository carrying a tag.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTag {
    #[serde(default)]
    variables: BTreeMap<String, String>,
    /// Secret name to the local file holding its value.
    #[serde(default)]
    secrets: BTreeMap<String, String>,
    #[serde(default)]
    forbidden_secrets: Vec<String>,
    /// Deployment environments that must exist, by name.
    #[serde(default)]
    environments: BTreeMap<String, Environment>,
    /// Rulesets from rulesets/, added to the repo's own.
    #[serde(default)]
    rulesets: Vec<String>,
    /// Shared workflows, by name, the repo must call.
    #[serde(default)]
    calls: Vec<String>,
    /// Repo settings, over the top-level ones and under the repo's own.
    #[serde(default)]
    settings: Map<String, Value>,
    /// Only for public repos, where GitHub offers what it sets. Every public
    /// repo has to decide: carry the tag, or `no-{tag}` to decline it.
    #[serde(default)]
    needs_public: bool,
    /// Lints every package's `[lints]` must set, at least this strictly.
    #[serde(default)]
    lints: Lints,
    /// Commands some `RUN` instruction of the root Dockerfile must run.
    #[serde(default)]
    dockerfile_runs: Vec<String>,
}

/// A deployment environment a tag needs.
#[derive(Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    /// Tag patterns that alone may deploy to it. Empty leaves its deployment
    /// policy alone.
    #[serde(default)]
    pub deploy_tags: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRepo {
    #[serde(default)]
    tags: Vec<String>,
    rulesets: Option<Vec<String>>,
    /// For repos releasing without the shared workflow, which otherwise
    /// decides the targets.
    #[serde(default)]
    release_targets: Vec<String>,
    release_tag_prefix: Option<String>,
    /// `false` for repos whose GitHub releases aren't versions of the project,
    /// like a Homebrew tap storing bottles.
    #[serde(default = "yes")]
    releases: bool,
    #[serde(default)]
    settings: Map<String, Value>,
    #[serde(default)]
    actions: Map<String, Value>,
}

pub struct Manifest {
    pub owner: String,
    pub repos: Vec<Repo>,
    /// Filled in by [`Self::fetch_workflows`].
    pub workflows: Vec<SharedWorkflow>,
    /// `[workflows]`: the file callers call from, and the `uses:` reference.
    workflow_refs: Vec<(String, String)>,
    pub stale: Stale,
    pub deps: Deps,
    pub leaks: Leaks,
    /// Whether private repos get their rulesets and environments
    /// (`private_protection`).
    pub private_protection: bool,
    /// The input private repos' calls must turn on (`private_ci`).
    private_ci: Option<String>,
    tags: BTreeMap<String, Tag>,
    /// `[pins]`, by workflow file, with the release each should name once
    /// [`Self::fetch_pins`] has looked it up.
    pub pins: Vec<(String, Pin, Option<Latest>)>,
}

/// A repository with the manifest defaults already merged in.
pub struct Repo {
    pub name: String,
    pub tags: Vec<String>,
    /// The top-level settings; tags' and then its own go over them in [`Needs`].
    settings: Map<String, Value>,
    own_settings: Map<String, Value>,
    pub actions: Actions,
    pub rulesets: Vec<Ruleset>,
    /// What the latest release's tag must start with, followed by the version.
    /// `None` for repos without versioned releases.
    pub release_tag_prefix: Option<String>,
    /// Targets the latest release must have a binary archive for, besides the
    /// ones its release workflow builds; `*` matches anything.
    pub release_targets: Vec<String>,
}

/// `[actions]`, one map per endpoint under
/// `/repos/{owner}/{repo}/actions/permissions`, with the repo's own keys over
/// the top-level ones.
#[derive(Default)]
pub struct Actions {
    /// The endpoint itself: whether Actions run, and which actions may.
    pub permissions: Map<String, Value>,
    /// `[actions.workflow]`: what workflows' `GITHUB_TOKEN` may do.
    pub workflow: Map<String, Value>,
    /// `[actions.selected-actions]`: the allowlist, used while
    /// `allowed_actions` is `selected`.
    pub allowlist: Map<String, Value>,
}

struct Tag {
    variables: BTreeMap<String, String>,
    secrets: BTreeMap<String, PathBuf>,
    forbidden_secrets: Vec<String>,
    environments: BTreeMap<String, Environment>,
    rulesets: Vec<Ruleset>,
    calls: Vec<String>,
    settings: Map<String, Value>,
    needs_public: bool,
    lints: Lints,
    dockerfile_runs: Vec<String>,
}

/// What a repository needs, from its manifest tags plus the tags its calls
/// of shared workflows imply.
#[derive(Default)]
pub struct Needs {
    pub tags: Vec<String>,
    /// Where the repo's visibility disagrees with the manifest: a tag that
    /// needs a public repo on a private one, or undecided on a public one (the
    /// manifest has to change), or a call not following `private_ci` (the
    /// repo has to change).
    pub visibility_problems: Vec<String>,
    /// Repo settings: top-level, then the tags', then the repo's own.
    pub settings: Map<String, Value>,
    /// Actions variables and their values.
    pub variables: BTreeMap<String, String>,
    /// Actions secrets that must exist and the local files holding their values.
    pub secrets: BTreeMap<String, PathBuf>,
    /// Actions secrets that must not exist.
    pub forbidden_secrets: Vec<String>,
    /// Deployment environments that must exist.
    pub environments: BTreeMap<String, Environment>,
    /// The repo's rulesets plus its tags'.
    pub rulesets: Vec<Ruleset>,
    /// Shared workflows, by name, the repo must call.
    pub calls: Vec<String>,
    /// Targets the latest release must have a binary archive for.
    pub release_targets: Vec<String>,
    /// Lints every package's `[lints]` must set, at least this strictly.
    pub lints: Lints,
    /// Commands some `RUN` instruction of the root Dockerfile must run.
    pub dockerfile_runs: Vec<String>,
}

#[derive(Clone)]
pub struct Ruleset {
    pub name: String,
    /// Request body for the rulesets API, `name` included.
    pub body: Value,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let raw: RawManifest =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        let rulesets_dir = dir.join("rulesets");

        let mut loaded: BTreeMap<String, Ruleset> = BTreeMap::new();
        let mut load_ruleset = |name: &str| -> Result<Ruleset> {
            if let Some(ruleset) = loaded.get(name) {
                return Ok(ruleset.clone());
            }
            let ruleset = Ruleset::read(&rulesets_dir, name)?;
            loaded.insert(name.to_owned(), ruleset.clone());
            Ok(ruleset)
        };

        let mut repos = Vec::new();
        for (name, repo) in raw.repos {
            let actions = Actions::merge(raw.actions.clone(), repo.actions)
                .with_context(|| format!("repos.{name}.actions"))?;
            let rulesets = repo
                .rulesets
                .as_ref()
                .unwrap_or(&raw.rulesets)
                .iter()
                .map(|r| load_ruleset(r))
                .collect::<Result<_>>()?;

            repos.push(Repo {
                name,
                tags: repo.tags,
                settings: raw.settings.clone(),
                own_settings: repo.settings,
                actions,
                rulesets,
                release_tag_prefix: if repo.releases {
                    repo.release_tag_prefix
                        .or_else(|| raw.release_tag_prefix.clone())
                } else {
                    None
                },
                release_targets: if repo.releases {
                    repo.release_targets
                } else {
                    Vec::new()
                },
            });
        }

        let tags: BTreeMap<String, Tag> = raw
            .tags
            .into_iter()
            .map(|(name, tag)| {
                let tag = Tag {
                    rulesets: tag
                        .rulesets
                        .iter()
                        .map(|r| load_ruleset(r))
                        .collect::<Result<_>>()?,
                    variables: tag.variables,
                    secrets: tag
                        .secrets
                        .iter()
                        .map(|(k, v)| (k.clone(), expand_home(v)))
                        .collect(),
                    forbidden_secrets: tag.forbidden_secrets,
                    environments: tag.environments,
                    calls: tag.calls,
                    settings: tag.settings,
                    needs_public: tag.needs_public,
                    lints: tag.lints,
                    dockerfile_runs: tag.dockerfile_runs,
                };
                Ok((name, tag))
            })
            .collect::<Result<_>>()?;

        let names = raw
            .workflows
            .values()
            .map(|uses| Ok(Reference::parse(uses)?.name().to_owned()))
            .collect::<Result<Vec<_>>>()?;
        for (name, tag) in &tags {
            for call in &tag.calls {
                if !names.contains(call) {
                    bail!("tag {name} calls {call}, which isn't in [workflows]");
                }
            }
        }

        Ok(Self {
            owner: raw.owner,
            repos,
            workflows: Vec::new(),
            workflow_refs: raw.workflows.into_iter().collect(),
            stale: raw.stale,
            deps: raw.deps,
            leaks: raw.leaks,
            private_protection: raw.private_protection,
            private_ci: raw.private_ci,
            tags,
            pins: raw.pins.into_iter().map(|(f, p)| (f, p, None)).collect(),
        })
    }

    /// Looks up the latest release each pin should name.
    pub fn fetch_pins(&mut self, gh: &Gh) -> Result<()> {
        for (file, pin, latest) in &mut self.pins {
            *latest = Some(
                pin.latest(gh)
                    .with_context(|| format!("reading the release [pins.\"{file}\"] names"))?,
            );
        }
        Ok(())
    }

    /// Reads each shared workflow in `[workflows]` with `read`, given the
    /// caller's file name and the `uses:` reference.
    pub fn fetch_workflows(
        &mut self,
        read: impl Fn(&str, &str) -> Result<SharedWorkflow>,
    ) -> Result<()> {
        self.workflows = self
            .workflow_refs
            .iter()
            .map(|(caller, uses)| read(caller, uses))
            .collect::<Result<_>>()?;
        if let Some(input) = &self.private_ci
            && !self.workflows.iter().any(|w| w.has_input(input))
        {
            bail!("private_ci = {input:?}, but no workflow in [workflows] has that input");
        }
        Ok(())
    }

    /// Merges the requirements of the repo's tags. A repo calling a shared
    /// workflow also gets a tag named after it, and one per input it turns on.
    /// `calls` pairs with [`Self::workflows`]. Tags needing a public repo are
    /// dropped on private ones.
    pub fn needs(&self, repo: &Repo, calls: &[Option<Call>], public: bool) -> Needs {
        let mut needs = Needs {
            settings: repo.settings.clone(),
            rulesets: repo.rulesets.clone(),
            ..Needs::default()
        };
        let add_tag = |needs: &mut Needs, tag: &str| {
            if !needs.tags.iter().any(|t| t == tag) {
                needs.tags.push(tag.to_owned());
            }
        };
        for tag in &repo.tags {
            add_tag(&mut needs, tag);
        }
        for (workflow, call) in self.workflows.iter().zip(calls) {
            if let Some(call) = call {
                add_tag(&mut needs, &workflow.name);
                for input in call.enabled() {
                    add_tag(&mut needs, input);
                }
            }
        }

        for (name, _) in self.tags.iter().filter(|(_, t)| t.needs_public) {
            let tagged = needs.tags.contains(name);
            if tagged && !public {
                needs.visibility_problems.push(format!(
                    "tag {name}: only works on public repos, and this one is private"
                ));
                needs.tags.retain(|t| t != name);
            } else if !tagged && public && !needs.tags.contains(&format!("no-{name}")) {
                needs.visibility_problems.push(format!(
                    "public, so it can have {name}: add the {name} tag, or no-{name} to hide this"
                ));
            }
        }
        if let Some(input) = &self.private_ci {
            for (workflow, call) in self.workflows.iter().zip(calls) {
                if let Some(call) = call {
                    needs
                        .visibility_problems
                        .extend(workflow.private_ci_problem(call, input, public));
                }
            }
        }

        for tag in needs.tags.clone().iter().filter_map(|t| self.tags.get(t)) {
            needs.variables.extend(tag.variables.clone());
            needs.secrets.extend(tag.secrets.clone());
            push_new(&mut needs.forbidden_secrets, &tag.forbidden_secrets);
            push_new(&mut needs.calls, &tag.calls);
            push_new(&mut needs.dockerfile_runs, &tag.dockerfile_runs);
            needs.settings.extend(tag.settings.clone());
            for (name, environment) in &tag.environments {
                let needed = needs.environments.entry(name.clone()).or_default();
                push_new(&mut needed.deploy_tags, &environment.deploy_tags);
            }
            for ruleset in &tag.rulesets {
                if !needs.rulesets.iter().any(|r| r.name == ruleset.name) {
                    needs.rulesets.push(ruleset.clone());
                }
            }
            for (group, lints) in &tag.lints {
                needs
                    .lints
                    .entry(group.clone())
                    .or_default()
                    .extend(lints.clone());
            }
        }
        needs.settings.extend(repo.own_settings.clone());
        // `no-{workflow}` declines a call a tag would otherwise require.
        needs
            .calls
            .retain(|call| !needs.tags.contains(&format!("no-{call}")));

        if repo.release_tag_prefix.is_some() {
            push_new(&mut needs.release_targets, &repo.release_targets);
            for (workflow, call) in self.workflows.iter().zip(calls) {
                if let Some(call) = call {
                    push_new(&mut needs.release_targets, &workflow.targets(call));
                }
            }
        }
        needs
    }
}

impl Actions {
    fn merge(top: Map<String, Value>, own: Map<String, Value>) -> Result<Self> {
        let mut actions = Self::split(top)?;
        let own = Self::split(own)?;
        actions.permissions.extend(own.permissions);
        actions.workflow.extend(own.workflow);
        actions.allowlist.extend(own.allowlist);
        Ok(actions)
    }

    fn split(mut permissions: Map<String, Value>) -> Result<Self> {
        let mut table = |key: &str| match permissions.remove(key) {
            None => Ok(Map::new()),
            Some(Value::Object(table)) => Ok(table),
            Some(_) => bail!("actions.{key} must be a table"),
        };
        let workflow = table("workflow")?;
        let allowlist = table("selected-actions")?;
        Ok(Self {
            permissions,
            workflow,
            allowlist,
        })
    }
}

impl Ruleset {
    /// Reads `{name}.json` from `dir`, or else the built-in ruleset of that
    /// name, naming the ruleset after it.
    fn read(dir: &Path, name: &str) -> Result<Self> {
        let file = dir.join(format!("{name}.json"));
        let (text, source) = if file.exists() {
            let text =
                fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
            (text, file.display().to_string())
        } else if let Some((_, text)) = BUILT_IN_RULESETS.iter().find(|(n, _)| *n == name) {
            ((*text).to_owned(), format!("built-in ruleset {name}"))
        } else {
            let built_in: Vec<_> = BUILT_IN_RULESETS.iter().map(|(n, _)| *n).collect();
            bail!(
                "no ruleset {name}: not in {} and not built in ({})",
                dir.display(),
                built_in.join(", ")
            );
        };
        let mut body: Value =
            serde_json::from_str(&text).with_context(|| format!("parsing {source}"))?;
        body.as_object_mut()
            .with_context(|| format!("{source} is not a JSON object"))?
            .insert("name".into(), name.into());
        Ok(Self {
            name: name.into(),
            body,
        })
    }
}

fn push_new(list: &mut Vec<String>, items: &[String]) {
    for item in items {
        if !list.contains(item) {
            list.push(item.clone());
        }
    }
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => PathBuf::from(path),
    }
}

const fn yes() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn map(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => panic!("not an object"),
        }
    }

    #[test]
    fn reads_built_in_and_custom_rulesets() {
        let dir = std::env::temp_dir().join(format!("forge-rulesets-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("mine.json"), r#"{"target": "tag"}"#).unwrap();
        let mine = Ruleset::read(&dir, "mine").unwrap();
        let built_in = Ruleset::read(&dir, "require-pr").unwrap();
        let missing = Ruleset::read(&dir, "nope").map(|_| ()).unwrap_err();
        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(mine.body, json!({ "target": "tag", "name": "mine" }));
        assert_eq!(built_in.body["name"], "require-pr");
        assert_eq!(built_in.body["target"], "branch");
        assert!(
            missing.to_string().contains("require-pr-title"),
            "{missing}"
        );
    }

    #[test]
    fn private_protection_is_opt_in() {
        let dir = std::env::temp_dir().join(format!("forge-private-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("forge.toml");
        fs::write(&path, "owner = \"me\"\n").unwrap();
        let default = Manifest::load(&path).unwrap();
        fs::write(&path, "owner = \"me\"\nprivate_protection = true\n").unwrap();
        let opted_in = Manifest::load(&path).unwrap();
        fs::remove_dir_all(&dir).unwrap();

        assert!(!default.private_protection);
        assert!(opted_in.private_protection);
    }

    #[test]
    fn private_ci_names_an_input_some_workflow_has() {
        let dir = std::env::temp_dir().join(format!("forge-private-ci-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("forge.toml");
        let workflows =
            "[workflows]\n\"ci.yml\" = \"lkurcak/forge/.github/workflows/rust-ci.yml@v1\"\n";
        let load = |private_ci: &str| {
            fs::write(&path, format!("owner = \"me\"\n{private_ci}{workflows}")).unwrap();
            let mut manifest = Manifest::load(&path).unwrap();
            manifest
                .fetch_workflows(crate::workflow::local)
                .map(|()| manifest)
        };
        let off = load("").unwrap();
        let light = load("private_ci = \"light\"\n").unwrap();
        let typo = load("private_ci = \"lite\"\n").map(|_| ()).unwrap_err();
        fs::remove_dir_all(&dir).unwrap();

        assert_eq!(off.private_ci, None);
        assert_eq!(light.private_ci.as_deref(), Some("light"));
        assert!(typo.to_string().contains("lite"), "{typo}");
    }

    #[test]
    fn splits_actions_by_endpoint() {
        let top = map(json!({
            "allowed_actions": "selected",
            "workflow": { "default_workflow_permissions": "read" },
            "selected-actions": { "patterns_allowed": ["a/*"] },
        }));
        let own = map(json!({ "workflow": { "can_approve_pull_request_reviews": true } }));
        let actions = Actions::merge(top, own).unwrap();
        assert_eq!(
            actions.permissions,
            map(json!({ "allowed_actions": "selected" }))
        );
        assert_eq!(
            actions.workflow,
            map(json!({
                "default_workflow_permissions": "read",
                "can_approve_pull_request_reviews": true,
            }))
        );
        assert_eq!(actions.allowlist["patterns_allowed"], json!(["a/*"]));
    }
}
