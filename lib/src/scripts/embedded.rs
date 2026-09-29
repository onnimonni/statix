//! Programs embedded in shell commands: `jq '.a | .b'`, `awk '{print $1}'`,
//! `python3 -c '...'`, checked with jq, gawk and ruff.

use super::{
    Finding,
    commands::{Commands, Simple},
    nixstr::PLACEHOLDER,
    tools,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Jq,
    Awk,
    Python,
}

impl Tool {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Tool::Jq => "jq",
            Tool::Awk => "awk",
            Tool::Python => "python",
        }
    }
}

/// A program given on a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Embedded {
    pub tool: Tool,
    pub program: String,
    /// Byte range of the word in the script (quotes included).
    pub start: usize,
    pub end: usize,
    /// `$names` the command line defines (jq `--arg name value`).
    pub vars: Vec<String>,
}

/// jq options taking a value, and how many.
fn jq_option_values(option: &str) -> Option<usize> {
    Some(match option {
        "--arg" | "--argjson" | "--slurpfile" | "--rawfile" => 2,
        "--indent" | "-L" | "--library-path" => 1,
        "-f" | "--from-file" => return None,
        _ => 0,
    })
}

fn jq_program(c: &Simple) -> Option<(usize, Vec<String>)> {
    // `--arg name value` may also come after the program
    let vars: Vec<String> = c
        .words
        .windows(2)
        .filter(|w| {
            matches!(
                w[0].0.as_deref(),
                Some("--arg" | "--argjson" | "--slurpfile" | "--rawfile")
            )
        })
        .filter_map(|w| w[1].0.clone())
        .collect();
    let mut k = 1;
    while let Some(word) = c.words.get(k) {
        let text = word.0.as_deref()?;
        if text == "--" {
            k += 1;
            break;
        }
        if !text.starts_with('-') || text == "-" {
            break;
        }
        k += 1 + jq_option_values(text)?;
    }
    // `--args`/`--jsonargs` after the program are positional values
    Some((k, vars)).filter(|(k, _)| *k < c.words.len())
}

fn awk_program(c: &Simple) -> Option<usize> {
    let mut k = 1;
    while let Some(word) = c.words.get(k) {
        let text = word.0.as_deref()?;
        match text {
            "--" => return Some(k + 1).filter(|k| *k < c.words.len()),
            "-f" | "--file" => return None,
            "-e" | "--source" => return Some(k + 1),
            "-F" | "-v" | "--assign" | "--field-separator" => k += 2,
            t if t.starts_with("-F") || t.starts_with("-v") => k += 1,
            t if t.starts_with('-') && t.len() > 1 => k += 1,
            _ => return Some(k),
        }
    }
    None
}

fn python_program(c: &Simple) -> Option<usize> {
    let mut k = 1;
    while let Some(text) = c.word(k) {
        match text {
            "-c" => return Some(k + 1),
            // flags before `-c`: -I, -B, -u, -E, -S, -O
            t if t.len() == 2 && t.starts_with('-') && "IBuESOsq".contains(&t[1..]) => k += 1,
            _ => return None,
        }
    }
    None
}

fn is_python(name: &str) -> bool {
    let name = name.rsplit('/').next().unwrap_or(name);
    name == "python"
        || name
            .strip_prefix("python3")
            .is_some_and(|v| v.is_empty() || v.starts_with('.'))
}

/// Programs given to jq, awk and python on command lines in `commands`.
/// Only static programs without `${...}`: an interpolation can be code.
#[must_use]
pub fn embedded(commands: &Commands) -> Vec<Embedded> {
    let mut out = Vec::new();
    for c in &commands.simples {
        let Some(name) = c.word(0) else { continue };
        let base = name.rsplit('/').next().unwrap_or(name);
        let found = match base {
            "jq" | "gojq" => jq_program(c).map(|(k, vars)| (Tool::Jq, k, vars)),
            "awk" | "gawk" => awk_program(c).map(|k| (Tool::Awk, k, Vec::new())),
            _ if is_python(base) => python_program(c).map(|k| (Tool::Python, k, Vec::new())),
            _ => None,
        };
        let Some((tool, k, vars)) = found else {
            continue;
        };
        let Some((Some(program), start, end)) = c.words.get(k) else {
            continue;
        };
        if program.contains(PLACEHOLDER) || program.trim().is_empty() {
            continue;
        }
        out.push(Embedded {
            tool,
            program: program.clone(),
            start: *start,
            end: *end,
            vars,
        });
    }
    out
}

type Key = (Tool, String, Vec<String>);

fn cache() -> &'static Mutex<HashMap<Key, Arc<Vec<Finding>>>> {
    static CACHE: OnceLock<Mutex<HashMap<Key, Arc<Vec<Finding>>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Findings for `e` (cached by program). `None` when the checker isn't
/// installed.
#[must_use]
pub fn check(e: &Embedded) -> Option<Arc<Vec<Finding>>> {
    let key = (e.tool, e.program.clone(), e.vars.clone());
    if let Some(found) = cache().lock().ok()?.get(&key) {
        return Some(found.clone());
    }
    let findings = match e.tool {
        Tool::Jq => tools::jq(&e.program, &e.vars)?,
        Tool::Awk => tools::awk(&e.program)?,
        // one-liners: errors only, not the project's style rules
        Tool::Python => tools::ruff(&e.program, None)?
            .into_iter()
            .filter(|f| {
                matches!(
                    f.code.as_str(),
                    "syntax-error" | "invalid-syntax" | "F821" | "E999"
                )
            })
            .collect(),
    };
    let findings = Arc::new(findings);
    cache().lock().ok()?.insert(key, findings.clone());
    Some(findings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripts::commands::commands;

    fn programs(script: &str) -> Vec<(Tool, String, Vec<String>)> {
        embedded(&commands(script, "bash"))
            .into_iter()
            .map(|e| (e.tool, e.program, e.vars))
            .collect()
    }

    #[test]
    fn finds_programs() {
        let s = |x: &str| x.to_string();
        assert_eq!(
            programs("jq -r --arg v x '.a | $v' f.json"),
            [(Tool::Jq, s(".a | $v"), vec![s("v")])]
        );
        assert_eq!(
            programs("jq -n '$a + $b' --arg a 1 --argjson b 2"),
            [(Tool::Jq, s("$a + $b"), vec![s("a"), s("b")])]
        );
        assert_eq!(programs("jq -f prog.jq x"), []);
        assert_eq!(
            programs("n=$(awk -F: '{print $1}' /etc/passwd)"),
            [(Tool::Awk, s("{print $1}"), vec![])]
        );
        assert_eq!(
            programs("python3 -c 'import sys; print(sys.argv)'"),
            [(Tool::Python, s("import sys; print(sys.argv)"), vec![])]
        );
        // dynamic or interpolated programs aren't checked
        assert_eq!(programs("jq \"$filter\" x"), []);
        assert_eq!(programs("jq '.a.__nix_interp__' x"), []);
    }
}
