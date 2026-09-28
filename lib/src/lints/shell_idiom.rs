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

/// Where the script runs: GNU coreutils (stdenv phases and `runCommand`,
/// devenv, NixOS modules) and whether setup.sh functions exist (stdenv
/// phases, `runCommand`).
/// Standard stdenv phases with `runHook pre<Name>` / `post<Name>`.
const PHASES: &[(&str, &str)] = &[
    ("unpackPhase", "Unpack"),
    ("patchPhase", "Patch"),
    ("configurePhase", "Configure"),
    ("buildPhase", "Build"),
    ("checkPhase", "Check"),
    ("installPhase", "Install"),
    ("installCheckPhase", "InstallCheck"),
    ("distPhase", "Dist"),
];

/// The stdenv phase `s` is the body of: `installPhase = ''...''`.
fn phase_of(s: &ast::Str) -> Option<&'static str> {
    let apv = AttrpathValue::cast(s.syntax().parent()?)?;
    let key = apv.attrpath()?.attrs().last()?;
    let name = utils::attr_name(&key)?;
    PHASES
        .iter()
        .find(|(p, _)| *p == name)
        .map(|(_, hook)| *hook)
}

fn context(s: &ast::Str, kind: Kind, shell: &str) -> idioms::Context {
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
    let build = kind == Kind::Hook || run_command;
    idioms::Context {
        gnu: build || devenv || linux,
        build,
        bash: shell == "bash",
        phase: phase_of(s).is_some(),
    }
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

/// `installPhase = ''...''` without `runHook preInstall` / `postInstall`:
/// hooks from `preInstall = ...` and setup hooks don't run.
fn missing_run_hooks(s: &ast::Str, text: &str) -> Vec<idioms::Idiom> {
    let Some(hook) = phase_of(s) else {
        return Vec::new();
    };
    let body = text.trim();
    // `installPhase = "true";`, `":"`: nothing to wrap
    if body.is_empty() || matches!(body, "true" | ":") {
        return Vec::new();
    }
    let (pre, post) = (format!("runHook pre{hook}"), format!("runHook post{hook}"));
    let start = text.len() - text.trim_start().len();
    let end = text.trim_end().len();
    let mut out = Vec::new();
    let note = Some(
        "Overriding a phase skips its hooks unless it runs them: `preInstall`/`postInstall` from the derivation and from setup hooks.",
    );
    if !text.contains(&pre) {
        out.push(idioms::Idiom {
            start,
            end: start,
            replacement: vec![idioms::Piece::Text(format!("{pre}\n"))],
            message: format!("The phase doesn't run `{pre}`"),
            note,
            exact: true,
        });
    }
    if !text.contains(&post) {
        out.push(idioms::Idiom {
            start: end,
            end,
            replacement: vec![idioms::Piece::Text(format!("\n{post}"))],
            message: format!("The phase doesn't run `{post}`"),
            note,
            exact: true,
        });
    }
    out
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
        let ctx = context(&s, kind, shell);
        let mut found =
            idioms::idioms_in(&script.text, &commands::commands(&script.text, shell), ctx);
        found.extend(missing_run_hooks(&s, &script.text));
        found.sort_by_key(|i| i.start);
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
            // one line in messages (`runHook preInstall\n`, multi-line commands)
            let shown = shown.trim();
            let shown = match shown.split_once('\n') {
                Some((first, _)) => format!("{} …", first.trim_end_matches([' ', '\\'])),
                None => shown.to_string(),
            };
            let message = idiom.message.clone();
            let help = match (shown.is_empty(), idiom.note) {
                (true, Some(caveat)) => caveat.to_string(),
                (false, Some(caveat)) => format!("Use `{shown}`. {caveat}"),
                (_, None) => format!("Use `{shown}`."),
            };
            report = match (&fixed, idiom.exact, suggested) {
                (Some(fixed), true, false) => {
                    suggested = true;
                    report.suggest(
                        at,
                        if message.contains(&shown) {
                            message.clone()
                        } else {
                            format!("{message}: `{shown}`")
                        },
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
