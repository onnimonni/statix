use crate::{
    Metadata, Report, Rule,
    scripts::{
        self, Lang,
        commands::{self, Use},
        declared::{self, Cond, Declared},
        directives::{self, Scoped},
        nixstr::{PLACEHOLDER, Script},
        programs::{self, Programs},
    },
};

use macros::lint;
use rnix::{NodeOrToken, SyntaxElement, SyntaxKind, TextRange, ast};
use rowan::ast::AstNode as _;

/// ## What it does
/// Checks that the commands a shell script written in Nix runs are declared
/// and exist:
///
/// - `writeShellApplication`: in `runtimeInputs`
/// - devenv `scripts`, `tasks`, `processes`, `enterShell`, `enterTest`: in
///   `packages`, `scripts.<name>.packages`, other scripts, enabled languages
///   and services, or the stdenv tools every devenv shell has
/// - NixOS `systemd.services.<name>` scripts: in `path` or the default path
///
/// Also checks `${pkgs.X}/bin/Y` and `${lib.getExe' pkgs.X "Y"}` (does X
/// have program Y?), absolute command paths outside the Nix store, and
/// relative paths in devenv scripts (do they exist?).
///
/// Commands are checked on the Linux systems and the one statix runs on
/// (`systems` in `statix.toml`), using nix-index-database data when the
/// statix package provides it (`STATIX_PROGRAMS`). Declarations inside
/// `lib.optionals stdenv.isDarwin [...]`, `lib.mkIf`, `if` and similar only
/// count on those platforms.
///
/// Commands checked with `command -v`, `type`, `which` or `hash` are
/// optional and never reported. Directives, like shellcheck's:
///
/// ```sh
/// # statix platforms=darwin          # the next command (or the script, at the top) runs on macOS only
/// # statix provided=pbcopy,osascript # the environment provides these
/// # statix disable=undeclared_command
/// ```
///
/// ## Why is this bad?
/// An undeclared command only works because it happens to be installed on
/// the machine running the script.
///
/// ## Example
///
/// ```nix
/// pkgs.writeShellApplication {
///   name = "x";
///   runtimeInputs = [ pkgs.curl ];
///   text = ''
///     curl -s "$1" | jq .
///   '';
/// }
/// ```
///
/// Declare `jq`:
///
/// ```nix
/// pkgs.writeShellApplication {
///   name = "x";
///   runtimeInputs = [ pkgs.curl pkgs.jq ];
///   text = ''
///     curl -s "$1" | jq .
///   '';
/// }
/// ```
#[lint(
    name = "undeclared_command",
    note = "Command in a shell script isn't declared or doesn't exist",
    code = 31,
    match_with = [SyntaxKind::NODE_STRING, SyntaxKind::TOKEN_COMMENT]
)]
struct UndeclaredCommand;

/// Absolute paths `impure_host_path` already reports.
const HOST_PATHS: &[&str] = &["/usr/local/", "/opt/homebrew/", "/usr/bin/", "/bin/bash"];
/// Absolute commands every system has.
const STANDARD_PATHS: &[&str] = &["/bin/sh", "/usr/bin/env"];

struct Finding {
    at: TextRange,
    message: String,
    help: String,
}

impl Rule for UndeclaredCommand {
    fn validate(&self, node: &SyntaxElement) -> Option<Report> {
        let node = match node {
            NodeOrToken::Node(node) => node,
            // `# statix platforms=...` before a package in a Nix list
            NodeOrToken::Token(comment) => {
                let d = directives::parse_line(comment.text())?;
                if d.unknown.is_empty() {
                    return None;
                }
                return Some(self.report().diagnostic_with_help(
                    comment.text_range(),
                    unknown_directive(&d.unknown),
                    DIRECTIVES_HELP.into(),
                ));
            }
        };
        let s = ast::Str::cast(node.clone())?;
        let (script, lang, kind) = scripts::script_of(&s)?;
        // pieces of a concatenated script: where commands start is unknown
        if kind == scripts::Kind::Fragment {
            return None;
        }
        let Lang::Shell(shell) = lang else {
            return None;
        };
        // other scripts (writeShellScript, stdenv phases): declarations are
        // unknown, but paths and interpolated programs are still checked
        let declared = declared::declared(&s, lang).unwrap_or_else(|| Declared {
            incomplete: true,
            platforms: declared::file_platforms(s.syntax()),
            ..Declared::default()
        });
        let findings = check(&s, &script, shell, &declared);
        if findings.is_empty() {
            return None;
        }
        let mut report = self.report();
        for f in findings {
            report = report.diagnostic_with_help(f.at, f.message, f.help);
        }
        Some(report)
    }
}

