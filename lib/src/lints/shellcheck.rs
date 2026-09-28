use crate::{Metadata, Report, Rule, Severity, Suggestion, shell::Script, utils};

use macros::lint;
use rnix::{
    NodeOrToken, SyntaxElement, SyntaxKind, SyntaxNode, TextRange, TextSize,
    ast::{self, AttrpathValue, Expr},
};
use rowan::ast::AstNode as _;
use serde::Deserialize;
use std::{
    io::Write as _,
    process::{Command, Stdio},
};

/// ## What it does
/// Runs [ShellCheck](https://www.shellcheck.net) on shell scripts written as
/// Nix strings and reports its findings at their position in the `.nix` file:
///
/// - devenv: `scripts.<name>.exec`, `tasks.<name>.exec`/`.status`,
///   `processes.<name>.exec`, `enterShell`, `enterTest`
/// - nixpkgs: `writeShellScript(Bin)`, `writeShellApplication { text }`,
///   `writers.writeBash(Bin)`, `writers.writeDash(Bin)`, and
///   `writeScript(Bin)` with a shell shebang
///
/// `${...}` interpolations are replaced by a placeholder word. shellcheck's
/// own fixes are applied by `statix fix`. Needs `shellcheck` in `PATH` (or
/// `STATIX_SHELLCHECK` pointing to it); without it this lint does nothing.
///
/// ## Why is this bad?
/// Nix doesn't check the scripts it writes, so quoting bugs and typos only
/// show up when the script runs.
///
/// ## Example
///
/// ```nix
/// {
///   scripts.hello.exec = ''
///     echo $1
///   '';
/// }
/// ```
///
/// Quote the variable:
///
/// ```nix
/// {
///   scripts.hello.exec = ''
///     echo "$1"
///   '';
/// }
/// ```
#[lint(
    name = "shellcheck",
    note = "ShellCheck found issues in an inline shell script",
    code = 28,
    match_with = SyntaxKind::NODE_STRING
)]
struct ShellCheck;

#[derive(Deserialize)]
struct Output {
    comments: Vec<Comment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Comment {
    line: usize,
    column: usize,
    end_line: usize,
    end_column: usize,
    level: String,
    code: u32,
    message: String,
    fix: Option<Fix>,
}

#[derive(Deserialize)]
struct Fix {
    replacements: Vec<Change>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Change {
    line: usize,
    column: usize,
    end_line: usize,
    end_column: usize,
    replacement: String,
}

impl Rule for ShellCheck {
    fn validate(&self, node: &SyntaxElement) -> Option<Report> {
        let NodeOrToken::Node(node) = node else {
            return None;
        };
        let s = ast::Str::cast(node.clone())?;
        // Strings interpolated into a script are checked as part of it.
        if node
            .ancestors()
            .skip(1)
            .any(|a| a.kind() == SyntaxKind::NODE_STRING)
        {
            return None;
        }
        let script = Script::new(&s);
        let shell = writer_dialect(&s, &script).or_else(|| option_dialect(node))?;
        if script.text.trim().is_empty() {
            return None;
        }
        let output = run_shellcheck(shell, &script.text)?;
        if output.comments.is_empty() {
            return None;
        }

        let mut report = self.report();
        let mut suggested = false;
        let mut severity = 0;
        for c in output.comments {
            let Some((start, _)) = script.source_offset(c.line, c.column, false) else {
                continue;
            };
            let end = script
                .source_offset(c.end_line, c.end_column, true)
                .map_or(start, |(e, _)| e.max(start));
            let at = TextRange::new(offset(start)?, offset(end)?);
            let message = format!("SC{}: {}", c.code, c.message);
            severity = severity.max(match c.level.as_str() {
                "error" => 2,
                "warning" => 1,
                _ => 0,
            });

            // `statix fix` applies one suggestion per report and re-runs lints
            // until nothing changes, so suggesting the first fix is enough.
            let fixed = (!suggested)
                .then(|| c.fix.as_ref().and_then(|f| apply_fix(&s, &script, f)))
                .flatten();
            if let Some(fixed) = fixed {
                suggested = true;
                let whole = s.syntax().text_range();
                report = report.suggest(
                    at,
                    message,
                    Suggestion::with_replacement(whole, fixed.syntax().clone()),
                );
            } else {
                report = report.diagnostic(at, message);
            }
        }
        Some(report.severity(match severity {
            2 => Severity::Error,
            1 => Severity::Warn,
            _ => Severity::Hint,
        }))
    }
}

fn offset(o: usize) -> Option<TextSize> {
    TextSize::try_from(o).ok()
}

/// Last name of `f` / `pkgs.writers.writeBash`.
fn fn_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Ident(i) => Some(i.syntax().text().to_string()),
        Expr::Select(s) => s.attrpath()?.attrs().last().map(|a| a.syntax().to_string()),
        _ => None,
    }
}

