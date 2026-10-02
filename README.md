# gh-pulse

**lazygit for GitHub: a keyboard-driven TUI over the `gh` CLI.**

Numbered panels on the left, a detail pane on the right. Review pull requests (diffs, checks, review threads),
triage issues, watch workflow runs, browse branches and releases, and act on all of it without leaving the
terminal. It shells out to [`gh`](https://cli.github.com), so there is no token handling: whatever `gh` can
see, gh-pulse can see.

```text
 cli/cli  branch:   user: you
╭ [1] Status ───────────────────────────────╮╭ Overview │ Checks │ Comments │ Diff ──────────────────────────────────╮
│cli/cli                                    ││`gh issue artifact` stack 8/10: Add `edit`                             │
╰───────────────────────────────────────────╯│cli/cli #14571  [open]                                                 │
╭ [2] Pull requests · All open (72) ────────╮│───────────────────────────────────────────────────────────────────────│
│#14577 [draft] Bump go-runewidth to v0.0.30││author    octocat                                                      │
│#14572 `gh issue artifact` stack 9/10: Docu││branch    octocat/artifact-create <- octocat/artifact-edit             │
│#14571 `gh issue artifact` stack 8/10: Add ││mergeable mergeable                                                    │
│#14570 `gh issue artifact` stack 7/10: Add ││decision  REVIEW_REQUIRED                                              │
│#14569 `gh issue artifact` stack 6/10: Add ││  hubot (requested)                                                     │
╰───────────────────────────────────────────╯│                                                                       │
╭ [3] Files (9) ────────────────────────────╮│Part of the pull request stack tracked in #14563.                      │
│M artifact.go  pkg/cmd/is…/artifact/  +2 -0││                                                                       │
╰───────────────────────────────────────────╯│<!--                                                                   │
╭ [4] Issues · Assigned (0) ────────────────╮│Thank you for contributing to GitHub CLI!                              │
│nothing here                               ││                                                                       │
╰───────────────────────────────────────────╯│If you are proposing a fix for a security issue, STOP and follow       │
╭ [5] Actions · Runs (100+) ────────────────╮│.github/SECURITY.md instead.                                           │
│✓ Triage Scheduled Tasks  Triage Scheduled ││                                                                       │
╰───────────────────────────────────────────╯│Keep the entire pull request self-contained, reviewer-facing, and      │
╭ [6] Branches (100+) ──────────────────────╮│diegetic: describe the                                                 │
│⎇ 1119-support-for-owner-based-queries     ││change, rationale, and evidence within the context of the repository   │
╰───────────────────────────────────────────╯│and pull request. Do not                                               │
╭ [7] Releases (100+) ──────────────────────╮│narrate how the pull request was produced or refer to private          │
│★ GitHub CLI 2.102.0                       ││conversations, prior agent work,                                       │
╰───────────────────────────────────────────╯│or other behind-the-scenes context. Omit those details unless they     │
╭ [8] Notifications (0) ────────────────────╮│materially affect review;                                              │
│nothing here                               ││if they do, state the relevant fact and its significance directly.     │
╰───────────────────────────────────────────╯╰───────────────────────────────────────────────────────────────────────╯
j/k move  Enter drill in  [ ] detail tab  { } list tab  / filter  x actions  ? help  q quit
```

<sub>Text capture of a real session against a public repository (usernames replaced).</sub>

## Features

- **Repo-scoped panels**: Status, Pull requests (Mine / Review requested / All open / Merged), Files, Issues,
  Actions (Runs / Workflows), Branches, Releases, Notifications. Filter any list with `/`.
- **Code review without the browser**: unified, split or auto diff with soft-wrap, word-level highlighting,
  optional syntax highlighting, per-file stats, review-thread badges and viewed marks.
- **Comments as cards**: GitHub-flavored markdown rendered in the terminal (headings, emphasis, inline and fenced
  code with optional syntax highlighting, quotes, lists, task lists, tables, links), author role badges, relative
  times, edited markers, reaction counts (`e` for names), nested review-thread replies with resolved/outdated
  badges. Bot boilerplate (`<details>`, HTML comments, huge blobs) is collapsed; `Enter` expands.
- **PR drill-in**: `Enter` on a PR turns the left column into that PR's Files, Checks and Comments, with check
  logs and full threads in the right pane.
- **Actions with a seatbelt**: approve, request changes, comment, merge, close, label, assign, reply to or
  resolve threads, comment on a diff line, re-run or cancel runs, create issues/releases, delete branches. Every
  one shows the exact command first.
- **Global view**: your PRs and issues across all repositories (`G`), plus a full-screen **repo browser** (`B`) over every repo you can access, with search, sort, favorites and hide.
- **Mouse and keyboard**: click panels, rows and tabs; wheel scrolls. `?` shows every key.
- **Themes**: dark and light palettes, truecolor with a 256-color fallback, ASCII and Nerd Font icon sets.
- **Command log**: `L` shows every `gh`/`git` command that was run.

## Requirements

- [`gh`](https://cli.github.com) installed and authenticated (`gh auth login`).
- Rust 1.88 or newer to build (edition 2024 with let-chains; developed and tested on 1.89).
- A terminal of at least 50x12 (80x24 or larger recommended). UTF-8 and truecolor are optional.

## Installation

```sh
# from GitHub
cargo install --git https://github.com/navbytes/gh-pulse

# from a clone
git clone https://github.com/navbytes/gh-pulse && cd gh-pulse
cargo install --path .

# with syntax-highlighted diffs (binary grows from ~1.6 MB to ~4.4 MB)
cargo install --path . --features syntax
```

## Usage

Run it inside a clone of a GitHub repository, or point it at one:

```sh
gh-pulse                       # repo of the current directory
gh-pulse -R cli/cli            # any repo you can access
gh-pulse --theme light --ascii
gh-pulse                       # then press B to browse every repo you can access
```

| Flag | Meaning |
|---|---|
| `-R, --repo owner/repo` | Repo to show. Checked up front; exits with a clear message if it is not found. |
| `--theme dark\|light` | Color palette (default `dark`). |
| `--ascii` | ASCII icons and borders. Also automatic when the locale is not UTF-8. |
| `--nerd` | Nerd Font icons. |

gh-pulse needs an interactive terminal; piping stdin/stdout prints a message and exits.

## Configuration

Optional `~/.config/gh-pulse/config.toml` (`$XDG_CONFIG_HOME` is honored): default theme and icons, favorite and
hidden repos (written by the repo browser), and key remapping. Flags override the file; a missing file means
defaults; an invalid file stops startup with `file:line: message`. See [docs/configuration.md](docs/configuration.md).
The file never contains credentials; authentication stays entirely with `gh`.

## Concepts

- **Repo scope.** Everything is about one repository: the current directory's, or `-R`. The header shows the
  repo, your local branch (only when the directory is a clone of it) and your user. `B` (or `Ctrl-r`) opens the repo browser to switch.
- **Panels.** Numbered like lazygit; the focused one expands, the rest collapse. `[` `]` change the detail
  tab, `{` `}` change the panel's own list (e.g. Mine vs Merged).
- **Files follows the PR.** Panel 3 always lists the files of the selected PR; moving through it changes the
  file shown in the diff.
- **Drill-in.** `Enter` on a PR replaces the left column with Files / Checks / Comments of that PR; `Esc` goes
  back and your cursor is where you left it.
- **Global view.** `G` (list focus) swaps the PR and Issues panels to searches across all your repos.
- **Safety.** See below.

## Keybindings

The full reference is in [docs/keybindings.md](docs/keybindings.md). The essentials:

| Context | Key | Action |
|---|---|---|
| Global | `?` / `q` | Help / quit |
| Global | `1`-`8`, `Tab`, `Shift-Tab` | Focus panel |
| Global | `x` | Action menu for the selected item |
| Global | `o` `y` `c` | Open in browser / copy URL / check out PR |
| Global | `r` `R` `L` | Refresh panel / all / command log |
| Lists | `j` `k` `Ctrl-d` `Ctrl-u` `g` `G` | Move |
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

gh-pulse never writes without asking.

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

```text
 cli/cli  branch:   user: you
╭ [1] Status ───────────────────────────────╮╭ Overview │ Checks │ Comments │ Diff ──────────────────────────────────╮
│cli/cli                                    ││pkg/cmd/issue/artifact/client/client.go  2/9  +31 -0  [auto:unified, wr│
╰───────────────────────────────────────────╯│@@ -39,6 +39,10 @@ type ArtifactClient interface {                     │
╭ [2] Pull requests · All open (72) ────────╮│ 39  39      // Create creates one artifact on an issue and returns    │
│#14571 `gh issue artifact` stack 8/10: Add ││        ↪it. The API doesn't                                           │
╰───────────────────────────────────────────╯│ 40  40      // return a new artifact's description.                   │
╭ [3] Files (9) ────────────────────────────╮│ 41  41      Create(repo ghrepo.Interface, issueNumber int,            │
│M artifact.go  pkg/cmd/is…/artifact/  +2 -0││        ↪artifactType, name, body string) (*Artifact, error)           │
│M client.go  pkg/cmd/iss…ct/client/  +31 -0││     42 +    // Update saves a new version of one artifact on an issue │
│M client_mock.go  pkg/cmd/…/client/  +68 -0││        ↪and returns the                                               │
│M client_test.go  pkg/cmd/…client/  +126 -0││     43 +    // artifact. A nil name or body keeps the current one.    │
│M create.go  pkg/cmd/iss…ct/create/  +1 -20││        ↪The API can't change                                          │
╰───────────────────────────────────────────╯│     44 +    // an artifact's type.                                    │
╭ [4] Issues · Assigned (0) ────────────────╮│     45 +    Update(repo ghrepo.Interface, issueNumber int, number int,│
│nothing here                               ││        ↪ name, body *string) (*Artifact, error)                       │
╰───────────────────────────────────────────╯│ 42  46  }                                                             │
╭ [5] Actions · Runs (100+) ────────────────╮│ 43  47                                                                │
│✓ Triage Scheduled Tasks  Triage Scheduled ││ 44  48  // maxPageSize is the most artifacts one request asks for.    │
╰───────────────────────────────────────────╯│@@ -163,3 +167,30 @@ func (c *artifactClient) Create(repo              │
╭ [6] Branches (100+) ──────────────────────╮│ghrepo.Interface, issueNumber int, artifact                            │
│⎇ 1119-support-for-owner-based-queries     ││163 167      }                                                         │
╰───────────────────────────────────────────╯│164 168      return &artifact, nil                                     │
╭ [7] Releases (100+) ──────────────────────╮│165 169  }                                                             │
│★ GitHub CLI 2.102.0                       ││    170 +                                                              │
╰───────────────────────────────────────────╯│    171 +// Update sends only the fields it changes, since the API     │
╭ [8] Notifications (0) ────────────────────╮│        ↪keeps any field a                                             │
│nothing here                               ││    172 +// request leaves out.                                        │
╰───────────────────────────────────────────╯╰───────────────────────────────────────────────────────────────────────╯
j/k file  l/Enter diff  v viewed  f zoom  [ ] panel  x actions  ? help  q quit
```

`t` cycles auto / unified / split (auto goes side-by-side when each half has 60+ columns). Long lines soft-wrap
at word boundaries with a `↪` marker; `w` switches to clipping. `f` zooms the right pane to full width.

## FAQ and troubleshooting

- **"not in a GitHub repo" / "repo not found or no access".** Run inside a clone or pass `-R owner/repo`;
  check `gh auth status` and that the account can see the repo.
- **Panels sit on "loading...".** gh-pulse waits on `gh`; slow networks or a large account mean several
  seconds. `L` shows what is running.
- **"Terminal too small".** The minimum is 50x12. On short terminals unfocused panels collapse to one line.
- **Colors look washed out or odd.** Your terminal probably lacks truecolor: set `COLORTERM=truecolor` if it
  supports it, or try `--theme light` on light backgrounds.
- **Boxes and icons are garbled.** Use `--ascii`, or install a Nerd Font and use `--nerd`.
- **A huge PR shows its diff anyway.** Over 300 files `gh pr diff` refuses; gh-pulse falls back to the files API.

## Roadmap

- Commits panel in the PR drill-in.
- Persisted (or GitHub-synced) viewed marks.
- Open PR/issue counts in more places; org-level views.
- Make syntax highlighting the default if the size cost is acceptable.

See [docs/BACKLOG.md](docs/BACKLOG.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and [docs/architecture.md](docs/architecture.md).

## License

MIT, see [LICENSE](LICENSE).
