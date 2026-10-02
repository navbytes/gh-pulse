# Backlog

## Deferred
- **All-repos view with hide list**: show PRs/issues across all the user's repos (extends the existing `G` global view) with a way to hide repos persistently (config file, e.g. `~/.config/gh-pulse/config.yml` `hidden_repos`; toggle from the UI). Keep Files/drill-in code free of single-repo assumptions.
- "Viewed" file ticks are in-memory only; persist or sync to GitHub's viewed state later.
- Light-theme contrast is a first pass; colours not yet eyeballed in a real terminal.
- `cargo audit` not run (not installed).
- syntect highlighting is opt-in (`--features syntax`); decide whether to make default.
- Bidi/RTL override chars can disguise names in confirm popups (cosmetic).