const DIRECTIVES_HELP: &str = "Directives are `# statix platforms=darwin,linux`, `# statix provided=cmd1,cmd2` and `# statix disable=<lint>`; platforms are `darwin`, `linux`, `aarch64`, `x86_64`, `unix`, `all` or a system like `aarch64-darwin`.";

fn unknown_directive(unknown: &[String]) -> String {
    format!("Unknown statix directive `{}`", unknown.join(" "))
}

/// Commands every host has, never reported.
const ALWAYS_AVAILABLE: &[&str] = &["rm"];

fn systems_text(systems: &[String]) -> String {
    systems.join(", ")
}

/// The `${...}` source of the placeholder at byte `at` of the script text.
fn interp_source(s: &ast::Str, script: &Script, u: &Use) -> Option<String> {
    // the word may start with a quote: `"${pkgs.jq}/bin/jq"`
    let at = u.start + script.text.get(u.start..u.end)?.find(PLACEHOLDER)?;
    let src = script.src_at(at).filter(|src| src.interp)?;
    let base: usize = s.syntax().text_range().start().into();
    let text = s.syntax().to_string();
    let interp = text.get(src.start - base..src.end - base)?;
    Some(
        interp
            .strip_prefix("${")?
            .strip_suffix('}')?
            .trim()
            .to_string(),
    )
}

/// Whether the bare placeholder `u` (a whole command word) may be a script
/// snippet (`${setup}`, `${cfg.hook}`) rather than a program
/// (`${lib.getExe pkgs.jq}`, `${pkgs.hello}`).
fn is_snippet(s: &ast::Str, script: &Script, u: &Use) -> bool {
    let Some(expr) = interp_source(s, script, u).and_then(|i| parse_expr(&i)) else {
        return true;
    };
    match &expr {
        // lib.getExe pkgs.jq; lib.optionalString c "f() { ...; }" is a snippet
        ast::Expr::Apply(a) => {
            let mut f = a.lambda();
            while let Some(ast::Expr::Apply(inner)) = f {
                f = inner.lambda();
            }
            !f.and_then(|f| scripts::context::fn_name(&f))
                .is_some_and(|n| n == "getExe" || n == "getExe'")
        }
        e => !e.syntax().text().to_string().starts_with("pkgs."),
    }
}

fn parse_expr(text: &str) -> Option<ast::Expr> {
    rnix::Root::parse(text).ok().ok()?.expr()
}

/// Package and program of an interpolated command: `${pkgs.X}/bin/Y`,
/// `${lib.getBin pkgs.X}/sbin/Y`, `${lib.getExe' pkgs.X "Y"}`.
fn interpolated(s: &ast::Str, script: &Script, u: &Use) -> Option<(String, String)> {
    let interp = interp_source(s, script, u)?;
    // `${pkgs.jq}/bin/jq`; `${pkg}/bin/x` is a local variable
    if !interp.contains("pkgs.") {
        return None;
    }
    let expr = parse_expr(&interp)?;
    if u.name == PLACEHOLDER {
        // lib.getExe' pkgs.X "Y"
        let ast::Expr::Apply(a) = &expr else {
            return None;
        };
        let ast::Expr::Apply(inner) = a.lambda()? else {
            return None;
        };
        if scripts::context::fn_name(&inner.lambda()?)? != "getExe'" {
            return None;
        }
        let attr = declared::package_attr(&inner.argument()?)?;
        let ast::Expr::Str(program) = a.argument()? else {
            return None;
        };
        let program = crate::utils::attr_name(&ast::Attr::Str(program))?;
        return Some((attr, program));
    }
    let rest = u.name.strip_prefix(PLACEHOLDER)?;
    let program = rest
        .strip_prefix("/bin/")
        .or_else(|| rest.strip_prefix("/sbin/"))?;
    if program.contains('/') {
        return None;
    }
    Some((declared::package_attr(&expr)?, program.to_string()))
}

