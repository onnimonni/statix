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
    // `passthru.tests.buildPhase`: not the derivation's phase
    if utils::enclosing_attrpath(apv.syntax()).is_some_and(|p| p.iter().any(|k| k == "passthru")) {
        return None;
    }
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
    // `shellHook` runs in a shell, not a build
    let shell_hook = node
        .parent()
        .and_then(AttrpathValue::cast)
        .and_then(|apv| utils::attr_name(&apv.attrpath()?.attrs().last()?))
        .is_some_and(|k| k == "shellHook");
    let build = (kind == Kind::Hook && !shell_hook) || run_command;
    idioms::Context {
        gnu: build || devenv || linux,
        build,
        bash: shell == "bash",
        phase: phase_of(s).is_some(),
    }
}

/// Opening and closing brace (as bytes: brace literals confuse tools that
/// count braces).
const OPEN: u8 = 0x7b;
const CLOSE: u8 = 0x7d;

/// The interpolations (dollar, brace ... brace) in Nix source `text`, in
/// order.
fn interpolations(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        // two single quotes before the dollar escape it: literal text
        let escaped = i >= 2 && &bytes[i - 2..i] == b"''";
        if bytes[i] == b'$' && bytes[i + 1] == OPEN && !escaped {
            let mut depth = 0;
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == OPEN {
                    depth += 1;
                } else if bytes[j] == CLOSE {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                j += 1;
            }
            out.push(text[i..=j.min(bytes.len() - 1)].to_string());
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
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
fn missing_run_hooks(
    s: &ast::Str,
    text: &str,
    commands: &commands::Commands,
) -> Vec<idioms::Idiom> {
    let Some(hook) = phase_of(s) else {
        return Vec::new();
    };
    let body = text.trim();
    // `${old.installPhase} ...`: the interpolated phase runs the hooks
    let interpolates_phase = s.normalized_parts().iter().any(|p| {
        matches!(p, ast::InterpolPart::Interpolation(i) if i.syntax().text().to_string().contains("Phase"))
    });
    if interpolates_phase {
        return Vec::new();
    }
    // `installPhase = "true";`, `":"`: nothing to wrap
    if body.is_empty() || matches!(body, "true" | ":") {
        return Vec::new();
    }
    let (pre, post) = (format!("runHook pre{hook}"), format!("runHook post{hook}"));
    // `runHook preInstall`, `runHook "preInstall"`: the parsed calls
    let runs = |name: &str| {
        commands
            .uses
            .iter()
            .any(|u| u.name == "runHook" && u.args.first().and_then(Option::as_deref) == Some(name))
    };
    let start = text.len() - text.trim_start().len();
    let end = text.trim_end().len();
    let mut out = Vec::new();
    let note = Some(
        "Overriding a phase skips its hooks unless it runs them: `preInstall`/`postInstall` from the derivation and from setup hooks. Leave them out only on purpose.",
    );
    if !runs(&format!("pre{hook}")) {
        out.push(idioms::Idiom {
            start,
            end: start,
            replacement: vec![idioms::Piece::Text(format!("{pre}\n"))],
            message: format!("The phase doesn't run `{pre}`"),
            note,
            exact: false,
        });
    }
    if !runs(&format!("post{hook}")) {
        out.push(idioms::Idiom {
            start: end,
            end,
            replacement: vec![idioms::Piece::Text(format!("\n{post}"))],
            message: format!("The phase doesn't run `{post}`"),
            note,
            exact: false,
        });
    }
    out
}

impl Rule for ShellIdiom {
    fn validate(&self, node: &SyntaxElement) -> Option<Report> {
        self.validate_all(node).into_iter().next()
    }

    /// Warnings (fixable idioms) and hints in separate reports: severity is
    /// per report.
    #[allow(clippy::too_many_lines)]
    fn validate_all(&self, node: &SyntaxElement) -> Vec<Report> {
        let NodeOrToken::Node(node) = node else {
            return Vec::new();
        };
        let Some(s) = ast::Str::cast(node.clone()) else {
            return Vec::new();
        };
        let Some((script, Lang::Shell(shell), kind)) = scripts::script_of(&s) else {
            return Vec::new();
        };
        let ctx = context(&s, kind, shell);
        let commands = commands::commands(&script.text, shell);
        let mut found = idioms::idioms_in(&script.text, &commands, ctx);
        found.extend(missing_run_hooks(&s, &script.text, &commands));
        found.sort_by_key(|i| i.start);
        if found.is_empty() {
            return Vec::new();
        }
        // words as written in Nix (`${...}`, not the placeholder)
        let base: usize = s.syntax().text_range().start().into();
        let source = s.syntax().to_string();
        let nix_word = |a: usize, b: usize| {
            let r = script.source_range(a, b)?;
            let (a, b): (usize, usize) = (r.start().into(), r.end().into());
            source.get(a - base..b - base).map(String::from)
        };
        // a fix puts `${...}` back in order: exact only if they keep it
        for idiom in &mut found {
            if idiom.exact {
                let before = nix_word(idiom.start, idiom.end).map(|t| interpolations(&t));
                let after = idiom.render(nix_word).map(|t| interpolations(&t));
                if before.is_none() || before != after {
                    idiom.exact = false;
                }
            }
        }
        // fix the exact ones together; those that can't apply even alone
        // are hints, those that clash with others are fixed in a later pass
        let mut accepted: Vec<Change> = Vec::new();
        let mut later = Vec::new();
        for (i, idiom) in found.iter_mut().enumerate() {
            if !idiom.exact {
                continue;
            }
            let Some(c) = change(&script, idiom) else {
                idiom.exact = false;
                continue;
            };
            let mut with = accepted.clone();
            with.push(c.clone());
            if script.apply(&s, &with).is_some() {
                accepted = with;
            } else if script.apply(&s, &[c]).is_some() {
                later.push(i);
            } else {
                idiom.exact = false;
            }
        }
        let fixed = script.apply(&s, &accepted);
        let mut warnings = self.report();
        let mut hints = self.report().severity(crate::Severity::Hint);
        let mut suggested = false;
        for (i, idiom) in found.iter().enumerate() {
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
            if !idiom.exact {
                hints = hints.diagnostic_with_help(at, message, help);
                continue;
            }
            warnings = match (&fixed, later.contains(&i), suggested) {
                (Some(fixed), false, false) => {
                    suggested = true;
                    let message = if message.contains(&shown) {
                        message
                    } else {
                        format!("{message}: `{shown}`")
                    };
                    warnings.suggest(
                        at,
                        message,
                        Suggestion::with_replacement(
                            s.syntax().text_range(),
                            fixed.syntax().clone(),
                        ),
                    )
                }
                _ => warnings.diagnostic_fixed_later(at, message),
            };
        }
        [warnings, hints]
            .into_iter()
            .filter(|r| !r.diagnostics.is_empty())
            .collect()
    }
}
