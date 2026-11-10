# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.0] - 2026-11-10

### Added

- `forge leaks` scans every private repo you own for secrets committed to
  it, with gitleaks: what GitHub's secret scanning does for public repos
  but not for a user's private ones. It reports where each one is, never the
  secret itself, and exits 1 if anything is found. The first run scans all
  history, including PRs' commits; later runs only new commits. `[leaks]
  ignore` in the manifest silences findings that are revoked or aren't
  secrets.

## [0.6.0] - 2026-11-10

### Added

- `forge deps` marks its PR's commit with a `forge/deps` status saying
  clippy and the tests passed locally, and `rust-ci.yml` with `light: true`
  skips CI on that PR instead of running the same checks again, so the
  update costs a private repo no Actions minutes.

## [0.5.1] - 2026-11-09

### Fixed

- `security_and_analysis.dependabot_security_updates` works on private
  repos: forge reads it from `/automated-security-fixes`, since private repos
  don't report `security_and_analysis`. It used to show as drift there
  forever, even after `forge apply` turned the updates on.

## [0.5.0] - 2026-11-09

### Added

- `vulnerability_alerts = true` in `[settings]` turns on Dependabot alerts,
  and `forge check` reports a repo where they're off. They're free on
  private repos too, and `forge deps` needs them for `alerts_only` tags.

### Fixed

- `forge check` counts an open `forge deps` PR as the fix for the Dependabot
  alerts it resolves, as it does Dependabot's own PRs, instead of reporting
  that no PR bumps them.

## [0.4.0] - 2026-11-09

### Added

- `private_ci = "light"` in the manifest has `forge check` report a private
  repo calling `rust-ci.yml` without `light: true`, and a public one calling
  it with it. Any input of the shared workflows can be named that way.
- `forge deps`: one PR per Rust repo moving Cargo.lock to the newest
  versions its Cargo.toml allows, opened only after clippy and the tests
  pass on your machine, with auto-merge where the repo allows it. It also
  lists the newer versions Cargo.toml holds back. Tags in
  `[deps] alerts_only` leave quiet repos alone until Dependabot raises an
  alert.

### Changed

- `rust-ci.yml` with `light` keeps the Rust build cache from the default
  branch only, so pull requests no longer spend time uploading caches that
  nothing reads again.
- `t-ci.yml` is a single job, `ok`, instead of a test job and a separate
  `ok` job waiting on it: one billed job a run instead of two.
- `pr-title.yml` skips private repos, whose minutes are billed: a minute
  for every PR event. With `private_rulesets = true`, forge no longer applies
  `require-pr-title` to private repos, where the skipped check would always
  pass.

## [0.3.0] - 2026-11-09

### Added

- `rust-ci.yml`'s `light` input, for repos whose Actions minutes are
  billed: pull requests, scheduled and manual runs get one Linux job, `ok`,
  running fmt, clippy and the tests (`tests: false` leaves them out), and a
  push only builds and publishes the container image, for linux/amd64
  unless `arm64: true`. That's two or three billed minutes a pull request
  instead of a dozen or more, and none for a push without an image.

### Changed

- Private repos only get rulesets with the manifest's new
  `private_rulesets = true`: they need GitHub Pro, which can lapse without
  the manifest changing. forge used to try, and skip them when GitHub
  refused.
- `rust-ci.yml` checks a `vX.Y.Z` tag against Cargo.toml's version in a job
  of its own, `version`, before building any image, instead of after.

### Fixed

- `rust-release.yml` on a default-branch push with nothing to release ran
  the jobs that also run after a skipped one (`crates-io`, the GitHub
  release, Homebrew, winget), which the release environment then rejected:
  a failed run, though nothing was published. They need `verify` to have
  succeeded now.

## [0.2.0] - 2026-11-09

### Added

- `[pins]`: shared-workflow inputs that pin another repo's release (a T
  repo's `t-version` and `t-sha256`). `forge check` reports one that's
  behind; `forge bump` opens a PR moving it, with auto-merge on.
- The `require-ci` ruleset, requiring the `ci / ok` check that `rust-ci.yml`
  and `t-ci.yml` now end with: it passes when every job of theirs passed or
  was skipped. With it, a PR's auto-merge waits for all of CI.
- `rust-release.yml` can be called on pushes to the default branch too: a
  version in Cargo.toml with no tag yet is tagged, and released from the tag
  (callers add the trigger, `workflow_dispatch` and `actions: write`).

## [0.1.1] - 2026-11-08

### Fixed

- Organizations' repositories can be managed: when gh isn't signed in as the
  manifest's `owner`, forge uses the active gh account instead of failing.

## [0.1.0] - 2026-11-08

### Added

- First release, as the `forgectl` crate; the command is `forge`.
- `forge check`, `apply`, `list` and `stale`, driven by a manifest found at
  `--manifest`, `$FORGE_MANIFEST`, `./forge.toml` or the config directory.
- `forge init` writes a starter manifest listing every repo an account owns.
- Built-in rulesets: `protect-default-branch`, `require-pr`,
  `require-pr-title` and `protect-release-tags`.
- Prebuilt binaries for Windows, macOS and Linux are attached to each
  [GitHub release](https://github.com/lkurcak/forge/releases).

[Unreleased]: https://github.com/lkurcak/forge/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/lkurcak/forge/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/lkurcak/forge/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/lkurcak/forge/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/lkurcak/forge/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/lkurcak/forge/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/lkurcak/forge/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/lkurcak/forge/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/lkurcak/forge/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/lkurcak/forge/releases/tag/v0.1.0
