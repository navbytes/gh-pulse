use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// `~/.config/gh-pulse/config.toml`. Never holds credentials: auth stays with `gh`.
#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    pub ascii: bool,
    pub nerd: bool,
    pub repos: ReposCfg,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub keys: BTreeMap<String, Keys>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ReposCfg {
    pub favorites: Vec<String>,
    pub hidden: Vec<String>,
}

impl ReposCfg {
    pub fn is_fav(&self, r: &str) -> bool {
        self.favorites.iter().any(|f| f.eq_ignore_ascii_case(r))
    }

    pub fn is_hidden(&self, r: &str) -> bool {
        self.hidden.iter().any(|f| f.eq_ignore_ascii_case(r))
    }

    /// Adds when absent, removes when present; returns the new state.
    fn toggle(list: &mut Vec<String>, r: &str) -> bool {
        match list.iter().position(|f| f.eq_ignore_ascii_case(r)) {
            Some(i) => {
                list.remove(i);
                false
            }
            None => {
                list.push(r.to_string());
                true
            }
        }
    }

    pub fn toggle_fav(&mut self, r: &str) -> bool {
        Self::toggle(&mut self.favorites, r)
    }

    pub fn toggle_hidden(&mut self, r: &str) -> bool {
        Self::toggle(&mut self.hidden, r)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Keys {
    One(String),
    Many(Vec<String>),
}

impl Keys {
    fn list(&self) -> Vec<&str> {
        match self {
            Keys::One(s) => vec![s],
            Keys::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))?;
    Some(base.join("gh-pulse").join("config.toml"))
}

pub fn parse(src: &str, name: &str) -> Result<Config, String> {
    let cfg: Config = toml::from_str(src).map_err(|e| {
        let line = e.span().map_or(1, |s| {
            src[..s.start.min(src.len())].matches('\n').count() + 1
        });
        format!("{name}:{line}: {}", e.message().trim())
    })?;
    if let Some(t) = &cfg.theme
        && !matches!(t.as_str(), "dark" | "light")
    {
        let line = src
            .lines()
            .position(|l| l.trim_start().starts_with("theme"))
            .map_or(1, |i| i + 1);
        return Err(format!(
            "{name}:{line}: theme must be \"dark\" or \"light\", got {t:?}"
        ));
    }
    Ok(cfg)
}

/// A missing file means defaults; an unreadable or invalid one is an error.
pub fn load_from(path: &Path) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => parse(&s, &path.display().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Write to a temp file in the same directory, then rename over the target. Refuses to overwrite a
/// file that no longer parses (the user may be mid-edit): their text is never clobbered.
pub fn save_to(path: &Path, cfg: &Config) -> Result<(), String> {
    load_from(path).map_err(|e| format!("not saving, config has errors: {e}"))?;
    let dir = path.parent().ok_or("bad config path")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let body = toml::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Named normal-mode actions whose keys can be remapped under `[keys]`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Act {
    Quit,
    Help,
    Refresh,
    RefreshAll,
    Actions,
    Approve,
    Comment,
    Merge,
    Open,
    CopyUrl,
    Checkout,
    CommandLog,
    Filter,
    Zoom,
    Global,
    Browser,
}

impl Act {
    pub const ALL: [Act; 16] = [
        Act::Quit,
        Act::Help,
        Act::Refresh,
        Act::RefreshAll,
        Act::Actions,
        Act::Approve,
        Act::Comment,
        Act::Merge,
        Act::Open,
        Act::CopyUrl,
        Act::Checkout,
        Act::CommandLog,
        Act::Filter,
        Act::Zoom,
        Act::Global,
        Act::Browser,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Act::Quit => "quit",
            Act::Help => "help",
            Act::Refresh => "refresh",
            Act::RefreshAll => "refresh_all",
            Act::Actions => "actions",
            Act::Approve => "approve",
            Act::Comment => "comment",
            Act::Merge => "merge",
            Act::Open => "open",
            Act::CopyUrl => "copy_url",
            Act::Checkout => "checkout",
            Act::CommandLog => "command_log",
            Act::Filter => "filter",
            Act::Zoom => "zoom",
            Act::Global => "global",
            Act::Browser => "repo_browser",
        }
    }

    fn default_keys(self) -> &'static [&'static str] {
        match self {
            Act::Quit => &["q"],
            Act::Help => &["?"],
            Act::Refresh => &["r"],
            Act::RefreshAll => &["R"],
            Act::Actions => &["x"],
            Act::Approve => &["a"],
            Act::Comment => &["C"],
            Act::Merge => &["m"],
            Act::Open => &["o"],
            Act::CopyUrl => &["y"],
            Act::Checkout => &["c"],
            Act::CommandLog => &["L"],
            Act::Filter => &["/"],
            Act::Zoom => &["f"],
            Act::Global => &["G"],
            Act::Browser => &["B", "ctrl-r"],
        }
    }
}

type KeyId = (KeyCode, bool);

/// Keys that navigate or belong to a context, so they can't be given to an action.
fn reserved(k: KeyId) -> bool {
    match k {
        (KeyCode::Char(c), false) => "jkhlgnpvtwesSTH.[]{}12345678".contains(c),
        (KeyCode::Char('c' | 'd' | 'u'), true) => true,
        (KeyCode::Char(_), _) => false,
        (_, _) => matches!(
            k.0,
            KeyCode::Enter
                | KeyCode::Esc
                | KeyCode::Tab
                | KeyCode::BackTab
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
        ),
    }
}

pub fn parse_key(s: &str) -> Result<KeyId, String> {
    let l = s.to_lowercase();
    if let Some(rest) = l.strip_prefix("ctrl-").or_else(|| l.strip_prefix("c-")) {
        let mut cs = rest.chars();
        return match (cs.next(), cs.next()) {
            (Some(c), None) => Ok((KeyCode::Char(c), true)),
            _ => Err(format!("bad key {s:?}: expected ctrl-<letter>")),
        };
    }
    let named = match l.as_str() {
        "tab" => Some(KeyCode::Tab),
        "backtab" | "s-tab" | "shift-tab" => Some(KeyCode::BackTab),
        "enter" | "return" => Some(KeyCode::Enter),
        "esc" | "escape" => Some(KeyCode::Esc),
        "space" => Some(KeyCode::Char(' ')),
        "backspace" => Some(KeyCode::Backspace),
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        _ => None,
    };
    if let Some(k) = named {
        return Ok((k, false));
    }
    let mut cs = s.chars();
    match (cs.next(), cs.next()) {
        (Some(c), None) => Ok((KeyCode::Char(c), false)),
        _ => Err(format!(
            "unknown key {s:?} (single character, ctrl-<letter>, or tab/enter/esc/space/...)"
        )),
    }
}

pub struct Keymap {
    by_key: HashMap<KeyId, Act>,
    shown: HashMap<Act, Vec<String>>,
}

impl Keymap {
    /// Defaults plus `[keys]` overrides (an override replaces that action's default keys).
    /// Unknown actions, unparsable keys, reserved keys and clashes are errors.
    pub fn build(over: &BTreeMap<String, Keys>) -> Result<Keymap, String> {
        let mut want: HashMap<Act, Vec<String>> = Act::ALL
            .iter()
            .map(|a| (*a, a.default_keys().iter().map(|s| s.to_string()).collect()))
            .collect();
        for (name, keys) in over {
            let act = Act::ALL.iter().find(|a| a.name() == name).ok_or_else(|| {
                let all: Vec<_> = Act::ALL.iter().map(|a| a.name()).collect();
                format!(
                    "[keys]: unknown action {name:?} (valid: {})",
                    all.join(", ")
                )
            })?;
            let list: Vec<String> = keys.list().iter().map(|s| s.to_string()).collect();
            if list.is_empty() {
                return Err(format!("[keys] {name}: give at least one key"));
            }
            want.insert(*act, list);
        }
        let (mut by_key, mut owner): (HashMap<KeyId, Act>, HashMap<KeyId, &str>) =
            Default::default();
        for act in Act::ALL {
            for k in &want[&act] {
                let id = parse_key(k).map_err(|e| format!("[keys] {}: {e}", act.name()))?;
                // the shipped defaults include keys that double as navigation (G), so only user keys are policed
                let user = over.contains_key(act.name());
                if user && reserved(id) {
                    return Err(format!(
                        "[keys] {}: {k:?} is a built-in navigation key and can't be remapped",
                        act.name()
                    ));
                }
                if let Some(prev) = owner.insert(id, act.name()) {
                    return Err(format!(
                        "[keys] {k:?} is bound to both {prev} and {}",
                        act.name()
                    ));
                }
                by_key.insert(id, act);
            }
        }
        Ok(Keymap {
            by_key,
            shown: want,
        })
    }

    pub fn get(&self, k: &KeyEvent) -> Option<Act> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        self.by_key.get(&(k.code, ctrl)).copied()
    }

    /// All keys bound to the action ("B / ctrl-r"), for the help screen.
    pub fn labels(&self, a: Act) -> String {
        self.shown
            .get(&a)
            .map(|v| v.join(" / "))
            .unwrap_or_default()
    }

    /// First key bound to the action, for hints.
    pub fn label(&self, a: Act) -> String {
        self.shown
            .get(&a)
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(toml: &str) -> BTreeMap<String, Keys> {
        parse(toml, "t").unwrap().keys
    }

    #[test]
    fn defaults_and_full_file() {
        assert_eq!(parse("", "t").unwrap(), Config::default());
        let c = parse(
            "theme = \"light\"\nascii = true\n[repos]\nfavorites = [\"o/a\"]\nhidden = [\"o/b\"]\n[keys]\nquit = [\"q\", \"ctrl-q\"]\nactions = \"space\"\n",
            "t",
        )
        .unwrap();
        assert_eq!(c.theme.as_deref(), Some("light"));
        assert!(c.ascii && !c.nerd);
        assert!(c.repos.is_fav("O/A") && c.repos.is_hidden("o/b"));
        assert_eq!(c.keys.len(), 2);
    }

    #[test]
    fn invalid_files_name_the_line() {
        let e = parse("ascii = true\nnerd = 3\n", "cfg").unwrap_err();
        assert!(e.starts_with("cfg:2:"), "{e}");
        let e = parse("ascii = true\ntheem = \"dark\"\n", "cfg").unwrap_err();
        assert!(e.starts_with("cfg:2:") && e.contains("theem"), "{e}");
        let e = parse("a = 1\n\ntheme = \"neon\"\n", "cfg");
        assert!(e.is_err());
        let e = parse("theme = \"neon\"\n", "cfg").unwrap_err();
        assert!(e.starts_with("cfg:1:") && e.contains("neon"), "{e}");
    }

    #[test]
    fn key_overrides_resolve_and_conflicts_fail() {
        let km = Keymap::build(&BTreeMap::new()).unwrap();
        let ev = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        assert_eq!(km.get(&ev('x')), Some(Act::Actions));
        assert_eq!(
            km.get(&KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            Some(Act::Browser)
        );
        assert_eq!(km.label(Act::Quit), "q");
        let km = Keymap::build(&keys(
            "[keys]\nactions = \"z\"\nquit = [\"Q\", \"ctrl-q\"]\n",
        ))
        .unwrap();
        assert_eq!(km.get(&ev('z')), Some(Act::Actions));
        assert_eq!(km.get(&ev('x')), None, "override replaces the default");
        assert_eq!(km.get(&ev('Q')), Some(Act::Quit));
        assert_eq!(km.label(Act::Quit), "Q");
        let err = |t: &str| Keymap::build(&keys(t)).err().unwrap_or_default();
        assert!(err("[keys]\nnope = \"z\"\n").contains("unknown action"));
        assert!(err("[keys]\nactions = \"j\"\n").contains("navigation"));
        assert!(
            err("[keys]\nactions = \"q\"\n").contains("both"),
            "clash with quit"
        );
        assert!(err("[keys]\nactions = \"abc\"\n").contains("unknown key"));
        assert!(err("[keys]\nactions = []\n").contains("at least one"));
    }

    #[test]
    fn atomic_save_round_trips_and_never_clobbers() {
        let dir = std::env::temp_dir().join(format!("gh-pulse-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("config.toml");
        assert_eq!(
            load_from(&path).unwrap(),
            Config::default(),
            "missing file = defaults"
        );
        let mut c = Config {
            theme: Some("dark".into()),
            nerd: true,
            ..Default::default()
        };
        c.repos.toggle_fav("o/a");
        c.repos.toggle_hidden("o/b");
        c.keys
            .insert("quit".into(), Keys::Many(vec!["q".into(), "ctrl-q".into()]));
        save_to(&path, &c).unwrap();
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "temp file renamed away"
        );
        assert_eq!(load_from(&path).unwrap(), c);
        assert!(
            !c.repos.clone().toggle_fav("o/a"),
            "toggle removes when present"
        );
        std::fs::write(&path, "ascii = [oops\n").unwrap();
        assert!(save_to(&path, &c).unwrap_err().contains("not saving"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "ascii = [oops\n",
            "invalid file left untouched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
