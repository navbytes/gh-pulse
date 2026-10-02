use ratatui::{
    style::Color,
    symbols::border::{self, Set},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IconSet {
    Ascii,
    Unicode,
    Nerd,
}

pub struct Icons {
    pub ok: &'static str,
    pub fail: &'static str,
    pub pending: &'static str,
    pub queued: &'static str,
    pub skip: &'static str,
    pub branch: &'static str,
    pub star: &'static str,
    pub unread: &'static str,
    /// Soft-wrap continuation marker.
    pub cont: &'static str,
    pub thread: &'static str,
    pub viewed: &'static str,
    pub dot: &'static str,
    /// Middle-ellipsis glyph.
    pub ell: &'static str,
    pub fav: &'static str,
    pub hidden: &'static str,
    pub up: &'static str,
    pub down: &'static str,
    pub spin: &'static [&'static str],
}

impl Icons {
    fn new(set: IconSet) -> Self {
        match set {
            IconSet::Ascii => Icons {
                ok: "+",
                fail: "x",
                pending: "*",
                queued: "o",
                skip: "-",
                branch: "",
                star: "*",
                unread: "*",
                cont: ">",
                thread: "#",
                viewed: "v",
                dot: "-",
                ell: "~",
                fav: "*",
                hidden: "x",
                up: "^",
                down: "v",
                spin: &["|", "/", "-", "\\"],
            },
            IconSet::Unicode => Icons {
                ok: "✓",
                fail: "✗",
                pending: "●",
                queued: "◌",
                skip: "-",
                branch: "⎇ ",
                star: "★",
                unread: "●",
                cont: "↪",
                thread: "◆",
                viewed: "✓",
                dot: "·",
                ell: "…",
                fav: "★",
                hidden: "⊘",
                up: "▲",
                down: "▼",
                spin: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
            },
            IconSet::Nerd => Icons {
                ok: "\u{f00c}",
                fail: "\u{f00d}",
                pending: "\u{f111}",
                queued: "\u{f10c}",
                skip: "\u{f068}",
                branch: "\u{e0a0} ",
                star: "\u{f005}",
                unread: "\u{f111}",
                cont: "↪",
                thread: "\u{f27b}",
                viewed: "\u{f00c}",
                dot: "·",
                ell: "…",
                fav: "\u{f005}",
                hidden: "\u{f070}",
                up: "▲",
                down: "▼",
                spin: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
            },
        }
    }
}

/// Every color the UI uses; `Reset` for body text so both terminal backgrounds work.
pub struct Theme {
    pub light: bool,
    pub accent: Color,
    pub text: Color,
    pub muted: Color,
    pub ok: Color,
    pub err: Color,
    pub warn: Color,
    pub merged: Color,
    pub hunk: Color,
    pub hunk_bg: Color,
    pub code_bg: Color,
    pub quote: Color,
    pub link: Color,
    pub ascii: bool,
    pub add_bg: Color,
    pub del_bg: Color,
    pub add_bg2: Color,
    pub del_bg2: Color,
    pub sel_bg: Color,
    pub border: Set<'static>,
    pub ic: Icons,
    truecolor: bool,
}

type Rgb = (u8, u8, u8);

