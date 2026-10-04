//! `workflow_dispatch` inputs, read from a workflow's YAML so the run form can ask for them.
use serde_json::Value;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InputKind {
    String,
    Boolean,
    Choice,
    Number,
    Environment,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InputDef {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub default: String,
    pub kind: InputKind,
    pub options: Vec<String>,
}

/// Largest workflow file we will parse.
pub(crate) const MAX_YAML: usize = 256 * 1024;
/// A "billion laughs" file needs many aliases; real workflows have a handful at most.
pub(crate) const MAX_ALIASES: usize = 20;

/// Counts `*name` alias tokens (not cron's `* * *` or `*/5`).
pub(crate) fn alias_count(yaml: &str) -> usize {
    let b = yaml.as_bytes();
    (0..b.len())
        .filter(|&i| {
            b[i] == b'*'
                && b.get(i + 1)
                    .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
                && (i == 0
                    || matches!(
                        b[i - 1],
                        b' ' | b'\t' | b'[' | b',' | b':' | b'-' | b'{' | b'\n'
                    ))
        })
        .count()
}

pub(crate) fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(), // true, 3, 1.5
    }
}

/// Ok(None): no `workflow_dispatch` trigger. Ok(Some(inputs)): dispatchable (possibly with no inputs).
/// Err: the YAML didn't parse; the caller falls back to free-form `key=value` entry.
pub fn parse(yaml: &str) -> Result<Option<Vec<InputDef>>, String> {
    // The file comes from the repo, i.e. from whoever can push to it: bound the work before parsing.
    if yaml.len() > MAX_YAML {
        return Err(format!(
            "workflow file is larger than {} KB",
            MAX_YAML / 1024
        ));
    }
    if alias_count(yaml) > MAX_ALIASES {
        return Err("workflow file uses too many YAML aliases".into());
    }
    let doc: Value = serde_yaml_ng::from_str(yaml).map_err(|e| e.to_string())?;
    // YAML 1.1 readers turn the key `on` into boolean true; accept both spellings
    let on = doc.get("on").or_else(|| doc.get("true"));
    let Some(on) = on else { return Ok(None) };
    let wd = match on {
        Value::Object(m) => match m.get("workflow_dispatch") {
            Some(v) => v,
            None => return Ok(None),
        },
        Value::Array(a) if a.iter().any(|x| x == "workflow_dispatch") => return Ok(Some(vec![])),
        Value::String(s) if s == "workflow_dispatch" => return Ok(Some(vec![])),
        _ => return Ok(None),
    };
    let inputs = wd.get("inputs").and_then(Value::as_object);
    let mut out = vec![];
    for (name, d) in inputs.into_iter().flatten() {
        let kind = match d.get("type").and_then(Value::as_str).unwrap_or("string") {
            "boolean" => InputKind::Boolean,
            "choice" => InputKind::Choice,
            "number" => InputKind::Number,
            "environment" => InputKind::Environment,
            _ => InputKind::String,
        };
        let options: Vec<String> = d
            .get("options")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(scalar).collect())
            .unwrap_or_default();
        out.push(InputDef {
            name: name.clone(),
            description: d.get("description").map(scalar).unwrap_or_default(),
            required: d.get("required") == Some(&Value::Bool(true)),
            default: d.get("default").map(scalar).unwrap_or_default(),
            kind,
            options,
        });
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_input_type() {
        let inputs = parse(include_str!("../tests/workflow_inputs.yml"))
            .unwrap()
            .unwrap();
        let by = |n: &str| inputs.iter().find(|i| i.name == n).unwrap().clone();
        assert_eq!(inputs.len(), 6);
        let msg = by("message");
        assert_eq!(
            (msg.kind, msg.required, msg.default.as_str()),
            (InputKind::String, true, "")
        );
        assert_eq!(msg.description, "What to say");
        let v = by("verbose");
        assert_eq!((v.kind, v.default.as_str()), (InputKind::Boolean, "true"));
        let lvl = by("level");
        assert_eq!(
            (lvl.kind, lvl.default.as_str()),
            (InputKind::Choice, "warn")
        );
        assert_eq!(lvl.options, ["info", "warn", "error"]);
        let c = by("count");
        assert_eq!((c.kind, c.default.as_str()), (InputKind::Number, "3"));
        assert_eq!(by("target").kind, InputKind::Environment);
        assert_eq!(by("plain").kind, InputKind::String, "no type means string");
    }

    #[test]
    fn detects_missing_or_inline_triggers_and_bad_yaml() {
        assert_eq!(
            parse("name: x\non: [push, workflow_dispatch]\njobs: {}\n").unwrap(),
            Some(vec![])
        );
        assert_eq!(parse("on: workflow_dispatch\n").unwrap(), Some(vec![]));
        assert_eq!(
            parse("on:\n  workflow_dispatch:\n").unwrap(),
            Some(vec![]),
            "bare trigger, no inputs"
        );
        assert_eq!(parse("on:\n  push:\n    branches: [main]\n").unwrap(), None);
        assert_eq!(parse("name: no triggers at all\n").unwrap(), None);
        assert_eq!(
            parse("true:\n  workflow_dispatch:\n").unwrap(),
            Some(vec![]),
            "`on` read as boolean"
        );
        assert!(parse("on: [unclosed\n  - x: : :\n").is_err());
    }

    /// A YAML "billion laughs": tiny text, astronomically large when aliases are expanded. It must
    /// be rejected up front (fast), not expanded.
    #[test]
    fn alias_bombs_and_huge_files_are_refused_quickly() {
        let t = std::time::Instant::now();
        let mut bomb = String::from(
            "on:\n  workflow_dispatch:\nx0: &a0 [lol, lol, lol, lol, lol, lol, lol, lol, lol]\n",
        );
        for i in 1..30 {
            let prev = i - 1;
            bomb += &format!(
                "x{i}: &a{i} [*a{prev}, *a{prev}, *a{prev}, *a{prev}, *a{prev}, *a{prev}, *a{prev}, *a{prev}, *a{prev}]\n"
            );
        }
        assert!(bomb.len() < MAX_YAML);
        assert!(parse(&bomb).unwrap_err().contains("aliases"));
        let big = format!("on: workflow_dispatch\n# {}\n", "x".repeat(1_000_000));
        assert!(parse(&big).unwrap_err().contains("larger than"));
        assert!(
            t.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            t.elapsed()
        );
        // ordinary uses are fine: cron stars and a couple of aliases
        let ok = "on:\n  schedule:\n    - cron: '*/5 * * * *'\n  workflow_dispatch:\nenv: &e {A: 1}\nuse: *e\n";
        assert_eq!(parse(ok).unwrap(), Some(vec![]));
    }
}