/// `pkgs.writeShellScript "name" ''...''` and friends.
fn writer_dialect(s: &ast::Str, script: &Script) -> Option<&'static str> {
    let parent = s.syntax().parent()?;

    // writeShellApplication { text = ''...''; }
    if let Some(apv) = AttrpathValue::cast(parent.clone()) {
        let keys: Vec<_> = apv.attrpath()?.attrs().collect();
        let [key] = keys.as_slice() else { return None };
        if utils::attr_name(key)? != "text" {
            return None;
        }
        let apply = ast::Apply::cast(apv.syntax().parent()?.parent()?)?;
        return (fn_name(&apply.lambda()?)? == "writeShellApplication").then_some("bash");
    }

    // f "name" ''...''
    let apply = ast::Apply::cast(parent)?;
    if apply.argument()?.syntax() != s.syntax() {
        return None;
    }
    let Expr::Apply(inner) = apply.lambda()? else {
        return None;
    };
    match fn_name(&inner.lambda()?)?.as_str() {
        "writeShellScript" | "writeShellScriptBin" | "writeBash" | "writeBashBin" => Some("bash"),
        "writeDash" | "writeDashBin" => Some("dash"),
        "writeScript" | "writeScriptBin" => shebang_dialect(&script.text),
        _ => None,
    }
}

fn shebang_dialect(text: &str) -> Option<&'static str> {
    let line = text.lines().find(|l| !l.trim().is_empty())?;
    let mut words = line.trim().strip_prefix("#!")?.split_whitespace();
    let mut prog = words.next()?.rsplit('/').next()?;
    if prog == "env" {
        prog = words.next()?;
    }
    match prog {
        "bash" => Some("bash"),
        "sh" => Some("sh"),
        "dash" => Some("dash"),
        "ksh" => Some("ksh"),
        _ => None,
    }
}

/// devenv options whose value is run by bash.
fn option_dialect(node: &SyntaxNode) -> Option<&'static str> {
    let apv = node.ancestors().find_map(AttrpathValue::cast)?;
    let path = utils::enclosing_attrpath(apv.syntax())?;
    let path = path.strip_prefix(&["config".to_string()]).unwrap_or(&path);
    let path: Vec<&str> = path.iter().map(String::as_str).collect();
    let is_script = matches!(
        path.as_slice(),
        ["scripts" | "processes", _, "exec"]
            | ["tasks", _, "exec" | "status"]
            | ["enterShell" | "enterTest"]
    );
    (is_script && !utils::has_non_shell_package(&apv)).then_some("bash")
}

fn run_shellcheck(shell: &str, script: &str) -> Option<Output> {
    let program = std::env::var_os("STATIX_SHELLCHECK").unwrap_or_else(|| "shellcheck".into());
    let mut child = Command::new(program)
        // SC1091: sourced files are usually store paths we can't see
        .args(["--format=json1", "--shell", shell, "--exclude=SC1091", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(script.as_bytes()).ok()?;
    let output = child.wait_with_output().ok()?;
    serde_json::from_slice(&output.stdout).ok()
}

/// The string with shellcheck's fix applied, if it can be expressed in the
/// Nix source (doesn't touch interpolations) and renders to exactly the
/// script shellcheck intended.
fn apply_fix(s: &ast::Str, script: &Script, fix: &Fix) -> Option<ast::Str> {
    let base: usize = s.syntax().text_range().start().into();
    let interps: Vec<(usize, usize)> = script
        .lines
        .iter()
        .flat_map(|l| &l.chars)
        .filter(|c| c.interp)
        .map(|c| (c.start, c.end))
        .collect();

    let mut edits = Vec::new();
    for r in &fix.replacements {
        let (a, exact_a) = script.source_offset(r.line, r.column, false)?;
        let (b, exact_b) = script.source_offset(r.end_line, r.end_column, true)?;
        let touches_interp = interps.iter().any(|&(s, e)| a < e && s < b);
        if !exact_a || !exact_b || b < a || touches_interp {
            return None;
        }
        let ta = script.text_offset(r.line, r.column)?;
        let tb = script.text_offset(r.end_line, r.end_column)?;
        edits.push((a - base, b - base, ta, tb, r.replacement.as_str()));
    }
    // Apply back to front; bail out on overlapping edits.
    edits.sort_by_key(|e| std::cmp::Reverse((e.0, e.1)));
    if edits.windows(2).any(|w| w[1].1 > w[0].0) {
        return None;
    }

    let mut src = s.syntax().to_string();
    let mut expected = script.text.clone();
    for (a, b, ta, tb, replacement) in edits {
        src.replace_range(a..b, &script.escape(replacement));
        expected.replace_range(ta..tb, replacement);
    }
    let parse = rnix::Root::parse(&src).ok().ok()?;
    let Some(Expr::Str(fixed)) = parse.expr() else {
        return None;
    };
    (Script::new(&fixed).text == expected).then_some(fixed)
}
