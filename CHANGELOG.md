# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.4.0]

### Added
- On-disk cache of PR and issue lists and details, tuned in a new `[cache]` section. Lists show from disk at once and
  refresh in the background when past their time (`hot_s` 2 min, `warm_s` 5 min, `cold_s` 15 min); a stale copy is
  marked `cached 3m ago, refreshing` and kept if the refresh fails. Details (overview, checks, comments, diff,
  commits) are stamped with the item's `updatedAt`, so a copy is exact until the PR changes; CI checks (45 s while
  pending) and the overview (5 min) get short times because they change without an update. Startup makes no request
  when the lists and facts are fresh, and tab counts share the list's copy. **This keeps the text of PRs and issues
  on disk, private repos included**: `[cache] details = false` stops details being kept, `[api] cache = false`
  turns the whole cache off.
- The repo list and header facts now stay fresh for an hour (`slow_s`) and labels, templates and organizations for a
  day, up from 10 min / 5 min / 1 h.
- Global home: `g` groups a list by PR author or repo (headings show the count and longest wait, longest-waiting group first); each PR/issue row shows the time since its last update.
- Global home: pull request sections only look at what was updated in a window, `7d` by default (`[ui] window`, `W` cycles 24h / 7d / 30d / all). One `updated:>=` qualifier on the existing search: no extra API calls.

### Changed
- Opening a PR or issue in the same session now refreshes its details when the list shows it as updated (the
  in-memory copy used to be kept until `r`).
- `?` opens a lazygit-style keybindings menu: grouped rows with a key column, a cursor, and `/` to filter.
- `H` hides the selected row's repo from any global section (Review requested, My PRs, Issues, custom), not only from the Repos panel.

## [0.3.0]

### Changed
- Renamed the project and GitHub CLI extension from gh-pulse (`gh pulse`) to gh-tui (`gh tui`).
- New installations use `gh extension install navbytes/gh-tui`; the standalone executable and XDG directories use `gh-tui`.
- Documentation and GitHub Pages move to `navbytes/gh-tui` and `/gh-tui/`.

### Compatibility
- Existing gh-pulse XDG directories continue to be used for reads and writes when the new directories are absent. Files are not moved or deleted automatically.
- `gh extension upgrade pulse` was verified with GitHub CLI 2.100.0 on macOS arm64 against the published v0.3.0 release. It keeps the old `gh pulse` command; install the new extension for `gh tui`.

## [0.2.0]

### Added
- Global home: outside a repo (or with `--start global`) gh-pulse opens a cross-repo home with Review requested, My PRs,
  Issues and Repos panels. `--start auto|repo|global` and `[ui] start` choose where to open; `G` swaps homes.
- Scope picker (`s`): all repos, favorites, one org (any name you type) or one repo; remembered between runs.
- Repos panel (Favorites / Recent) with open, scope, favorite and hide actions.
- Custom `[[sections]]`: your own GitHub searches as panels in the global home, the repo home or both.
- Screenshots of the global home (synthetic data).

### Changed
- API usage: lazy tab counts, a quota chip (`⚡`) with automatic pause when the quota runs low, honoring
  `Retry-After`, a bounded search budget, and an on-disk cache for slow-changing lookups (`[api]` settings).
- Faster test suite.

## [0.1.0]

Initial release: lazygit-style TUI over the `gh` CLI with PR review (diffs, checks, comments), issues, Actions runs,
branches, tags and releases, an inbox, and confirm-before-write actions.
