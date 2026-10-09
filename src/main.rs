mod diff;
mod gh;
mod manifest;
mod reconcile;
mod release;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

use crate::gh::Gh;
use crate::manifest::{Manifest, Repo};
use crate::reconcile::{Mode, reconcile};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Manifest describing the managed repositories.
    #[arg(long, global = true, default_value = "forge.toml")]
    manifest: PathBuf,

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
    let manifest = Manifest::load(&cli.manifest)?;
    let gh = Gh::new(&manifest.owner)?;

    match cli.command {
        Command::Check { repos } => sync(&gh, &manifest, &repos, Mode::Check),
        Command::Apply {
            repos,
            update_secrets,
        } => sync(&gh, &manifest, &repos, Mode::Apply { update_secrets }),
        Command::List => list(&gh, &manifest),
    }
}

/// How many repositories to work on at once.
const WORKERS: usize = 8;

fn sync(gh: &Gh, manifest: &Manifest, names: &[String], mode: Mode) -> Result<ExitCode> {
    let repos = select(manifest, names)?;
    let outcomes = in_parallel(&repos, |repo| reconcile(gh, &manifest.owner, repo, mode));
    let mut failed = false;

    for (repo, outcome) in repos.iter().zip(outcomes) {
        let mark = if !outcome.errors.is_empty() {
            "!"
        } else if !outcome.manual.is_empty() {
            "✗"
        } else if outcome.drift.is_empty() {
            "✓"
        } else if mode == Mode::Check {
            "✗"
        } else {
            "↻"
        };
        println!("{mark} {}", repo.name);
        for line in &outcome.drift {
            println!("    {line}");
        }
        for line in &outcome.manual {
            println!("    {line} (fix by hand)");
        }
        for line in &outcome.skipped {
            println!("    skipped {line}");
        }
        for line in &outcome.errors {
            println!("    error: {line}");
        }
        failed |= !outcome.errors.is_empty()
            || !outcome.manual.is_empty()
            || (mode == Mode::Check && !outcome.drift.is_empty());
    }

    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Maps `items` through `f` on a few threads, keeping the results in order.
fn in_parallel<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<R>>> = Mutex::new(items.iter().map(|_| None).collect());
    thread::scope(|scope| {
        for _ in 0..WORKERS.min(items.len()) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let result = f(item);
                    results.lock().expect("worker panicked")[i] = Some(result);
                }
            });
        }
    });
    results
        .into_inner()
        .expect("worker panicked")
        .into_iter()
        .map(|r| r.expect("every item is processed"))
        .collect()
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
    for repo in &manifest.repos {
        let tags: Vec<_> = repo.tags.iter().map(|t| format!("#{t}")).collect();
        println!("{:<24} {}", repo.name, tags.join(" "));
    }

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
