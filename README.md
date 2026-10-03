# gh-tui

**lazygit for GitHub: a keyboard-driven TUI over the `gh` CLI.**

[Website](https://navbytes.github.io/gh-tui/) · [Install](#installation) · [Documentation](docs/configuration.md)

Numbered panels on the left, a detail pane on the right. Review pull requests (diffs, checks, review threads),
triage issues, watch workflow runs, browse branches and releases, and act on all of it without leaving the
terminal. It shells out to [`gh`](https://cli.github.com), so there is no token handling: whatever `gh` can
see, gh-tui can see.

![gh-tui main layout](docs/img/1-main.png)

<sub>Real session against a public repository (login shown as `you`).</sub>

## Screenshots

Captured from the real binary (`scripts/screenshots.py`) on public repositories: gh-tui itself (its own merged PRs, CI runs
and v0.1.0 release) and [cli/cli](https://github.com/cli/cli) for the comment thread. The **global home** shots further down
use **synthetic data**: the binary runs against a fake `gh` (`scripts/fake-gh`) serving made-up users, orgs, repos, PRs and
notifications, so nothing of a real account appears.

![Five panels with a merged PR's overview](docs/img/1-main.png)
*The five panels: [1] PRs (Mine / Review / All / Merged), [2] Files, [3] Issues, [4] Actions, [5] Repo; the right pane shows the selected PR's Overview.*

![Split diff with syntax highlighting](docs/img/2-diff-split.png)
*Zoomed Diff tab (`f`) of a Rust change: auto layout picks side-by-side at width, with syntax highlighting and intra-line highlights.*

![Unified diff of a Markdown file with wrap](docs/img/3-diff-prose.png)
*Unified layout with soft wrap (`t` / `w`) keeps long prose lines readable.*

![PR drill-in](docs/img/4-files.png)
*Enter on a PR drills into Files / Commits / Checks / Comments; markers show files with review comments.*

![Comments tab](docs/img/5-comments.png)
*Comment cards with markdown, role badges and review summaries; bot boilerplate is collapsed.*

![Actions panel](docs/img/6-actions.png)
*[4] Actions: workflow runs with their jobs and steps.*

![Checks tab](docs/img/6b-checks.png)
*Checks in a PR drill-in: CI jobs with their duration; a failed check shows its failed-step log.*

![Repo panel, Releases tab](docs/img/7-releases.png)
*[5] Repo: Branches / Tags / Releases tabs; a release shows its notes and assets.*

![Action menu](docs/img/8-menu.png)
![Confirm popup](docs/img/8b-confirm.png)
*Every action is confirmed first and shows the exact `gh` command it will run (only `y` runs it; this one was cancelled).*

![Help](docs/img/9-help.png)
*`?` lists every key.*

![Compact layout at 80x24](docs/img/10-compact.png)
*At 80x24 the panels shrink to short titles and the key hints truncate.*

### Global home (synthetic data)

![Global home](docs/img/11-global-home.png)
*Started outside a repo: **Review requested**, **My PRs**, **Issues** and **Repos** on the left, the selected item's Overview on the right. Rows read `owner/repo#N`.*

![Cross-repo diff](docs/img/12-global-diff.png)
*Diff tab zoomed with `f`, split layout: diffs, checks and comments work on a row from any repo without switching.*

![Scope picker](docs/img/13-scope-picker.png)
*`s` opens the scope picker: all repos, favorites, one org or one repo.*

![Repos panel](docs/img/14-repos-panel.png)
*[4] Repos: Favorites and Recent tabs; `Enter` opens a repo, `s` scopes the home to it, `f` favorites, `H` hides.*

![Repo browser](docs/img/15-repo-browser.png)
*`B` browses every repo you can access, with search, sort, type filter, favorites (★) and hide; `.` shows hidden repos (⊘).*

![Inbox](docs/img/16-inbox.png)
*`N` opens the inbox: unread notifications across repos.*

![Global home at 80x24](docs/img/17-global-compact.png)
*The global home at 80x24.*

Regenerate with `GH_TUI_SHOT_BLOCKLIST=word,word python3 scripts/screenshots.py` (needs `pyte`, `rsvg-convert` and `cargo build --release`; public repos only; the blocklist aborts a shot if a private string shows). The global-home shots are `python3 scripts/screenshots.py --fake [shot ...]`: offline, synthetic data from [scripts/fake-gh](scripts/fake-gh/README.md), in a throwaway `HOME` and config.

## Features

- **Five repo-scoped panels**: Pull requests (Mine / Review / All / Merged), Files, Issues (Assigned / Mine / All),
  Actions (Runs / Workflows) and Repo (Branches / Tags / Releases). Every title shows the count of every tab
  (`[5] Branches 32 · Tags 14 · Releases 81`, `…` until known). Filter any list with `/`. Choose, order and trim the
  panels and their tabs under `[panels]` ([configuration](docs/configuration.md)).
- **Inbox**: `✉ N` in the header counts your unread notifications; `N` opens them across all repos. `Enter` shows the
  PR or issue in the app (switching repo), `m` marks it read (after the usual exact-command confirm), `o` opens it in the browser.
- **Code review without the browser**: unified, split or auto diff with soft-wrap, word-level highlighting,
  syntax highlighting (on by default), per-file stats, review-thread badges and viewed marks.
- **Comments as cards**: GitHub-flavored markdown rendered in the terminal (headings, emphasis, inline and fenced
  code with syntax highlighting, quotes, lists, task lists, tables, links), author role badges, relative
  times, edited markers, reaction counts (`e` for names), nested review-thread replies with resolved/outdated
  badges. Bot boilerplate (`<details>`, HTML comments, huge blobs) is collapsed; `Enter` expands. Long threads page in
  gently: the first page at once, up to 500 more automatically, the rest as you scroll or press `m` (with a clear
  "GitHub rate limit, retry in Ns" message if GitHub throttles).
- **PR drill-in**: `Enter` on a PR turns the left column into that PR's Files, Commits, Checks and Comments. The
  right pane shows the diff of the selected file or commit, a check's log, or a full thread.
- **Actions with a seatbelt**: approve, request changes, comment, merge, close, label, assign, reply to or
  resolve threads, comment on a diff line, re-run or cancel runs, create issues/releases, delete branches. Every
  one shows the exact command first.
- **Global home**: outside a repo (or with `--start global`) gh-tui opens a cross-repo home: **Review requested**,
  **My PRs** (Open / Merged / Closed), **Issues** (Assigned / Mine / Mentioned), an optional **Involved**, and a
  **Repos** panel (Favorites / Recent). Rows read `owner/repo#N`; the detail pane, diffs, checks, comments and every
  action work across repos without switching. `s` narrows the home to all repos, your favorites, one org or one repo;
  `S` opens the selected item's repo (`G` goes back); hidden repos are left out. The scope picker also takes any org
  or user name you type. A full-screen **repo browser** (`B`)
  covers every repo you can access, with search, sort, favorites and hide.
- **Custom sections**: add your own GitHub searches as panels with `[[sections]]` in the config (gh-dash style), for the
  global home, the repo home or both, e.g. `filter = "is:open review-requested:@me org:acme"`. They load when focused,
  cost one search per refresh, and the scope picker applies unless the filter names its own `repo:` / `org:` / `user:`.
- **Mouse and keyboard**: click panels, rows and tabs; wheel scrolls. `?` shows every key.
- **Themes**: dark and light palettes, truecolor with a 256-color fallback, ASCII and Nerd Font icon sets.
- **Create from the terminal**: new issue (labels validated, `.md` templates, body in a multi-line field), new PR
  (base picker, draft, reviewers, `--fill`, PR template, warns if the branch isn't pushed), and run a workflow with
  a form generated from its `workflow_dispatch` inputs (read from the default branch; branches and tags offered as the ref). Each ends in the exact-command confirm popup.
- **Viewed files**: remembered locally per PR head; optionally mirrored to GitHub (`sync_viewed`).
- **Command log**: `L` shows every `gh`/`git` command that was run.

## Requirements

- [`gh`](https://cli.github.com) installed and authenticated (`gh auth login`).
- Rust 1.89 or newer to build (edition 2024 with let-chains; 1.89 is the oldest toolchain it is tested on).
- A terminal of at least 50x12 (80x24 or larger recommended). UTF-8 and truecolor are optional.

## Installation

### As a GitHub CLI extension

```sh
gh extension install navbytes/gh-tui
gh tui                      # same flags as the standalone binary
gh extension upgrade tui
```

This downloads a precompiled binary from the latest GitHub release.

### Moving from gh-pulse

The new extension can coexist with an existing `gh pulse` installation. Install it with
`gh extension install navbytes/gh-tui`, then check `gh tui --version` before removing the old extension with
`gh extension remove pulse` if you no longer need it. With GitHub CLI 2.100.0 on macOS arm64, `gh extension upgrade pulse` was verified
against v0.3.0 through the repository redirect; it updates the old extension but its command remains `gh pulse`.
Install `gh-tui` separately for `gh tui`. If a new XDG directory does not exist, gh-tui keeps
using its existing gh-pulse config, state, or cache directory for reads and writes; it does not move files. A
previously installed standalone `gh-pulse` binary is not removed automatically.

Prebuilt platforms: macOS (arm64, amd64) and Linux (arm64, amd64; glibc 2.35 or newer, built on Ubuntu 22.04).
Windows is unsupported and untested.

Each release carries a `SHA256SUMS` file and build provenance attestations. To verify a downloaded binary: `sha256sum -c SHA256SUMS --ignore-missing` (Linux) or `shasum -a 256 -c SHA256SUMS --ignore-missing` (macOS), or
`gh attestation verify gh-tui-linux-amd64 --repo navbytes/gh-tui`.

### With cargo

```sh
# from GitHub
cargo install --git https://github.com/navbytes/gh-tui

# from a clone
git clone https://github.com/navbytes/gh-tui && cd gh-tui
cargo install --path .

# smaller plain build without syntax highlighting
cargo install --path . --no-default-features
```

Syntax highlighting (the `syntax` cargo feature, built on the pure-Rust `syntect`) is on by default. It adds about
2.8 MB: the release binary is about 5.4 MB with it and about 2.6 MB with `--no-default-features`, and a clean
build takes a few seconds longer. Diffs fall back to plain +/- coloring in the small build. Highlighting is
incremental and cached per file; jumping to the end of a huge diff skips the lines in between (they stay plain).
(To opt out when installing from GitHub: `cargo install --git https://github.com/navbytes/gh-tui --no-default-features`.)

## Usage

Run it inside a clone of a GitHub repository, point it at one, or run it anywhere for the global home:

```sh
gh-tui                       # in a clone: that repo. Outside a repo: the global home (--start auto)
gh-tui -R cli/cli            # any repo you can access
gh-tui --start global        # the global home even inside a clone (G reaches the repo)
gh-tui --theme light --ascii
gh-tui                       # then press B to browse every repo you can access
```

Outside a repo there is no error any more: gh-tui opens the global home (`--start repo` / `[ui] start = "repo"`
brings the old "not in a GitHub repo" message back).

| Flag | Meaning |
|---|---|
| `-R, --repo owner/repo` | Repo to show. Checked up front; exits with a clear message if it is not found. |
| `--start auto\|repo\|global` | Where to open (config: `[ui] start`, default `auto`): `auto` = the current directory's repo or `-R`, otherwise the global home; `repo` = always a repo (an error outside one); `global` = always the global home. |
| `--theme dark\|light` | Color palette (default `dark`). |
| `--ascii` | ASCII icons and borders. Also automatic when the locale is not UTF-8. |
| `--nerd` | Nerd Font icons. |
| `--clear-cache` | Delete gh-tui's on-disk cache (`~/.cache/gh-tui`) and exit. |

gh-tui needs an interactive terminal; piping stdin/stdout prints a message and exits.

## Configuration

Optional `~/.config/gh-tui/config.toml` (`$XDG_CONFIG_HOME` is honored): theme and icons, start mode, panels and
tabs, custom sections, API tuning, favorite and hidden repos (written by the repo browser), and key remapping. Flags override the file; a missing file means
defaults; an invalid file stops startup with `file:line: message`. See [docs/configuration.md](docs/configuration.md).
The file never contains credentials; authentication stays entirely with `gh`.
If `gh-tui` has no config directory and `gh-pulse` does, gh-tui keeps using the old directory for reads and writes.
Nothing is moved automatically. The same directory fallback applies to state and cache.

Custom sections are your own GitHub searches as panels (gh-dash style); each is one search, run when you focus it.
Three examples (`where` defaults to `"global"`; full reference in [docs/configuration.md](docs/configuration.md#custom-sections)):

```toml
[[sections]]
title  = "Needs my review"
kind   = "prs"
filter = "is:open review-requested:@me org:acme draft:false"

[[sections]]
title  = "My stale PRs"
kind   = "prs"
filter = "is:open author:@me updated:<2026-01-01"

[[sections]]
title  = "Bugs in acme/widgets"
kind   = "issues"
filter = "is:open label:bug repo:acme/widgets"
limit  = 50                   # optional, 1..100 (default 30)
```

## Local state

`$XDG_STATE_HOME/gh-tui` (default `~/.local/state/gh-tui`) holds `viewed.json` (files marked viewed), `scope.json`
(the global home's last scope) and `recent.json` (the last 20 repos you entered, per host). Files are `0600`, written
atomically, and a corrupt one is ignored. The directory is `0700` and must be yours (a foreign-owned or symlinked one
is refused with a notice); files are read without following links. `scope.json` and `recent.json` remember their host.
None of it contains credentials.

Slow-changing lookups are cached under `$XDG_CACHE_HOME/gh-tui` (default `~/.cache/gh-tui`; a `0700` directory
you own, files `0600`; a relative `XDG_CACHE_HOME` is ignored, and if the directory is not yours or is a symlink the
cache switches itself off with a notice). What is stored:

- **Repo list** (10 min) and **repo header facts** (5 min): repo names, descriptions, topics, counts; keyed by host and
  login, so another account or host never sees them. Cached rows are cleaned again when read.
- **Raw file text from repos: issue/PR templates and workflow YAML (1 h), labels (1 h) and tags (10 min).** These come
  from the repo you opened, private ones included, so their contents sit on your disk for up to an hour. They are
  gh's own cache entries (`gh api --cache`, keyed on host, token and request), kept inside gh-tui's directory.

Never cached: tokens, comments, notifications, PR details, diffs, anything from a write. `gh-tui --clear-cache`
deletes the whole directory; `[api] cache = false` turns caching off; `r` and `R` skip it.

Files you mark viewed (`v`) reset when the PR's head commit changes; nothing is written to GitHub unless you set `sync_viewed`.

## Concepts

- **Two homes.** The *repo home* is about one repository; the *global home* is about you, across repos. `G` swaps
  between them and each remembers its cursor, tab and filter; `S` (or Enter on a row of the Repos panel) opens an
  item's repo, with `G` leading back to where you came from. The global home's scope (`s`: all / favorites / an org /
  one repo) is remembered between runs in the state directory.
- **Repo scope.** In the repo home everything is about one repository: the current directory's, or `-R`. The header shows the
  repo, your local branch (only when the directory is a clone of it), your user, and the repo's stars, visibility, default branch and open-PR count. `B` (or `Ctrl-r`) opens the repo browser to switch.
- **Panels.** Numbered like lazygit; the focused one expands, the rest collapse. `[` `]` change the detail
  tab, `{` `}` change the panel's own list (e.g. Mine vs Merged); pressing the focused panel's number again also steps to
  its next list tab, and the tab labels in the title are clickable.
- **Files follows the PR.** Panel 2 lists the files of the selected PR; moving through it changes the
  file shown in the diff.
- **Drill-in.** `Enter` on a PR replaces the left column with Files / Commits / Checks / Comments of that PR; `Esc` goes
  back and your cursor is where you left it.
- **Search budget.** (See also *API usage* below.) Each global section is one GitHub search (the search API allows 30 a minute), made when you
  focus the section, change the scope or press `r` (favorites scope: one search per four repos, at most 16 repos).
  Nothing searches in the background and unopened search tabs show `?`. A section never costs more than 8 searches,
  `r` while one is running is ignored, and a refresh the rest of the minute can't pay for is refused with the reset time.
- **Safety.** See below.

## Keybindings

The full reference is in [docs/keybindings.md](docs/keybindings.md). The essentials:

| Context | Key | Action |
|---|---|---|
| Global | `?` / `q` | Help / quit |
| Global | `1`-`7`, `Tab`, `Shift-Tab` | Focus panel (as many as are shown) |
| Global | `x` | Action menu for the selected item |
| Global | `o` `y` `c` | Open in browser / copy URL / check out PR |
| Global | `r` `R` `L` | Refresh selected item / reload everything / command log |
| Global | `B` `N` | Repo browser / inbox |
| Global home | `G` | Swap between the repo home and the global home |
| Global home | `s` `S` | Scope picker / open the row's repo (`G` returns) |
| Repos panel | `Enter` `s` `f` `H` | Open repo / scope the home to it / favorite / hide |
| Lists | `j` `k` `Ctrl-d` `Ctrl-u` `g` | Move (`End` for the bottom; `G` swaps homes) |
| Lists | `/` `Esc` | Filter / clear |
| Lists | `{` `}` | Panel list tab |
| Lists | `Enter` | Drill into a PR, else focus detail |
| Detail | `[` `]` | Detail tab |
| Detail | `h` `Esc` | Back to the list |
| Diff | `n` `p` | Next / previous file |
| Diff | `t` `w` `v` `f` | Layout / wrap / viewed / zoom |
| Actions | `a` `C` `m` | Approve / comment / merge |
| Popups | `y` / `n` | Confirm / cancel |
| Popups | `Ctrl-S` | Submit text input |

## Safety model

gh-tui never writes without asking.

1. Every mutation opens a **confirm popup that shows the exact command line** (`gh pr merge 12 -R owner/repo --squash`).
2. Only **`y`** runs it. `Enter` does not, so a double-tapped menu key can't fire it; `n` or `Esc` cancels.
3. Text input (comments, labels, titles) uses a multi-line popup, `Ctrl-S` to continue, and then the same
   confirm step.
4. The **command log** (`L`) lists every `gh`/`git` command that ran, reads included, and errors.
5. Local actions (check out a PR, `git switch`) only run when the current directory is a clone of that repo.
6. User text is passed as separate arguments, never through a shell.

## Theming and icons

`--theme light|dark` picks the palette. Truecolor is used when `COLORTERM` is `truecolor`/`24bit`, otherwise the
nearest of 256 colors. Icons are Unicode by default, `--ascii` for plain ASCII (borders too), `--nerd` for Nerd
Font glyphs.

## Diff view

`t` cycles auto / unified / split (auto goes side-by-side when each half has 60+ columns). Long lines soft-wrap
at word boundaries with a `↪` marker; `w` switches to clipping. `f` zooms the right pane to full width.

## API usage and rate limits

- One GraphQL request fills the first screen; other panels load when first focused, at most 4 `gh` processes run at
  once, and what you ask for goes before anything automatic.
- **Tab counts** are `lazy` by default (`[api] counts = "lazy" | "eager" | "off"`); search-backed tabs show `?` until opened.
- **Quota chip.** `⚡ 412/5000` in the header means a quota is under 20% (`low_quota_percent`; `rate_header = false` hides it).
- **Pause.** Under 10% (`pause_percent`) everything automatic stops (counts, extra comment pages, the inbox badge)
  until the window resets (`resets HH:MM`); your own actions still work. `Retry-After` and secondary-limit messages are honored.
- **Cache.** Slow-changing lookups (repo list, header facts, labels, templates, tags) are cached on disk;
  `r` / `R` skip it, `[api] cache = false` disables it, `--clear-cache` wipes it.
- The only polling is the unread badge (2 min) and a free `gh api rate_limit` check (5 min).

Details and every `[api]` key: [docs/configuration.md](docs/configuration.md#api-etiquette).

## FAQ and troubleshooting

- **"not in a GitHub repo" / "repo not found or no access".** The first only appears with `--start repo`; by default
  gh-tui opens the global home outside a clone. For the second, pass a repo you can see with `-R owner/repo` and
  check `gh auth status`.
- **Panels sit on "loading...".** gh-tui waits on `gh`; slow networks or a large account mean several
  seconds. `L` shows what is running.
- **"Terminal too small".** The minimum is 50x12. On short terminals unfocused panels collapse to one line.
- **Colors look washed out or odd.** Your terminal probably lacks truecolor: set `COLORTERM=truecolor` if it
  supports it, or try `--theme light` on light backgrounds.
- **Boxes and icons are garbled.** Use `--ascii`, or install a Nerd Font and use `--nerd`.
- **A huge PR shows its diff anyway.** Over 300 files `gh pr diff` refuses; gh-tui falls back to the files API.

## Roadmap

- Polish for the Commits tab and PR creation.
- GitHub Enterprise Server support is untested.
- Windows is unsupported.

See [docs/BACKLOG.md](docs/BACKLOG.md) for known limitations, and [CHANGELOG.md](CHANGELOG.md) for what changed in each release.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and [docs/architecture.md](docs/architecture.md).

## License

MIT, see [LICENSE](LICENSE).
