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
| `src/syn.rs` | Optional syntect highlighter (feature `syntax`); a no-op stub otherwise |
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
- Files, Checks and Comments are *derived panels*: their rows are rebuilt from that cache for the selected (or
  drilled-into) PR by `sync_derived`, so they never fetch on their own.

## Staleness: generation counters

Every async result carries the generation it was requested in; mismatches are dropped.

- Each panel has a `seq` bumped on every reload; `Msg::List` carries it (plus the list tab).
- `dgen` is bumped whenever the detail cache is cleared or the repo scope changes; `Msg::Detail` and `Msg::Log`
  carry it (a log must also still belong to the selected item).
- `repos_seq` does the same for the repo switcher.

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
- Colors all come from `Theme`; `Reset` is used for body text so light and dark terminals both work.

## Testing

Parsers have fixture tests (`tests/*.json`, synthetic data). Layout is tested with ratatui's `TestBackend` using
a seeded `App` (`App::with(.., load = false)` and the test-only `seed`). Live smoke tests are `#[ignore]` and read
their target from environment variables (`GH_PULSE_REPO`, `GH_PULSE_HUGE_PR`).
