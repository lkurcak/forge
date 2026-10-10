use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Map, Value};

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
    #[serde(default)]
    release_targets: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRepo {
    #[serde(default)]
    tags: Vec<String>,
    rulesets: Option<Vec<String>>,
    release_targets: Option<Vec<String>>,
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
}

/// A repository with the manifest defaults already merged in.
pub struct Repo {
    pub name: String,
    pub tags: Vec<String>,
    pub settings: Map<String, Value>,
    pub actions: Map<String, Value>,
    pub rulesets: Vec<Ruleset>,
    /// Actions variables and their values, from the repo's tags.
    pub variables: BTreeMap<String, String>,
    /// Actions secrets that must exist and the local files holding their
    /// values, from the repo's tags.
    pub secrets: BTreeMap<String, PathBuf>,
    /// Actions secrets that must not exist, from the repo's tags.
    pub forbidden_secrets: Vec<String>,
    /// What the latest release's tag must start with, followed by the version.
    pub release_tag_prefix: Option<String>,
    /// Targets the latest release must have a binary archive for; `*` matches
    /// anything.
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
        let rulesets_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("rulesets");

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

            let mut variables = BTreeMap::new();
            let mut secrets = BTreeMap::new();
            let mut forbidden_secrets = Vec::new();
            let mut release_targets = Vec::new();
            for tag in repo.tags.iter().filter_map(|t| raw.tags.get(t)) {
                variables.extend(tag.variables.clone());
                secrets.extend(tag.secrets.iter().map(|(k, v)| (k.clone(), expand_home(v))));
                forbidden_secrets.extend(tag.forbidden_secrets.iter().cloned());
                for target in &tag.release_targets {
                    if !release_targets.contains(target) {
                        release_targets.push(target.clone());
                    }
                }
            }

            repos.push(Repo {
                name,
                tags: repo.tags,
                settings,
                actions,
                rulesets,
                variables,
                secrets,
                forbidden_secrets,
                release_tag_prefix: if repo.releases {
                    repo.release_tag_prefix
                        .or_else(|| raw.release_tag_prefix.clone())
                } else {
                    None
                },
                release_targets: if repo.releases {
                    repo.release_targets.unwrap_or(release_targets)
                } else {
                    Vec::new()
                },
            });
        }

        Ok(Self {
            owner: raw.owner,
            repos,
        })
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
