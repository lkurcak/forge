# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/lkurcak/forge/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/lkurcak/forge/releases/tag/v0.1.0
