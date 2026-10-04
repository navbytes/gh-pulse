# Configuration

gh-tui reads an optional `config.toml` from `$XDG_CONFIG_HOME/gh-tui/` (default `~/.config/gh-tui/`).
If the `gh-tui` directory is absent but the legacy `gh-pulse` directory exists, gh-tui reads and writes there instead.
It does not migrate files. The same directory fallback applies to state and cache.

- A missing file means defaults.
- An invalid file (syntax error, unknown key, bad value, clashing key binding) stops startup with a message like
  `~/.config/gh-tui/config.toml:7: unknown field `theem``, exit code 2. Nothing is ignored silently.
- Command-line flags override the file.
- The app writes only the `[repos]` favorites and hidden lists (when you press `f` or `H`), in place: your comments,
  ordering and every other line stay exactly as you wrote them. Saves are atomic (temp file + rename) and refused
  if the file currently fails to load, so hand edits in progress are never overwritten.
- It never contains credentials. Authentication belongs to `gh`.

## Editing the config

You edit the file in your own editor; gh-tui helps you get there and picks the result up.

| | |
|---|---|
| `E` (action `edit_config`) | In gh-tui: opens the config in `$VISUAL`, else `$EDITOR`, else `vi`; on exit the file is applied. |
| `gh-tui --edit-config` | The same without the UI; afterwards it says `ok` or names the line that is wrong (exit code 1). |
| `gh-tui --print-config` | Prints the commented default config and exits (`gh-tui --print-config > config.toml` to start one by hand). |
| `gh-tui --config-path` | Prints where the file lives. |

The first time, the file is created from a commented template: **every setting is listed with its default, commented
out**, each with a one-line note. A line that starts with `#` directly followed by a name (`#window = "7d"`) is a
setting: remove the `#` (and the one in front of its `[table]` header) to change it. Lines that start with `# ` are
notes. Because defaults stay commented, a new release can change a default without leaving a stale copy in your file.
The file is never created behind your back: only `E`, `--edit-config` or saving a favorite writes it.

