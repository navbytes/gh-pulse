# Backlog

## Deferred
- ~~All-repos view with hide list~~ shipped as the repo browser (`B`); remaining: apply the hide list to notifications and the Actions/Branches panels if wanted.
- `sync_viewed` mirrors only files GitHub reports on the first 3000; it never removes local marks when GitHub unviews.
- Workflow dispatch: dispatch support is detected when the form opens (not in the workflow list); inputs are read from the default branch's file, not the chosen ref.
- Create forms: YAML issue forms (`.yml` templates) are not offered; only `.md` templates prefill a body.
- Reaction names list at most 25 users per reaction ("+N more" beyond).
- Light-theme contrast is a first pass; colours not yet eyeballed in a real terminal.
- `cargo audit`: 0 vulnerabilities; 1 accepted warning, `bincode` 1.3.3 unmaintained (RUSTSEC-2025-0141), pulled in by `syntect`. Re-check when syntect updates.
- Syntax highlighting: after jumping far into a huge diff, scrolling back up through the skipped lines shows them plain.
