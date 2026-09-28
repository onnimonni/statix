use crate::{
    Metadata, Report, Rule, Severity, Suggestion,
    shell::{PLACEHOLDER, Script},
    shell_fixes::{self, Change},
    utils,
};

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
/// - `let` bindings used as one of the above, or interpolated into one
///   (`${snippet}`), are checked as script fragments
///
/// `${...}` interpolations are replaced by a placeholder word; findings that
/// only exist because of the placeholder are dropped. `statix fix` applies
/// shellcheck's own fixes plus built-in ones for SC2045, SC2115, SC2155 and
/// SC2162. Other findings carry a `help` text (see `--format agent`).
///
/// Needs `shellcheck` in `PATH` (or `STATIX_SHELLCHECK` pointing to it);
/// without it this lint does nothing.
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

/// How deep `${snippet}` references are followed.
const MAX_DEPTH: usize = 4;

/// Codes that make no sense for a fragment interpolated into a bigger script.
const FRAGMENT_NOISE: &[u32] = &[2034, 2154];

/// Pointers to an earlier parse error, not findings of their own.
const META: &[u32] = &[1072, 1073];

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
        let (shell, fragment) = dialect(&s, &script, 0)?;
        // `#!/usr/bin/env python3` in a devenv script: bash would run it, but
        // `devenv_exec_shebang` already reports the real problem.
        let foreign_shebang = script.text.trim_start().starts_with("#!")
            && shebang_dialect(&script.text).is_none();
        if script.text.trim().is_empty() || foreign_shebang {
            return None;
        }
        let output = run_shellcheck(shell, &script.text)?;

        let mut report = self.report();
        let mut suggested = false;
        let mut severity = 0;
        for c in output.comments {
            let noise = META.contains(&c.code)
                || (fragment &&(c.code < 2000 || FRAGMENT_NOISE.contains(&c.code)))
                || (shell_fixes::PLACEHOLDER_ARTIFACTS.contains(&c.code)
                    && line_has_interp(&script, c.line));
            if noise {
                continue;
            }
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
            // until nothing changes: suggest the first fix, mark the others as
            // fixed later.
            let fixed = c
                .fix
                .map(|f| f.replacements)
                .or_else(|| {
                    shell_fixes::builtin(
                        c.code,
                        &script.text,
                        c.line,
                        c.column,
                        c.end_line,
                        c.end_column,
                    )
                })
                .and_then(|changes| apply_fix(&s, &script, &changes));
            if fixed.is_some() && suggested {
                report = report.diagnostic_fixed_later(at, message);
            } else if let Some(fixed) = fixed {
                suggested = true;
                let whole = s.syntax().text_range();
                report = report.suggest(
                    at,
                    message,
                    Suggestion::with_replacement(whole, fixed.syntax().clone()),
                );
            } else {
                report = report.diagnostic_with_help(at, message, help(c.code));
            }
        }
        if report.diagnostics.is_empty() {
            return None;
        }
        Some(report.severity(match severity {
            2 => Severity::Error,
            1 => Severity::Warn,
            _ => Severity::Hint,
        }))
    }
}

fn help(code: u32) -> String {
    let hint = shell_fixes::hint(code).map_or_else(String::new, |h| format!("{h} "));
    format!("{hint}See https://www.shellcheck.net/wiki/SC{code}")
}

fn offset(o: usize) -> Option<TextSize> {
    TextSize::try_from(o).ok()
}

fn line_has_interp(script: &Script, line: usize) -> bool {
    script
        .lines
        .get(line.wrapping_sub(1))
        .is_some_and(|l| l.chars.iter().any(|c| c.interp))
}

/// Last name of `f` / `pkgs.writers.writeBash`.
fn fn_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Ident(i) => Some(i.syntax().text().to_string()),
        Expr::Select(s) => s.attrpath()?.attrs().last().map(|a| a.syntax().to_string()),
        _ => None,
    }
}

/// Shell dialect of the script in `s`, and whether it's only a fragment of a
/// script (a `let` binding interpolated into one).
fn dialect(s: &ast::Str, script: &Script, depth: usize) -> Option<(&'static str, bool)> {
    if let Some(shell) =
        writer_dialect(s.syntax(), &script.text).or_else(|| option_dialect(s.syntax()))
    {
        return Some((shell, false));
    }
    if depth >= MAX_DEPTH {
        return None;
    }

    // let snippet = ''...''; used as a script, or interpolated into one
    let binding = AttrpathValue::cast(s.syntax().parent()?)?;
    let let_in = ast::LetIn::cast(binding.syntax().parent()?)?;
    let keys: Vec<_> = binding.attrpath()?.attrs().collect();
    let [key] = keys.as_slice() else { return None };
    let name = utils::attr_name(key)?;

    let references = let_in
        .syntax()
        .descendants()
        .filter_map(ast::Ident::cast)
        .filter(|i| i.syntax().text() == name.as_str())
        .filter(|i| i.syntax().parent().map(|p| p.kind()) != Some(SyntaxKind::NODE_ATTRPATH));
    for reference in references {
        let node = reference.syntax();
        if let Some(shell) = writer_dialect(node, &script.text).or_else(|| option_dialect(node)) {
            return Some((shell, false));
        }
        let outer = node
            .ancestors()
            .find(|a| a.kind() == SyntaxKind::NODE_INTERPOL)
            .and_then(|interp| interp.parent())
            .and_then(ast::Str::cast);
        if let Some(outer) = outer
            && let Some((shell, _)) = dialect(&outer, &Script::new(&outer), depth + 1)
        {
            return Some((shell, true));
        }
    }
    None
}

