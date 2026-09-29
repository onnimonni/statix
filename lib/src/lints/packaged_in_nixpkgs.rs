use crate::{
    Metadata, Report, Rule, Severity,
    github::{self, Packaged, Source},
    scripts,
};

use macros::lint;
use rnix::{NodeOrToken, SyntaxElement, SyntaxKind, ast};
use rowan::ast::AstNode as _;
use std::cmp::Ordering;

/// ## What it does
/// Checks for software built from GitHub (`fetchFromGitHub`, release
/// downloads) that nixpkgs already packages in the same or a newer version.
///
/// ## Why is this bad?
/// A package of your own has to be written, reviewed and kept up to date;
/// the one in nixpkgs is maintained, tested and in the binary cache. For a
/// change to it, `pkgs.x.overrideAttrs` keeps nixpkgs' build.
///
/// ## Example
///
/// ```nix
/// packages = [
///   (pkgs.buildGoModule {
///     pname = "dexter";
///     version = "0.7.2";
///     src = pkgs.fetchFromGitHub { owner = "remoteoss"; repo = "dexter"; tag = "v0.7.2"; hash = "..."; };
///     vendorHash = "...";
///   })
/// ];
/// ```
///
/// Use the package from nixpkgs:
///
/// ```nix
/// packages = [ pkgs.dexter ];
/// ```
#[lint(
    name = "packaged_in_nixpkgs",
    note = "Software from GitHub that nixpkgs already packages",
    code = 35,
    match_with = SyntaxKind::NODE_APPLY
)]
struct PackagedInNixpkgs;

/// Names alike, as nixpkgs spells them: `pytest_twisted` is `pytest-twisted`.
fn same_name(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.to_ascii_lowercase().replace('_', "-");
    norm(a) == norm(b)
}

/// `# statix disable=packaged_in_nixpkgs` on the line before the binding or
/// list element the node is in.
fn disabled(node: &rnix::SyntaxNode) -> bool {
    node.ancestors()
        .filter(|n| {
            n.kind() == SyntaxKind::NODE_ATTRPATH_VALUE
                || n.parent()
                    .is_some_and(|p| p.kind() == SyntaxKind::NODE_LIST)
        })
        .any(|n| {
            let mut token = n.first_token().and_then(|t| t.prev_token());
            while let Some(t) = token {
                match t.kind() {
                    SyntaxKind::TOKEN_WHITESPACE => {}
                    SyntaxKind::TOKEN_COMMENT => {
                        return scripts::directives::parse_line(t.text()).is_some_and(|d| {
                            d.disable
                                .iter()
                                .any(|x| x == "packaged_in_nixpkgs" || x == "all")
                        });
                    }
                    _ => return false,
                }
                token = t.prev_token();
            }
            false
        })
}

/// What nixpkgs has for a source: the same package, or only others from
/// the repository.
enum Match<'a> {
    Same(&'a Packaged),
    Others(Vec<&'a str>),
}

/// The nixpkgs package `source` builds, if any: named alike (`pname` like
/// the attribute or nixpkgs' `pname`), the newest of those; unnamed, the
/// repository's one package.
fn find_match<'a>(source: &Source, candidates: &[&'a Packaged]) -> Option<Match<'a>> {
    let mut attrs: Vec<&str> = candidates.iter().map(|p| p.attr.as_str()).collect();
    attrs.sort_unstable();
    attrs.dedup();
    // Python packages are in `python3Packages`; applications may be too
    let builder = source.builder.as_deref().unwrap_or("");
    let python_only = builder.contains("PythonPackage");
    let python_app = builder.contains("PythonApplication");
    let fits = |p: &Packaged| match p.attr.rsplit_once('.').map_or("", |(s, _)| s) {
        "python3Packages" => python_only || python_app,
        "" => !python_only,
        _ => false,
    };
    let named: Vec<&Packaged> = candidates
        .iter()
        .copied()
        .filter(|p| {
            let name = p.attr.rsplit_once('.').map_or(p.attr.as_str(), |(_, n)| n);
            source.pname.as_deref().is_some_and(|ours| {
                same_name(ours, name) || p.pname.as_deref().is_some_and(|t| same_name(ours, t))
            }) && fits(p)
        })
        .collect();
    let same = if named.is_empty() {
        // a bare source, a builder without `pname`: not a package set's
        if source.pname.is_none() && attrs.len() == 1 && fits(candidates[0]) {
            vec![candidates[0]]
        } else {
            Vec::new()
        }
    } else {
        named
    };
    // the newest of them (`abseil-cpp_202505`, `abseil-cpp_202601`)
    let best = same.into_iter().max_by(|a, b| {
        github::compare_versions(
            a.version.as_deref().unwrap_or(""),
            b.version.as_deref().unwrap_or(""),
        )
    });
    match best {
        Some(p) => Some(Match::Same(p)),
        None if attrs.is_empty() || !source.built => None,
        None => Some(Match::Others(attrs)),
    }
}

