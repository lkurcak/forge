# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/lkurcak/forge/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/lkurcak/forge/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/lkurcak/forge/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/lkurcak/forge/releases/tag/v0.1.0
