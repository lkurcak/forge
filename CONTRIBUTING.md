# Contributing

Keep each pull request focused on one logical change. Its title must follow
[Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/):

```text
feat: add stale issue reporting
fix(stale): include issues quiet for exactly N days
ci: test the minimum supported Rust version
feat(cli)!: rename the apply command
```

Use a lowercase type: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`,
`test`, `build`, `ci`, `chore`, or `revert`. The scope is optional; if used,
keep it nonempty and without spaces. Follow the colon with a space and a
short description.

Use `feat` for new behavior and `fix` for a bug fix. Mark breaking changes
with `!` before the colon, and explain the impact and migration in the PR
description. This includes incompatible CLI, configuration, and shared
workflow input changes. The title check validates the format; review still
needs to check that the type and breaking-change marker are accurate.

PRs are squash merged, so the title becomes the commit subject and the
description becomes its body. Intermediate commit messages and branch names
are unrestricted. Check the final squash message when merging.

The PR title check runs on new PRs, title edits, new commits, and reopened
PRs. Tracked repos share the check through the `conventional-commits` tag.
The Homebrew tap uses the check-only `pr-title` tag to preserve its bottle
publisher, which pushes commits directly to main.
Automated PRs follow the same convention; Dependabot uses a `chore` prefix,
and formula updates use titles such as `chore(tend): update to 1.2.3`.
Existing PRs may need retitling when the check is introduced. Releases and
version bumps remain manual for now.
