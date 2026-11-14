# forge

Keeps a GitHub account's repositories in line with one manifest. It sets what
the API can set: repo settings, Actions permissions, rulesets, variables,
secrets and deployment environments. It checks and reports what needs a
commit or a person to fix: shared CI workflows, Cargo lints, Dockerfiles,
releases that didn't reach crates.io, Homebrew or winget, open Dependabot
alerts, and deployed sites without icons, an app manifest or a health check.

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
forge bump [--dry-run] [REPO…]  # open PRs moving [pins] to the latest release
forge deps [--dry-run] [REPO…]  # a PR per Rust repo updating Cargo.lock, tested here first
forge leaks [--public] [--full] [REPO…]  # secrets committed to private repos
forge web [REPO…]           # deployed sites' icons, app manifest and health check
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
  endpoint takes works. `vulnerability_alerts = true` also turns on
  Dependabot alerts, and
  `security_and_analysis.dependabot_security_updates = { status = "enabled" }`
  its PRs fixing them. Both are read and set through endpoints of their
  own, and unlike secret scanning they're free on private repos too
  (though the PRs' CI isn't).
- `[actions]` goes to `/actions/permissions` (whether Actions run and which
  actions may), `[actions.workflow]` to `/actions/permissions/workflow` (the
  `GITHUB_TOKEN`'s defaults), and `[actions.selected-actions]` to the
  allowlist, checked while `allowed_actions` is `selected`.
- `rulesets = [...]` names rulesets, applied as repository rulesets. Private
  repos only get theirs with `private_protection = true` at the top level,
  since rulesets there need GitHub Pro (or Team, for an organization): the
  plan can lapse without the manifest changing. Built in:
  - `protect-default-branch`: no deleting or force-pushing the default branch
  - `require-pr`: changes to the default branch go through a PR
  - `require-pr-title`: the shared PR title check must pass
  - `protect-release-tags`: no deleting or moving `vX.Y.Z` tags

  A `rulesets/NAME.json` next to the manifest adds a ruleset, or replaces a
  built-in one, with a body in the
  [REST API shape](https://docs.github.com/en/rest/repos/rules).
- `private_ci = "INPUT"` names an input of the shared workflows that
  private repos' calls must turn on and public ones' leave off, like
  `rust-ci.yml`'s `light`: only private repos' Actions minutes are billed.
  Off by default, so every repo runs full CI. Calls of a workflow without
  that input aren't affected.
- `release_tag_prefix`: the latest GitHub release of each repo must be
  tagged this plus `X.Y.Z`. A repo can set its own, or `releases = false` for
  releases that aren't versions of it (a Homebrew tap's bottles, say).
- A repo's `own_workflows = ["build.yml"]` lists the files in its
  `.github/workflows` that aren't calls of a shared workflow. Any other file
  there is reported, so a repo can't grow a workflow of its own without the
  manifest saying so; a comment next to the list is the place to say why.

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
  branch. Like rulesets, private repos only get theirs with
  `private_protection = true`, since that rule needs GitHub Pro there too;
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
same ref and only inputs it has, and grant every permission its jobs ask for:
GitHub won't start a shared workflow when one of its jobs asks for more than
the call grants, even a job that would be skipped. Calling one tags the repo with the
workflow's name (`rust-ci`) and with each input it turns on
(`container: true` tags it `container`), so those tags' requirements apply.

A caller's file holds the call alone: another job in it is reported (move it
to a file of its own, listed in `own_workflows`). Its triggers are checked
against `callers/NAME.yml`, an example caller kept next to the shared
workflow, at the same ref: a caller may leave triggers out, but add none,
like a tag push to a workflow that only expects the default branch.
`$default-branch` there stands for each repo's default branch. A shared
workflow without that file takes any triggers.

This repo's workflows are public, and anyone can call them at `@v1`, which
moves along with compatible changes:

- [`rust-ci.yml`](.github/workflows/rust-ci.yml): fmt, clippy, tests on
  Linux, macOS and Windows, docs, packaging and the MSRV. With
  `container: true` it builds and publishes a multi-arch image to ghcr.io
  instead of testing on macOS and Windows. Pushes to the default branch
  publish it as `latest`, and while the registry lacks Cargo.toml's version,
  as `X.Y.Z` and `X.Y` too: merging a version bump releases the image, once,
  and tags build nothing. With `light: true`, for repos
  whose Actions minutes are billed (private ones), it's a sanity check: one
  Linux job on pull requests (fmt, clippy and, unless `tests: false`, the
  tests), and on pushes only the image, for linux/amd64 unless
  `arm64: true`. Pull requests from `forge deps` skip even that one job.
