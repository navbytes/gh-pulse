# Configuration

gh-pulse reads an optional `config.toml` from `$XDG_CONFIG_HOME/gh-pulse/` (default `~/.config/gh-pulse/`).

- A missing file means defaults.
- An invalid file (syntax error, unknown key, bad value, clashing key binding) stops startup with a message like
  `~/.config/gh-pulse/config.toml:7: unknown field `theem``, exit code 2. Nothing is ignored silently.
- Command-line flags override the file.
- The file is written by the repo browser (favorites, hidden). Saves are atomic (temp file + rename) and refused
  if the file currently fails to parse, so hand edits in progress are never overwritten. Comments are not
  preserved when the app rewrites the file.
- It never contains credentials. Authentication belongs to `gh`.

## Example

```toml
theme = "dark"      # "dark" or "light"
ascii = false       # ASCII icons and borders
nerd = false        # Nerd Font icons
sync_viewed = false # also mark files viewed on GitHub with `v` (see below)

[repos]
favorites = ["octocat/hello-world"]
hidden = ["octocat/old-experiments"]

[keys]
quit = ["q", "ctrl-q"]
actions = "space"

[api]
max_concurrent = 4        # gh processes at once (1..8)
counts = "lazy"           # lazy | eager | off: how tab counts are fetched
low_quota_percent = 20    # show the quota chip below this
pause_percent = 10        # stop background refreshing below this
rate_header = true        # the chip at all
settle_ms = 250           # idle time before a Comments/Diff/Commits/Checks fetch
timeout_s = 60            # a gh read that takes longer is killed (pagination 2x, writes 5x)
cache = true              # on-disk cache of slow-changing lookups

[panels]
show = ["prs", "files", "issues", "actions", "repo"]   # order = numbering
hide_empty = false

[panels.repo]
tabs = ["branches", "releases"]    # drop Tags
default_tab = "branches"
```

## API etiquette

gh-pulse is a polite client. Every `gh` process runs through one small worker pool: at most `max_concurrent`
(default 4) at once, what you asked for (the selected item, actions, forms) before anything automatic, and queued
work for something you have already moved past is dropped before it starts (an already-running `gh` is left to finish).

- **Reliability.** Every `gh` child has a deadline (`timeout_s`, default 60 s; paginated reads double it, writes get five
  times) and is killed with a `gh timed out after Ns` error; children still running when you quit are killed. A job
  that panics or is lost reports an error instead of leaving a panel on "loading...". Writes (a confirmed command, the
  `sync_viewed` mark) run on a thread of their own, never behind reads, never dropped for lack of room. The repo
  browser loads one page per job so a long account does not hold a worker.
- **Startup** is one GraphQL request: the active tab of the PR and Issues lists, who you are, the repo's header facts
  and the quota. The Actions and Repo panels (REST) load when you first focus them. Failing that request (not a rate
  limit) falls back to the old one-command-per-panel loading.
- **Quota awareness.** Our own GraphQL queries ask for `rateLimit { cost remaining resetAt limit }` (free) and `gh api
  rate_limit` is read at startup and every five minutes (also free). The `L` log shows each query's cost and the
  remaining quota. With less than `low_quota_percent` of the GraphQL or core quota left (or fewer than 5 search
  requests) the header shows `⚡ remaining/limit` (`rate_header = false` hides it). Under `pause_percent` everything
  automatic stops until the window resets (tab counts, automatic comment pages, the inbox badge poll; the quota
  check itself still runs) and the status line says `paused background refresh (rate limit low, resets HH:MM)`.
  Your own actions keep working; at exhaustion the usual rate-limit message includes the reset time. A secondary
  limit or `Retry-After` message backs the automatic work off for the time GitHub names.
- **Tab counts** (`[panels]` titles; a list or count that failed shows `✗`, never `0`, and a failed count is not retried until the next reload). `lazy` (default): the focused panel's other tabs, one at a time, after a second
  of idling; never in the global view; never for tabs backed by GitHub's search (Mine / Review requested PRs,
  Assigned / Mine issues), which show `?` until you open them. `eager` fetches every panel's tabs (still skipping
  search-backed ones); `off` never fetches (`?`). The tab you are on always counts its own list.
- **Comments.** The first request fetches 50 items per connection and 20 replies per thread, reactions as counts
  only; `e` fetches the names of one comment's reactors. The rest pages in as background work (5 pages, then
  scroll or `m`), and not at all while the quota is low.
- **Settle delay.** The Overview of the selected item loads once the cursor has rested for a whole idle tick (about 200 ms), so only the row you stop on is fetched; Comments, Diff, Commits and Checks
  after `settle_ms` (default 250), so scrolling through a list does not fire a request per row.
- **Refresh.** `r` refreshes the selected item and its list tab, `R` everything. The only polling in the app is the
  inbox badge (every 2 minutes, background priority) and the free quota check (every 5 minutes).

### Cache

