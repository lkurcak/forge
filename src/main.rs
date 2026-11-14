mod cargo;
mod days;
mod dependabot;
mod deps;
mod diff;
mod docker;
mod gh;
mod init;
mod leaks;
mod manifest;
mod pins;
mod progress;
mod reconcile;
mod release;
mod runs;
mod stale;
mod web;
mod workflow;

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::gh::Gh;
use crate::manifest::{Manifest, Repo};
use crate::progress::Progress;
use crate::reconcile::{Mode, Outcome, reconcile};
use crate::workflow::SharedWorkflow;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Manifest describing the managed repositories [default: ./forge.toml,
    /// else forge/forge.toml in the config directory, like ~/.config]
    #[arg(long, global = true, env = "FORGE_MANIFEST")]
    manifest: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Report where repositories drift from the manifest.
    Check {
        /// Repositories to check (default: all managed).
        repos: Vec<String>,
    },
    /// Bring repositories in line with the manifest.
    Apply {
        /// Repositories to update (default: all managed).
        repos: Vec<String>,
        /// Also overwrite secrets that already exist, from their local files
        /// (after renewing a token, say). Normally only missing ones are set.
        #[arg(long)]
        update_secrets: bool,
    },
    /// List managed repositories, and owned ones the manifest doesn't cover.
    List,
    /// Write a starter manifest listing every repository you own.
    ///
    /// It goes to --manifest, or ./forge.toml. Review it, then `forge check`.
    Init {
        /// Account whose repositories to manage [default: the one gh is
        /// signed in as]
        #[arg(long)]
        owner: Option<String>,
    },
    /// Open a PR in each repository whose `[pins]` input is behind the
    /// release it pins, moving it there, with auto-merge on.
    Bump {
        /// Repositories to bump (default: all managed).
        repos: Vec<String>,
        /// Only say which PRs would be opened.
        #[arg(long)]
        dry_run: bool,
    },
    /// Update each Rust repo's Cargo.lock to the newest versions its
    /// Cargo.toml allows, and open a PR with it once clippy and the tests
    /// pass on this machine.
    ///
    /// Repos are cloned into forge's cache directory, and kept there with
    /// their build cache for the next run.
    Deps {
        /// Repositories to update (default: every managed one with a
        /// Cargo.lock, but those with a `[deps] alerts_only` tag only while
        /// they have open Dependabot alerts).
        repos: Vec<String>,
        /// Update and run the checks, but open no PRs.
        #[arg(long)]
        dry_run: bool,
    },
    /// Scan every private repo you own for secrets committed to it, with
    /// gitleaks.
    ///
    /// GitHub's secret scanning covers public repos, but not a user's
    /// private ones. Every commit a branch, tag or PR reaches is scanned the
    /// first time, and only new ones after that. Secrets aren't shown, just
    /// where they are. Exits with 1 when something is found.
    Leaks {
        /// Repositories to scan, managed or not (default: every private
        /// one, forks and archived ones aside).
        repos: Vec<String>,
        /// Scan public repositories too.
        #[arg(long)]
        public: bool,
        /// Forget earlier runs: scan all history again, and report only
        /// what's found in it now.
        #[arg(long)]
        full: bool,
    },
    /// Check each deployed site the way a browser or phone meets it: its
    /// icons, the manifest that lets it install as an app, and a health
    /// check that answers without a login.
    ///
    /// Only repos with a `url` in the manifest are checked. Exits with 1
    /// when a check fails; warnings don't.
    Web {
        /// Repositories whose sites to check (default: every one with a url).
        repos: Vec<String>,
    },
    /// List open issues and PRs in any owned repo that have gone quiet.
    Stale {
        /// Days without activity before one counts as quiet.
        #[arg(long, default_value_t = 90)]
        days: i64,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    if let Command::Init { owner } = cli.command {
        let path = cli.manifest.unwrap_or_else(|| PathBuf::from(MANIFEST));
        return init::run(&path, owner).map(|()| ExitCode::SUCCESS);
    }

    let path = find_manifest(cli.manifest)?;
    let mut manifest = Manifest::load(&path)?;
    // Sites are checked from outside, without GitHub.
    if let Command::Web { repos } = &cli.command {
        return web::run(&select(&manifest, repos)?, !repos.is_empty());
    }
    let gh = Gh::new(&manifest.owner)?;
    manifest.fetch_workflows(|caller, uses| SharedWorkflow::fetch(&gh, caller, uses))?;
    if matches!(cli.command, Command::Check { .. } | Command::Bump { .. }) {
        manifest.fetch_pins(&gh)?;
    }

    match cli.command {
        Command::Check { repos } => sync(&gh, &manifest, &repos, Mode::Check),
        Command::Apply {
            repos,
            update_secrets,
        } => sync(&gh, &manifest, &repos, Mode::Apply { update_secrets }),
        Command::List => list(&gh, &manifest),
        Command::Stale { days } => stale(&gh, &manifest, days),
        Command::Bump { repos, dry_run } => bump(&gh, &manifest, &repos, dry_run),
        Command::Deps { repos, dry_run } => {
            let selected = select(&manifest, &repos)?;
            Ok(deps::run(
                &gh,
                &manifest,
                &selected,
                !repos.is_empty(),
                dry_run,
                &forge_cache()?,
            ))
        }
        Command::Leaks {
            repos,
            public,
            full,
        } => leaks::run(
            &gh,
            &manifest.owner,
            &repos,
            public,
            full,
            &manifest.leaks.ignore,
            &forge_cache()?,
        ),
        Command::Init { .. } => unreachable!("handled before loading a manifest"),
        Command::Web { .. } => unreachable!("handled before reading GitHub"),
    }
}

const MANIFEST: &str = "forge.toml";

/// The manifest to use: the one given, else `./forge.toml`, else
/// `forge/forge.toml` in the config directory. Symlinks are resolved, so
/// custom rulesets are found next to the real file.
fn find_manifest(given: Option<PathBuf>) -> Result<PathBuf> {
    let path = if let Some(path) = given {
        path
    } else {
        let config = config_dir().map(|dir| dir.join("forge").join(MANIFEST));
        match config {
            _ if Path::new(MANIFEST).exists() => PathBuf::from(MANIFEST),
            Some(config) if config.exists() => config,
            config => bail!(
                "no {MANIFEST} here{}; write one with `forge init`, or pass --manifest",
                config.map_or_else(String::new, |c| format!(" or at {}", c.display()))
            ),
        }
    };
    fs::canonicalize(&path).with_context(|| format!("reading {}", path.display()))
}

/// `$XDG_CONFIG_HOME`, else `~/.config`, else (on Windows) `%APPDATA%`.
fn config_dir() -> Option<PathBuf> {
    let var = |name| std::env::var_os(name).filter(|v| !v.is_empty());
    var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| var("HOME").map(|home| Path::new(&home).join(".config")))
        .or_else(|| var("APPDATA").map(PathBuf::from))
}

