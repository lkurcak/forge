---
name: publish
description: Publish one of lkurcak's Rust repos to a package manager (winget, Homebrew, crates.io) or give it prebuilt binaries, through forge's shared release workflow. Use for requests like "publish gud to winget", "add tend to homebrew", "ship binaries for X", or moving a repo onto the shared release workflow.
---

# Publishing a repo

Everything about how a repo releases lives in `forge.toml`, as tags. The repo's own
`.github/workflows/release.yml` is generated from them and only calls
`.github/workflows/rust-release.yml` here. Adding a channel means adding a tag, letting
forge fix the repo, and cutting a release.

| Tag | What it does |
| --- | --- |
| `rust-release` | Repo releases through the shared workflow (required for the rest) |
| `crates-io` | `cargo publish` via trusted publishing |
| `binaries` | `{repo}-vX.Y.Z-{target}` archives on the GitHub release |
| `homebrew` | PR bumping `Formula/{repo}.rb` in `lkurcak/homebrew-tap` |
| `winget` | x64 + arm64 Windows zips, submitted to winget-pkgs as `lkurcak.{repo}` |

`forge check REPO` reports anything missing, including whether each channel has the latest
release (open PRs show as `pending`).

## Steps

1. **Check the repo first.** Clone or open it (`~/repos/*/REPO`). It must build on every
   target in its tags (`cargo build --target …` problems show up only in CI otherwise):
   Windows-only or Unix-only code paths are the usual suspects. The binary must be named
   like the repo.
2. **Tag it in `forge.toml`.** Add `rust-release` plus the channels. `winget` brings the
   Windows targets with it; it doesn't need `binaries`, but most repos want both.
3. **One-time prerequisites per channel**, if the repo is new to it:
   - `crates-io`: the crate needs a trusted publisher on crates.io (repo, workflow
     `release.yml`, environment `release`). Only the owner can set it up, in the crate's
     settings, and forge can't check it; ask them. forge creates the `release`
     environment itself. A crate that's never been published needs a first manual
     `cargo publish` by the owner.
   - `homebrew`: `Formula/{repo}.rb` must exist in homebrew-tap (copy `gud.rb`). The tap
     App needs no change.
   - `winget`: nothing until after the first release (step 6).
4. **Run `forge apply REPO`.** It sets secrets and variables and opens a PR generating
   `release.yml`. Writing workflow files needs gh's `workflow` scope; if it fails on that,
   have the user run `! gh auth refresh -h github.com -s workflow`. Merge the PR (ask the
   user) and check `forge check REPO` shows no `release.yml` drift.
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
(`git tag -f v1`; the user runs `git push origin main && git push -f origin v1`). An
incompatible change (renamed or removed input) needs `v2` and the `rust-release` tag in
`forge.toml` pointing at it, so `forge apply` migrates repos one PR at a time.

Runners per target are picked in the workflow's `verify` job; a new target needs a line
there.
