//! Multi-field popups for creating issues and pull requests. The form only collects text; `build`
//! turns it into the exact `gh` argv (validated), which then goes through the usual confirm popup.
use crate::dispatch::{InputDef, InputKind};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Bodies above this go through `--body-file -` on stdin instead of argv.
const ARGV_BODY_MAX: usize = 60_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Line,
    Text,
    Toggle,
    Pick,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub key: &'static str,
    pub label: String,
    pub kind: Kind,
    pub text: String,
    pub on: bool,
    pub options: Vec<String>,
    pub idx: usize,
    /// Dispatch inputs: the input's name and its description.
    pub name: String,
    pub hint: String,
}

impl Field {
    fn new(key: &'static str, label: &str, kind: Kind) -> Self {
        Field {
            key,
            label: label.into(),
            kind,
            text: String::new(),
            on: false,
            options: vec![],
            idx: 0,
            name: String::new(),
            hint: String::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Spec {
    Issue,
    /// `head` is the branch the PR comes from (raw name).
    Pr {
        head: String,
    },
    /// Run a workflow (`id` is its numeric id) with `workflow_dispatch` inputs.
    Dispatch {
        id: String,
        inputs: Vec<InputDef>,
        free_form: bool,
        blocked: Option<String>,
    },
}

#[derive(Debug)]
pub enum Out {
    None,
    Cancel,
    Submit,
    /// A Pick field changed (so the owner can react, e.g. prefill the body from a template).
    Picked(&'static str),
}

/// What the form produced: the command and an optional stdin payload (large bodies).
#[derive(Debug, PartialEq)]
pub struct Built {
    pub argv: Vec<String>,
    pub stdin: Option<String>,
}

pub struct Form {
    pub title: String,
    pub repo: String,
    pub spec: Spec,
    pub fields: Vec<Field>,
    pub focus: usize,
    /// Validation error from the last submit.
    pub error: Option<String>,
    /// Non-blocking remark shown under the title (e.g. "loading labels...").
    pub note: Option<String>,
    /// Label names of the repo once loaded; unknown labels are rejected only after that.
    pub labels: Option<Vec<String>>,
    /// Issue templates (name, body) once loaded.
    pub templates: Vec<(String, String)>,
    /// `Some(false)`: the PR head branch is not on the remote.
    pub head_pushed: Option<bool>,
    prefilled: Option<String>,
    /// The user typed or pasted something (Esc then asks before discarding).
    typed: bool,
    /// Waiting for y/n on "discard this form?".
    pub discarding: bool,
}

type Parsed = Result<Option<Vec<InputDef>>, String>;

/// Repos with this many labels or more skip label validation (the list may be truncated); gh decides.
const LABELS_MAX: usize = 300;

/// Control characters out; newlines survive only where `multiline`.
fn paste_clean(s: &str, multiline: bool) -> String {
    s.replace("\r\n", "\n")
        .chars()
        .filter(|c| (*c == '\n' && multiline) || !c.is_control())
        .collect()
}

impl Form {
    pub fn issue(repo: &str) -> Form {
        let mut f = Form::base(repo, "New issue", Spec::Issue);
        f.fields = vec![
            Field::new("title", "Title", Kind::Line),
            Field::new("body", "Body", Kind::Text),
            Field::new("labels", "Labels (comma separated)", Kind::Line),
            Field::new(
                "assignees",
                "Assignees (comma separated, @me ok)",
                Kind::Line,
            ),
        ];
        f
    }

    pub fn pr(repo: &str, head: &str) -> Form {
        let mut f = Form::base(
            repo,
            &format!("New pull request from {head}"),
            Spec::Pr { head: head.into() },
        );
        let mut base = Field::new("base", "Base branch", Kind::Pick);
        base.options = vec!["(loading...)".into()];
        f.fields = vec![
            Field::new("title", "Title", Kind::Line),
            Field::new("body", "Body", Kind::Text),
            Field::new(
                "fill",
                "Take title and body from the commits (--fill)",
                Kind::Toggle,
            ),
            base,
            Field::new("draft", "Open as draft", Kind::Toggle),
            Field::new("reviewers", "Reviewers (comma separated)", Kind::Line),
            Field::new("labels", "Labels (comma separated)", Kind::Line),
            Field::new(
                "assignees",
                "Assignees (comma separated, @me ok)",
                Kind::Line,
            ),
        ];
        f
    }

    pub fn dispatch(repo: &str, id: &str, name: &str) -> Form {
        let spec = Spec::Dispatch {
            id: id.into(),
            inputs: vec![],
            free_form: false,
            blocked: None,
        };
        let mut f = Form::base(repo, &format!("Run workflow: {name}"), spec);
        let mut r = Field::new("ref", "Branch or tag to run on", Kind::Pick);
        r.options = vec!["(loading...)".into()];
        f.fields = vec![r];
        f.note = Some("reading the workflow file...".into());
        f
    }

    /// Branches and tags for the ref picker (default first) and the parsed workflow inputs (None: not fetched).
    pub fn set_dispatch(
        &mut self,
        default_branch: &str,
        branches: Vec<String>,
        tags: Vec<String>,
        parsed: Option<Parsed>,
    ) {
        let mut opts = vec![default_branch.to_string()];
        opts.extend(
            branches
                .into_iter()
                .chain(tags)
                .filter(|b| b != default_branch),
        );
        if let Some(f) = self.field_mut("ref") {
            (f.options, f.idx) = (opts, 0);
        }
        self.fields.retain(|f| f.key != "input" && f.key != "extra");
        let (mut defs, mut free, mut blocked) = (vec![], false, None);
        match parsed {
            Some(Ok(Some(d))) => defs = d,
            Some(Ok(None)) => {
                blocked = Some(
                    "This workflow has no workflow_dispatch trigger, so it cannot be run manually."
                        .to_string(),
                )
            }
            Some(Err(_)) | None => free = true,
        }
        for d in &defs {
            let kind = match d.kind {
                InputKind::Boolean => Kind::Toggle,
                InputKind::Choice if !d.options.is_empty() => Kind::Pick,
                _ => Kind::Line,
            };
            let mut f = Field::new("input", &d.name, kind);
            f.name = d.name.clone();
            f.hint = format!(
                "{}{}",
                if d.required { "(required) " } else { "" },
                d.description
            );
            match d.kind {
                InputKind::Boolean => f.on = d.default == "true",
                InputKind::Choice if !d.options.is_empty() => {
                    f.options = d.options.clone();
                    f.idx = d.options.iter().position(|o| *o == d.default).unwrap_or(0);
                }
                _ => f.text = d.default.clone(),
            }
            self.fields.push(f);
        }
        if free {
            self.fields.push(Field::new(
                "extra",
                "Inputs as key=value, comma separated (workflow file could not be read)",
                Kind::Line,
            ));
        }
        self.note = blocked.clone().or_else(|| {
            Some(if free {
                "could not read the workflow inputs; enter them by hand".to_string()
            } else {
                format!("inputs are read from the default branch ({default_branch})")
            })
        });
        if let Spec::Dispatch {
            inputs,
            free_form,
            blocked: b,
            ..
        } = &mut self.spec
        {
            (*inputs, *free_form, *b) = (defs, free, blocked);
        }
    }

    fn base(repo: &str, title: &str, spec: Spec) -> Form {
        Form {
            title: title.into(),
            repo: repo.into(),
            spec,
            fields: vec![],
            focus: 0,
            error: None,
            note: Some("loading labels and templates...".into()),
            labels: None,
            templates: vec![],
            head_pushed: None,
            prefilled: None,
            typed: false,
            discarding: false,
        }
    }

    fn field(&self, key: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.key == key)
    }

    fn field_mut(&mut self, key: &str) -> Option<&mut Field> {
        self.fields.iter_mut().find(|f| f.key == key)
    }

    pub fn text(&self, key: &str) -> &str {
        self.field(key).map_or("", |f| f.text.as_str())
    }

    pub fn on(&self, key: &str) -> bool {
        self.field(key).is_some_and(|f| f.on)
    }

    pub fn picked(&self, key: &str) -> &str {
        self.field(key)
            .and_then(|f| f.options.get(f.idx))
            .map_or("", String::as_str)
    }

    /// Loaded in the background; fills in what the form can validate or offer.
    pub fn set_labels(&mut self, labels: Vec<String>) {
        self.labels = (labels.len() < LABELS_MAX).then_some(labels);
    }

    /// Adds a "Template" picker (first field) when the repo has .md issue templates.
    pub fn set_templates(&mut self, templates: Vec<(String, String)>) {
        if templates.is_empty() || self.field("template").is_some() {
            return;
        }
        let mut f = Field::new("template", "Template (left/right)", Kind::Pick);
        f.options = std::iter::once("(none)".to_string())
            .chain(templates.iter().map(|t| t.0.clone()))
            .collect();
        self.fields.insert(0, f);
        self.focus += 1;
        self.templates = templates;
    }

    /// PR extras: base branch choices (default first), whether the head is pushed, a body template.
    pub fn set_pr_data(
        &mut self,
        default_branch: &str,
        branches: Vec<String>,
        pushed: Option<bool>,
        template: Option<String>,
    ) {
        let mut opts: Vec<String> = vec![default_branch.to_string()];
        opts.extend(branches.into_iter().filter(|b| b != default_branch));
        if let Some(f) = self.field_mut("base") {
            (f.options, f.idx) = (opts, 0);
        }
        self.head_pushed = pushed;
        if pushed == Some(false)
            && let Spec::Pr { head } = &self.spec
        {
            self.note = Some(format!(
                "'{head}' was not found on {}: push it first (git push -u origin {head}), unless this is a fork. Nothing is pushed automatically.",
                self.repo
            ));
        }
        if let Some(t) = template
            && self.text("body").is_empty()
            && let Some(f) = self.field_mut("body")
        {
            f.text = t.clone();
            self.prefilled = Some(t);
        }
    }

    /// After the template picker moved: replace the body, unless the user has typed their own.
    pub fn apply_template(&mut self) {
        let idx = self.field("template").map_or(0, |f| f.idx);
        let new = if idx == 0 {
            String::new()
        } else {
            self.templates
                .get(idx - 1)
                .map(|t| t.1.clone())
                .unwrap_or_default()
        };
        let body = self.text("body").to_string();
        if (body.is_empty() || self.prefilled.as_deref() == Some(body.as_str()))
            && let Some(f) = self.field_mut("body")
        {
            f.text = new.clone();
            self.prefilled = Some(new);
        }
    }

    pub fn key(&mut self, k: KeyEvent) -> Out {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        self.error = None;
        if self.discarding {
            self.discarding = false;
            return if matches!(k.code, KeyCode::Char('y' | 'Y')) {
                Out::Cancel
            } else {
                Out::None
            };
        }
        let last = self.fields.len().saturating_sub(1);
        match k.code {
            KeyCode::Esc if self.typed => {
                self.discarding = true;
                return Out::None;
            }
            KeyCode::Esc => return Out::Cancel,
            KeyCode::Char('s') if ctrl => return Out::Submit,
            KeyCode::Tab => self.focus = (self.focus + 1).min(last),
            KeyCode::BackTab => self.focus = self.focus.saturating_sub(1),
            _ => {}
        }
        let Some(f) = self.fields.get_mut(self.focus) else {
            return Out::None;
        };
        match (f.kind, k.code) {
            (Kind::Line | Kind::Text, KeyCode::Backspace) => {
                f.text.pop();
            }
            (Kind::Line | Kind::Text, KeyCode::Char(c)) if !ctrl => {
                f.text.push(c);
                self.typed = true;
            }
            (Kind::Text, KeyCode::Enter) => {
                f.text.push('\n');
                self.typed = true;
            }
            (Kind::Line, KeyCode::Enter | KeyCode::Down) => self.focus = (self.focus + 1).min(last),
            (Kind::Line | Kind::Toggle | Kind::Pick, KeyCode::Up) => {
                self.focus = self.focus.saturating_sub(1)
            }
            (Kind::Toggle, KeyCode::Down) => self.focus = (self.focus + 1).min(last),
            (Kind::Toggle, KeyCode::Char(' ') | KeyCode::Enter) => f.on = !f.on,
            (Kind::Pick, KeyCode::Down) => self.focus = (self.focus + 1).min(last),
            (Kind::Pick, KeyCode::Right | KeyCode::Char(' ') | KeyCode::Enter)
                if !f.options.is_empty() =>
            {
                f.idx = (f.idx + 1) % f.options.len();
                return Out::Picked(f.key);
            }
            (Kind::Pick, KeyCode::Left) if !f.options.is_empty() => {
                f.idx = (f.idx + f.options.len() - 1) % f.options.len();
                return Out::Picked(f.key);
            }
            _ => {}
        }
        Out::None
    }

    /// Bracketed paste: literal text into the focused text field.
    pub fn paste(&mut self, s: &str) {
        if let Some(f) = self.fields.get_mut(self.focus)
            && matches!(f.kind, Kind::Line | Kind::Text)
        {
            f.text.push_str(&paste_clean(s, f.kind == Kind::Text));
            self.typed = true;
            self.error = None;
            self.discarding = false;
        }
    }

    /// Validate and build the command; the Err text is shown inside the form.
    pub fn build(&self) -> Result<Built, String> {
        if let Spec::Dispatch {
            id,
            inputs,
            free_form,
            blocked,
        } = &self.spec
        {
            return self.build_dispatch(id, inputs, *free_form, blocked.as_deref());
        }
        let labels = self.resolve_labels()?;
        let list = |key: &str| split_list(self.text(key));
        let (title, body) = (self.text("title").trim(), self.text("body"));
        let mut argv: Vec<String> = [
            "gh",
            match self.spec {
                Spec::Issue => "issue",
                Spec::Pr { .. } => "pr",
                Spec::Dispatch { .. } => unreachable!("built above"),
            },
            "create",
            "-R",
            &self.repo,
        ]
        .map(String::from)
        .to_vec();
        let mut stdin = None;
        let fill = self.on("fill");
        match &self.spec {
            Spec::Issue => {
                if title.is_empty() {
                    return Err("Title is required".into());
                }
            }
            Spec::Dispatch { .. } => unreachable!(),
            Spec::Pr { head } => {
                let base = self.picked("base");
                if base.is_empty() || base.starts_with('(') {
                    return Err("Base branch is still loading".into());
                }
                if base == head {
                    return Err(format!(
                        "Head and base are both '{head}'; pick another base"
                    ));
                }
                if !fill && title.is_empty() {
                    return Err("Title is required (or switch on --fill)".into());
                }
                argv.extend([
                    "--head".to_string(),
                    head.clone(),
                    "--base".into(),
                    base.into(),
                ]);
            }
        }
        if !fill {
            argv.extend(["--title".to_string(), title.to_string()]);
            if body.len() > ARGV_BODY_MAX {
                argv.extend(["--body-file".to_string(), "-".into()]);
                stdin = Some(body.to_string());
            } else {
                argv.extend(["--body".to_string(), body.to_string()]);
            }
        } else {
            argv.push("--fill".into());
        }
        if self.on("draft") {
            argv.push("--draft".into());
        }
        for (flag, values) in [
            ("--reviewer", list("reviewers")),
            ("--label", labels),
            ("--assignee", list("assignees")),
        ] {
            for v in values {
                argv.extend([flag.to_string(), v]);
            }
        }
        Ok(Built { argv, stdin })
    }

    fn build_dispatch(
        &self,
        id: &str,
        inputs: &[InputDef],
        free_form: bool,
        blocked: Option<&str>,
    ) -> Result<Built, String> {
        if let Some(b) = blocked {
            return Err(b.into());
        }
        let r = self.picked("ref");
        if r.is_empty() || r.starts_with('(') {
            return Err("Branch list is still loading".into());
        }
        let mut argv: Vec<String> = ["gh", "workflow", "run", id, "-R", &self.repo, "--ref", r]
            .map(String::from)
            .to_vec();
        let mut fields = self.fields.iter().filter(|f| f.key == "input");
        for d in inputs {
            let Some(f) = fields.next() else { break };
            let value = match d.kind {
                InputKind::Boolean => f.on.to_string(),
                InputKind::Choice if f.kind == Kind::Pick => {
                    f.options.get(f.idx).cloned().unwrap_or_default()
                }
                _ => f.text.clone(),
            };
            if value.is_empty() {
                if d.required {
                    return Err(format!("Input '{}' is required", d.name));
                }
                continue;
            }
            if d.kind == InputKind::Number && !value.trim().parse::<f64>().is_ok_and(f64::is_finite)
            {
                return Err(format!(
                    "Input '{}' must be a number, got '{value}'",
                    d.name
                ));
            }
            argv.extend(["-f".to_string(), format!("{}={value}", d.name)]);
        }
        if free_form {
            for pair in split_list(self.text("extra")) {
                if !pair.contains('=') || pair.starts_with('=') {
                    return Err(format!("'{pair}' is not key=value"));
                }
                argv.extend(["-f".to_string(), pair]);
            }
        }
        Ok(Built { argv, stdin: None })
    }

    /// Typed labels matched against the repo's (case-insensitively, to the canonical spelling).
    fn resolve_labels(&self) -> Result<Vec<String>, String> {
        let typed = split_list(self.text("labels"));
        let Some(known) = &self.labels else {
            return Ok(typed);
        };
        typed
            .into_iter()
            .map(|t| {
                known
                    .iter()
                    .find(|k| k.eq_ignore_ascii_case(&t))
                    .cloned()
                    .ok_or_else(|| {
                        let some: Vec<_> = known.iter().take(8).cloned().collect();
                        format!(
                            "Unknown label '{t}' (repo has: {}{})",
                            some.join(", "),
                            if known.len() > 8 { ", ..." } else { "" }
                        )
                    })
            })
            .collect()
    }
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(f: &mut Form, c: KeyCode) -> Out {
        f.key(KeyEvent::new(c, KeyModifiers::NONE))
    }

    fn type_in(f: &mut Form, s: &str) {
        for c in s.chars() {
            press(f, KeyCode::Char(c));
        }
    }

    #[test]
    fn issue_form_builds_the_exact_command() {
        let mut f = Form::issue("o/r");
        assert!(f.build().unwrap_err().contains("Title"));
        type_in(&mut f, "Crash on start");
        press(&mut f, KeyCode::Enter); // single line: Enter moves on
        type_in(&mut f, "line one");
        press(&mut f, KeyCode::Enter); // multi-line body: Enter is a newline
        type_in(&mut f, "line two");
        press(&mut f, KeyCode::Tab);
        type_in(&mut f, "bug, help wanted");
        press(&mut f, KeyCode::Tab);
        type_in(&mut f, "@me");
        let b = f.build().unwrap();
        assert_eq!(
            b.argv,
            [
                "gh",
                "issue",
                "create",
                "-R",
                "o/r",
                "--title",
                "Crash on start",
                "--body",
                "line one\nline two",
                "--label",
                "bug",
                "--label",
                "help wanted",
                "--assignee",
                "@me"
            ]
        );
        assert_eq!(b.stdin, None);
    }

    #[test]
    fn unknown_labels_are_rejected_once_the_repo_labels_are_known() {
        let mut f = Form::issue("o/r");
        type_in(&mut f, "T");
        press(&mut f, KeyCode::Tab);
        press(&mut f, KeyCode::Tab);
        type_in(&mut f, "BUG, nope");
        assert!(
            f.build().is_ok(),
            "nothing to validate against until the labels have loaded"
        );
        f.set_labels(vec!["bug".into(), "docs".into()]);
        let e = f.build().unwrap_err();
        assert!(
            e.contains("Unknown label 'nope'") && e.contains("bug, docs"),
            "{e}"
        );
        f.fields[2].text = "BUG".into();
        assert!(
            f.build()
                .unwrap()
                .argv
                .windows(2)
                .any(|w| w == ["--label", "bug"]),
            "canonical spelling is used"
        );
    }

    #[test]
    fn templates_prefill_the_body_unless_the_user_wrote_their_own() {
        let mut f = Form::issue("o/r");
        f.set_templates(vec![
            ("Bug".into(), "## Steps\n".into()),
            ("Idea".into(), "## Why\n".into()),
        ]);
        assert_eq!(f.fields[0].key, "template");
        assert_eq!(f.focus, 1, "focus stays on Title");
        f.focus = 0;
        assert!(matches!(
            press(&mut f, KeyCode::Right),
            Out::Picked("template")
        ));
        f.apply_template();
        assert_eq!(f.text("body"), "## Steps\n");
        press(&mut f, KeyCode::Right);
        f.apply_template();
        assert_eq!(
            f.text("body"),
            "## Why\n",
            "a pure prefill is replaced by the next template"
        );
        f.fields[2].text.push_str("my own words");
        press(&mut f, KeyCode::Left);
        f.apply_template();
        assert!(
            f.text("body").ends_with("my own words"),
            "edited text is never overwritten"
        );
    }

    #[test]
    fn pr_form_validates_head_base_and_builds_the_command() {
        let mut f = Form::pr("o/r", "feat/x");
        assert!(f.build().unwrap_err().contains("loading"));
        f.set_pr_data(
            "main",
            vec!["dev".into(), "main".into()],
            Some(true),
            Some("## Summary\n".into()),
        );
        assert_eq!(
            f.fields[3].options,
            ["main", "dev"],
            "default branch first, no duplicate"
        );
        assert_eq!(
            f.text("body"),
            "## Summary\n",
            "pull_request_template.md prefill"
        );
        assert!(f.build().unwrap_err().contains("Title"));
        type_in(&mut f, "Add x");
        f.focus = 4; // draft
        press(&mut f, KeyCode::Char(' '));
        f.fields[5].text = "alice, bob".into();
        let b = f.build().unwrap();
        assert_eq!(
            b.argv,
            [
                "gh",
                "pr",
                "create",
                "-R",
                "o/r",
                "--head",
                "feat/x",
                "--base",
                "main",
                "--title",
                "Add x",
                "--body",
                "## Summary\n",
                "--draft",
                "--reviewer",
                "alice",
                "--reviewer",
                "bob"
            ]
        );
        f.fields[2].on = true; // --fill ignores title/body
        let b = f.build().unwrap();
        assert!(b.argv.contains(&"--fill".to_string()) && !b.argv.contains(&"--title".to_string()));
        f.fields[3].idx = 1; // base = dev ... then head == base
        f.fields[3].options = vec!["feat/x".into()];
        f.fields[3].idx = 0;
        assert!(f.build().unwrap_err().contains("both 'feat/x'"));
    }

    #[test]
    fn unpushed_head_is_reported_not_pushed() {
        let mut f = Form::pr("o/r", "local-only");
        f.set_pr_data("main", vec![], Some(false), None);
        type_in(&mut f, "T");
        let n = f.note.clone().unwrap();
        assert!(
            n.contains("not found on o/r")
                && n.contains("git push -u origin local-only")
                && n.contains("fork")
                && n.contains("Nothing is pushed"),
            "{n}"
        );
        assert!(f.build().is_ok(), "a warning, not a block (forks, unknown)");
    }

    fn yaml(s: &str) -> Option<Parsed> {
        Some(crate::dispatch::parse(s))
    }

    #[test]
    fn paste_is_literal_and_newlines_stay_in_multiline_fields() {
        let mut f = Form::issue("o/r");
        f.paste("a\r\nb\x1b[31m\tc");
        assert_eq!(
            f.text("title"),
            "ab[31mc",
            "no newline or control chars in a line field"
        );
        press(&mut f, KeyCode::Tab);
        f.paste("l1\r\nl2\x07");
        assert_eq!(f.text("body"), "l1\nl2");
        press(&mut f, KeyCode::Esc);
        assert!(f.discarding, "pasted text counts as content");
    }

    #[test]
    fn esc_in_a_form_with_content_asks_before_discarding() {
        let mut f = Form::issue("o/r");
        assert!(
            matches!(press(&mut f, KeyCode::Esc), Out::Cancel),
            "empty form closes at once"
        );
        let mut f = Form::issue("o/r");
        type_in(&mut f, "x");
        assert!(matches!(press(&mut f, KeyCode::Esc), Out::None) && f.discarding);
        assert!(matches!(press(&mut f, KeyCode::Char('n')), Out::None) && !f.discarding);
        assert_eq!(f.text("title"), "x", "n keeps the form and its text");
        press(&mut f, KeyCode::Esc);
        assert!(matches!(press(&mut f, KeyCode::Char('y')), Out::Cancel));
    }

    #[test]
    fn big_label_lists_skip_validation_and_names_stay_raw() {
        let mut f = Form::issue("o/r");
        f.set_labels((0..LABELS_MAX).map(|i| format!("l{i}")).collect());
        f.fields[0].text = "T".into();
        f.fields[2].text = "whatever".into();
        assert!(
            f.build().is_ok(),
            "gh decides when the list may be truncated"
        );
        let mut f = Form::issue("o/r");
        f.set_labels(vec!["bug\u{202e}x".into()]);
        f.fields[0].text = "T".into();
        f.fields[2].text = "BUG\u{202e}X".into();
        let a = f.build().unwrap().argv;
        assert_eq!(
            a.last().unwrap(),
            "bug\u{202e}x",
            "the raw name reaches argv"
        );
    }

    #[test]
    fn huge_bodies_go_through_stdin_and_text_stays_raw() {
        let mut f = Form::issue("o/r");
        f.fields[0].text = "T\\u{202e}x".into();
        f.fields[1].text = "a".repeat(ARGV_BODY_MAX + 1);
        let b = f.build().unwrap();
        assert!(
            b.argv.windows(2).any(|w| w == ["--body-file", "-"])
                && !b.argv.contains(&"--body".to_string())
        );
        assert_eq!(b.stdin.as_ref().map(String::len), Some(ARGV_BODY_MAX + 1));
        assert!(
            b.argv.contains(&"T\\u{202e}x".to_string()),
            "typed text reaches gh untouched; the popup shows it neutralized"
        );
    }

    #[test]
    fn navigation_toggles_and_cancel() {
        let mut f = Form::pr("o/r", "h");
        f.set_pr_data("main", vec![], Some(true), None);
        press(&mut f, KeyCode::Tab);
        press(&mut f, KeyCode::Tab);
        assert_eq!(f.focus, 2);
        press(&mut f, KeyCode::Char(' '));
        assert!(f.on("fill"));
        press(&mut f, KeyCode::Down);
        assert!(matches!(press(&mut f, KeyCode::Right), Out::Picked("base")));
        press(&mut f, KeyCode::BackTab);
        assert_eq!(f.focus, 2);
        assert!(matches!(press(&mut f, KeyCode::Esc), Out::Cancel));
        assert!(matches!(
            f.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            Out::Submit
        ));
    }

    #[test]
    fn dispatch_form_is_generated_from_the_workflow_yaml() {
        let mut f = Form::dispatch("o/r", "4242", "Deploy");
        assert!(f.build().unwrap_err().contains("loading"));
        f.set_dispatch(
            "main",
            vec!["dev".into(), "main".into()],
            vec!["v1".into()],
            yaml(include_str!("../tests/workflow_inputs.yml")),
        );
        let keys: Vec<_> = f
            .fields
            .iter()
            .map(|x| (x.key, x.name.as_str(), x.kind))
            .collect();
        assert_eq!(keys[0], ("ref", "", Kind::Pick));
        assert_eq!(f.fields.len(), 1 + 6);
        assert!(f.fields[1].hint.starts_with("(required) What to say"));
        assert_eq!(f.fields[2].kind, Kind::Toggle);
        assert!(f.fields[2].on, "boolean default honoured");
        assert_eq!(f.fields[3].options, ["info", "warn", "error"]);
        assert_eq!(f.fields[3].idx, 1, "choice default honoured");
        assert_eq!(f.fields[4].text, "3", "number default honoured");
        assert!(f.build().unwrap_err().contains("'message' is required"));
        f.fields[1].text = "hello there".into();
        f.fields[2].on = false;
        f.fields[3].idx = 2;
        f.fields[4].text = "7".into();
        let b = f.build().unwrap();
        assert_eq!(
            b.argv,
            [
                "gh",
                "workflow",
                "run",
                "4242",
                "-R",
                "o/r",
                "--ref",
                "main",
                "-f",
                "message=hello there",
                "-f",
                "verbose=false",
                "-f",
                "level=error",
                "-f",
                "count=7"
            ],
            "optional empty inputs (target, plain) are not sent"
        );
        f.fields[4].text = "seven".into();
        assert!(f.build().unwrap_err().contains("must be a number"));
        for bad in ["NaN", "inf", "-infinity"] {
            f.fields[4].text = bad.into();
            assert!(f.build().unwrap_err().contains("must be a number"), "{bad}");
        }
        f.fields[1].text = "  padded  ".into();
        f.fields[4].text = "7".into();
        assert!(
            f.build()
                .unwrap()
                .argv
                .contains(&"message=  padded  ".to_string()),
            "values are not trimmed"
        );
        assert!(
            f.fields[0].options.contains(&"v1".to_string()),
            "tags are offered"
        );
        assert!(f.note.as_deref().unwrap().contains("default branch (main)"));
    }

    #[test]
    fn dispatch_form_blocks_workflows_without_the_trigger_and_falls_back_for_bad_yaml() {
        let mut f = Form::dispatch("o/r", "1", "CI");
        f.set_dispatch("main", vec![], vec![], yaml("on:\n  push:\n"));
        assert!(
            f.build()
                .unwrap_err()
                .contains("no workflow_dispatch trigger")
        );
        let mut f = Form::dispatch("o/r", "1", "CI");
        f.set_dispatch("main", vec![], vec![], yaml("on: [unclosed\n  - x: : :\n"));
        assert!(
            f.fields.iter().any(|x| x.key == "extra"),
            "free-form entry replaces the generated inputs"
        );
        f.fields.last_mut().unwrap().text = "a=1, b=two words".into();
        assert_eq!(
            &f.build().unwrap().argv[8..],
            ["-f", "a=1", "-f", "b=two words"]
        );
        f.fields.last_mut().unwrap().text = "nokey".into();
        assert!(f.build().unwrap_err().contains("not key=value"));
        let mut f = Form::dispatch("o/r", "1", "CI");
        f.set_dispatch("main", vec![], vec![], None); // workflow file could not be fetched
        assert!(f.fields.iter().any(|x| x.key == "extra"));
    }

    #[test]
    fn required_choice_without_options_is_free_text() {
        let mut f = Form::dispatch("o/r", "1", "CI");
        let y = "on:\n  workflow_dispatch:\n    inputs:\n      env:\n        required: true\n        type: choice\n";
        f.set_dispatch("main", vec![], vec![], yaml(y));
        assert_eq!(f.fields[1].kind, Kind::Line);
        assert!(f.build().unwrap_err().contains("'env' is required"));
        f.fields[1].text = "prod".into();
        assert!(f.build().unwrap().argv.contains(&"env=prod".to_string()));
    }
}
