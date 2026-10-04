//! Issue forms (`.github/ISSUE_TEMPLATE/*.yml`) as templates. GitHub renders a form as inputs in the
//! browser and writes the answers into the issue as `### Label` sections; `gh` cannot show a form at
//! all. So a form becomes a Markdown skeleton with those same sections (task lists for checkboxes, the
//! options as a hint for a dropdown) plus its default title, labels and assignees, offered in the same
//! Template picker as `.md` templates. The file comes from the repo, i.e. from whoever can push to it:
//! its size, aliases, item counts and text lengths are bounded and everything is neutralized.
use crate::dispatch::{MAX_ALIASES, MAX_YAML, alias_count, scalar};
use crate::sanitize;
use serde_json::Value;

/// Form items read; more are ignored.
const MAX_ITEMS: usize = 50;
const MAX_OPTIONS: usize = 50;
/// Characters kept of any one text of the form.
const MAX_TEXT: usize = 2000;
/// The generated body never exceeds this (the form body has to stay editable and fit a command line).
const MAX_BODY: usize = 30_000;

/// What the Template picker offers: a body to start from and, for a form, defaults for the other fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Template {
    pub name: String,
    pub body: String,
    pub title: Option<String>,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
}

fn text(v: Option<&Value>) -> String {
    let t = v.map(scalar).unwrap_or_default();
    sanitize::clean(&t).chars().take(MAX_TEXT).collect()
}

/// One line, neutralized (a label or an option).
fn line(v: Option<&Value>) -> String {
    let t = v.map(scalar).unwrap_or_default();
    sanitize::line(t.trim()).chars().take(200).collect()
}

/// The default title, kept as written: a prefix like `[Bug]: ` ends in the space the user types after.
fn title(v: Option<&Value>) -> Option<String> {
    let t = sanitize::line(&v.map(scalar).unwrap_or_default())
        .chars()
        .take(200)
        .collect::<String>();
    (!t.trim().is_empty()).then(|| t.trim_start().to_string())
}

/// `<!-- hint -->`, which cannot be closed early by the hint itself and shows nowhere once submitted.
fn hint(h: &str) -> String {
    let h = h.replace("-->", "- ->").replace(['\n', '\r'], " ");
    let h = h.trim();
    if h.is_empty() {
        String::new()
    } else {
        format!("<!-- {h} -->")
    }
}

/// A list of names written as a YAML list or as a comma separated string.
fn names(v: Option<&Value>, max: usize) -> Vec<String> {
    let raw: Vec<String> = match v {
        Some(Value::Array(a)) => a.iter().map(scalar).collect(),
        Some(Value::String(s)) => s.split(',').map(str::to_string).collect(),
        _ => vec![],
    };
    raw.iter()
        .map(|s| sanitize::line(s.trim()).into_owned())
        .filter(|s| !s.is_empty() && s.len() <= 100)
        .take(max)
        .collect()
}

/// Is this directory entry a form (rather than the `config.yml` that configures the chooser)?
pub fn is_form_file(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    (l.ends_with(".yml") || l.ends_with(".yaml")) && !(l == "config.yml" || l == "config.yaml")
}

