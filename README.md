# forge

Keeps a GitHub account's repositories in line with one manifest. It sets what
the API can set: repo settings, Actions permissions, rulesets, variables,
secrets and deployment environments. It checks and reports what needs a
commit or a person to fix: shared CI workflows, Cargo lints, Dockerfiles,
releases that didn't reach crates.io, Homebrew or winget, and open Dependabot
alerts.

## Install

```sh
cargo install forgectl            # installs the `forge` command
brew install lkurcak/tap/forge
```

Binaries for Linux, macOS and Windows are on the
[releases page](https://github.com/lkurcak/forge/releases). forge calls the
API through the [GitHub CLI](https://cli.github.com/), so `gh` must be
installed and signed in.

## Getting started

```sh
forge init     # write forge.toml, listing every repo you own
forge check    # report where repos differ from it; exits 1 if any do
forge apply    # fix what forge can, and list the rest
```

`forge init` tags each repo by language and visibility, under a starter set
of settings and tags. Remove repos forge shouldn't manage and adjust the rest
before the first `forge apply`. Only keys in the manifest are checked, so
anything it leaves out stays as it is.

The manifest is the first of:

1. `--manifest PATH`, or `$FORGE_MANIFEST`;
2. `forge.toml` in the current directory;
3. `forge/forge.toml` in the config directory: `$XDG_CONFIG_HOME`, else
   `~/.config` (`%APPDATA%` on Windows).

A repo of its own gives the manifest history and review. Symlink it into the
config directory to run forge from anywhere.

## Commands

```sh
forge check [REPO…]         # report drift; exits 1 if anything differs
forge apply [REPO…]         # fix drift
forge list                  # managed repos, and owned ones the manifest doesn't cover
forge stale [--days 90]     # open issues and PRs nobody has touched in that long
forge init [--owner NAME]   # write a starter manifest
```

`forge stale` covers every repo the owner has, managed or not, leaving out
issues with a label in `[stale] ignore_labels` (a backlog of feature ideas,
say). It only lists them.

## Manifest

```toml
owner = "you"
release_tag_prefix = "v"

[settings]
allow_merge_commit = false

[tags.rust]
calls = ["rust-ci"]

[repos.tool]
tags = ["rust", "security"]
rulesets = ["protect-default-branch"]
```

- Top-level `settings`, `actions` and `rulesets` apply to every repo. A
  repo's own table overrides them: `settings` and `actions` key by key,
  `rulesets` wholesale.
- `[settings]` goes to `PATCH /repos/{owner}/{repo}`, so any key that
  endpoint takes works.
- `[actions]` goes to `/actions/permissions` (whether Actions run and which
  actions may), `[actions.workflow]` to `/actions/permissions/workflow` (the
  `GITHUB_TOKEN`'s defaults), and `[actions.selected-actions]` to the
  allowlist, checked while `allowed_actions` is `selected`.
- `rulesets = [...]` names rulesets, applied as repository rulesets. Private
  repos are skipped, since rulesets there need GitHub Pro. Built in:
  - `protect-default-branch`: no deleting or force-pushing the default branch
  - `require-pr`: changes to the default branch go through a PR
  - `require-pr-title`: the shared PR title check must pass
  - `protect-release-tags`: no deleting or moving `vX.Y.Z` tags

  A `rulesets/NAME.json` next to the manifest adds a ruleset, or replaces a
  built-in one, with a body in the
  [REST API shape](https://docs.github.com/en/rest/repos/rules).
- `release_tag_prefix`: the latest GitHub release of each repo must be
  tagged this plus `X.Y.Z`. A repo can set its own, or `releases = false` for
  releases that aren't versions of it (a Homebrew tap's bottles, say).

### Tags

A repo's `tags = [...]` bring the requirements under `[tags.NAME]`. A tag
without a table is just a label. A tag can carry:

- `settings`, applied over the top-level ones and under the repo's own;
- `rulesets`, added to the repo's own;
- `variables = { NAME = "value" }`, Actions variables;
- `secrets = { NAME = "~/path" }`, Actions secrets, read from local files.
  They're only set when missing, since GitHub never reveals their values;
  `forge apply --update-secrets` overwrites them, after renewing a token, say.
  `forbidden_secrets = [...]` must not exist;
- `[tags.NAME.environments.ENV]`, a deployment environment that must exist.
  `deploy_tags = [...]` lets only those tag patterns deploy to it, and no
  branch;
- `calls = [...]`, shared workflows the repo must call (see below). A repo
  declines one with the `no-WORKFLOW` tag;
- `needs_public = true`, for features GitHub only offers public repos. Every
  public repo then has to carry the tag or decline it with `no-NAME`, and a
  private one carrying it is reported, so a visibility change shows up;
- `[tags.NAME.lints]`, lints every Cargo package must set (or inherit from
  `[workspace.lints]`), at least this strictly, in Cargo's `[lints]` shape;
- `dockerfile_runs = [...]`, commands some `RUN` of the root Dockerfile must
  run.

Lints, Dockerfiles and workflow calls are only reported, since fixing them
means committing to the repo.

### Shared workflows

`[workflows]` maps the file a repo calls a shared workflow from to the
workflow's `uses:` reference:

```toml
[workflows]
"ci.yml" = "lkurcak/forge/.github/workflows/rust-ci.yml@v1"
```

forge reads each shared workflow at that ref, and checks that callers use the
same ref and only inputs it has. Calling one tags the repo with the
workflow's name (`rust-ci`) and with each input it turns on
(`container: true` tags it `container`), so those tags' requirements apply.

This repo's workflows are public, and anyone can call them at `@v1`, which
moves along with compatible changes:

- [`rust-ci.yml`](.github/workflows/rust-ci.yml): fmt, clippy, tests on
  Linux, macOS and Windows, docs, packaging and the MSRV. With
  `container: true` it builds and publishes a multi-arch image to ghcr.io
  instead of testing on macOS and Windows.
- [`rust-release.yml`](.github/workflows/rust-release.yml): on a `vX.Y.Z`
  tag, publishes to crates.io through trusted publishing, attaches binaries
  to the GitHub release, and bumps the formula in `{owner}/homebrew-tap` and
  the package in winget-pkgs. forge then checks the latest release reached
  each of them.
- [`pr-title.yml`](.github/workflows/pr-title.yml): PR titles follow
  [Conventional Commits](https://www.conventionalcommits.org/).
- [`t-ci.yml`](.github/workflows/t-ci.yml): checks repos written in
  [T](https://github.com/lkurcak/t-lang).

Each file's header shows how to call it, and its inputs say what they do.

### Also reported

- A latest release whose tag doesn't match `release_tag_prefix`, that lacks
  one of the archives its release workflow builds, or that hasn't reached
  crates.io, the Homebrew formula or winget-pkgs. An open PR delivering it
  shows as pending.
- Open Dependabot alerts, as pending while a bot PR fixing one is under a
  week old. Bot PRs open longer than a week are reported too.

## Credentials

forge uses the token gh holds for the manifest's `owner`
(`gh auth token --user OWNER`), so the active gh account doesn't matter.
`GH_TOKEN` overrides it.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).