/// Severity, message and help for `source` against nixpkgs' `best`, or
/// `None` when nixpkgs' version is older.
fn verdict(source: &Source, best: &Packaged) -> Option<(Severity, String, String)> {
    let attr = &best.attr;
    let theirs = best.version.as_deref();
    let shown = theirs.map_or_else(String::new, |v| format!(" {v}"));
    let repo = &source.repo;
    // nixpkgs has an older version: nothing to suggest
    if let (Some(ours), Some(nix)) = (source.version.as_deref(), theirs)
        && !source.pinned
        && github::compare_versions(nix, ours) == Ordering::Less
    {
        return None;
    }
    let hint = |message: String, help: String| Some((Severity::Hint, message, help));
    if !source.built {
        let version = theirs.map_or_else(String::new, |v| format!(" ({v})"));
        return hint(
            format!("`{repo}` is the source of `pkgs.{attr}`{shown}"),
            format!(
                "If nixpkgs' version{version} does, `pkgs.{attr}.src` is this source, already fetched and in the binary cache."
            ),
        );
    }
    if source.pinned {
        return hint(
            format!("`{repo}` is in nixpkgs as `pkgs.{attr}`{shown}"),
            format!(
                "This builds a commit; if nixpkgs' version does, use `pkgs.{attr}`, or `pkgs.{attr}.overrideAttrs (old: {{ src = ...; }})` to build this revision with nixpkgs' build."
            ),
        );
    }
    // broken, unfree or for one OS only in nixpkgs: it may not do
    if !best.flags.is_empty() {
        let why = best.flags.join(", ");
        return hint(
            format!("`{repo}` is in nixpkgs as `pkgs.{attr}`{shown} ({why} there)"),
            format!(
                "nixpkgs' package is {why}; if it works for you, use `pkgs.{attr}` or `pkgs.{attr}.overrideAttrs`. Silence this with `# statix disable=packaged_in_nixpkgs` on the line before."
            ),
        );
    }
    let (Some(ours), Some(nix)) = (source.version.as_deref(), theirs) else {
        return hint(
            format!("`{repo}` is in nixpkgs as `pkgs.{attr}`{shown}"),
            format!(
                "If nixpkgs' version does, use `pkgs.{attr}`; `pkgs.{attr}.overrideAttrs` changes it while keeping nixpkgs' build."
            ),
        );
    };
    let newer = if github::compare_versions(nix, ours) == Ordering::Equal {
        "the same version"
    } else {
        "newer"
    };
    Some((
        Severity::Warn,
        format!("`{repo}` is in nixpkgs as `pkgs.{attr}` ({nix}, {newer})"),
        format!(
            "Use `pkgs.{attr}` instead of building it; to change it, `pkgs.{attr}.overrideAttrs` keeps nixpkgs' build. Versions are from the nixpkgs statix was built with: check yours with `nix eval nixpkgs#{attr}.version`. If you build it on purpose, add `# statix disable=packaged_in_nixpkgs` on the line before."
        ),
    ))
}

impl Rule for PackagedInNixpkgs {
    fn validate(&self, node: &SyntaxElement) -> Option<Report> {
        let NodeOrToken::Node(node) = node else {
            return None;
        };
        let apply = ast::Apply::cast(node.clone())?;
        let source = github::source_of(&apply)?;
        if github::in_nixpkgs() || disabled(node) {
            return None;
        }
        let candidates: Vec<&Packaged> = github::packaged().get(&source.repo)?.iter().collect();
        let (severity, message, help) = match find_match(&source, &candidates)? {
            Match::Same(best) => verdict(&source, best)?,
            // a monorepo, or a component of it: say what's there
            Match::Others(attrs) => {
                let list = attrs
                    .iter()
                    .take(5)
                    .map(|a| format!("`pkgs.{a}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                (
                    Severity::Hint,
                    format!("nixpkgs builds packages from `{}`: {list}", source.repo),
                    "If one of them is what this builds, use it instead of building it here; `overrideAttrs` changes it while keeping nixpkgs' build.".to_string(),
                )
            }
        };
        Some(self.report().severity(severity).diagnostic_with_help(
            node.text_range(),
            message,
            help,
        ))
    }
}
