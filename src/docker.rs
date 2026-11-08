//! A repository's root Dockerfile, read through the API.

use anyhow::Result;

use crate::gh::Gh;

/// The commands in `expected` that no `RUN` instruction of the default
/// branch's Dockerfile runs, or `None` without a Dockerfile.
pub fn missing_runs(gh: &Gh, repo: &str, expected: &[String]) -> Result<Option<Vec<String>>> {
    let Some(text) = gh.file(repo, "Dockerfile", None)? else {
        return Ok(None);
    };
    Ok(Some(missing(&text, expected)))
}

fn missing(dockerfile: &str, expected: &[String]) -> Vec<String> {
    let runs: Vec<String> = instructions(dockerfile)
        .into_iter()
        .filter_map(|line| {
            let (keyword, rest) = line.split_once(' ')?;
            keyword.eq_ignore_ascii_case("RUN").then(|| rest.to_owned())
        })
        .collect();
    expected
        .iter()
        .filter(|command| {
            let command = words(command);
            !runs.iter().any(|run| words(run).contains(&command))
        })
        .map(|command| format!("Dockerfile: no RUN of `{command}`"))
        .collect()
}

/// One line per instruction, with continuation lines joined and comments and
/// blank lines left out, as Docker reads them.
fn instructions(dockerfile: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for line in dockerfile.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(start) = line.strip_suffix('\\') {
            current.push_str(start);
            current.push(' ');
        } else {
            current.push_str(line);
            lines.push(words(&current));
            current.clear();
        }
    }
    if !current.trim().is_empty() {
        lines.push(words(&current));
    }
    lines
}

/// `text` with every run of whitespace made a single space.
fn words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expected() -> Vec<String> {
        vec!["cargo chef cook".to_owned()]
    }

    #[test]
    fn finds_commands_in_run_instructions() {
        let chef = "FROM rust AS builder\n\
                    RUN cargo chef cook --release --recipe-path recipe.json\n\
                    RUN cargo build --release\n";
        assert_eq!(missing(chef, &expected()), Vec::<String>::new());

        let plain = "FROM rust AS builder\nRUN cargo build --release\n";
        assert_eq!(
            missing(plain, &expected()),
            ["Dockerfile: no RUN of `cargo chef cook`"]
        );
    }

    #[test]
    fn joins_continuation_lines() {
        let split = "RUN apt-get update \\\n\
                     # a comment inside the instruction\n\
                     \x20   && cargo chef \\\n\
                     \x20      cook --release\n";
        assert_eq!(missing(split, &expected()), Vec::<String>::new());
    }

    #[test]
    fn ignores_comments_and_other_instructions() {
        let mentioned = "# RUN cargo chef cook\nLABEL note=\"cargo chef cook\"\nRUN cargo build\n";
        assert_eq!(missing(mentioned, &expected()).len(), 1);
    }
}