/// Platform condition and provided commands from the directives covering
/// byte `at`, and whether `undeclared_command` is disabled there.
fn directives_at(directives: &[Scoped], at: usize) -> (Cond, Vec<String>, bool) {
    let mut cond = Cond::True;
    let mut provided = Vec::new();
    let mut disabled = false;
    for d in directives
        .iter()
        .filter(|d| d.scope.0 <= at && at < d.scope.1.max(d.scope.0 + 1))
    {
        if let Some(platforms) = &d.directive.platforms {
            let values: Vec<&str> = platforms.iter().map(String::as_str).collect();
            if let Some(c) = Cond::from_platforms(&values) {
                cond = cond.and(c);
            }
        }
        provided.extend(d.directive.provided.iter().cloned());
        disabled |= d
            .directive
            .disable
            .iter()
            .any(|x| x == "undeclared_command" || x == "all");
    }
    (cond, provided, disabled)
}

#[allow(clippy::too_many_lines)]
fn check(s: &ast::Str, script: &Script, shell: &str, declared: &Declared) -> Vec<Finding> {
    let programs = programs::programs();
    let settings = programs::settings();
    let available = |system: &str, attr: &str| programs.available(system, attr);
    let commands = commands::commands(&script.text, shell);
    let directives = directives::scoped(&script.text, &commands.spans, &commands.data);
    // functions may come from sourced files or interpolated snippets
    // (`${setup}` on its own): commands can't be called undeclared
    let opaque = commands.sources
        || commands
            .uses
            .iter()
            .any(|u| u.name == PLACEHOLDER && is_snippet(s, script, u));
    let range = |start: usize, end: usize| script.source_range(start, end);

    let mut out: Vec<Finding> = Vec::new();
    // unknown directive keys and values
    for d in &directives {
        if !d.directive.unknown.is_empty()
            && let Some(at) = range(d.at.0, d.at.1)
        {
            out.push(Finding {
                at,
                message: unknown_directive(&d.directive.unknown),
                help: DIRECTIVES_HELP.into(),
            });
        }
    }

    let script_cond = declared.platforms.clone().unwrap_or(Cond::True);
    let systems = programs::checked_systems();
    let mut reported: Vec<String> = Vec::new();

    for u in &commands.uses {
        let name = u.name.as_str();
        if commands.probed.contains(name) || reported.iter().any(|r| r == name) {
            continue;
        }
        let (use_cond, provided, disabled) = directives_at(&directives, u.start);
        if ALWAYS_AVAILABLE.contains(&name) {
            continue;
        }
        if disabled
            || provided.iter().any(|p| p == name)
            || settings.provided.iter().any(|p| p == name)
        {
            continue;
        }
        let effective: Vec<String> = systems
            .iter()
            .filter(|sys| script_cond.allows(sys, &available) && use_cond.allows(sys, &available))
            .cloned()
            .collect();
        if effective.is_empty() {
            continue;
        }
        let Some(at) = range(u.start, u.end) else {
            continue;
        };
        let finding = if name.contains(PLACEHOLDER) {
            check_interpolated(s, script, u, programs, &effective, at)
        } else if name.starts_with('/') {
            check_absolute(name, at)
        } else if name.contains('/') {
            check_relative(name, declared, commands.changes_dir, at)
        } else if opaque {
            None
        } else {
            check_declared(name, declared, programs, &effective, &available, at)
        };
        if let Some(f) = finding {
            reported.push(name.to_string());
            out.push(f);
        }
    }
    out.sort_by_key(|f| f.at.start());
    out
}

fn check_interpolated(
    s: &ast::Str,
    script: &Script,
    u: &Use,
    programs: &Programs,
    effective: &[String],
    at: TextRange,
) -> Option<Finding> {
    let (attr, program) = interpolated(s, script, u)?;
    let missing: Vec<String> = effective
        .iter()
        .filter(|sys| programs.available(sys, &attr) == Some(true))
        .filter(|sys| programs.provides(sys, &attr, &program) == Some(false))
        .cloned()
        .collect();
    if missing.is_empty() {
        return None;
    }
    let on = if missing.len() == effective.len() {
        String::new()
    } else {
        format!(" on {}", systems_text(&missing))
    };
    Some(Finding {
        at,
        message: format!("`pkgs.{attr}` has no program `{program}`{on}"),
        help: format!(
            "Check the program name: `pkgs.{attr}` doesn't have `bin/{program}`. If the program comes from another package, interpolate that one (`${{lib.getExe' pkgs.<package> \"{program}\"}}`)."
        ),
    })
}

