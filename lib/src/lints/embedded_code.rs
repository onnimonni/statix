use crate::{
    Metadata, Report, Rule,
    scripts::{
        self, Lang, commands,
        embedded::{self, Embedded},
        nixstr::Script,
        text_offset,
    },
};

use macros::lint;
use rnix::{NodeOrToken, SyntaxElement, SyntaxKind, ast};
use rowan::ast::AstNode as _;

/// ## What it does
/// Checks programs written inline in shell scripts: `jq` filters (compiled
/// by jq without running them), `awk` programs (parsed by gawk) and
/// `python -c` code (checked with ruff).
///
/// ## Why is this bad?
/// A syntax error in a quoted jq or awk program only shows when the script
/// runs, often in a build or service that fails much later.
///
/// ## Example
///
/// ```nix
/// pkgs.writeShellScript "x" ''
///   jq '.items[] | .name,' data.json
/// ''
/// ```
///
/// jq reports `syntax error, unexpected end of file`.
#[lint(
    name = "embedded_code",
    note = "Error in a jq, awk or python program inside a shell script",
    code = 34,
    match_with = SyntaxKind::NODE_STRING
)]
struct EmbeddedCode;

/// Where finding `line`/`column` (1-based, in the program) is in the script:
/// exact when the word is the program in quotes as is.
fn position(script: &Script, e: &Embedded, line: usize, column: usize) -> Option<(usize, usize)> {
    let raw = script.text.get(e.start..e.end)?;
    let inner = raw
        .strip_prefix('\'')
        .and_then(|r| r.strip_suffix('\''))
        .or_else(|| raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')))?;
    if inner != e.program {
        return None;
    }
    let at = e.start + 1 + text_offset(&e.program, line, column)?;
    let line_end = script.text[at..].find('\n').map_or(e.end - 1, |n| at + n);
    // an error at the program's end: point at its last character
    let at = at.min(e.end.saturating_sub(2)).max(e.start);
    let end = line_end
        .max(at + 1)
        .min(e.end.saturating_sub(1).max(at + 1));
    Some((at, end))
}

impl Rule for EmbeddedCode {
    fn validate(&self, node: &SyntaxElement) -> Option<Report> {
        let NodeOrToken::Node(node) = node else {
            return None;
        };
        let s = ast::Str::cast(node.clone())?;
        let (script, Lang::Shell(shell), _) = scripts::script_of(&s)? else {
            return None;
        };
        // cheap check before parsing
        if !["jq", "awk", "python"]
            .iter()
            .any(|t| script.text.contains(t))
        {
            return None;
        }
        let commands = commands::commands(&script.text, shell);
        let mut report = self.report();
        for e in embedded::embedded(&commands) {
            let Some(findings) = embedded::check(&e) else {
                continue;
            };
            for f in findings.iter() {
                let range = position(&script, &e, f.line, f.column)
                    .and_then(|(a, b)| script.source_range(a, b))
                    .or_else(|| script.source_range(e.start, e.end));
                let Some(at) = range else { continue };
                let message = format!("{}: {}", e.tool.name(), f.message);
                let message = if f.code == e.tool.name() || f.code == "syntax-error" {
                    message
                } else {
                    format!("{message} ({})", f.code)
                };
                let help = f.url.as_ref().map_or_else(
                    || {
                        format!(
                            "Fix the {} program; it's checked without running it.",
                            e.tool.name()
                        )
                    },
                    |url| format!("See {url}"),
                );
                report = report.diagnostic_with_help(at, message, help);
            }
        }
        (!report.diagnostics.is_empty()).then_some(report)
    }
}
