mod cargo;
mod days;
mod dependabot;
mod diff;
mod gh;
mod manifest;
mod progress;
mod reconcile;
mod release;
mod stale;
mod workflow;

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

use crate::gh::Gh;
use crate::manifest::{Manifest, Repo};
use crate::progress::Progress;
use crate::reconcile::{Mode, Outcome, reconcile};

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
    let manifest = Manifest::load(&cli.manifest)?;
    let gh = Gh::new(&manifest.owner)?;

    match cli.command {
        Command::Check { repos } => sync(&gh, &manifest, &repos, Mode::Check),
        Command::Apply {
            repos,
            update_secrets,
        } => sync(&gh, &manifest, &repos, Mode::Apply { update_secrets }),
        Command::List => list(&gh, &manifest),
        Command::Stale { days } => stale(&gh, &manifest, days),
    }
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
    } else if !outcome.manual.is_empty() {
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