fn check_absolute(name: &str, at: TextRange) -> Option<Finding> {
    if name.starts_with("/nix/store/")
        || STANDARD_PATHS.contains(&name)
        || HOST_PATHS.iter().any(|h| name.starts_with(h))
    {
        return None;
    }
    let program = name.rsplit('/').next().unwrap_or(name);
    Some(Finding {
        at,
        message: format!("`{name}` depends on the host system"),
        help: format!(
            "Use the program from nixpkgs instead of a path on the machine: interpolate it (`${{lib.getExe' pkgs.<package> \"{program}\"}}`) or declare the package and call `{program}`."
        ),
    })
}

fn check_relative(
    name: &str,
    declared: &Declared,
    changes_dir: bool,
    at: TextRange,
) -> Option<Finding> {
    // devenv runs scripts in the project root, the directory of devenv.nix;
    // for other modules, or after `cd`, it's unknown
    if !declared.devenv || changes_dir || !scripts::current_file_is("devenv.nix") {
        return None;
    }
    let dir = scripts::current_dir()?;
    let path = dir.join(name.trim_start_matches("./"));
    if path.exists() {
        scripts::note_dependency(path);
        return None;
    }
    Some(Finding {
        at,
        message: format!("`{name}` doesn't exist"),
        help: "devenv runs scripts in the project root (`$DEVENV_ROOT`), and the path doesn't exist there. Fix the path, or refer to the file from Nix (`${./path}`) so it's copied to the store.".into(),
    })
}

fn check_declared(
    name: &str,
    declared: &Declared,
    programs: &Programs,
    effective: &[String],
    available: &dyn Fn(&str, &str) -> Option<bool>,
    at: TextRange,
) -> Option<Finding> {
    if declared.commands.iter().any(|c| c == name) {
        return None;
    }
    let mut undeclared = Vec::new();
    let mut unavailable: Vec<(String, String)> = Vec::new();
    for sys in effective {
        if programs::os_provides(sys, name) {
            continue;
        }
        let mut declared_here = false;
        for p in declared
            .packages
            .iter()
            .filter(|p| p.cond.allows(sys, available))
        {
            match programs.provides(sys, &p.attr, name) {
                Some(true) | None => declared_here = true,
                Some(false) => {
                    // declared, provides it elsewhere, but isn't available here
                    if programs.available(sys, &p.attr) == Some(false)
                        && programs::INDEXED_SYSTEMS
                            .iter()
                            .any(|other| programs.provides(other, &p.attr, name) == Some(true))
                    {
                        unavailable.push((sys.clone(), p.attr.clone()));
                    }
                }
            }
            if declared_here {
                break;
            }
        }
        if !declared_here {
            undeclared.push(sys.clone());
        }
    }
    // unknown declarations may provide it, on any system
    if undeclared.is_empty() || declared.incomplete {
        return None;
    }
    let on = |systems: &[String]| {
        if systems.len() == effective.len() {
            String::new()
        } else {
            format!(" on {}", systems_text(systems))
        }
    };
    let unavailable_systems: Vec<String> = unavailable
        .iter()
        .map(|(s, _)| s.clone())
        .filter(|s| undeclared.contains(s))
        .collect();
    if let Some((_, attr)) = unavailable.first()
        && unavailable_systems.len() == undeclared.len()
    {
        let others: Vec<&str> = ["linux", "darwin"]
            .into_iter()
            .filter(|family| {
                !undeclared
                    .iter()
                    .any(|s| programs::platform_matches(family, s) == Some(true))
            })
            .collect();
        let only = others.first().copied().unwrap_or("linux");
        let is = if only == "darwin" {
            "isDarwin"
        } else {
            "isLinux"
        };
        return Some(Finding {
            at,
            message: format!(
                "`{name}` (`pkgs.{attr}`) isn't available{}",
                on(&undeclared)
            ),
            help: format!(
                "`pkgs.{attr}` doesn't exist on {}. Declare it only where it exists, e.g. `lib.optionals pkgs.stdenv.{is} [ pkgs.{attr} ]`, and mark its use with `# statix platforms={only}` on the line before.",
                systems_text(&undeclared)
            ),
        });
    }
    let suggestion = programs.providers(name).into_iter().next().map_or_else(
        || format!("the package providing `{name}`"),
        |p| format!("`pkgs.{p}`"),
    );
    Some(Finding {
        at,
        message: format!("`{name}` isn't declared{}", on(&undeclared)),
        help: format!(
            "Add {suggestion} to `{}`. If the environment provides it, add `# statix provided={name}` at the top of the script (or `provided = [ \"{name}\" ]` in statix.toml); if it's optional, check for it with `command -v {name}`.",
            declared.place
        ),
    })
}
