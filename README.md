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
- The latest GitHub release is checked too: its tag must look like
  `release_tag_prefix` + `X.Y.Z`, and repos with `release_targets` (e.g. via the
  `binaries` tag) must ship `{repo}-vX.Y.Z-{target}.tar.gz` (`.zip` on Windows)
  for each target. These are only reported; the fix is the next release.
- Repos tagged `rust-release` release through the shared
  [`rust-release.yml`](.github/workflows/rust-release.yml): forge renders their
  `.github/workflows/release.yml` from their tags (`crates-io`, `homebrew`,
  `binaries`, `winget`), and `apply` opens a PR when it differs.
- Tags naming a package manager also check it has the latest release: crates.io,
  the `homebrew-tap` formula and winget-pkgs. An open PR delivering it shows as
  `pending` and doesn't fail the check.

Only keys present in the manifest are checked, so the live state can carry
anything else without counting as drift.

## Credentials

forge calls the API through `gh`, using the token gh holds for the manifest's
`owner` (`gh auth token --user <owner>`), so the active gh account doesn't
matter. `GH_TOKEN` overrides this. Writing workflow files needs the `workflow`
scope: `gh auth refresh -h github.com -s workflow`.
