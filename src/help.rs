//! The `?` keybindings menu: structured rows (section, key, description) so the popup can show a
//! key column, move a cursor over them and filter them with `/`, like lazygit's keybindings menu.
use crate::app::App;
use crate::config::Act;

pub struct Entry {
    pub section: &'static str,
    pub key: String,
    pub desc: String,
}

/// Every row, in display order. Remappable actions show whatever keys the active keymap gives them.
pub fn entries(app: &App) -> Vec<Entry> {
    let k = |a: Act| app.keys.labels(a);
    let n = app.panels.len();
    let nums = if n > 1 {
        format!("1-{}", n.min(7))
    } else {
        "1".into()
    };
    let mut v: Vec<Entry> = vec![];
    let mut add = |section: &'static str, key: String, desc: &str| {
        v.push(Entry {
            section,
            key,
            desc: desc.to_string(),
        });
    };
    let s = |x: &str| x.to_string();

    add(
        "Navigation",
        format!("{nums} / Tab / S-Tab"),
        "focus a panel (digits stop at 7, Tab reaches the rest)",
    );
    add(
        "Navigation",
        s("1-7 (again)"),
        "next list tab of the focused panel",
    );
    add("Navigation", s("{ }"), "previous / next list tab");
    add("Navigation", s("j / k / arrows"), "move");
    add("Navigation", s("Ctrl-d / Ctrl-u"), "half page down / up");
    add(
        "Navigation",
        s("g / G / Home / End"),
        "top / bottom (g groups instead in a global list; use Home)",
    );
    add(
        "Navigation",
        k(Act::Filter),
        "filter the list (Enter applies, Esc clears)",
    );
    add("Navigation", s("l / Right"), "focus the detail pane");
    add("Navigation", s("h / Esc / Left"), "back to the list");
    add(
        "Navigation",
        k(Act::Global),
        "switch between the repo and the global home",
    );
    add(
        "Navigation",
        k(Act::Browser),
        "repo browser: all your repos",
    );
    add(
        "Navigation",
        k(Act::Inbox),
        "inbox: unread notifications of all repos",
    );
    add(
        "Navigation",
        s("Mouse"),
        "click a panel, row or tab; the wheel scrolls",
    );

    add(
        "Global home",
        k(Act::Scope),
        "scope: all / favorites / an org / one repo",
    );
    add(
        "Global home",
        k(Act::SwitchRepoContext),
        "open the selected item's repo (G returns)",
    );
    add(
        "Global home",
        s("H"),
        "hide the selected row's repo (unhide in the Repos panel / browser)",
    );
    add("Global home", s("Enter (Repos panel)"), "open the repo");
    add(
        "Global home",
        format!("{} (Repos panel)", k(Act::Scope)),
        "scope the home to the repo",
    );
    add(
        "Global home",
        format!("{} (Repos panel)", k(Act::Zoom)),
        "favorite the repo",
    );
    add(
        "Global home",
        s("g"),
        "group by author / repo / none (longest wait first)",
    );
    add(
        "Global home",
        s("W"),
        "PR window: 24h / 7d / 30d / all (sections search again)",
    );
    add(
        "Global home",
        s("[[sections]]"),
        "your own searches as panels (see docs/configuration.md)",
    );

    add(
        "Repo panel",
        s("Enter / l"),
        "details (a tag: commit, date, release)",
    );
    add(
        "Repo panel",
        format!("{} (tag)", k(Act::CopyUrl)),
        "copy the tag name",
    );
    add("Repo panel", k(Act::Actions), "branch / release actions");

    add(
        "Pull requests",
        s("Enter"),
        "drill in: Files / Commits / Checks / Comments (Esc returns)",
    );
    add(
        "Pull requests",
        s("[ ]"),
        "detail tab; in a drill-in: next / previous panel",
    );
    add(
        "Pull requests",
        s("j / k (Files)"),
        "change the file shown in the diff",
    );
    add(
        "Pull requests",
        s("j / k (Commits)"),
        "pick a commit; the right pane shows its diff",
    );

    add("Diff", s("j / k"), "line down / up");
    add("Diff", s("Ctrl-d / Ctrl-u"), "half page");
    add("Diff", s("g / G"), "first / last row");
    add("Diff", s("n / p"), "next / previous file");
    add("Diff", s("t"), "unified / split / auto");
    add("Diff", s("w"), "wrap / clip");
    add("Diff", s("v"), "mark the file viewed");
    add("Diff", k(Act::Zoom), "zoom");

    add("Comments", s("j / k"), "move by comment");
    add(
        "Comments",
        s("Enter"),
        "expand / collapse (long comments, <details>)",
    );
    add(
        "Comments",
        s("e"),
        "show who reacted (asks GitHub once per comment)",
    );

    add(
        "Actions (always confirm, showing the exact command)",
        k(Act::Actions),
        "action menu for the selected item",
    );
    add(
        "Actions (always confirm, showing the exact command)",
        k(Act::Approve),
        "approve",
    );
    add(
        "Actions (always confirm, showing the exact command)",
        k(Act::Comment),
        "comment",
    );
    add(
        "Actions (always confirm, showing the exact command)",
        k(Act::Merge),
        "merge",
    );
    add(
        "Actions (always confirm, showing the exact command)",
        s("n"),
        "new issue (Issues) / new PR from the cwd branch (PRs)",
    );
    add(
        "Actions (always confirm, showing the exact command)",
        s("d"),
        "run workflow (Actions panel)",
    );
    add(
        "Actions (always confirm, showing the exact command)",
        s("Tab / S-Tab, Space"),
        "forms: fields, toggle",
    );
    add(
        "Actions (always confirm, showing the exact command)",
        s("Left / Right, Ctrl-S, Esc"),
        "forms: pickers, review the command, cancel",
    );

    add("Anywhere", k(Act::Open), "open in browser");
    add("Anywhere", k(Act::CopyUrl), "copy URL");
    add("Anywhere", k(Act::Checkout), "checkout the selected PR");
    add(
        "Anywhere",
        k(Act::Refresh),
        "refresh the selected item and its list",
    );
    add(
        "Anywhere",
        k(Act::RefreshAll),
        "reload everything (skips the cache)",
    );
    add(
        "Anywhere",
        k(Act::EditConfig),
        "edit the config in your editor, applied when it exits",
    );
    add("Anywhere", k(Act::CommandLog), "command log");
    add("Anywhere", k(Act::Help), "this menu");
    add("Anywhere", k(Act::Quit), "quit");
    v
}

/// Rows matching every space-separated word of `filter` (case-insensitive) in section, key or
/// description. An empty filter keeps everything.
pub fn visible<'a>(all: &'a [Entry], filter: &str) -> Vec<&'a Entry> {
    let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
    all.iter()
        .filter(|e| {
            let hay = format!("{} {} {}", e.section, e.key, e.desc).to_lowercase();
            words.iter().all(|w| hay.contains(w))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{IconSet, Theme};

    #[test]
    fn every_key_the_global_home_adds_is_in_the_menu() {
        let a = App::build_start(
            Some("o/r".into()),
            true,
            Theme::new(false, IconSet::Unicode, true),
            false,
            crate::config::Config::default(),
        );
        let rows = entries(&a);
        for key in ["s", "S", "H", "g", "W", "[[sections]]"] {
            assert!(
                rows.iter()
                    .any(|e| e.section == "Global home" && e.key == key),
                "{key} is missing from the Global home section of the ? menu"
            );
        }
        let anywhere = |what: &str| {
            rows.iter()
                .any(|e| e.section == "Anywhere" && e.desc.contains(what))
        };
        assert!(anywhere("edit the config") && anywhere("command log"));
    }
}
