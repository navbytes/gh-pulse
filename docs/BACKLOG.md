# Backlog

## Deferred
- ~~All-repos view with hide list~~ shipped as the repo browser (`B`); the inbox honours the hide list; Actions/Repo panels do not need it.
- `sync_viewed` mirrors only files GitHub reports on the first 3000; it never removes local marks when GitHub unviews.
- Workflow dispatch: dispatch support is detected when the form opens (not in the workflow list); inputs are read from the default branch's file, not the chosen ref.
- Create forms: YAML issue forms (`.yml` templates) are not offered; only `.md` templates prefill a body.
- Reaction names list at most 25 users per reaction ("+N more" beyond).
- Light-theme contrast is a first pass; colours not yet eyeballed in a real terminal.
- `cargo audit`: 0 vulnerabilities; 1 accepted warning, `bincode` 1.3.3 unmaintained (RUSTSEC-2025-0141), pulled in by `syntect`. Re-check when syntect updates.
- Syntax highlighting: after jumping far into a huge diff, scrolling back up through the skipped lines shows them plain.
- Inbox: `Enter` looks for a PR/issue in the *open* lists; a merged/closed one is opened on GitHub in the browser instead (no in-app view of closed items). Only unread notifications are listed (the API default); 100 at most.
- Tag detail makes two API calls per selected tag (commit, release); the Tags tab lists 300 at most (`300+`).
- README's "Diff view" block and screenshots still show the old 8-panel layout until they are regenerated.
- Tab counts are fetched in full (one list call per hidden tab); a cheaper count endpoint per source could replace that.
- The cache identity comes from gh's `hosts.yml` (`user:`); with a token from the environment and no `gh auth login`, the repo list and header facts are simply not cached.
- API throttling: in-flight `gh` processes for work the user moved past are left to finish (not killed). Counts for search-backed tabs have no cheap source (they show `?` until opened).
- `r` / `R` fetch fresh data but cannot overwrite `gh --cache`'s stored copy: it expires on its own TTL (a plain request would otherwise repeat the old answer within that window).
- Startup fetches only the PR and Issues lists in the batch; the Actions and Repo panels (REST) load on first focus and show a placeholder until then.
- Reaction names make one extra call per commented card the first time `e` is pressed.

