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

[panels]
show = ["prs", "files", "issues", "actions", "repo"]   # order = numbering
hide_empty = false

[panels.repo]
tabs = ["branches", "releases"]    # drop Tags
default_tab = "branches"
```

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
| `refresh` | `r` | Refresh the focused panel |
| `refresh_all` | `R` | Refresh everything |
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