/// Read one issue form. `fallback` names it when the file has no `name:`. Err when it is not a usable form.
pub fn parse(yaml: &str, fallback: &str) -> Result<Template, String> {
    if yaml.len() > MAX_YAML {
        return Err(format!("form is larger than {} KB", MAX_YAML / 1024));
    }
    if alias_count(yaml) > MAX_ALIASES {
        return Err("form uses too many YAML aliases".into());
    }
    let doc: Value = serde_yaml_ng::from_str(yaml).map_err(|e| e.to_string())?;
    let items = doc
        .get("body")
        .and_then(Value::as_array)
        .ok_or("no `body:` list: not an issue form")?;
    let mut out = String::new();
    for item in items.iter().take(MAX_ITEMS) {
        let attrs = item.get("attributes");
        let label = line(attrs.and_then(|a| a.get("label")));
        let ty = item.get("type").and_then(Value::as_str).unwrap_or("");
        if label.is_empty() || ty == "markdown" {
            continue; // prose for the person filling the form: not part of the issue GitHub writes
        }
        let attr = |k: &str| attrs.and_then(|a| a.get(k));
        let content = match ty {
            "input" | "textarea" => {
                let value = text(attr("value"));
                let render: String = text(attr("render"))
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || "+-_#.".contains(*c))
                    .collect();
                if !render.is_empty() && ty == "textarea" {
                    format!("```{render}\n{value}\n```")
                } else if !value.is_empty() {
                    value
                } else {
                    let mut h = text(attr("description"));
                    if h.is_empty() {
                        h = text(attr("placeholder"));
                    }
                    hint(&h)
                }
            }
            "dropdown" => {
                let options: Vec<String> = attr("options")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .take(MAX_OPTIONS)
                    .map(|o| line(Some(o)))
                    .filter(|o| !o.is_empty())
                    .collect();
                let first = attr("default")
                    .and_then(Value::as_u64)
                    .and_then(|i| options.get(i as usize));
                match first {
                    Some(o) => o.clone(),
                    None if options.is_empty() => String::new(),
                    None => {
                        let many = attr("multiple") == Some(&Value::Bool(true));
                        hint(&format!(
                            "choose {}: {}",
                            if many { "any" } else { "one" },
                            options.join(" | ")
                        ))
                    }
                }
            }
            "checkboxes" => attr("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .take(MAX_OPTIONS)
                .map(|o| line(o.get("label")))
                .filter(|l| !l.is_empty())
                .map(|l| format!("- [ ] {l}"))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => continue,
        };
        out.push_str(&format!("### {label}\n\n{content}\n\n"));
        if out.len() > MAX_BODY {
            break;
        }
    }
    let mut body = out.trim_end().to_string();
    if body.len() > MAX_BODY {
        let cut = (0..=MAX_BODY)
            .rev()
            .find(|i| body.is_char_boundary(*i))
            .unwrap_or(0);
        body.truncate(cut);
    }
    if body.is_empty() {
        return Err("the form has no fields that make up the issue".into());
    }
    body.push('\n');
    let name = line(doc.get("name"));
    Ok(Template {
        name: if name.is_empty() {
            fallback.to_string()
        } else {
            name
        },
        body,
        title: title(doc.get("title")),
        labels: names(doc.get("labels"), 20),
        assignees: names(doc.get("assignees"), 10),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUG: &str = r#"
name: Bug report
description: File a bug
title: "[Bug]: "
labels: ["bug", "triage"]
assignees:
  - octocat
body:
  - type: markdown
    attributes:
      value: |
        Thanks for filing a bug!
  - type: input
    id: contact
    attributes:
      label: Contact
      description: How can we reach you?
      placeholder: you@example.com
    validations:
      required: false
  - type: textarea
    id: what
    attributes:
      label: What happened?
      description: Also tell us what you expected.
      value: "A bug happened!"
    validations:
      required: true
  - type: dropdown
    id: version
    attributes:
      label: Version
      options:
        - "1.0.2 (latest)"
        - "1.0.1"
  - type: dropdown
    id: browsers
    attributes:
      label: Browsers
      multiple: true
      options: [Firefox, Chrome]
  - type: dropdown
    id: pick
    attributes:
      label: Default one
      options: [a, b]
      default: 1
  - type: textarea
    id: logs
    attributes:
      label: Relevant log output
      render: shell
  - type: checkboxes
    id: terms
    attributes:
      label: Code of Conduct
      options:
        - label: I agree to follow this project's Code of Conduct
          required: true
        - label: I searched for duplicates
"#;

    #[test]
    fn a_form_becomes_the_sections_github_would_write_with_its_defaults() {
        let t = parse(BUG, "bug_report").unwrap();
        assert_eq!(t.name, "Bug report");
        assert_eq!(t.title.as_deref(), Some("[Bug]: "));
        assert_eq!(t.labels, ["bug", "triage"]);
        assert_eq!(t.assignees, ["octocat"]);
        let want = "\
### Contact

<!-- How can we reach you? -->

### What happened?

A bug happened!

### Version

<!-- choose one: 1.0.2 (latest) | 1.0.1 -->

### Browsers

<!-- choose any: Firefox | Chrome -->

### Default one

b

### Relevant log output

```shell

```

### Code of Conduct

- [ ] I agree to follow this project's Code of Conduct
- [ ] I searched for duplicates
";
        assert_eq!(t.body, want, "{}", t.body);
        assert!(
            !t.body.contains("Thanks for filing"),
            "markdown blocks are not part of the issue"
        );
    }

    #[test]
    fn names_titles_and_label_lists_have_fallbacks_and_two_spellings() {
        let y =
            "labels: bug, help wanted\nbody:\n  - type: input\n    attributes:\n      label: Why\n";
        let t = parse(y, "feature_request").unwrap();
        assert_eq!(
            (t.name.as_str(), t.title, t.labels.as_slice()),
            (
                "feature_request",
                None,
                &["bug".to_string(), "help wanted".to_string()][..]
            )
        );
        assert!(t.assignees.is_empty());
    }

    #[test]
    fn what_is_not_a_usable_form_is_an_error_not_a_blank_body() {
        for (yaml, want) in [
            ("name: x\n", "body"),
            ("body: nope\n", "body"),
            ("body: [", ""),
            ("body: []\n", "no fields"),
            (
                "body:\n  - type: markdown\n    attributes:\n      value: hi\n",
                "no fields",
            ),
            ("body:\n  - type: input\n    attributes: {}\n", "no fields"),
            (
                "body:\n  - type: widget\n    attributes:\n      label: X\n",
                "no fields",
            ),
        ] {
            let e = parse(yaml, "f").unwrap_err();
            assert!(e.contains(want), "{yaml:?}: {e}");
        }
        // a YAML bomb and an oversized file are refused before they are parsed
        let bomb = format!("a: &a [x]\nb: &b [{}]\nbody:\\n", "*a, ".repeat(40));
        assert!(parse(&bomb, "f").unwrap_err().contains("aliases"));
        assert!(
            parse(&"x".repeat(MAX_YAML + 1), "f")
                .unwrap_err()
                .contains("larger")
        );
    }

    #[test]
    fn text_from_the_repo_is_neutralized_and_cannot_break_out_of_a_comment() {
        let y = "name: \"Bug\\u202e report\"\nbody:\n  - type: input\n    attributes:\n      label: \"Why\\u001b[2J\\u202e?\"\n      description: \"first --> <script>alert(1)</script>\\nsecond\"\n";
        let t = parse(y, "f").unwrap();
        assert!(
            !t.name.contains('\u{202e}')
                && !t.body.contains('\u{202e}')
                && !t.body.contains('\u{1b}'),
            "{:?}",
            t.body
        );
        assert!(t.name.contains("<U+202E>"), "{}", t.name);
        // one comment, closed once, by us
        assert_eq!(t.body.matches("<!--").count(), 1);
        assert_eq!(t.body.matches("-->").count(), 1, "{}", t.body);
        assert!(
            !t.body.contains('\n') || !t.body.lines().any(|l| l.starts_with("second")),
            "{}",
            t.body
        );
    }

    #[test]
    fn the_amount_read_from_one_form_is_bounded() {
        // long answers: the body stops growing at its cap, well inside the size a file may have
        let mut y = String::from("body:\n");
        for i in 0..100 {
            y += &format!(
                "  - type: textarea\n    attributes:\n      label: Q{i}\n      value: \"{}\"\n",
                "x".repeat(1500)
            );
        }
        assert!(y.len() < MAX_YAML);
        let t = parse(&y, "f").unwrap();
        assert!(t.body.len() <= MAX_BODY + 1, "{}", t.body.len());
        assert!(t.body.contains("### Q0") && !t.body.contains("### Q99"));
        // many small items: only the first MAX_ITEMS are read
        let mut y = String::from("body:\n");
        for i in 0..120 {
            y += &format!("  - type: input\n    attributes:\n      label: Item{i}\n");
        }
        let t = parse(&y, "f").unwrap();
        assert_eq!(t.body.matches("### Item").count(), MAX_ITEMS);
        // many options: only the first MAX_OPTIONS are listed
        let opts = (0..500)
            .map(|i| format!("o{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let many = format!(
            "body:\n  - type: dropdown\n    attributes:\n      label: D\n      options: [{opts}]\n"
        );
        let d = parse(&many, "f").unwrap();
        assert!(
            d.body.contains("o49") && !d.body.contains("o50 "),
            "{}",
            d.body
        );
    }

    #[test]
    fn only_yaml_files_other_than_the_chooser_config_are_forms() {
        assert!(is_form_file("bug.yml") && is_form_file("Bug.YAML"));
        assert!(!is_form_file("config.yml") && !is_form_file("config.yaml"));
        assert!(!is_form_file("bug.md") && !is_form_file("README"));
    }
}