Slow-changing lookups are cached under `$XDG_CACHE_HOME/gh-pulse` (default `~/.cache/gh-pulse`; only absolute paths
are honored). The directory must be a real directory you own: an existing one with looser permissions is tightened to
`0700`, one owned by someone else or reached through a symlink is refused and the cache turns itself off (the status
line says why). Files are `0600`, written through unique temp files created exclusively, and read only if they are
regular files you own.

| What | TTL | Where | Keyed by |
|---|---|---|---|
| Your repo list (repo browser) | 10 min (shown at once, refreshed behind it when older) | `repos-<host>-<login>.json` | host + login, also stored inside and checked |
| A repo's header facts | 5 min | `meta-<host>-<repo>.json` | host + repo; the stored login must match yours |
| Labels, issue/PR templates, workflow YAML | 1 hour | `gh/...` (gh's own entries, via `gh api --cache`) | URL + token + request (verified: another token misses the cache) |
| Tags | 10 min | `gh/...` | same |

**Templates and workflow files are raw text from the repo you opened, private repos included, and stay on disk for up to
an hour.** If the repo list or header facts cannot be tied to your login (a token from the environment rather than
`gh auth login`), they are not cached at all. Text read back from the cache is neutralized again like everything else.
Never cached: tokens, comments, notifications, PR details, diffs, anything from a write. `r` / `R` fetch fresh data
(the cached copy still expires on its own schedule). `gh-pulse --clear-cache` deletes the directory (only after the same
ownership checks); `[api] cache = false` turns caching off.

## Panels

`[panels] show` lists the left-column panels in display order; the order is also the numbering (`1`, `2`, ...).
Any subset works, at least one is required, repeats are an error. Names:

| Name | Panel |
|---|---|
| `prs` | Pull requests |
| `files` | Files of the selected PR (follows `prs`; it is dropped when `prs` is not shown) |
| `issues` | Issues |
| `actions` | Workflow runs and workflows |
| `repo` | Branches / Tags / Releases |
| `notifications` | The repo's notifications, as a panel (not shown by default; the `✉` badge and the `N` inbox cover all repos) |
| `status` | Repo summary (not shown by default; the header carries the same facts) |

`[panels.prs]`, `[panels.issues]`, `[panels.actions]` and `[panels.repo]` take `tabs` (which list tabs to offer, in
this order) and `default_tab` (which one opens first, and must be among `tabs`). Tab names: prs `mine` `review` `all`
`merged`; issues `assigned` `mine` `all`; actions `runs` `workflows`; repo `branches` `tags` `releases`.
`hide_empty = true` collapses a panel with nothing in any of its tabs to a single line (the focused panel never
collapses). A bad name is a startup error naming the file, the line and the valid choices. With `files` not shown, the
Diff tab still works: `n` / `p` pick the file.

## Syncing viewed marks with GitHub

`v` always remembers viewed files locally (`$XDG_STATE_HOME/gh-pulse/viewed.json`). With `sync_viewed = true` it also
tells GitHub, through the `markFileAsViewed` / `unmarkFileAsViewed` GraphQL mutations (PR id and path travel as
GraphQL variables), and when you open a PR's files, the files GitHub already has marked viewed are added to your local
marks. This is a write to GitHub, so it is **off by default and setting it to true is the consent**: there is no
confirm popup per keypress. The calls show up in the command log (`L`). If one fails, the local mark is kept and the
status line says so. Local marks are never removed because of GitHub's state.

## Key remapping

`[keys]` maps an action name to one key or a list of keys. Remapping an action replaces its default keys.
Keys are a single character (case matters: `C` is shift-c), `ctrl-<letter>`, or one of `tab`, `backtab`,
`enter`, `esc`, `space`, `backspace`, `up`, `down`, `left`, `right`, `home`, `end`.

| Action | Default | What it does |
|---|---|---|
| `quit` | `q` | Quit |
| `help` | `?` | Help popup |
| `refresh` | `r` | Refresh the selected item and its list tab |
| `refresh_all` | `R` | Reload everything (skips the cache) |
| `actions` | `x` | Action menu |
| `approve` | `a` | Approve (PRs) |
| `comment` | `C` | Comment |
| `merge` | `m` | Merge |
| `open` | `o` | Open in browser |
| `copy_url` | `y` | Copy URL |
| `checkout` | `c` | Check out the PR |
| `command_log` | `L` | Toggle the command log |
| `filter` | `/` | Filter the focused list |
| `zoom` | `f` | Zoom the right pane |
| `global` | `G` | Toggle the global view |
| `repo_browser` | `B`, `ctrl-r` | Open the repo browser |
| `inbox` | `N` | Open the notifications inbox |

The `?` help and the bottom bar show the keys you configured. Startup fails (with the config path and the offending entry) on an unknown action, an unparsable key, a key bound to two actions, an
empty list, or a key that is reserved for navigation: `j k h l g n p t w v e s S T H .`, `[ ] { }`, digits `1`-`7`,
`Enter`, `Esc`, `Tab`, arrows, `Home`, `End`, and `ctrl-c`/`ctrl-d`/`ctrl-u`. Inside popups and the repo browser the
keys are fixed.
