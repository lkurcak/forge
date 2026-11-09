# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/lkurcak/forge/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/lkurcak/forge/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/lkurcak/forge/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/lkurcak/forge/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/lkurcak/forge/releases/tag/v0.1.0
