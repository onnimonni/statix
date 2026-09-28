//! Scripts embedded in or referenced from Nix code: finding them, running
//! the language's checker on them and applying its fixes.

pub mod context;
pub mod fixes;
pub mod nixstr;
mod tools;

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
};

use rnix::ast;
use rowan::ast::AstNode as _;

use fixes::Change;
use nixstr::{PLACEHOLDER, Script};

/// Language of a script, with the shell dialect for shell scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Shell(&'static str),
    Python,
}

/// How complete the script is, which decides what counts as noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A whole script.
    Script,
    /// Code run inside an environment that defines variables and functions
    /// (stdenv phases, setup hooks, activation snippets).
    Hook,
    /// A piece interpolated into another script.
    Fragment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Info,
    Warning,
    Error,
}

/// A checker finding in 1-based coordinates of the checked text.
#[derive(Debug, Clone)]
pub struct Finding {
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
    pub level: Level,
    /// `SC2086`, `F401` ...
    pub code: String,
    pub message: String,
    pub url: Option<String>,
    pub fix: Option<Vec<Change>>,
}

impl Finding {
    /// How to fix it by hand.
    #[must_use]
    pub fn help(&self) -> String {
        let hint = self
            .code
            .strip_prefix("SC")
            .and_then(|c| c.parse().ok())
            .and_then(fixes::hint)
            .map_or_else(String::new, |h| format!("{h} "));
        match &self.url {
            Some(url) => format!("{hint}See {url}"),
            None => hint.trim_end().to_string(),
        }
    }
}