When the editor exits the file is loaded again. A mistake (a typo'd key, a bad value, a clashing key binding) shows
`config not applied (E to fix it): file:line: message` and the old settings stay in force. What applies **now**: theme
and icons (`theme`, `ascii`, `nerd`), `[keys]`, `sync_viewed`, `[cache]`, `[api]` except `max_concurrent`,
`[ui] window`, and `[repos]`. What needs a **restart** (the status line names it): `[panels]` and `[[sections]]`, `[ui] start`,
`[api] max_concurrent`. Command-line flags still override the file. Editors that return at once (`code`, `subl`, `zed`)
need their wait flag, e.g. `EDITOR="code --wait"`, or the file is read before you have saved.

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

[ui]
start = "auto"            # auto | repo | global (flag: --start)
window = "7d"             # PRs in the global home updated within: 24h | 7d | 30d | all (W cycles it)

[cache]                   # seconds a cached copy counts as fresh (5..604800); older ones still show at once
details = true            # keep PR/issue details (comments, diffs, commits, checks) on disk between runs
hot_s = 120               # Review requested, My PRs (open), a repo's PR list
warm_s = 300              # Involved, issues, a repo's issue list
cold_s = 900              # merged / closed tabs
checks_s = 45             # CI checks while one is pending
overview_s = 300          # PR overview (review state, mergeability), and finished checks
detail_s = 86400          # comments, diffs, commits: valid while the PR is unchanged, never longer than this
slow_s = 3600             # repo list, repo facts, tags, workflow files (labels, templates, orgs: 24x this)

[panels]
show = ["prs", "files", "issues", "actions", "repo"]   # order = numbering
global = ["review", "mine", "assigned", "repos"]       # the global home
hide_empty = false

[panels.repo]
tabs = ["branches", "releases"]    # drop Tags
default_tab = "branches"

[[sections]]                       # your own search panels, see "Custom sections"
title  = "Needs my review (acme)"
kind   = "prs"                     # prs | issues
filter = "is:open review-requested:@me org:acme draft:false"
```

## API etiquette

gh-tui is a polite client. Every `gh` process runs through one small worker pool: at most `max_concurrent`
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
  requests; your own searches are counted locally between the five-minute checks, and `L` shows `search N/30 left`) the header shows `⚡ remaining/limit` (`rate_header = false` hides it). Under `pause_percent` everything
  automatic stops until the window resets (tab counts, automatic comment pages, the inbox badge poll; the quota
  check itself still runs) and the status line says `paused background refresh (rate limit low, resets HH:MM)`.
  Your own actions keep working; at exhaustion the usual rate-limit message includes the reset time. A secondary
  limit or `Retry-After` message backs the automatic work off for the time GitHub names.
- **Tab counts** (`[panels]` titles; a list or count that failed shows `✗`, never `0`, and a failed count is not retried
  until the next reload). In the repo home every PR, issue and repo count (Mine / Review / All / Merged, Assigned /
  Mine / All, Branches / Tags / Releases) comes from **one GraphQL request**, about a point of quota: exact totals
  (`412`, not `100+`), including the tabs GitHub's REST search would have rationed. It is kept in the on-disk cache
  for `warm_s`, so a restart inside that time makes no request. `lazy` (default) and `eager` behave the same for
  these; `off` never fetches (`?`). The Actions tabs (Runs / Workflows), and every tab if that request fails, use
  the older per-tab path: `lazy` counts the focused panel's other tabs one at a time after a second of idling,
  `eager` every panel's, never in the global home and never for tabs backed by GitHub's search (those show `?`
  until you open them). The tab you are on always counts its own list.
- **Comments.** The first request fetches 50 items per connection and 20 replies per thread, reactions as counts
  only; `e` fetches the names of one comment's reactors. The rest pages in as background work (5 pages, then
  scroll or `m`), and not at all while the quota is low.
- **Settle delay.** The Overview of the selected item loads once the cursor has rested for a whole idle tick (about 200 ms), so only the row you stop on is fetched; Comments, Diff, Commits and Checks
  after `settle_ms` (default 250), so scrolling through a list does not fire a request per row.
- **Refresh.** `r` refreshes the selected item and its list tab, `R` everything. The only polling in the app is the
  inbox badge (every 2 minutes, background priority) and the free quota check (every 5 minutes).

### Cache

gh-tui keeps what it fetched under `$XDG_CACHE_HOME/gh-tui` (default `~/.cache/gh-tui`; only absolute paths
are honored) and shows it at once on the next focus or run. The directory must be a real directory you own: an
existing one with looser permissions is tightened to `0700`, one owned by someone else or reached through a symlink
is refused and the cache turns itself off (the status line says why). Files are `0600`, written through unique temp
files created exclusively, and read only if they are regular files you own. Everything is stored per host and login,
and read back through the same cleaning as fresh data.

**What is kept, and for how long** (`[cache]`, see the example above):

| Data | Fresh for | Why |
|---|---|---|
| Review requested, My PRs (open), a repo's PR list | `hot_s` (2 min) | what you act on |
| Involved, issues, a repo's issue list | `warm_s` (5 min) | changes less often |
| Merged / closed tabs | `cold_s` (15 min) | rarely change |
| PR comments, diff, commits | while the PR is unchanged, at most `detail_s` (a day) | the list's `updatedAt` is compared, so a new comment or push makes the copy stale at once |
| CI checks | `checks_s` (45 s) while one is pending, then `overview_s` | results do not bump `updatedAt` |
| PR overview (review state, mergeability) | `overview_s` (5 min) | mergeability changes when the base moves, with no update to the PR |
| Repo list, repo header facts, tags, workflow files | `slow_s` (1 h) | |
| Labels, issue/PR templates, organizations | 24 x `slow_s` (a day) | |

A copy that is past its time is still shown at once, marked `cached 3m ago, refreshing` in the list's border, and
replaced when the new data arrives; if the refresh fails the copy stays and the status line says so. Lists are
requested again only when they are stale (or on `r` / `R`), so re-focusing a panel or restarting costs no API calls
while they are fresh, and tab counts share the list's copy instead of fetching it twice. A list that was only partly
fetched (one of several searches failed) is never kept. Files stay on disk for at most a week, at most 400 details
(150 MB together) and 200 lists (20 MB) are kept, the oldest going first, and a detail over 4 MB is not stored.

**This puts the text of PRs and issues on disk, private repos included**: titles, bodies, comments, diffs and
templates, in `0600` files in a `0700` directory under your home, readable by you with the same `gh` commands.
`[cache] details = false` stops details being kept (lists and slow facts still are); `[api] cache = false` turns the
whole cache off; `gh-tui --clear-cache` deletes it.

| What | TTL | Where | Keyed by |
|---|---|---|---|
| Your repo list (repo browser) | `slow_s` (shown at once, refreshed behind it when older) | `repos-<host>-<login>.json` | host + login, also stored inside and checked |
| A repo's header facts | `slow_s` | `meta-<host>-<repo>.json` | host + repo; the stored login must match yours |
| PR / issue details | see above | `d-<host>-<login>-<repo>-<kind><number>-<tab>.json` | host + login, also stored inside and checked |
| PR / issue lists | see above | `l-<host>-<login>-<scope, window, ...>.json` | host + login + everything the rows depend on |
| Labels, issue/PR templates, workflow YAML, tags, organizations | see above | `gh/...` (gh's own entries, via `gh api --cache`) | URL + token + request (verified: another token misses the cache) |

Templates and workflow files are raw text from the repo you opened, private repos included. If something cannot be
tied to your login, it is not cached at all. The login comes from `gh`'s own `hosts.yml`; when a token from the environment (`GH_TOKEN`, `GITHUB_TOKEN`, or `GH_ENTERPRISE_TOKEN` for another host) is set it overrides what `gh auth login` stored and `hosts.yml` cannot say whose requests these are, so gh-tui asks GitHub once at startup (`gh api user`: one REST call, and the first load waits for the answer) and caches under that login. If that call fails nothing is cached for the run. Never
cached: tokens, notifications, logs, anything from a write. `r` / `R` fetch fresh data (`r` the selected item and its
list, `R` everything), skipping the disk copy.

## Start mode and the global home

`[ui] start` (or `--start`, which wins) decides where gh-tui opens:

| Mode | In a clone / with `-R` | Elsewhere |
|---|---|---|
| `auto` (default) | that repo | the global home |
| `repo` | that repo | error: "not in a GitHub repo" |
| `global` | the global home (`G` reaches the repo) | the global home |

`[ui] window` is how far back the global home's pull request sections (Review requested, My PRs, Involved) look:
`24h`, `7d` (default), `30d` or `all`. It is one `updated:>=` qualifier on the search you already make, so it costs
no extra calls and shrinks the pages; issues and your own `[[sections]]` keep their full history. `W` cycles it for the
session (the sections search again). Note that *updated* means last activity: a review request older than the window
that nobody has touched since is hidden, so use `all` if you want to see everything that is waiting.

Inside a git work tree `auto` asks GitHub which repo it is (one `gh repo view`); outside one it makes no call at all.
The global home's panels come from `[panels] global` (any of `review`, `mine`, `assigned`, `involved`, `repos`,
`files`; default `review, mine, assigned, repos`). Sections are GitHub searches, each fetched when focused (one `gh
search` call, or one per four favorites with the favorites scope, at most 4): `review-requested:@me is:open
archived:false`; `author:@me` with `is:open` / `is:merged` / `is:closed is:unmerged`; `assignee:@me`, `author:@me` or
`mentions:@me` with `is:open` for issues; `involves:@me` (PRs and issues, two calls) for Involved. Rows from hidden
repos (`[repos] hidden`, the repo browser's `h`) are dropped client side; when that empties a full page one bigger
request tops it up. The scope (`all`, `favorites`, `org:x`, `repo:a/b`; `org:` also takes any org or user you type, member or not) is stored in `$XDG_STATE_HOME/gh-tui/scope.json`.
Search budget: one section refresh costs one search per scope chunk (two for Involved), so at most 8, plus - only
for a single-chunk scope with at least 10 searches (and a third of the minute) left - one bigger re-ask of just the
searches that came back full after hidden repos emptied a page. A refresh that the rest of the minute can't pay for
is refused with "search quota low, resets HH:MM" (the list on screen is kept); `r` while a section is already
searching is ignored; if some favorites chunks fail the others' rows still show, with a note. `-R` takes whatever
`gh repo view` takes (`OWNER/REPO`, `HOST/OWNER/REPO`, a URL) and is resolved as before; only names that flow into
search queries (scope, favorites, recent) are held to a strict `owner/name` form (no `.`/`..` parts, no leading `-`).
`[repos] favorites` and `hidden` are plain `owner/name` lists with no host: they apply to whichever host you run
against (`GH_HOST`), while `scope.json` and `recent.json` remember their host and are ignored on another one.
Hiding a repo (repo browser `H`, or `H` in the Repos panel) drops its rows from the loaded sections at once; unhiding
searches the sections again (the focused one now, the others when focused, subject to the search quota). One-line
fields from GitHub (titles, names, labels) show line breaks and tabs as a single space; repo names that are not plain
`owner/name` are shown neutralized and never passed to `gh` (their actions are disabled).
The `repos` panel (Favorites from `[repos] favorites`, Recent from `recent.json`) can also be listed in `[panels] show`
for the repo home; it makes no API calls (details come from the cached repo list).

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
Custom searches from `[[sections]]` are named `section:<title>` here (see Custom sections). `hide_empty = true` collapses a panel with nothing in any of its tabs to a single line (the focused panel never
collapses). A bad name is a startup error naming the file, the line and the valid choices. With `files` not shown, the
Diff tab still works: `n` / `p` pick the file.

## Custom sections

`[[sections]]` adds your own GitHub searches as panels, like gh-dash's sections. Each entry is one panel, appended
after the built-in ones (order them with `section:<title>` in `[panels] show` / `global`, see below).

```toml
[[sections]]
title  = "Needs my review (acme)"   # panel name, 1..40 characters, unique
kind   = "prs"                      # "prs" | "issues"
filter = "is:open review-requested:@me org:acme draft:false"
limit  = 50                         # optional, 1..100, default 30
where  = "global"                   # "global" (default) | "repo" | "both"
```

| Key | Meaning |
|---|---|
| `title` | Panel title, at most 40 columns wide as displayed (control characters are shown neutralized as `<U+XXXX>`), not blank. Titles must be unique after that; at most 12 sections |
| `kind` | `prs` runs `gh search prs`, `issues` runs `gh search issues` |
| `filter` | GitHub search syntax, at most 256 characters, one line (see the rules below) |
| `limit` | Rows to fetch (default 30, at most 100). A title count ending in `+` means the limit was reached |
| `where` | `global`: a panel of the global home. `repo`: a panel of the repo home. `both`: both homes |

**How the search is built.** The filter is split into terms (whitespace separates them; double quotes group words, so
`label:"good first issue"` is one term) and each term is passed to `gh search` as its own argument after `--`, never
through a shell and never as a flag. Added to your terms:

- `archived:false`, unless the filter has an `archived:` or `is:archived` qualifier (`archived:true` and `is:archived` switch the default off).
- Global home: the active scope (`s`: `org:x`, `repo:a/b`, or a group of favorites) **unless the filter already has a
  `repo:`, `org:` or `user:` qualifier**. Then the scope does not apply, and the panel title says `(own filter)`.
- Repo home (`where = "repo"` or `"both"`): `repo:<the current repo>`, unless the filter has its own `repo:`.
- If the filter uses `OR`, it is wrapped in parentheses first, so the added qualifiers apply to the whole thing. Parentheses outside quotes are sent as separate terms; they must be balanced (an error with `file:line`), parentheses inside quotes are plain text.
- `@me` is you, as in the built-in sections.

**Rules (checked at startup, errors name `file:line`).** No control characters or newlines; terms may not start with
`--`; the characters allowed are letters, digits and `- _ . : / @ * < > = ! , ~ ( ) ' # + ?` (so no `$`, backtick, `;`,
`|`, `&` or `\`). GitHub allows at most 5 `AND` / `OR` / `NOT` operators in a query; `-qualifier` terms count too. More
than 5 is a **warning** at startup (the status line), not an error, because GitHub decides in the end. With the
favorites scope each group of favorites uses operators as well (`OR` between repos); a filter that already uses
operators gets smaller groups (down to one repo per search, so fewer favorites are searched; the rest are reported as
"not shown").

**Behavior.** A section is lazy: its title count is `?` until you focus it, then it makes one search (favorites scope:
one per group of up to four repos, at most four; the same guards and quota refusal as the built-in sections). `r`
searches it again, `s` changes the scope for sections without their own scope qualifier. Hidden repos
(`[repos] hidden`) are dropped client side, with the same one-time top-up request as the built-in sections. Rows, the
detail pane (Overview / Checks / Comments / Diff / Commits), drill-in, actions and `-R` behave like the other global
panels. In the global home a `kind = "prs"` section also feeds the Files panel.

Referring to a section in `[panels]`: `show = ["prs", "section:Bugs"]` places it in the repo home,
`global = ["section:Needs my review (acme)", "review"]` in the global home. An unknown title, or a section whose
`where` does not include that home, is an error. Sections not listed are appended after the built-in panels.
Number keys `1`-`7` focus the first seven panels; use `Tab` for the rest.

Examples, in the spirit of gh-dash:

```toml
[[sections]]
title  = "My open PRs"
kind   = "prs"
filter = "is:open author:@me"

[[sections]]
title  = "Needs my review"
kind   = "prs"
filter = "is:open review-requested:@me -author:app/dependabot draft:false"

[[sections]]
title  = "Bugs I own"
kind   = "issues"
filter = "is:open assignee:@me label:bug"

[[sections]]
title  = "Good first issues"
kind   = "issues"
filter = 'is:open label:"good first issue" org:cli'   # own scope: the `s` scope does not apply
limit  = 20

[[sections]]
title  = "Stale PRs here"
kind   = "prs"
filter = "is:open updated:<2026-01-01"
where  = "repo"                                       # only in the repo home, for the current repo

[[sections]]
title  = "Team PRs"
kind   = "prs"
filter = "is:open (author:alice OR author:bob)"
where  = "both"
```

## Custom actions

`[[actions]]` entries are commands you define and run on the selected row. They appear at the end of the `x` menu on
the rows they fit, and on their own key if you give one (listed in the `?` menu). The idea is lazygit's custom
commands: your command, your terminal.

```toml
# Talk it through with Claude, then come back
[[actions]]
name = "Claude review"
on   = "pr"
run  = ["claude", "Review {url}. Focus on correctness and security."]

# A review per roost tab, while you keep triaging
[[actions]]
name = "Review in a roost tab"
on   = "pr"
mode = "detach"
run  = ["roost", "spawn", "claude", "--tab", "--title", "pr-{number}", "--input", "Review {url}"]

# The same in tmux, from a key
[[actions]]
name = "Review in a tmux window"
on   = "pr"
mode = "detach"
key  = "ctrl-o"
run  = ["tmux", "new-window", "-n", "pr-{number}", "claude", "Review {url}"]

# A pipeline: row data arrives as variables
[[actions]]
name  = "Headless review to a file"
on    = "pr"
mode  = "detach"
shell = 'claude -p "Review $GHTUI_URL" > ~/reviews/$GHTUI_OWNER-$GHTUI_NAME-$GHTUI_NUMBER.md'
```

| Key | Meaning |
|---|---|
| `name` | The menu label (unique, up to 40 characters). |
| `on` | The rows it applies to: `pr`, `issue`, `repo`, `run`, `workflow`, `branch`, `release`, `tag` or `any`; one word or a list. While you are inside a PR (files, checks, comments, diff) the row is that PR. |
| `run` | The command as a list. `{url}`, `{repo}` (`owner/name`), `{owner}`, `{name}`, `{number}`, `{kind}`, `{author}` and `{ref}` (the exact branch, tag or release name) are filled in per element. `{{` and `}}` give a literal brace. A typo is an error when the config loads. |
| `shell` | Instead of `run`: a `sh -c` script for pipes and `&&`. Nothing is filled into it; row data comes as `$GHTUI_URL`, `$GHTUI_REPO`, `$GHTUI_OWNER`, `$GHTUI_NAME`, `$GHTUI_NUMBER`, `$GHTUI_KIND`, `$GHTUI_AUTHOR`, `$GHTUI_REF`, `$GHTUI_TITLE` and `$GHTUI_STATE`. `run` commands get the same variables. |
| `mode` | `foreground` (default) or `detach`, below. |
| `key` | A key that runs it from the list: `"ctrl-o"`, `"O"`. Built-in keys, navigation keys and other actions' keys are refused. |
| `pause` | Foreground only. `true`: wait for Enter before returning; `false`: never. Unset: only when the command failed, so its error stays readable. |
| `confirm` | `true` shows the full command and asks first. |

**Foreground** (the default) is the terminal handoff `E` uses for your editor: gh-tui leaves the alternate screen, the
command gets the real terminal (an interactive `claude`, `lazygit`, `vim`, `less`), and gh-tui comes back when it exits,
re-reading the row it ran on, since the command may have changed it. Ctrl-C reaches the command and not gh-tui. A
non-zero exit is shown in the status line.

**Detach** starts the command and returns at once; gh-tui never gives up the terminal. Its output is discarded, it
runs in its own process group, and it keeps running if you quit. `started` is shown, and only a failure
(`name: exited with status 2: <last line of stderr>`) is reported afterwards. This is for commands that open
somewhere else: `tmux new-window` / `split-window`, `roost spawn`, a browser, a notification.

Trust: the commands are yours, run with your permissions, like `$EDITOR`. The data they run on is not: titles, branch
names and authors are written by other people. So `{...}` becomes a whole argument and is never parsed by a shell, and
the `shell` form gets `$GHTUI_*` variables instead of pasted text. Keep to those and a branch called `x; rm -rf ~` is
only a strange name. Actions come only from your own `config.toml`, never from a repository. If your command hands a PR's
text to an AI tool, treat that text as untrusted input to the tool: pass `{url}` and let it fetch what it needs, and
don't give a reviewer on someone else's PR write or shell access.

## Syncing viewed marks with GitHub

`v` always remembers viewed files locally (`$XDG_STATE_HOME/gh-tui/viewed.json`). With `sync_viewed = true` it also
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
| `edit_config` | `E` | Open the config in your editor and apply it on exit |
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
| `scope` | `s` | Global home scope picker (Repos row: scope to that repo) |
| `switch_repo_context` | `S` | Open the selected item's repo as the app's repo |

The `?` help and the bottom bar show the keys you configured. Startup fails (with the config path and the offending entry) on an unknown action, an unparsable key, a key bound to two actions, an
empty list, or a key that is reserved for navigation: `j k h l g n p t w v e s S T H .`, `[ ] { }`, digits `1`-`7`,
`Enter`, `Esc`, `Tab`, arrows, `Home`, `End`, and `ctrl-c`/`ctrl-d`/`ctrl-u`. Inside popups and the repo browser the
keys are fixed.
