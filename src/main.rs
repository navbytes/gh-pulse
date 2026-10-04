mod act;
mod app;
mod browse;
mod cache;
mod config;
mod dcache;
mod diff;
mod dispatch;
mod editor;
mod form;
mod gh;
mod global;
mod help;
mod md;
mod paths;
mod pool;
mod rate;
mod sanitize;
mod start;
mod state;
mod syn;
#[cfg(test)]
mod testshim;
mod theme;
mod ui;

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind, MouseEventKind,
};
use std::io::IsTerminal;
use std::time::Duration;
use theme::{IconSet, Theme};

const USAGE: &str = "usage: gh-tui [-R owner/repo] [--start auto|repo|global] [--theme dark|light] [--ascii] [--nerd] [--clear-cache] [--print-config | --config-path | --edit-config] [-V]";

const HELP: &str = "gh-tui: a lazygit-style terminal UI for GitHub

usage: gh-tui [-R owner/repo] [--start auto|repo|global] [--theme dark|light] [--ascii] [--nerd] [--clear-cache] [--print-config | --config-path | --edit-config] [-V]

  -R, --repo owner/repo   open this repo
  --start MODE            where to open (config: [ui] start):
                            auto    the repo of the current directory (or -R); outside one, the global home
                            repo    always a repo; outside a clone, an error
                            global  the global home: review requests, your PRs, assigned issues, repos
                                    (G switches between it and a repo)
  --theme dark|light      color palette
  --ascii | --nerd        ASCII icons and borders | Nerd Font icons
  --clear-cache           delete gh-tui's on-disk cache and exit
  --edit-config           open the config in $VISUAL / $EDITOR (creating it from the commented template
                          first if it does not exist) and check it afterwards; E does this inside gh-tui
  --print-config          print the commented default config (every setting, with its default) and exit
  --config-path           print where the config lives and exit
  -V, --version           print the version
  -h, --help              this text

also runs as a GitHub CLI extension: `gh tui [flags]`";

fn version() -> String {
    format!("gh-tui {}", env!("CARGO_PKG_VERSION"))
}

struct MouseGuard;

impl Drop for MouseGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(
            std::io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste
        );
    }
}

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2)
}

/// The palette and icons: the flags win over the config file.
fn build_theme(cfg: &config::Config, (light, ascii, nerd): (Option<bool>, bool, bool)) -> Theme {
    let (ascii, nerd) = (ascii || cfg.ascii, nerd || cfg.nerd);
    let light = light.unwrap_or(cfg.theme.as_deref() == Some("light"));
    let icons = if ascii || !theme::utf8() {
        IconSet::Ascii
    } else if nerd {
        IconSet::Nerd
    } else {
        IconSet::Unicode
    };
    Theme::new(light, icons, theme::truecolor())
}

/// `gh-tui --edit-config`: create the file from the template if needed, edit it, and say whether it is
/// valid afterwards (exit code 1 when it is not, so a script or a second run can fix it).
fn edit_config_cli() -> std::io::Result<()> {
    let Some(path) = config::path() else {
        die("no config directory: neither XDG_CONFIG_HOME nor HOME is set")
    };
    match editor::ensure_file(&path) {
        Ok(true) => eprintln!("created {} from the commented template", path.display()),
        Ok(false) => {}
        Err(e) => die(&e),
    }
    if let Err(e) = editor::edit(&path) {
        die(&e);
    }
    match config::load_checked(&path) {
        Ok(_) => println!("{}: ok", path.display()),
        Err(e) => {
            eprintln!("{e}\nnot valid yet: run `gh-tui --edit-config` again to fix it");
            std::process::exit(1);
        }
    }
    Ok(())
}

/// `E`: hand the terminal to the editor, take it back, and apply the file if it loads.
fn edit_config_in_tui(
    term: &mut ratatui::DefaultTerminal,
    app: &mut app::App,
    look: (Option<bool>, bool, bool),
) -> std::io::Result<()> {
    let Some(path) = config::path() else {
        app.set_status("no config directory: neither XDG_CONFIG_HOME nor HOME is set");
        return Ok(());
    };
    let created = match editor::ensure_file(&path) {
        Ok(c) => c,
        Err(e) => {
            app.set_status(e);
            return Ok(());
        }
    };
    let mut out = std::io::stdout();
    crossterm::execute!(out, DisableMouseCapture, DisableBracketedPaste)?;
    ratatui::restore();
    let edited = editor::edit(&path);
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        out,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    // a fresh terminal repaints everything; `Terminal::clear` would ask the terminal where the cursor
    // is, and wait for an answer a slow link (or a terminal that never answers) may not give
    *term = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))?;
    if let Err(e) = edited {
        app.set_status(format!("editor: {e}"));
        return Ok(());
    }
    match config::load_checked(&path) {
        Ok((cfg, keys)) => {
            let theme = build_theme(&cfg, look);
            let restart = app.apply_config(cfg, keys, theme);
            let made = if created {
                "created from the template, "
            } else {
                ""
            };
            app.set_status(if restart.is_empty() {
                format!("{made}config applied")
            } else {
                format!("{made}config applied; restart for: {}", restart.join(", "))
            });
        }
        // the old settings stay in force; E opens the file again to fix it
        Err(e) => app.set_status(format!("config not applied (E to fix it): {e}")),
    }
    Ok(())
}