- [`rust-release.yml`](.github/workflows/rust-release.yml): on a `vX.Y.Z`
  tag, publishes to crates.io through trusted publishing, attaches binaries
  to the GitHub release, and bumps the formula in `{owner}/homebrew-tap` and
  the package in winget-pkgs. forge then checks the latest release reached
  each of them.
- [`pr-title.yml`](.github/workflows/pr-title.yml): PR titles follow
  [Conventional Commits](https://www.conventionalcommits.org/). Private
  repos skip it, since their minutes are billed. A skipped check counts as
  passed, so forge leaves the `require-pr-title` ruleset out on private
  repos, even with `private_protection = true`.
- [`t-ci.yml`](.github/workflows/t-ci.yml): checks repos written in
  [T](https://github.com/lkurcak/t-lang).

[`callers/`](callers) has an example call of each, and each workflow's
inputs say what they do.

### Pins

A shared workflow's inputs can pin a release of another repo, like a T
repo's `t-version` and the sha256 of its archive. `[pins]` says which, by
the file the call is in:

```toml
[pins."t.yml"]
release = "lkurcak/t-lang"   # whose latest release the inputs should name
version = "t-version"        # the input holding its tag
sha256 = "t-sha256"          # the input holding an asset's sha256, if any,
asset = "t-lang-{tag}-x86_64-unknown-linux-gnu.tar.gz"   # of this asset
```

`forge check` reports a repo whose pin is behind that repo's latest release.
`forge bump` opens a PR in each such repo moving the inputs there (the
sha256 from the digest GitHub keeps of the asset), and turns on auto-merge,
so it merges itself once the repo's required checks pass: give those repos
the `require-ci` ruleset and `allow_auto_merge`.

### Updating dependencies

`forge deps` moves each Rust repo's Cargo.lock to the newest versions its
Cargo.toml allows (`cargo update`), runs clippy and the tests on this
machine the way `rust-ci.yml` does, and if they pass opens one PR,
`chore(deps): update dependencies`, listing each crate it moved. Where the
repo allows auto-merge, it's turned on. Newer versions Cargo.toml doesn't
allow are listed too, since taking them means editing it. `--dry-run` stops
before the PR. A repo whose PR from an earlier run is still open is left
alone.

The PR's commit carries a `forge/deps` status saying what passed. Since the
PR changes nothing but Cargo.lock, `rust-ci.yml` with `light: true` doesn't
run its checks again on it (unless `no-std: true`, whose build forge
doesn't run), so on a private repo the PR costs no Actions minutes. Its
`ci / ok` shows as skipped, so a ruleset requiring that check would block
the PR; private repos have none unless `private_protection` is set.

It takes the place of Dependabot's version updates, which open a PR per
crate, each running CI on minutes a private repo pays for. Dependabot's
security updates still cover vulnerabilities between runs.

Repos are cloned into `forge/repos/OWNER/REPO` in the cache directory
(`$XDG_CACHE_HOME`, else `~/.cache`; `%LOCALAPPDATA%` on Windows) and kept
there, build cache and all, so later runs build incrementally. The checks'
output goes to `forge/logs/REPO.log`. A path dependency leading out of the
repo, like `../../libs/x`, is cloned from the owner's repo `x`, to where the
path expects it.

```toml
[deps]
alerts_only = ["maintenance"]
```

Repos with a tag in `alerts_only` are only updated while they have open
Dependabot alerts about Rust crates, unless named on the command line.

### Committed secrets

GitHub's secret scanning and push protection cover public repos, but not a
user's private ones. `forge leaks` scans those with
[gitleaks](https://github.com/gitleaks/gitleaks#installing), which has to be
installed: every private repo the owner has, managed or not (forks and
archived ones aside), or the repos named, and with `--public` the public
ones too. It reports where each possible secret is (the rule it matched,
when it was committed, and a link to the line) but never the secret, and
exits 1 if it found any. It changes nothing: a secret that made it into a
repo's history is in every clone, so revoke it where it was issued.

Every commit a branch, tag or PR reaches is scanned, since a PR's commits
stay on GitHub after its branch is gone. Repos are fetched into bare clones
under `forge/leaks/OWNER` in the cache directory; the first run scans all
their history, later ones only new commits, and what was found is reported
again on every run. `--full` forgets that and scans all history again.

Each finding has an id. Once its secret is revoked, or if it isn't one,
list the id to stop reporting it:

```toml
[leaks]
ignore = ["home-server:2f1c…:docker/.env:generic-api-key:12"]
```

### Deployed sites

A repo deployed as a website can give the address of its page:

```toml
[repos.shop]
url = "https://shop.example.com/"
health = "https://shop.example.com/health"   # default: /healthz on the url's host
```

`forge web` fetches each such page the way a browser or phone meets it,
from this machine and without cookies, and reports:

- a missing `<title>`, `<meta name="theme-color">`, or
  `<meta name="viewport">` with `width=device-width` (without it, phones
  show a zoomed-out desktop page);
- an icon that isn't an image: each `<link rel="icon">`, or else
  `/favicon.ico`. A single-page app's page, answering for any path, doesn't
  count;
- no icon for iOS's home screen: `<link rel="apple-touch-icon">`, or else
  `/apple-touch-icon.png`, a square PNG at least 180 pixels wide. Transparent
  pixels get a warning, since iOS shows them as black;
- no web app manifest (`<link rel="manifest">`), which lets browsers install
  the site as an app, or one lacking `name`, `short_name`, `start_url`,
  `display` (`standalone`, `fullscreen` or `minimal-ui`), `theme_color`,
  `background_color`, or icons at 192x192 and 512x512 and a maskable one,
  each there and the size it says. A Content-Type other than
  `application/manifest+json` (or `application/json`) gets a warning;
- a health check that doesn't answer 2xx to anyone, or answers with a
  redirect (to a login, say) or an HTML page. If it answers JSON with a
  `version`, and maybe a `commit`, forge prints them next to the repo, which
  tells you whether a merge has been deployed; if not, that's a warning.

`health = false` leaves the health check out, for a static site. Only repos
with a `url` are checked, and forge exits 1 if a check fails; warnings
don't. Uptime and certificate expiry are better left to an uptime monitor,
which checks every minute rather than when you run forge.

### Merging on green

The `require-ci` ruleset requires the check `ci / ok`. `rust-ci.yml` ends
with it: it passes only if every other job passed or was skipped, whichever
ones a repo's inputs turn on. `t-ci.yml` is that one job. So call them from
a job named `ci`. With it, and the `allow_auto_merge` setting, `gh pr merge
--auto` merges a PR once all of CI is green. On private repos it needs
`private_protection = true`, and so GitHub Pro.

### Releasing a version bump

`rust-release.yml` releases on a `vX.Y.Z` tag push. A repo that also calls
it on pushes to its default branch (see the file's header) has every
version bump released when it lands: if Cargo.toml's version has no tag
yet, the workflow tags the pushed commit and runs again on the tag. Only a
tag may deploy to the `release` environment, so crates.io and Homebrew are
still only published from tags. On a private repo, that rule needs
`private_protection = true`.

### Also reported

- A `[pins]` input behind the release it pins (`forge bump` opens PRs).
- A latest release whose tag doesn't match `release_tag_prefix`, that lacks
  one of the archives its release workflow builds, or that hasn't reached
  crates.io, the Homebrew formula or winget-pkgs. An open PR delivering it
  shows as pending.
- Open Dependabot alerts, as pending while a bot PR fixing one is under a
  week old. Bot PRs open longer than a week are reported too.
- With `private_ci` set, a private repo's workflow that built the same
  commit more than once in the last week: a branch and a tag pushed
  together each start a run of a workflow both trigger, and both are billed.

## Credentials

forge uses the token gh holds for the manifest's `owner`
(`gh auth token --user OWNER`), so the active gh account doesn't matter.
When gh isn't signed in as the owner, as for an organization, it uses the
active account's token instead, which needs admin access to the owner's
repos. `GH_TOKEN` overrides both.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).
