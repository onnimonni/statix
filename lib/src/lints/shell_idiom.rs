use crate::{
    Metadata, Report, Rule, Suggestion,
    scripts::{
        self, Kind, Lang, commands, context::fn_name, declared, fixes::Change, idioms,
        nixstr::Script,
    },
    utils,
};

use macros::lint;
use rnix::{
    NodeOrToken, SyntaxElement, SyntaxKind,
    ast::{self, AttrpathValue},
};
use rowan::ast::AstNode as _;

/// ## What it does
/// Suggests shorter idioms for common command sequences in shell scripts
/// written in Nix. `mkdir -p`, `cp`, `chmod` and `chown` of one file are a
/// single `install`:
///
/// ```sh
/// mkdir -p $out/bin
/// cp tool $out/bin/tool
/// chmod 755 $out/bin/tool
/// ```
///
/// is
///
/// ```sh
/// install -Dm755 tool $out/bin/tool
/// ```
///
/// ## Why is this bad?
/// Four lines do what one well known command does; `install` also creates
/// the file with its mode instead of changing it after copying.
///
/// `statix fix` rewrites sequences with an explicit numeric mode. Without
/// `chmod` it's only a suggestion: `install` sets mode 755 unless given
/// `-m`, where `cp` keeps the source's mode. `-D` and `-t` are GNU options,
/// suggested only where GNU coreutils run the script (build phases, devenv,
/// NixOS).
#[lint(
    name = "shell_idiom",
    note = "Shell commands with a shorter idiom",
    code = 33,
    match_with = SyntaxKind::NODE_STRING
)]
struct ShellIdiom;

/// Whether GNU coreutils run the script: stdenv phases and `runCommand`,
/// devenv (its stdenv), NixOS modules (Linux).
fn gnu_coreutils(s: &ast::Str, kind: Kind) -> bool {
    if kind == Kind::Hook {
        return true;
    }
    let node = s.syntax();
    let run_command = node
        .ancestors()
        .filter_map(ast::Apply::cast)
        .filter_map(|a| {
            let mut f = a.lambda()?;
            while let ast::Expr::Apply(inner) = f {
                f = inner.lambda()?;
            }
            fn_name(&f)
        })
        .any(|f| f.starts_with("runCommand"));
    let devenv = node.ancestors().filter_map(AttrpathValue::cast).any(|apv| {
        utils::enclosing_attrpath(apv.syntax()).is_some_and(|p| {
            matches!(
                p.first().map(String::as_str),
                Some("scripts" | "tasks" | "processes" | "enterShell" | "enterTest")
            )
        })
    });
    let linux = declared::file_platforms(node)
        .is_some_and(|c| c == declared::Cond::Platform("linux".into()));
    run_command || devenv || linux
}

/// 1-based (line, column in chars) of byte `at` in `text`.
fn line_col(text: &str, at: usize) -> Option<(usize, usize)> {
    let before = text.get(..at)?;
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    Some((line, col))
}

fn change(script: &Script, idiom: &idioms::Idiom) -> Option<Change> {
    let (line, column) = line_col(&script.text, idiom.start)?;
    let (end_line, end_column) = line_col(&script.text, idiom.end)?;
    Some(Change {
        line,
        column,
        end_line,
        end_column,
        replacement: idiom.render(|s, e| script.text.get(s..e).map(String::from))?,
    })
}

impl Rule for ShellIdiom {
    fn validate(&self, node: &SyntaxElement) -> Option<Report> {
        let NodeOrToken::Node(node) = node else {
            return None;
        };
        let s = ast::Str::cast(node.clone())?;
        let (script, lang, kind) = scripts::script_of(&s)?;
        let Lang::Shell(shell) = lang else {
            return None;
        };
        // cheap check before parsing
        if !["cp ", "cat ", "tee ", "cd "]
            .iter()
            .any(|c| script.text.contains(c))
        {
            return None;
        }
        let gnu = gnu_coreutils(&s, kind);
        let found = idioms::idioms(&script.text, &commands::commands(&script.text, shell), gnu);
        if found.is_empty() {
            return None;
        }
        // the exact ones are fixed together
        let changes: Vec<Change> = found
            .iter()
            .filter(|i| i.exact)
            .filter_map(|i| change(&script, i))
            .collect();
        let fixed = script.apply(&s, &changes);
        let mut report = self.report();
        let mut suggested = false;
        // words as written in Nix (`${...}`, not the placeholder)
        let base: usize = s.syntax().text_range().start().into();
        let source = s.syntax().to_string();
        let nix_word = |a: usize, b: usize| {
            let r = script.source_range(a, b)?;
            let (a, b): (usize, usize) = (r.start().into(), r.end().into());
            source.get(a - base..b - base).map(String::from)
        };
        for idiom in &found {
            let Some(at) = script.source_range(idiom.start, idiom.end) else {
                continue;
            };
            let Some(shown) = idiom.render(nix_word) else {
                continue;
            };
            let message = idiom.message.clone();
            let help = match idiom.note {
                Some(caveat) => format!("Use `{shown}`. {caveat}"),
                None => format!("Use `{shown}`."),
            };
            report = match (&fixed, idiom.exact, suggested) {
                (Some(fixed), true, false) => {
                    suggested = true;
                    report.suggest(
                        at,
                        format!("{message}: `{shown}`"),
                        Suggestion::with_replacement(
                            s.syntax().text_range(),
                            fixed.syntax().clone(),
                        ),
                    )
                }
                (Some(_), true, true) => report.diagnostic_fixed_later(at, message),
                _ => report.diagnostic_with_help(at, message, help),
            };
        }
        // only suggestions that change the mode: a hint, not a warning
        if !found.iter().any(|i| i.exact) {
            report = report.severity(crate::Severity::Hint);
        }
        (!report.diagnostics.is_empty()).then_some(report)
    }
}