thread_local! {
    static CURRENT_FILE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Run `f` with `path` as the file being linted, so lints can resolve paths
/// relative to it.
pub fn with_current_file<R>(path: Option<&Path>, f: impl FnOnce() -> R) -> R {
    let previous = CURRENT_FILE.with(|c| c.replace(path.map(Path::to_path_buf)));
    let result = f();
    CURRENT_FILE.with(|c| *c.borrow_mut() = previous);
    result
}

/// Directory of the file being linted.
#[must_use]
pub fn current_dir() -> Option<PathBuf> {
    CURRENT_FILE.with(|c| {
        c.borrow().as_ref().map(|p| {
            p.parent()
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
        })
    })
}

/// Findings of the checker for `lang`, without the ones that are noise for
/// `kind`. `None` when the checker isn't installed. `has_interp(line)` tells
/// whether a line contains a `${...}` placeholder.
pub fn check(
    lang: Lang,
    kind: Kind,
    text: &str,
    filename: Option<&Path>,
    has_interp: &dyn Fn(usize) -> bool,
) -> Option<Vec<Finding>> {
    let findings = match lang {
        Lang::Shell(shell) => tools::shellcheck(shell, text)?,
        Lang::Python => tools::ruff(text, filename)?,
    };
    Some(
        findings
            .into_iter()
            .filter(|f| !is_noise(f, lang, kind, has_interp(f.line)))
            .collect(),
    )
}

fn is_noise(f: &Finding, lang: Lang, kind: Kind, line_has_interp: bool) -> bool {
    match lang {
        Lang::Shell(_) => {
            let Some(code) = f
                .code
                .strip_prefix("SC")
                .and_then(|c| c.parse::<u32>().ok())
            else {
                return false;
            };
            fixes::META.contains(&code)
                || (fixes::PLACEHOLDER_ARTIFACTS.contains(&code) && line_has_interp)
                || match kind {
                    Kind::Script => false,
                    Kind::Hook => fixes::HOOK_NOISE.contains(&code),
                    Kind::Fragment => code < 2000 || fixes::HOOK_NOISE.contains(&code),
                }
        }
        // `${...}` used as Python code shows up as an undefined name
        Lang::Python => f.message.contains(PLACEHOLDER),
    }
}

/// Byte offset in `text` of a 1-based (line, column in chars).
#[must_use]
pub fn text_offset(text: &str, line: usize, col: usize) -> Option<usize> {
    let line_start = text
        .split('\n')
        .take(line.checked_sub(1)?)
        .map(|l| l.len() + 1)
        .sum::<usize>();
    let rest = text.get(line_start..)?.split('\n').next()?;
    let idx = col.checked_sub(1)?;
    let byte = rest.char_indices().nth(idx).map_or_else(
        || (idx == rest.chars().count()).then_some(rest.len()),
        |(b, _)| Some(b),
    )?;
    Some(line_start + byte)
}

/// Indices of the findings whose fixes can be applied together: as many as
/// possible without overlaps, in finding order.
fn non_overlapping(findings: &[Finding]) -> Vec<usize> {
    let mut taken: Vec<((usize, usize), (usize, usize))> = Vec::new();
    let mut out = Vec::new();
    // Reformatting fixes (ruff's import sorting) go last: they would
    // rearrange what the other fixes remove.
    let mut order: Vec<usize> = (0..findings.len()).collect();
    order.sort_by_key(|&i| findings[i].code.starts_with('I'));
    for i in order {
        let f = &findings[i];
        let Some(changes) = &f.fix else { continue };
        let spans: Vec<_> = changes
            .iter()
            .map(|c| ((c.line, c.column), (c.end_line, c.end_column)))
            .collect();
        let overlaps = spans
            .iter()
            .any(|&(a, b)| taken.iter().any(|&(x, y)| (a < y && x < b) || a == x));
        if !overlaps {
            taken.extend(spans);
            out.push(i);
        }
    }
    out
}

fn changes_of(findings: &[Finding], ids: &[usize]) -> Vec<Change> {
    ids.iter()
        .filter_map(|&i| findings[i].fix.clone())
        .flatten()
        .collect()
}

/// Apply `changes` to plain text (a script file).
#[must_use]
pub fn apply_plain(text: &str, changes: &[Change]) -> Option<String> {
    let mut edits = changes
        .iter()
        .map(|c| {
            Some((
                text_offset(text, c.line, c.column)?,
                text_offset(text, c.end_line, c.end_column)?,
                c.replacement.as_str(),
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    edits.sort_by_key(|e| std::cmp::Reverse((e.0, e.1)));
    if edits.windows(2).any(|w| w[1].1 > w[0].0) {
        return None;
    }
    let mut out = text.to_string();
    for (a, b, r) in edits {
        out.replace_range(a..b, r);
    }
    Some(out)
}

/// `text` with all fixes the checker offers applied, repeating until it has
/// no more. `None` when nothing changes.
#[must_use]
pub fn fix_text(lang: Lang, kind: Kind, text: &str, filename: Option<&Path>) -> Option<String> {
    const MAX_PASSES: usize = 10;
    if lang == Lang::Python {
        return tools::ruff_fix(text, filename);
    }
    let mut current = text.to_string();
    for _ in 0..MAX_PASSES {
        let findings = check(lang, kind, &current, filename, &|_| false)?;
        let changes = changes_of(&findings, &non_overlapping(&findings));
        match apply_plain(&current, &changes) {
            Some(next) if next != current => current = next,
            _ => break,
        }
    }
    (current != text).then_some(current)
}

/// A Nix string rendered as a script, checked, with its fixes prepared.
pub struct Checked {
    pub script: Script,
    pub findings: Vec<Finding>,
    /// The string with as many fixes applied as can be verified.
    pub fixed: Option<ast::Str>,
    /// Indices of the findings `fixed` takes care of.
    pub fixed_findings: Vec<usize>,
}

/// Check the script in `s` if it is one written in `lang_filter`'s family.
#[must_use]
pub fn check_string(s: &ast::Str, python: bool) -> Option<Checked> {
    // Strings interpolated into a script are checked as part of it.
    if s.syntax()
        .ancestors()
        .skip(1)
        .any(|a| a.kind() == rnix::SyntaxKind::NODE_STRING)
    {
        return None;
    }
    let script = Script::new(s);
    if script.text.trim().is_empty() {
        return None;
    }
    let (lang, kind) = context::string_context(s, &script.text, 0)?;
    if python != (lang == Lang::Python) {
        return None;
    }
    // `#!/usr/bin/env python3` in a devenv script: bash would run it, but
    // `devenv_exec_shebang` already reports the real problem.
    let foreign_shebang = matches!(lang, Lang::Shell(_))
        && script.text.trim_start().starts_with("#!")
        && !matches!(context::shebang_lang(&script.text), Some(Lang::Shell(_)));
    if foreign_shebang {
        return None;
    }
    let filename = current_dir().map(|d| d.join("inline.py"));
    let findings = check(lang, kind, &script.text, filename.as_deref(), &|l| {
        script.line_has_interp(l)
    })?;

    // Try all fixes at once, fall back to one at a time.
    let together = non_overlapping(&findings);
    let (fixed, fixed_findings) = match script.apply(s, &changes_of(&findings, &together)) {
        Some(fixed) if !together.is_empty() => (Some(fixed), together),
        _ => findings
            .iter()
            .enumerate()
            .find_map(|(i, f)| Some((script.apply(s, f.fix.as_ref()?)?, vec![i])))
            .map_or((None, Vec::new()), |(fixed, ids)| (Some(fixed), ids)),
    };
    Some(Checked {
        script,
        findings,
        fixed,
        fixed_findings,
    })
}

/// Script files a Nix file refers to, for `statix fix` to fix alongside it.
#[must_use]
pub fn referenced_files(src: &str, nix_file: &Path) -> Vec<(PathBuf, Lang, Kind)> {
    let root = rnix::Root::parse(src).tree();
    with_current_file(Some(nix_file), || {
        let mut out: Vec<(PathBuf, Lang, Kind)> = Vec::new();
        for node in root.syntax().descendants() {
            for r in context::references(&node) {
                if !out.iter().any(|(p, _, _)| *p == r.path) {
                    out.push((r.path, r.lang, r.kind));
                }
            }
        }
        out
    })
}

/// Turn a checked Nix string into a report: the first fixable finding gets
/// the whole-string suggestion, the other fixable ones are fixed with it (or
/// in a later `statix fix` pass), the rest get help text.
#[must_use]
pub fn string_report(
    mut report: crate::Report,
    s: &ast::Str,
    checked: &Checked,
) -> Option<crate::Report> {
    use rnix::{TextRange, TextSize};
    let mut suggested = false;
    let mut level = Level::Info;
    for (i, f) in checked.findings.iter().enumerate() {
        let Some((start, _)) = checked.script.source_offset(f.line, f.column, false) else {
            continue;
        };
        let end = checked
            .script
            .source_offset(f.end_line, f.end_column, true)
            .map_or(start, |(e, _)| e.max(start));
        let at = TextRange::new(
            TextSize::try_from(start).ok()?,
            TextSize::try_from(end).ok()?,
        );
        let message = format!("{}: {}", f.code, f.message);
        level = level.max(f.level);
        let fixed_now = checked.fixed_findings.contains(&i);
        report = match (&checked.fixed, fixed_now, suggested) {
            (Some(fixed), true, false) => {
                suggested = true;
                report.suggest(
                    at,
                    message,
                    crate::Suggestion::with_replacement(
                        s.syntax().text_range(),
                        fixed.syntax().clone(),
                    ),
                )
            }
            _ if fixed_now
                || f.fix
                    .as_ref()
                    .is_some_and(|c| checked.script.apply(s, c).is_some()) =>
            {
                report.diagnostic_fixed_later(at, message)
            }
            _ => report.diagnostic_with_help(at, message, f.help()),
        };
    }
    if report.diagnostics.is_empty() {
        return None;
    }
    Some(report.severity(severity(level)))
}

#[must_use]
pub fn severity(level: Level) -> crate::Severity {
    match level {
        Level::Error => crate::Severity::Error,
        Level::Warning => crate::Severity::Warn,
        Level::Info => crate::Severity::Hint,
    }
}
