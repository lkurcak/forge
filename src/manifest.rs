use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::workflow::{Release, SharedWorkflow};

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
    /// `uses:` reference to the shared release workflow, which lives next to
    /// the manifest.
    release_workflow: String,
    #[serde(default)]
    tags: BTreeMap<String, RawTag>,
    #[serde(default)]
    repos: BTreeMap<String, RawRepo>,
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
    /// Deployment environments that must exist.
    #[serde(default)]
    environments: Vec<String>,
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
    pub release_workflow: SharedWorkflow,
    tags: BTreeMap<String, Tag>,
}

/// A repository with the manifest defaults already merged in.
pub struct Repo {
    pub name: String,
    pub tags: Vec<String>,
    pub settings: Map<String, Value>,
    pub actions: Map<String, Value>,
    pub rulesets: Vec<Ruleset>,
    /// What the latest release's tag must start with, followed by the version.
    /// `None` for repos without versioned releases.
    pub release_tag_prefix: Option<String>,
    /// Targets the latest release must have a binary archive for, besides the
    /// ones its release workflow builds; `*` matches anything.
    pub release_targets: Vec<String>,
}

struct Tag {
    variables: BTreeMap<String, String>,
    secrets: BTreeMap<String, PathBuf>,
    forbidden_secrets: Vec<String>,
    environments: Vec<String>,
}

/// What a repository needs, from its manifest tags plus the tags its
/// `release.yml` implies.
#[derive(Default)]
pub struct Needs {
    pub tags: Vec<String>,
    /// Actions variables and their values.
    pub variables: BTreeMap<String, String>,
    /// Actions secrets that must exist and the local files holding their values.
    pub secrets: BTreeMap<String, PathBuf>,
    /// Actions secrets that must not exist.
    pub forbidden_secrets: Vec<String>,
    /// Deployment environments that must exist.
    pub environments: Vec<String>,
    /// Targets the latest release must have a binary archive for.
    pub release_targets: Vec<String>,
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
            let file = rulesets_dir.join(format!("{name}.json"));
            let text =
                fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
            let mut body: Value = serde_json::from_str(&text)
                .with_context(|| format!("parsing {}", file.display()))?;
            body.as_object_mut()
                .with_context(|| format!("{} is not a JSON object", file.display()))?
                .insert("name".into(), name.into());
            let ruleset = Ruleset {
                name: name.into(),
                body,
            };
            loaded.insert(name.to_owned(), ruleset.clone());
            Ok(ruleset)
        };

        let mut repos = Vec::new();
        for (name, repo) in raw.repos {
            let mut settings = raw.settings.clone();
            settings.extend(repo.settings);
            let mut actions = raw.actions.clone();
            actions.extend(repo.actions);
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
                settings,
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

        let tags = raw
            .tags
            .into_iter()
            .map(|(name, tag)| {
                let tag = Tag {
                    variables: tag.variables,
                    secrets: tag
                        .secrets
                        .iter()
                        .map(|(k, v)| (k.clone(), expand_home(v)))
                        .collect(),
                    forbidden_secrets: tag.forbidden_secrets,
                    environments: tag.environments,
                };
                (name, tag)
            })
            .collect();

        Ok(Self {
            owner: raw.owner,
            repos,
            release_workflow: SharedWorkflow::load(dir, &raw.release_workflow)?,
            tags,
        })
    }

    /// Merges the requirements of the repo's tags. A repo calling the shared
    /// workflow also gets a tag named after it, and one per input it turns on.
    pub fn needs(&self, repo: &Repo, release: Option<&Release>) -> Needs {
        let mut needs = Needs::default();
        let add_tag = |needs: &mut Needs, tag: &str| {
            if !needs.tags.iter().any(|t| t == tag) {
                needs.tags.push(tag.to_owned());
            }
        };
        for tag in &repo.tags {
            add_tag(&mut needs, tag);
        }
        if let Some(release) = release {
            add_tag(&mut needs, &self.release_workflow.name);
            for input in release.enabled() {
                add_tag(&mut needs, input);
            }
        }

        for tag in needs.tags.clone().iter().filter_map(|t| self.tags.get(t)) {
            needs.variables.extend(tag.variables.clone());
            needs.secrets.extend(tag.secrets.clone());
            push_new(&mut needs.forbidden_secrets, &tag.forbidden_secrets);
            push_new(&mut needs.environments, &tag.environments);
        }

        if repo.release_tag_prefix.is_some() {
            push_new(&mut needs.release_targets, &repo.release_targets);
            if let Some(release) = release {
                push_new(
                    &mut needs.release_targets,
                    &self.release_workflow.targets(release),
                );
            }
        }
        needs
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
