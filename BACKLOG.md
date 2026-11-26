# Backlog

## Now

## Later

- Notice repos that have gone quiet and ask: still active, or tag it `maintenance`? And the
  reverse: report a `maintenance` repo that gets real work again (#46).
- `scripts/winget-new.sh` handles a repo whose binary isn't named like it. It assumes
  `{repo}.exe`, so such a repo's first winget manifest is fixed by hand.

## Done

- The shared Rust CI tests the MSRV only when the call sets `msrv: true`, meant for libraries;
  applications no longer get a second full Linux test run (#47).
- The shared Rust CI's build cache no longer deletes rustup and cargo from a runner of your own
  (#44).
- Container images get the commit they're built from as the build arg `GIT_COMMIT`, so an app's
  health check can report it and `forge web` shows whether a merge is deployed (#43).
- `forge stale` no longer fails with HTTP 422: it asks for issues and pull requests in turn
  (#42, 0.11.1).
- `[runner]` runs private repos' Linux jobs on self-hosted runners, whose minutes GitHub doesn't
  bill (#40, 0.11.0).
