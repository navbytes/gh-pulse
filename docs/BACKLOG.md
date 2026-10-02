# Backlog

## Deferred
- ~~All-repos view with hide list~~ shipped as the repo browser (`B`); remaining: apply the hide list to notifications and the Actions/Branches panels if wanted.
- Sync viewed marks with GitHub's viewed state: a GraphQL mutation, so it must go through the confirm popup (local persistence shipped).
- Reaction names list at most 25 users per reaction ("+N more" beyond).
- Light-theme contrast is a first pass; colours not yet eyeballed in a real terminal.
- `cargo audit`: 0 vulnerabilities; 1 accepted warning, `bincode` 1.3.3 unmaintained (RUSTSEC-2025-0141), pulled in by `syntect`. Re-check when syntect updates.
- Syntax highlighting: after jumping far into a huge diff, scrolling back up through the skipped lines shows them plain.
