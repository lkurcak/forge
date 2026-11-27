# forge: notes for agents

## Backlog

What's next is tracked in `BACKLOG.md` at the repo root. It's shown as it stands on the
default branch, so it has to be true there.

- Sections: a `# Backlog` title, then `## Now` (one ordered list, in order), `## Later`,
  `## Done`. Later may be grouped under `###` headings; Now may not.
- An item is one list item of a line or two, and something that can be finished. If it needs
  more, it links to a details file as a plain Markdown link
  (`1. [Jump and fast fall](backlog/jump.md)`), deleted when the item is done. A standing
  practice (read the feedback first, audit the data) isn't an item: it goes in the repo's
  `CLAUDE.md`.
- An item that can't start before a date starts with it: "From 17 Dec: …". An item only the
  owner can do (a decision, a key, a setting on their account) starts with "Owner:".
- Keep it current: move what you finished to Done in the same change, and put new ideas
  under Later instead of leaving them in the chat. Don't invent items.
- Done is newest first, a line each, ending with the pull request's number where there is
  one (`(#63)`). It keeps about the last ten; git has the rest.
- On "go", read `BACKLOG.md` and continue with the first item under Now.
