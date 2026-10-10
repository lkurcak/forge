# forge

Keeps my GitHub repositories in line with one manifest: merge settings,
Actions permissions and branch rulesets.

```sh
cargo install --path .

forge list            # managed repos, plus owned repos the manifest doesn't cover
forge check [REPO…]   # report drift; exits 1 if anything differs
forge apply [REPO…]   # fix drift
```

- `forge.toml` lists the managed repos. Top-level `settings`, `actions` and
  `rulesets` apply to all of them; a repo's own table overrides them.
- `rulesets/*.json` are ruleset bodies in the
  [REST API shape](https://docs.github.com/en/rest/repos/rules), matched by file
  name. Repos opt in with `rulesets = [...]`.
- `[tags.<name>]` adds Actions variables and secrets to every repo with that tag.
  Secret values come from local files (e.g. `~/.config/forge/tap-app.pem`) and are
  only set when the secret is missing, since GitHub never reveals them.
- Rust repos release through the shared
  [`rust-release.yml`](.github/workflows/rust-release.yml), calling it from
  their own `.github/workflows/release.yml`. The inputs they pass (`crates-io`,
  `binaries`, `homebrew`, `winget`) are where they say what they ship. forge
  reads that call: each input turned on counts as a tag, so it brings that tag's
  secrets, and calling the workflow at all brings the `rust-release` tag.
- The latest GitHub release is checked too: its tag must look like
  `release_tag_prefix` + `X.Y.Z`, it must have a
  `{repo}-vX.Y.Z-{target}.tar.gz` (`.zip` on Windows) for each target the
  workflow builds, and it must have reached crates.io, the `homebrew-tap`
  formula and winget-pkgs, for repos shipping there. An open PR delivering it
  shows as `pending`; the rest is only reported, since the fix is the next
  release.

Only keys present in the manifest are checked, so the live state can carry
anything else without counting as drift.

## Credentials

forge calls the API through `gh`, using the token gh holds for the manifest's
`owner` (`gh auth token --user <owner>`), so the active gh account doesn't
matter. `GH_TOKEN` overrides this.
