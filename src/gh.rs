use std::fmt::Write as _;
use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// A repository, as `forge init` sees it.
pub struct RepoSummary {
    pub name: String,
    pub public: bool,
    pub language: Option<String>,
}

/// Calls the GitHub API through the `gh` CLI, as a specific account.
pub struct Gh {
    token: String,
}

impl Gh {
    /// Uses `GH_TOKEN` if set, otherwise the token gh holds for `owner`,
    /// otherwise (for an organization, say) the active gh account's.
    ///
    /// Asking for the owner's account first means forge works even when the
    /// active gh account is a different one.
    pub fn new(owner: &str) -> Result<Self> {
        if let Ok(token) = std::env::var("GH_TOKEN") {
            return Ok(Self { token });
        }
        let token = |user: Option<&str>| -> Result<Option<String>> {
            let mut cmd = Command::new("gh");
            cmd.args(["auth", "token", "--hostname", "github.com"]);
            if let Some(user) = user {
                cmd.args(["--user", user]);
            }
            let out = cmd.output().context("running gh")?;
            Ok(out
                .status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned()))
        };
        match token(Some(owner))?.or(token(None)?) {
            Some(token) => Ok(Self { token }),
            None => bail!("gh isn't signed in; run `gh auth login`"),
        }
    }

    pub fn get(&self, path: &str) -> Result<Value> {
        self.api("GET", path, None)
    }

    pub fn api(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut cmd = Command::new("gh");
        cmd.args(["api", "--method", method, path])
            .env("GH_TOKEN", &self.token)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if body.is_some() {
            cmd.args(["--input", "-"]).stdin(Stdio::piped());
        }
        let mut child = cmd.spawn().context("running gh")?;
        if let Some(body) = body {
            let mut stdin = child.stdin.take().expect("stdin is piped");
            stdin.write_all(&serde_json::to_vec(body)?)?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!(
                "{method} {path}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        if out.stdout.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&out.stdout)
            .with_context(|| format!("{method} {path}: unexpected response"))
    }

    /// A file's contents at `git_ref` (the default branch if `None`), or
    /// `None` if it doesn't exist there.
    pub fn file(&self, repo: &str, path: &str, git_ref: Option<&str>) -> Result<Option<String>> {
        let mut endpoint = format!("repos/{repo}/contents/{path}");
        if let Some(git_ref) = git_ref {
            write!(endpoint, "?ref={git_ref}")?;
        }
        let out = Command::new("gh")
            .args(["api", "-H", "Accept: application/vnd.github.raw", &endpoint])
            .env("GH_TOKEN", &self.token)
            .output()
            .context("running gh")?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("(HTTP 404)") {
                return Ok(None);
            }
            bail!("GET {endpoint}: {}", stderr.trim());
        }
        Ok(Some(String::from_utf8(out.stdout)?))
    }

    /// Turns on auto-merge (squash) for the PR at `url`: it merges once its
    /// required checks pass.
    pub fn auto_merge(&self, url: &str) -> Result<()> {
        let out = Command::new("gh")
            .args(["pr", "merge", url, "--auto", "--squash"])
            .env("GH_TOKEN", &self.token)
            .output()
            .context("running gh")?;
        if !out.status.success() {
            bail!(
                "turning on auto-merge for {url}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// Sets an Actions secret; gh takes care of encrypting it for the repo.
    pub fn set_secret(&self, repo: &str, name: &str, value: &[u8]) -> Result<()> {
        let mut child = Command::new("gh")
            .args(["secret", "set", name, "--repo", repo])
            .env("GH_TOKEN", &self.token)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("running gh")?;
        child
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(value)?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!(
                "setting secret {name}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// The account gh is signed in as.
    pub fn active_user() -> Result<String> {
        let out = Command::new("gh")
            .args(["api", "user", "--jq", ".login"])
            .output()
            .context("running gh")?;
        if !out.status.success() {
            bail!(
                "gh isn't signed in; run `gh auth login`\n{}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8(out.stdout)?.trim().to_owned())
    }

    /// Non-fork, non-archived repositories owned by `owner`, with whether
    /// each is public and its main language.
    pub fn source_repo_summaries(&self, owner: &str) -> Result<Vec<RepoSummary>> {
        let out = Command::new("gh")
            .args(["repo", "list", owner, "--source", "--no-archived"])
            .args([
                "--limit",
                "1000",
                "--json",
                "name,visibility,primaryLanguage",
            ])
            .env("GH_TOKEN", &self.token)
            .output()
            .context("running gh")?;
        if !out.status.success() {
            bail!(
                "listing repos: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let repos: Vec<Value> = serde_json::from_slice(&out.stdout)?;
        Ok(repos
            .iter()
            .filter_map(|repo| {
                Some(RepoSummary {
                    name: repo["name"].as_str()?.to_owned(),
                    public: repo["visibility"] == "PUBLIC",
                    language: repo["primaryLanguage"]["name"].as_str().map(str::to_owned),
                })
            })
            .collect())
    }

    /// Non-fork, non-archived repositories owned by `owner`.
    pub fn source_repos(&self, owner: &str) -> Result<Vec<String>> {
        let out = Command::new("gh")
            .args([
                "repo",
                "list",
                owner,
                "--source",
                "--no-archived",
                "--limit",
                "1000",
            ])
            .args(["--json", "name", "--jq", ".[].name"])
            .env("GH_TOKEN", &self.token)
            .output()
            .context("running gh")?;
        if !out.status.success() {
            bail!(
                "listing repos: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8(out.stdout)?
            .lines()
            .map(str::to_owned)
            .collect())
    }
}
