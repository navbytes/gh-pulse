# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.3.0] - Unreleased

### Changed
- Renamed the project and GitHub CLI extension from gh-pulse (`gh pulse`) to gh-tui (`gh tui`).
- New installations use `gh extension install navbytes/gh-tui`; the standalone executable and XDG directories use `gh-tui`.
- Documentation and GitHub Pages move to `navbytes/gh-tui` and `/gh-tui/`.

### Compatibility
- Existing gh-pulse XDG directories continue to be used for reads and writes when the new directories are absent. Files are not moved or deleted automatically.
- Whether `gh extension upgrade pulse` follows the repository redirect must be verified with the published release.

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
