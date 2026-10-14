---
name: publish
description: Publish one of lkurcak's Rust repos to a package manager (winget, Homebrew, crates.io) or give it prebuilt binaries, through forge's shared release workflow. Use for requests like "publish gud to winget", "add tend to homebrew", "ship binaries for X", or moving a repo onto the shared release workflow.
---

# Publishing a repo

A Rust repo releases through forge's shared workflow,
`.github/workflows/rust-release.yml` here. The repo's own
`.github/workflows/release.yml` calls it, and the inputs it passes are where the repo
says what it ships:

| Input | What it does |
| --- | --- |
| `crates-io` | `cargo publish` via trusted publishing |
| `binaries` | `{repo}-vX.Y.Z-{target}` archives for `BINARIES_TARGETS` on the GitHub release |
| `homebrew` | PR bumping `Formula/{repo}.rb` in `lkurcak/homebrew-tap` |
| `winget` | Builds `WINGET_TARGETS` (x64 + arm64 Windows) and submits `lkurcak.{repo}` to winget-pkgs |

forge reads that call. Each input set to true counts as a tag in `forge.toml`, so
`forge apply` sets up what it needs (the tap App secrets, `WINGET_PAT`, the `release`
environment), and `forge check` reports what's missing, including whether each channel
has the latest release (open PRs show as `pending`). Adding a channel means turning an
input on, running `forge apply`, and cutting a release.

## Steps

1. **Check the repo first.** Clone or open it (`~/repos/*/REPO`). It must build on every
   target in its tags (`cargo build --target …` problems show up only in CI otherwise):
   Windows-only or Unix-only code paths are the usual suspects. The archives ship the
   binary named like the repo, or else the workspace's only binary (t-lang ships `t`);
   anything else fails the release's verify job. `scripts/winget-new.sh` still assumes
   `{repo}.exe`, so fix its manifest by hand for a differently named binary.
2. **Turn the inputs on** in the repo's `.github/workflows/release.yml`. If it doesn't
   call the shared workflow yet, replace the file with the call (copy gud's). The repo
   must be in `forge.toml` (any tags; channels don't go there). Commit it with the rest
   of the change.
3. **One-time prerequisites per channel**, if the repo is new to it:
   - `crates-io`: the crate needs a trusted publisher on crates.io (repo, workflow
     `release.yml`, environment `release`). Only the owner can set it up, in the crate's
     settings, and forge can't check it; ask them. forge creates the `release`
     environment itself. A crate that's never been published needs a first manual
     `cargo publish` by the owner.
   - `homebrew`: `Formula/{repo}.rb` must exist in homebrew-tap (copy `gud.rb`). The tap
     App needs no change.
   - `winget`: nothing until after the first release (step 6).
4. **Run `forge apply REPO`** once the `release.yml` change is on the default branch. It
   sets the secrets, variables and environment the inputs need. `forge check REPO` then
   shows only release-side problems.
5. **Release.** Bump the version in `Cargo.toml` (and `Cargo.lock`), add a CHANGELOG entry
   only for user-visible changes (new binaries or platforms count; CI changes don't), commit
   `release X.Y.Z`, tag `vX.Y.Z`. The user pushes — their SSH key has a passphrase — so give
   them `git push origin main vX.Y.Z`. Then follow the run with
   `gh run watch -R lkurcak/REPO <id>`. Binaries are built before anything is published,
   so a failed build leaves nothing half-released: fix it and re-tag.
6. **First winget version only:** the release's winget job skips with a warning, because
   the action can only update existing packages. Run `scripts/winget-new.sh REPO`, review
   the manifests it prints, then `scripts/winget-new.sh REPO --submit`. A moderator reviews
   the PR, usually within a day; every later release goes through on its own. Add
   `winget install lkurcak.REPO` to the repo's README once it's merged.
7. **Confirm** with `forge check REPO`: only `pending` lines (PRs under review) may remain.

## Changing the shared workflow

Callers use `@v1`, a moving tag. For compatible changes, commit and move the tag
(`git tag -f v1`; the user runs `git push origin main && git push -f origin v1`). For an
incompatible change (renamed or removed input), point `release_workflow` in `forge.toml`
at `v2`; `forge check` then lists every repo still calling `v1`, to migrate one by one.

Targets are the workflow's `{INPUT}_TARGETS` env lists, which forge reads too. Runners per
target are picked in its `verify` job; a new target may need a line there.
