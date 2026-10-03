mod act;
mod app;
mod browse;
mod cache;
mod config;
mod diff;
mod dispatch;
mod form;
mod gh;
mod global;
mod md;
mod pool;
mod rate;
mod sanitize;
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

const USAGE: &str =
    "usage: gh-pulse [-R owner/repo] [--theme dark|light] [--ascii] [--nerd] [--clear-cache]";

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

fn main() -> std::io::Result<()> {
    let (mut repo, mut light, mut ascii, mut nerd) = (None, None, false, false);
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
            "-h" | "--help" => die(USAGE),
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
        die("gh-pulse needs an interactive terminal");
    }
    let repo = match repo {
        Some(r) => gh::resolve_repo(&r).unwrap_or_else(|e| die(&e)),
        None => gh::repo_here()
            .unwrap_or_else(|e| die(&format!("not in a GitHub repo ({e}); pass -R owner/repo"))),
    };
    // Flags win over the config file.
    let (ascii, nerd) = (ascii || cfg.ascii, nerd || cfg.nerd);
    let light = light.unwrap_or(cfg.theme.as_deref() == Some("light"));
    let icons = if ascii || !theme::utf8() {
        IconSet::Ascii
    } else if nerd {
        IconSet::Nerd
    } else {
        IconSet::Unicode
    };
    let theme = Theme::new(light, icons, theme::truecolor());

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
        let mut app = app::App::from_config(repo, theme, cfg, keys);
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