/// forge's directory in [`cache_dir`].
fn forge_cache() -> Result<PathBuf> {
    Ok(cache_dir()
        .context("no cache directory: set XDG_CACHE_HOME or HOME")?
        .join("forge"))
}

/// `$XDG_CACHE_HOME`, else `~/.cache`, else (on Windows) `%LOCALAPPDATA%`.
fn cache_dir() -> Option<PathBuf> {
    let var = |name| std::env::var_os(name).filter(|v| !v.is_empty());
    var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| var("HOME").map(|home| Path::new(&home).join(".cache")))
        .or_else(|| var("LOCALAPPDATA").map(PathBuf::from))
}

/// How many repositories to work on at once.
const WORKERS: usize = 8;

fn sync(gh: &Gh, manifest: &Manifest, names: &[String], mode: Mode) -> Result<ExitCode> {
    let repos = select(manifest, names)?;
    let progress = Progress::new(&repos.iter().map(|r| r.name.as_str()).collect::<Vec<_>>());
    let mut failed = false;

    in_parallel(
        &repos,
        |i, repo| {
            progress.start(i);
            let outcome = reconcile(gh, manifest, repo, mode);
            progress.finish(i, mark(&outcome, mode));
            outcome
        },
        |i, outcome| {
            progress.report(i, &report(&repos[i].name, &outcome, mode));
            failed |= !outcome.errors.is_empty()
                || !outcome.manual.is_empty()
                || !outcome.behind.is_empty()
                || (mode == Mode::Check && !outcome.drift.is_empty());
        },
    );

    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn mark(outcome: &Outcome, mode: Mode) -> &'static str {
    if !outcome.errors.is_empty() {
        "!"
    } else if !outcome.manual.is_empty() || !outcome.behind.is_empty() {
        "✗"
    } else if outcome.drift.is_empty() {
        "✓"
    } else if mode == Mode::Check {
        "✗"
    } else {
        "↻"
    }
}

/// A repo's mark and name, then a line per finding.
fn report(name: &str, outcome: &Outcome, mode: Mode) -> String {
    let mut text = format!("{} {name}", mark(outcome, mode));
    let sections = [
        ("", "", &outcome.drift),
        ("", " (fix by hand)", &outcome.manual),
        ("pending: ", "", &outcome.pending),
        ("behind: ", " (`forge bump` opens a PR)", &outcome.behind),
        ("skipped ", "", &outcome.skipped),
        ("error: ", "", &outcome.errors),
    ];
    for (prefix, suffix, lines) in sections {
        for line in lines {
            write!(text, "\n    {prefix}{line}{suffix}").expect("writing to a String");
        }
    }
    text
}

