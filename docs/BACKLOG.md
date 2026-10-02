# Backlog

## Deferred
- ~~All-repos view with hide list~~ shipped as the repo browser (`B`); remaining: apply the hide list to notifications and the Actions/Branches panels if wanted.
- Comments: only the first 100 comments / reviews / threads (and 50 replies per thread) are fetched; no pagination yet. Bare URLs are not auto-linked; reaction names show at most 10 users.
- "Viewed" file ticks are in-memory only; persist or sync to GitHub's viewed state later.
- Light-theme contrast is a first pass; colours not yet eyeballed in a real terminal.
- `cargo audit` not run (not installed).
- syntect highlighting is opt-in (`--features syntax`); decide whether to make default.
- Bidi/RTL override chars can disguise names in confirm popups (cosmetic).
