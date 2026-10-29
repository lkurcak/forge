# forge

Keeps my GitHub repositories in line with one manifest: merge settings,
Actions permissions and branch rulesets.

```sh
cargo install --path .

forge list            # managed repos, plus owned repos the manifest doesn't cover
forge check [REPO…]   # report drift; exits 1 if anything differs
forge apply [REPO…]   # fix drift
forge stale           # open issues and PRs nobody has touched in 90 days
```

- `forge.toml` lists the managed repos. Top-level `settings`, `actions` and
  `rulesets` apply to all of them; a repo's own table overrides them.
- `[actions]` goes to `/actions/permissions` (whether Actions run and which
  actions may), `[actions.workflow]` to `/actions/permissions/workflow` (the
  `GITHUB_TOKEN`'s defaults), and `[actions.selected-actions]` to the
  allowlist, checked while `allowed_actions` is `selected`.
- `rulesets/*.json` are ruleset bodies in the
  [REST API shape](https://docs.github.com/en/rest/repos/rules), matched by file
  name. Repos opt in with `rulesets = [...]`, or get them from a tag's.
- `[tags.<name>]` adds Actions variables and secrets to every repo with that tag.
  Secret values come from local files (e.g. `~/.config/forge/tap-app.pem`) and are
  only set when the secret is missing, since GitHub never reveals them. Tags
  can carry `settings` too, applied over the top-level ones and under the repo's.
- A tag with `needs_public = true` only works on public repos. Every public
  repo must carry it or decline it with `no-<tag>`, and a private repo
  carrying it is reported, so making a repo public or private shows up.
- `[tags.<name>.environments.<env>]` makes a deployment environment exist;
  `deploy_tags = [...]` lets only those tag patterns deploy to it, and no branch.
- A tag's `calls = [...]` names shared workflows its repos must call; one
  that doesn't is reported, unless it carries `no-<workflow>` to decline it.
- `[tags.<name>.lints]` lists lints each Cargo package must set (or inherit
  from `[workspace.lints]`), in Cargo's `[lints]` shape. Missing ones are only
  reported, since fixing them means committing to the repo.
- Rust repos share their CI and release workflows,
  [`rust-ci.yml`](.github/workflows/rust-ci.yml) and
  [`rust-release.yml`](.github/workflows/rust-release.yml), calling them from
  their own `.github/workflows/ci.yml` and `release.yml` (`[workflows]` in the
  manifest). The inputs they pass (`crates-io`,
  `binaries`, `homebrew`, `winget`; `container` for CI) are where they say what they ship. forge
  reads that call: each input turned on counts as a tag, so it brings that tag's
  secrets, and calling the workflow at all brings the `rust-release` tag.
  Homebrew can't reach a private repo, so from one `homebrew` ships the
  `binaries` archives through a `{repo}-dist-vX.Y.Z` release on the tap.
- The latest GitHub release is checked too: its tag must look like
  `release_tag_prefix` + `X.Y.Z`, it must have a
  `{repo}-vX.Y.Z-{target}.tar.gz` (`.zip` on Windows) for each target the
  workflow builds, and it must have reached crates.io, the `homebrew-tap`
  formula and winget-pkgs, for repos shipping there. An open PR delivering it
  shows as `pending`; the rest is only reported, since the fix is the next
  release.
- Open Dependabot alerts are reported, as `pending` while a bot PR fixing
  one is under a week old; dismiss on GitHub any that don't apply. Bot PRs
  open longer than a week are reported too.

`forge stale [--days 90]` lists open issues and PRs in every owned repo,
managed or not, that have had no activity for that long, leaving out any
with a label in `[stale] ignore_labels` (a backlog of feature ideas, say).
It only lists them.

Only keys present in the manifest are checked, so the live state can carry
anything else without counting as drift.

Contributing to forge? See [CONTRIBUTING.md](CONTRIBUTING.md) for the PR title
convention. The [PR title workflow](.github/workflows/pr-title.yml) runs here
and can be called by other repos later; this pilot does not change their policy.
Merge the workflow before running `forge apply forge` to enable the required
check on forge's default branch.

## Credentials

forge calls the API through `gh`, using the token gh holds for the manifest's
`owner` (`gh auth token --user <owner>`), so the active gh account doesn't
matter. `GH_TOKEN` overrides this.
