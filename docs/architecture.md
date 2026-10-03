# Architecture

gh-pulse is a single binary: a synchronous ratatui event loop that talks to GitHub only by running `gh` (and
`git`) as subprocesses on background threads. No async runtime.

## Module map

| File | Role |
|---|---|
| `src/main.rs` | Arg parsing, repo resolution, terminal setup/teardown (panic hook, mouse capture guard), the event loop |
| `src/app.rs` | `App` state, key and mouse handling, panels (`PK`), drill-in context, caches, message handling |
| `src/ui.rs` | Pure rendering of `&App` into a ratatui `Frame`; also records hit regions for the mouse |
| `src/gh.rs` | Everything that runs `gh`: list/detail fetchers, JSON models and parsers, command log, `shell()` quoting |
| `src/act.rs` | Mutations as data: `Action` = label + optional prompt + `build(text) -> argv` |
| `src/diff.rs` | Unified-diff parser, split-row builder, word-partner pairing, `wrap_ranges`, layout mode |
| `src/syn.rs` | syntect highlighter (cargo feature `syntax`, on by default); a no-op stub with `--no-default-features` |
| `src/config.rs` | `config.toml` model (incl. `[panels]` layout and tab sets), validation with line numbers, atomic save, the named-action `Keymap` |
| `src/browse.rs` | Repo browser: GraphQL page parser, filter/sort/favorite/hide logic, browser key handling |
| `src/md.rs` | Markdown to styled, wrapped lines (pulldown-cmark): headings, code, quotes, lists, tables, folding of `<details>` and long comments |
| `src/sanitize.rs` | Makes bidi/zero-width/control characters visible (`<U+202E>`) in everything GitHub-sourced and in confirm-popup commands |
| `src/state.rs` | Persistent viewed marks (`viewed.json`), keyed by repo + PR + head sha |
| `src/form.rs` | Multi-field popups (new issue, new PR, run workflow): editing, validation, and building the exact argv (+ optional stdin) |
| `src/dispatch.rs` | Reads `workflow_dispatch.inputs` from a workflow's YAML (`serde_yaml_ng`); refuses files over 256 KB or with more than 20 aliases (the form then falls back to `key=value`), and runs in the fetch thread |
| `src/theme.rs` | Palettes, icon sets, color depth fallback, locale/truecolor detection |

## Data flow

```
key/mouse event -> App (state change) -> spawn thread -> gh subprocess
                                                  \-> mpsc Msg -> App::poll() -> state -> ui::draw(&App)
```

- The loop in `main.rs` polls input every 100 ms. Each iteration: `app.poll()` drains the channel, `draw`, then
  either handles one event or, when idle, calls `app.ensure()`, which starts fetches for whatever the screen needs
  and is not cached yet. Fetching on idle is a debounce while you hold `j`.
- `draw(&App)` never mutates observable state, except the `Cell`/`RefCell` fields that carry layout results back
  (scroll position, hit rectangles, wrapped-row offsets).
- Lists come from `gh::list`; per-item detail (overview, checks, comments, diff, logs) from `gh::detail`, cached
  in `App.cache` keyed by `(item key, tab)`.
- The left column is built from `[panels]` (`config::PanelsCfg::layout`). A panel has configured list tabs; the Repo
  panel's tabs are different sources (`gh::list` panels 4 Branches, 7 Tags, 5 Releases). Titles show every tab's
  count: the showing tab counts its own rows, the others are fetched one per idle tick (`App::counts`, dropped on
  reload by a generation).
- The header badge and the inbox (`N`) share one `gh api notifications` fetch (`Msg::Inbox`); marking read is an
  ordinary `Action` through the confirm popup.
- Files, Checks and Comments are *derived panels*: their rows are rebuilt from that cache for the selected (or
  drilled-into) PR by `sync_derived`, so they never fetch on their own.

## Paged data

Comments are fetched one GraphQL page at a time. `gh::detail` returns the first page with the cursors left over
(`Pending`); the fetch thread then keeps paging with `gh::more_pages` and sends each page as `Msg::More`, which is
merged into the cached `CommentsData` (`apply`) so the UI never blocks. At most `AUTO_PAGES` (5) extra pages load by
themselves, at least 300 ms apart; the rest load on demand, one page at a time: when the view is within 20 rows of the
loaded end, or on `m`. Every message carries the cursors still to fetch, so loading can pause and resume. A rate limit
(GraphQL "rate limit", HTTP 403, secondary limit, or a later page suddenly "not found") shows "GitHub rate limit - retry
in Ns" and is never retried automatically. The tab shows "loading more... (n/total)", the drill-in list ends with an
"N more" row, and a failed page says why (`m` retries).

## Forms

A form never runs anything. `Form::build` validates and returns the argv (and, for bodies over 60 KB, a stdin
payload for `--body-file -`); the app puts that into the normal confirm popup. Labels, templates, branches and the
workflow YAML are fetched in the background after the form opens (`gh::form_data`), so validation against the repo's
labels starts once they arrive. Typed text reaches `gh` untouched; the popups show it neutralized like everything else.
After a confirmed create, the new item's number (the URL's last segment) is selected once the lists reload.

## Hardening

Variation selectors (U+FE0E/FE0F) are dropped on the way in too: terminals and width tables disagree on whether
`\u26a0\ufe0f` is 1 or 2 cells, which desynchronizes the screen diff and leaves ghost text from the previous frame.

Everything fetched from GitHub passes `sanitize::clean` once on the way in (`gh::list`/`detail` wrappers, the diff
parser, the repo browser), and `gh::shell` cleans the command shown in confirm popups and the command log. Bidi
overrides, zero-width characters and control characters (terminal escape injection) are shown as `<U+XXXX>`.

## Staleness: generation counters

Every async result carries the generation it was requested in; mismatches are dropped.

- Each panel has a `seq` bumped on every reload; `Msg::List` carries it (plus the list tab).
- `dgen` is bumped whenever the detail cache is cleared or the repo scope changes; `Msg::Detail` and `Msg::Log`
  carry it (a log must also still belong to the selected item).
- `repos_seq` does the same for the repo browser's paged load (each page is a message; stale loads are dropped).

## Safety and confirm design

All writes are `Action`s built in `act.rs` as complete argv vectors. The UI flow is
`menu -> (input popup) -> confirm popup -> gh::run`. The confirm popup renders `gh::shell(argv)`, the exact
command, and accepts only `y`. Arguments are passed to the process individually (no shell), branch and tag names
that start with `-` are rejected or placed after `--`, API paths are percent-encoded, and local actions carry the
repo the working directory must be a clone of, checked at run time. `gh.rs` appends every command to an in-memory
log shown with `L`.

## Rendering notes

- Left column: focused panel expands; on short terminals the rest collapse to one borderless line.
- Diff: logical rows have display heights (soft-wrap); the scroll offset is in display rows, and
  `App.row_starts` maps clicks back to logical rows. Highlighting (syntect) is incremental and cached per file.
- Scrolling a selection taller than the pane (an expanded comment) keeps the view inside that selection; snapping to its end and back on alternate frames once made the pane flicker and tear (regression test `tall_expanded_comment_is_stable_and_pageable`).
- Colors all come from `Theme`; `Reset` is used for body text so light and dark terminals both work.

## Testing

Parsers have fixture tests (`tests/*.json`, synthetic data). Layout is tested with ratatui's `TestBackend` using
a seeded `App` (`App::with(.., load = false)` and the test-only `seed`). Live smoke tests are `#[ignore]` and read
their target from environment variables (`GH_PULSE_REPO`, `GH_PULSE_HUGE_PR`).
