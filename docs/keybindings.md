# Keybindings

Press `?` in the app for the same list. Keys are checked against `src/app.rs` (`on_key` and `modal_key`).
"List focus" means a left-column panel has focus; "detail focus" means the right pane has it (`l` / `Right`).

## Global

| Key | Action |
|---|---|
| `?` | Keybindings menu (lazygit style): grouped rows with a key column; `j` `k` / arrows move, `/` filters by key, description or section (Enter keeps the filter), `Esc` clears the filter and then closes (`q` closes too) |
| `q` | Quit (closes the log view first when one is open) |
| `Ctrl-C` | Quit, from anywhere including popups |
| `1`-`7` | Focus panel by number (`1` to the number of panels shown, at most `7`; `Tab` / `Shift-Tab` reach any beyond that, e.g. with custom `[[sections]]`; a PR drill-in has 4). Pressing the number of the already focused panel steps to its next list tab |
| `Tab` / `Shift-Tab` | Next / previous panel |
| `x` | Action menu for the selected item or row |
| `a` | Approve (shortcut into the menu, PRs) |
| `C` | Comment (PRs and issues) |
| `m` | Merge (open PRs) |
| `o` | Open in the browser (`open`, or `xdg-open` off macOS) |
| `y` | Copy the URL (`pbcopy`, `wl-copy` or `xclip`); on a tag, its name |
| `c` | Check out the selected PR (cwd must be a clone of its repo); not available for tags |
| `r` | Refresh the selected item (its cached details) and the list tab it is in, skipping the on-disk cache |
| `R` | Reload everything, skipping the on-disk cache |
| `L` | Toggle the command log strip |
| `E` | Edit the config in `$VISUAL` / `$EDITOR`, then apply it (see [configuration.md](configuration.md#editing-the-config)) |
| `f` | Zoom the right pane to full width; `f` or `Esc` restores |
| `B` / `Ctrl-r` | Repo browser (full screen) |
| `N` | Inbox (full screen): unread notifications across repos. `j` `k` `g` `G` move, `Enter` opens the PR/issue in the app (switching repo; a closed or merged one opens in the browser), `m` marks read (confirm shows `gh api -X PATCH notifications/threads/<id>`), `o` opens in the browser, `r` refreshes, `N` / `Esc` / `q` close. Hidden repos are filtered |

The keys of the actions in this table can be remapped in `config.toml`; see [configuration.md](configuration.md).

## Lists (left column)

| Key | Action |
|---|---|
| `j` `k` / `Down` `Up` | Move |
| `Ctrl-d` / `Ctrl-u` | Half page down / up |
| `g` / `Home`, `G` / `End` | Top / bottom. Exception: `G` in a normal-layout list focus (PR, Issues, ...) toggles the global view, so use `End` there. In the Files panel and in a PR drill-in `G` jumps to the last row and says so in the status line |
| `{` `}` | Previous / next list tab of the panel (Mine, Review, All, Merged; Assigned, Mine, All; Runs, Workflows; Branches, Tags, Releases). Clicking a tab label in the panel title does the same |
| `/` | Filter: type, `Enter` applies, `Esc` clears |
| `Esc` | Clear the filter (after leaving zoom, detail focus and drill-in) |
| `Enter` | On a PR: drill in. Elsewhere: focus the detail pane |
| `l` / `Right` | Focus the detail pane |
| `G` | Swap between the repo home and the global home (each remembers its cursor, tab and filter); from the lists only (not Files, not inside a drill-in). With no repo context yet it says so |
| `s` | In the global home: scope picker (All, Favorites, Org..., Repo...). In the Repos panel: scope the global home to the selected repo. In a repo: explains |
| `S` | Open the selected item's repo as the app's repo (`G` returns to the global home). On a Repos row: same as `Enter` |

The Files panel (2) has no filter and follows the selected PR; the Checks and Comments panels exist only in a drill-in.

## Global home

Sections: `1` Review requested, `2` My PRs (`{` `}`: Open / Merged / Closed), `3` Issues (Assigned / Mine / Mentioned),
`4` Repos (Favorites / Recent); `[panels] global` reorders, drops or adds Involved and Files. Everything else (detail
tabs, drill-in with `Enter`, actions, `x`, `a`, `m`, `C`) works on a row with that row's repo.
Custom `[[sections]]` follow the built-in panels (`5`, `6`, ... or `Tab`); they search when first focused, `r` searches again,
and `s` applies to those whose filter has no `repo:` / `org:` / `user:` of its own (their title says `(own filter)`).

| Key | Action |
|---|---|
| `s` | Scope picker: `j` `k` / arrows, `Enter` picks, typing filters Org... and Repo... lists (a full `owner/name` is accepted; in Org... any valid org or user name offers `Use org 'name'`, member or not), `Backspace` goes back, `Esc` cancels |
| `g` | Group the list: flat, by PR author, by repo. Each heading shows the count and the longest wait; the group that has waited longest comes first. (Elsewhere `g` still jumps to the top; `Home` always does.) |
| `W` | Cycle the pull request window: 24h, 7d (default, `[ui] window`), 30d, all. The PR sections search again |
| `S` | Open the row's repo as the app's repo; `G` leads back |
| `r` | Search this section again |
| Repos panel: `Enter` | Open the repo (`S` too) |
| `H` | On any row of a global section (Review requested, My PRs, Issues, custom...): hide that row's repo everywhere; unhide it in the Repos panel or the browser (`.` shows hidden repos) |
| Repos panel: `s` / `f` / `H` | Scope the home to it / toggle favorite (same list as the repo browser) / hide it everywhere |

## PR drill-in (after `Enter` on a PR)

Panels: `1` Files, `2` Commits, `3` Checks, `4` Comments.

| Key | Action |
|---|---|
| `[` `]` | Previous / next panel (Files, Checks, Comments) |
| `1`-`3`, `Tab` | Focus a panel |
| `Esc` | Back to the normal panels, cursor preserved |

Right pane: Files shows the diff of the selected file, Commits the diff of the selected commit (`n`/`p` pick its file, `t` `w` `f` as in Diff), Checks the failed-step log of the selected check, Comments the full thread.

## Detail pane

| Key | Action |
|---|---|
| `j` `k` `Ctrl-d` `Ctrl-u` `g` `G` | Scroll, or move the row cursor on Checks / Comments / Diff tabs |
| `[` `]` | Previous / next detail tab, from list focus and from the Files panel (focusing Files starts on Diff) (Overview, Checks, Comments, Diff for PRs; Overview, Comments for issues; Jobs, Logs for runs; Overview, Commits for branches; Overview, Assets for releases). Works from list focus too. |
| `Enter` | On the Checks tab: show the check's failed-step log (`Esc`, `q`, `h` close it). On Comments: expand / collapse the selected comment (long comments, `<details>` blocks). In the drill-in Comments panel it works straight from the list |
| `h` / `Left` / `Esc` | Back to the list |

## Creating things

| Key | Action |
|---|---|
| `n` | In the Issues panel: new issue. In the Pull requests panel: new PR from the branch checked out in the current directory (if it is a clone of the repo) |
| `d` | In the Actions panel on an active workflow: run it (`workflow_dispatch`) |
| `x` | The menu has the same entries: "New issue...", "New pull request from the current branch...", "Run workflow...", and on a branch "Create pull request..." |

Forms: `Tab` / `Shift-Tab` (or `Up` / `Down` on one-line fields) move between fields, `Enter` adds a newline in a Body and
moves on in a one-line field, `Space` toggles, `Left` / `Right` change a picker (template, base branch, ref, choice
input), `Ctrl-S` validates and shows the exact `gh ...` command in the usual confirm popup (only `y` runs it), `Esc`
cancels. Nothing is created without that confirm.

## Comments (Comments tab, issue Comments, drill-in Comments)

| Key | Action |
|---|---|
| `j` `k` | Move by comment |
| `Ctrl-d` `Ctrl-u` | Half a page: scrolls inside a comment taller than the pane first, then moves to the next/previous comment |
| `Enter` | Expand / collapse the comment (long bodies, `<details>`) |
| `e` | Show / hide who reacted (the first page has counts only; names are one call per comment, made on demand) |
| `m` | Load the next page of comments (shadows the merge shortcut on this tab; merge stays in `x`). The next page also loads by itself when you scroll within 20 rows of the end |
| `x` | Menu: reply to / resolve the thread, comment |

## Diff (Diff tab or Files panel)

| Key | Action |
|---|---|
| `n` / `p` | Next / previous file |
| `j` `k`, `Ctrl-d` `Ctrl-u`, `g` `G` | Move the line cursor / half page / first and last row (the cursor line is what inline comments attach to) |
| `t` | Cycle layout: auto, unified, split. Auto goes side-by-side when each half has at least 60 columns |
| `w` | Toggle soft-wrap / clip |
| `v` | Mark the file viewed (saved to `$XDG_STATE_HOME/gh-tui/viewed.json`, per PR head commit; PR files only; also on GitHub with `sync_viewed = true`) |
| `f` | Zoom |
| `x` | Menu, including "Comment on file:line" for the cursor line |

## Popups

| Popup | Keys |
|---|---|
| Action menu | `j` `k` / arrows move, `Enter` picks, `Esc` / `q` cancels |
| Text input | typing, `Enter` newline, `Backspace`, `Ctrl-S` continue, `Esc` cancel |
| Form | `Tab` / `Shift-Tab` fields, `Ctrl-S` review command, `Esc` cancel (asks `y/n` once you typed something); bracketed paste inserts literally (newlines only in the body) |
| Confirm | `y` runs the command; `n` / `Esc` cancels (a form-originated command goes back to its form); if the command is taller than the screen it scrolls (`j` `k` `PgUp` `PgDn` `Home` `End`/`G`) and `y` is refused until the end has been shown. `Enter` does nothing on purpose |
| Filter prompt | typing, `Backspace`, `Enter` applies, `Esc` clears |

## Repo browser (`B` / `Ctrl-r`)

Opens with the search box focused: type to filter by name or language. Up/Down, `PageUp`/`PageDown`, `Ctrl-d`/`Ctrl-u`,
`Home`/`End` move the cursor even while typing. `Enter` or `Esc` leaves the box; `/` returns to it.

| Key | Action |
|---|---|
| `Enter` | Switch the app to the selected repo (same as `-R`) |
| `j` `k` `g` `G` | Move (outside the search box) |
| `/` | Focus the search box |
| `s` | Cycle sort: pushed, name, stars |
| `S` | Reverse the sort direction |
| `T` | Cycle type: all, owned (yours), member (someone else's personal repo you collaborate on), org, favorites |
| `f` | Toggle favorite (favorites always sort to the top) |
| `H` | Hide / unhide the repo (hidden repos vanish from the browser and from the global view) |
| `.` | Show hidden repos (dimmed, with an icon) so they can be unhidden |
| `r` | Reload from GitHub |
| `Esc` / `q` / `B` / `Ctrl-r` | Clear the search, then close |

Favorites and hidden repos are saved to `config.toml` immediately.

## Mouse

Click a panel to focus it, a row to select it, a detail tab to switch to it, a diff or check row to put the cursor there. The wheel scrolls lists and the detail pane. Popups ignore the mouse.
