# Keybindings

Press `?` in the app for the same list. Keys are checked against `src/app.rs` (`on_key` and `modal_key`).
"List focus" means a left-column panel has focus; "detail focus" means the right pane has it (`l` / `Right`).

## Global

| Key | Action |
|---|---|
| `?` | Help popup (sized to its content; `j` `k` scroll when the terminal is short, any other key closes) |
| `q` | Quit (closes the log view first when one is open) |
| `Ctrl-C` | Quit, from anywhere including popups |
| `1`-`8` | Focus panel by number (only as many as are shown; a PR drill-in has 3) |
| `Tab` / `Shift-Tab` | Next / previous panel |
| `x` | Action menu for the selected item or row |
| `a` | Approve (shortcut into the menu, PRs) |
| `C` | Comment (PRs and issues) |
| `m` | Merge (open PRs) |
| `o` | Open in the browser (`open`, or `xdg-open` off macOS) |
| `y` | Copy the URL (`pbcopy`, `wl-copy` or `xclip`) |
| `c` | Check out the selected PR (cwd must be a clone of its repo) |
| `r` | Refresh the focused panel (and clear cached details) |
| `R` | Refresh all panels |
| `L` | Toggle the command log strip |
| `f` | Zoom the right pane to full width; `f` or `Esc` restores |
| `Ctrl-r` | Repo switcher |

## Lists (left column)

| Key | Action |
|---|---|
| `j` `k` / `Down` `Up` | Move |
| `Ctrl-d` / `Ctrl-u` | Half page down / up |
| `g` / `Home`, `G` / `End` | Top / bottom. Exception: `G` in a normal-layout list focus (PR, Issues, ...) toggles the global view, so use `End` there. In the Files panel and in a PR drill-in `G` jumps to the last row and says so in the status line |
| `{` `}` | Previous / next list tab of the panel (Mine, Review requested, All open, Merged; Assigned, Mine, All open; Runs, Workflows) |
| `/` | Filter: type, `Enter` applies, `Esc` clears |
| `Esc` | Clear the filter (after leaving zoom, detail focus and drill-in) |
| `Enter` | On a PR: drill in. Elsewhere: focus the detail pane |
| `l` / `Right` | Focus the detail pane |
| `G` | Toggle the global view (PRs and issues across all repos); from the PR, Issues, Actions, Branches, Releases and Notifications panels only (not Files, not inside a drill-in) |

The Files panel (3) has no filter and follows the selected PR; the Checks and Comments panels exist only in a drill-in.

## PR drill-in (after `Enter` on a PR)

| Key | Action |
|---|---|
| `[` `]` | Previous / next panel (Files, Checks, Comments) |
| `1`-`3`, `Tab` | Focus a panel |
| `Esc` | Back to the normal panels, cursor preserved |

Right pane: Files shows the diff of the selected file, Checks the failed-step log of the selected check, Comments the full thread.

## Detail pane

| Key | Action |
|---|---|
| `j` `k` `Ctrl-d` `Ctrl-u` `g` `G` | Scroll, or move the row cursor on Checks / Comments / Diff tabs |
| `[` `]` | Previous / next detail tab, from list focus and from the Files panel (focusing Files starts on Diff) (Overview, Checks, Comments, Diff for PRs; Overview, Comments for issues; Jobs, Logs for runs; Overview, Commits for branches; Overview, Assets for releases). Works from list focus too. |
| `Enter` | On the Checks tab: show the check's failed-step log (`Esc`, `q`, `h` close it) |
| `h` / `Left` / `Esc` | Back to the list |

## Diff (Diff tab or Files panel)

| Key | Action |
|---|---|
| `n` / `p` | Next / previous file |
| `j` `k`, `Ctrl-d` `Ctrl-u`, `g` `G` | Move the line cursor / half page / first and last row (the cursor line is what inline comments attach to) |
| `t` | Cycle layout: auto, unified, split. Auto goes side-by-side when each half has at least 60 columns |
| `w` | Toggle soft-wrap / clip |
| `v` | Mark the file viewed (in memory) |
| `f` | Zoom |
| `x` | Menu, including "Comment on file:line" for the cursor line |

## Popups

| Popup | Keys |
|---|---|
| Action menu | `j` `k` / arrows move, `Enter` picks, `Esc` / `q` cancels |
| Text input | typing, `Enter` newline, `Backspace`, `Ctrl-S` continue, `Esc` cancel |
| Confirm | `y` runs the command; `n` / `Esc` cancels; `j` `k` scroll if the command is taller than the screen. `Enter` does nothing on purpose |
| Repo switcher | `j` `k` / arrows move, `Enter` picks, `Esc` / `q` cancels |
| Filter prompt | typing, `Backspace`, `Enter` applies, `Esc` clears |

## Mouse

Click a panel to focus it, a row to select it, a detail tab to switch to it, a diff or check row to put the cursor there. The wheel scrolls lists and the detail pane. Popups ignore the mouse.