/// `pkgs.writeShellScript "name" <node>` and friends.
fn writer_dialect(node: &SyntaxNode, text: &str) -> Option<&'static str> {
    let parent = node.parent()?;

    // writeShellApplication { text = <node>; }
    if let Some(apv) = AttrpathValue::cast(parent.clone()) {
        let keys: Vec<_> = apv.attrpath()?.attrs().collect();
        let [key] = keys.as_slice() else { return None };
        if utils::attr_name(key)? != "text" {
            return None;
        }
        let apply = ast::Apply::cast(apv.syntax().parent()?.parent()?)?;
        return (fn_name(&apply.lambda()?)? == "writeShellApplication").then_some("bash");
    }

    // f "name" <node>
    let apply = ast::Apply::cast(parent)?;
    if apply.argument()?.syntax() != node {
        return None;
    }
    let Expr::Apply(inner) = apply.lambda()? else {
        return None;
    };
    match fn_name(&inner.lambda()?)?.as_str() {
        "writeShellScript" | "writeShellScriptBin" | "writeBash" | "writeBashBin" => Some("bash"),
        "writeDash" | "writeDashBin" => Some("dash"),
        "writeScript" | "writeScriptBin" => shebang_dialect(text),
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
    // `let x = ...` bindings aren't options
    if apv.syntax().parent()?.kind() == SyntaxKind::NODE_LET_IN {
        return None;
    }
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

/// The string with `changes` applied, if they can be expressed in the Nix
/// source and it renders to exactly the script shellcheck intended.
/// Interpolations may only be replaced as a whole, by a replacement that
/// keeps their placeholders.
fn apply_fix(s: &ast::Str, script: &Script, changes: &[Change]) -> Option<ast::Str> {
    let base: usize = s.syntax().text_range().start().into();
    let original = s.syntax().to_string();
    let mut interps: Vec<(usize, usize)> = script
        .lines
        .iter()
        .flat_map(|l| &l.chars)
        .filter(|c| c.interp)
        .map(|c| (c.start, c.end))
        .collect();
    interps.dedup();

    let mut edits = Vec::new();
    for r in changes {
        let (a, exact_a) = script.source_offset(r.line, r.column, false)?;
        let (b, exact_b) = script.source_offset(r.end_line, r.end_column, true)?;
        if !exact_a || !exact_b || b < a {
            return None;
        }
        let covered: Vec<_> = interps
            .iter()
            .filter(|&&(s, e)| a < e && s < b)
            .copied()
            .collect();
        if covered.iter().any(|&(s, e)| s < a || e > b) {
            return None;
        }
        // Escape the replacement, putting the covered `${...}` back in place
        // of their placeholders.
        let pieces: Vec<&str> = r.replacement.split(PLACEHOLDER).collect();
        if pieces.len() != covered.len() + 1 {
            return None;
        }
        let mut source = script.escape(pieces[0]);
        for (piece, (s, e)) in pieces[1..].iter().zip(&covered) {
            source.push_str(&original[s - base..e - base]);
            source.push_str(&script.escape(piece));
        }

        let ta = script.text_offset(r.line, r.column)?;
        let tb = script.text_offset(r.end_line, r.end_column)?;
        edits.push((a - base, b - base, ta, tb, source, r.replacement.as_str()));
    }
    // Apply back to front; bail out on overlapping edits.
    edits.sort_by_key(|e| std::cmp::Reverse((e.0, e.1)));
    if edits.windows(2).any(|w| w[1].1 > w[0].0) {
        return None;
    }

    let mut src = original.clone();
    let mut expected = script.text.clone();
    for (a, b, ta, tb, source, replacement) in edits {
        src.replace_range(a..b, &source);
        expected.replace_range(ta..tb, replacement);
    }
    let parse = rnix::Root::parse(&src).ok().ok()?;
    let Some(Expr::Str(fixed)) = parse.expr() else {
        return None;
    };
    (Script::new(&fixed).text == expected).then_some(fixed)
}
