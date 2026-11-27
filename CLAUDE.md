# Notes for agents

forge's backlog lives in the owner's private manifest repo: its items name
private repos.

## Decisions

- forge suits other users too: full CI stays the default, and saving
  Actions minutes is an opt-in setting in the manifest (`private_ci`),
  which forge then enforces (2026-11-09).
- forge checks rules that mean something, not strange little ones
  (2026-10-11).
- No backward-compatibility shims: break freely, reach a good state, and
  bring the repos along (2026-10-11).
- The repos' workflows are the same everywhere; drift between repos is what
  forge is there to stop (2026-11-12).