/// Maps `items` through `f` on a few threads, handing each result to `emit`
/// as soon as it and every result before it are in, so output keeps the
/// items' order without waiting for all of them. `f` and `emit` get the
/// item's index too.
fn in_parallel<T: Sync, R: Send>(
    items: &[T],
    f: impl Fn(usize, &T) -> R + Sync,
    emit: impl FnMut(usize, R) + Send,
) {
    struct Done<R, E> {
        results: Vec<Option<R>>,
        emitted: usize,
        emit: E,
    }
    let next = AtomicUsize::new(0);
    let done = Mutex::new(Done {
        results: items.iter().map(|_| None).collect(),
        emitted: 0,
        emit,
    });
    thread::scope(|scope| {
        for _ in 0..WORKERS.min(items.len()) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let result = f(i, item);
                    let mut guard = done.lock().expect("worker panicked");
                    let done = &mut *guard;
                    done.results[i] = Some(result);
                    while let Some(result) =
                        done.results.get_mut(done.emitted).and_then(Option::take)
                    {
                        let at = done.emitted;
                        (done.emit)(at, result);
                        done.emitted += 1;
                    }
                    drop(guard);
                }
            });
        }
    });
}

fn select<'a>(manifest: &'a Manifest, names: &[String]) -> Result<Vec<&'a Repo>> {
    if names.is_empty() {
        return Ok(manifest.repos.iter().collect());
    }
    names
        .iter()
        .map(
            |name| match manifest.repos.iter().find(|r| &r.name == name) {
                Some(repo) => Ok(repo),
                None => bail!("{name} is not in the manifest"),
            },
        )
        .collect()
}

fn list(gh: &Gh, manifest: &Manifest) -> Result<ExitCode> {
    let calls = |repo: &Repo| {
        manifest
            .workflows
            .iter()
            .filter_map(
                |workflow| match workflow.call(gh, &manifest.owner, &repo.name) {
                    Ok(Some(call)) => Some(format!("{}: {}", workflow.name, call.describe())),
                    Ok(None) => None,
                    Err(err) => Some(format!("error: {err:#}")),
                },
            )
            .collect::<Vec<_>>()
    };
    in_parallel(
        &manifest.repos,
        |_, repo| calls(repo),
        |i, calls| {
            let repo = &manifest.repos[i];
            let tags: Vec<_> = repo.tags.iter().map(|t| format!("#{t}")).collect();
            println!(
                "{:<20} {:<16} {}",
                repo.name,
                tags.join(" "),
                calls.join(" | ")
            );
        },
    );

    let unmanaged: Vec<_> = gh
        .source_repos(&manifest.owner)?
        .into_iter()
        .filter(|name| !manifest.repos.iter().any(|r| &r.name == name))
        .collect();
    if !unmanaged.is_empty() {
        println!("\nnot managed (excluding forks and archived):");
        for name in unmanaged {
            println!("  {name}");
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn bump(gh: &Gh, manifest: &Manifest, names: &[String], dry_run: bool) -> Result<ExitCode> {
    let repos = select(manifest, names)?;
    let mut failed = false;
    for (file, pin, latest) in &manifest.pins {
        let Some(latest) = latest else { continue };
        for repo in &repos {
            let result =
                pins::behind(gh, &manifest.owner, &repo.name, file, pin, latest).and_then(|b| {
                    match b {
                        None => Ok(None),
                        Some(_) if dry_run => Ok(Some("would open a PR".to_owned())),
                        Some(b) => Ok(Some(
                            pins::bump(gh, &manifest.owner, pin, &b)?
                                .unwrap_or_else(|| "a PR is open already".to_owned()),
                        )),
                    }
                });
            match result {
                Ok(Some(what)) => println!(
                    "{} {}: {} → {}: {what}",
                    repo.name, file, pin.version, latest.tag
                ),
                Ok(None) => {}
                Err(err) => {
                    failed = true;
                    eprintln!("{} {file}: error: {err:#}", repo.name);
                }
            }
        }
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn stale(gh: &Gh, manifest: &Manifest, days: i64) -> Result<ExitCode> {
    let repos = stale::find(gh, &manifest.owner, days, &manifest.stale.ignore_labels)?;
    if repos.is_empty() {
        println!("nothing open has been quiet for {days} days");
    }
    for (repo, lines) in repos {
        println!("{repo}");
        for line in lines {
            println!("    {line}");
        }
    }
    Ok(ExitCode::SUCCESS)
}