impl Theme {
    pub fn new(light: bool, icons: IconSet, truecolor: bool) -> Self {
        let p = |dark: Rgb, lite: Rgb| if light { lite } else { dark };
        let mut t = Theme {
            light,
            accent: Color::Reset,
            text: Color::Reset,
            muted: Color::Reset,
            ok: Color::Reset,
            err: Color::Reset,
            warn: Color::Reset,
            merged: Color::Reset,
            hunk: Color::Reset,
            hunk_bg: Color::Reset,
            code_bg: Color::Reset,
            quote: Color::Reset,
            link: Color::Reset,
            ascii: icons == IconSet::Ascii,
            add_bg: Color::Reset,
            del_bg: Color::Reset,
            add_bg2: Color::Reset,
            del_bg2: Color::Reset,
            sel_bg: Color::Reset,
            border: if icons == IconSet::Ascii {
                ASCII
            } else {
                border::ROUNDED
            },
            ic: Icons::new(icons),
            truecolor,
        };
        t.accent = t.rgb(p((126, 211, 129), (20, 130, 60)));
        t.muted = t.rgb(p((120, 126, 142), (120, 126, 138)));
        t.ok = t.rgb(p((126, 211, 129), (20, 130, 60)));
        t.err = t.rgb(p((240, 98, 98), (200, 40, 40)));
        t.warn = t.rgb(p((229, 192, 90), (170, 120, 0)));
        t.merged = t.rgb(p((176, 126, 230), (130, 70, 190)));
        t.hunk = t.rgb(p((110, 170, 230), (30, 100, 180)));
        t.code_bg = t.rgb(p((34, 38, 50), (236, 239, 244)));
        t.quote = t.rgb(p((150, 156, 172), (95, 100, 115)));
        t.link = t.rgb(p((110, 170, 230), (30, 100, 180)));
        t.hunk_bg = t.rgb(p((30, 40, 62), (228, 236, 250)));
        t.add_bg = t.rgb(p((22, 42, 31), (230, 246, 233)));
        t.del_bg = t.rgb(p((54, 28, 33), (253, 234, 234)));
        t.add_bg2 = t.rgb(p((38, 104, 62), (150, 214, 168)));
        t.del_bg2 = t.rgb(p((132, 46, 56), (243, 160, 160)));
        t.sel_bg = t.rgb(p((52, 58, 80), (214, 224, 244)));
        t
    }

    /// 24-bit when the terminal advertises it, else the nearest xterm-256 color.
    pub fn rgb(&self, (r, g, b): Rgb) -> Color {
        if self.truecolor {
            Color::Rgb(r, g, b)
        } else {
            Color::Indexed(to_256(r, g, b))
        }
    }

    /// Owned converter for background-thread-free users (the syntax highlighter cache).
    pub fn rgb_fn(&self) -> Box<dyn Fn(Rgb) -> Color> {
        let tc = self.truecolor;
        Box::new(move |(r, g, b)| {
            if tc {
                Color::Rgb(r, g, b)
            } else {
                Color::Indexed(to_256(r, g, b))
            }
        })
    }

    pub fn syntect_theme(&self) -> &'static str {
        if self.light {
            "InspiredGitHub"
        } else {
            "base16-ocean.dark"
        }
    }
}

const ASCII: Set<'static> = Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

fn to_256(r: u8, g: u8, b: u8) -> u8 {
    let (ri, gi, bi) = (r as i32, g as i32, b as i32);
    if (ri - gi).abs() < 10 && (gi - bi).abs() < 10 {
        let avg = (ri + gi + bi) / 3;
        return match avg {
            0..=7 => 16,
            239.. => 231,
            _ => (232 + (avg - 8) * 24 / 232) as u8,
        };
    }
    let c = |v: i32| ((v * 5 + 127) / 255) as u8;
    16 + 36 * c(ri) + 6 * c(gi) + c(bi)
}

fn env_has(var: &str, needles: &[&str]) -> bool {
    std::env::var(var).is_ok_and(|v| {
        let v = v.to_lowercase();
        needles.iter().any(|n| v.contains(n))
    })
}

pub fn truecolor() -> bool {
    env_has("COLORTERM", &["truecolor", "24bit"])
}

/// Locale check, as LC_ALL > LC_CTYPE > LANG precedence says.
pub fn utf8() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
        .is_some_and(|s| {
            let s = s.to_lowercase();
            s.contains("utf-8") || s.contains("utf8")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_fallback() {
        assert_eq!(to_256(255, 0, 0), 196);
        assert_eq!(to_256(0, 0, 0), 16);
        assert!(matches!(
            Theme::new(false, IconSet::Unicode, false).accent,
            Color::Indexed(_)
        ));
        assert!(matches!(
            Theme::new(true, IconSet::Unicode, true).accent,
            Color::Rgb(..)
        ));
    }
}
