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

[repos]
favorites = ["octocat/hello-world"]
hidden = ["octocat/old-experiments"]

[keys]
quit = ["q", "ctrl-q"]
actions = "space"
```

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

The `?` help and the bottom bar show the keys you configured. Startup fails (with the config path and the offending entry) on an unknown action, an unparsable key, a key bound to two actions, an
empty list, or a key that is reserved for navigation: `j k h l g n p t w v e s S T H .`, `[ ] { }`, digits `1`-`8`,
`Enter`, `Esc`, `Tab`, arrows, `Home`, `End`, and `ctrl-c`/`ctrl-d`/`ctrl-u`. Inside popups and the repo browser the
keys are fixed.