fn main() -> std::io::Result<()> {
    let (mut repo, mut light, mut ascii, mut nerd) = (None, None, false, false);
    let mut start_flag = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-R" | "--repo" => repo = args.next().or_else(|| die(USAGE)),
            "--theme" => match args.next().as_deref() {
                Some("light") => light = Some(true),
                Some("dark") => light = Some(false),
                _ => die(USAGE),
            },
            "--ascii" => ascii = true,
            "--nerd" => nerd = true,
            "--clear-cache" => {
                match cache::clear() {
                    Ok(Some(d)) => println!("removed {}", d.display()),
                    Ok(None) => println!("nothing cached"),
                    Err(e) => die(&e),
                }
                return Ok(());
            }
            "--print-config" => {
                print!("{}", config::template());
                return Ok(());
            }
            "--config-path" => {
                match config::path() {
                    Some(p) => println!("{}", p.display()),
                    None => die("no config directory: neither XDG_CONFIG_HOME nor HOME is set"),
                }
                return Ok(());
            }
            "--edit-config" => return edit_config_cli(),
            "--start" => match args.next().as_deref().and_then(config::StartMode::parse) {
                Some(m) => start_flag = Some(m),
                None => die("--start takes auto, repo or global"),
            },
            "-h" | "--help" => {
                println!("{}\n\n{HELP}", version());
                return Ok(());
            }
            "-V" | "--version" => {
                println!("{}", version());
                return Ok(());
            }
            _ => die(USAGE),
        }
    }
    // Config problems are startup errors, never silent: a typo'd key or a clashing binding stops here.
    let cfg = match config::path() {
        Some(p) => config::load_from(&p).unwrap_or_else(|e| die(&e)),
        None => config::Config::default(),
    };
    let keys = config::Keymap::build(&cfg.keys).unwrap_or_else(|e| {
        let at = config::path().map_or_else(|| "config".into(), |p| p.display().to_string());
        die(&format!("{at}: {e}"))
    });
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        die("gh-tui needs an interactive terminal");
    }
    let mode = start_flag.unwrap_or(cfg.ui.start);
    // `gh repo view` (an API call) only runs inside a git work tree, and never when -R is given
    let start = start::decide(
        mode,
        repo.clone(),
        || gh::in_git_repo().then(|| gh::repo_here().ok()).flatten(),
        gh::local_repo,
    )
    .unwrap_or_else(|e| die(&e));
    let (repo, global) =
        start::canonical(start, repo.is_some(), gh::resolve_repo).unwrap_or_else(|e| die(&e));
    // Flags win over the config file.
    let look = (light, ascii, nerd);
    let theme = build_theme(&cfg, look);

    // Mouse capture must be undone on panic too, or the shell keeps receiving click escape codes.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |i| {
        let _ = crossterm::execute!(
            std::io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste
        );
        prev(i);
    }));
    // ratatui::run installs a panic hook and restores the terminal on every exit path.
    let result = ratatui::run(|term| {
        crossterm::execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
        let _guard = MouseGuard;
        let mut app = app::App::from_config(repo, global, theme, cfg, keys);
        loop {
            app.poll();
            term.draw(|f| ui::draw(f, &app))?;
            if !event::poll(Duration::from_millis(100))? {
                app.ensure();
                continue;
            }
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    if app.on_key(k) {
                        return Ok(());
                    }
                    if app.take_edit_config() {
                        edit_config_in_tui(term, &mut app, look)?;
                    }
                }
                Event::Mouse(m) if !matches!(m.kind, MouseEventKind::Moved) => app.on_mouse(m),
                Event::Paste(t) => app.on_paste(&t),
                _ => {} // Resize: the next draw re-lays-out from the new size
            }
        }
    });
    // no orphaned `gh` processes after we are gone
    gh::kill_children();
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_line_uses_package_version() {
        assert_eq!(
            super::version(),
            format!("gh-tui {}", env!("CARGO_PKG_VERSION"))
        );
    }
}
