use std::fmt::Write as _;
use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// Calls the GitHub API through the `gh` CLI, as a specific account.
pub struct Gh {
    token: String,
}

impl Gh {
    /// Uses `GH_TOKEN` if set, otherwise the token gh holds for `user`.
    ///
    /// Asking for a specific account means forge works even when the active
    /// gh account is a different one.
    pub fn new(user: &str) -> Result<Self> {
        if let Ok(token) = std::env::var("GH_TOKEN") {
            return Ok(Self { token });
        }
        let out = Command::new("gh")
            .args(["auth", "token", "--hostname", "github.com", "--user", user])
            .output()
            .context("running gh")?;
        if !out.status.success() {
            bail!(
                "gh has no credentials for {user}; run `gh auth login` and sign in as {user}\n{}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(Self {
            token: String::from_utf8(out.stdout)?.trim().to_owned(),
        })
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
