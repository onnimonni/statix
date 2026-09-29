use crate::{
    Metadata, Report, Rule, Severity,
    github::{self, Packaged},
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

impl Rule for PackagedInNixpkgs {
    fn validate(&self, node: &SyntaxElement) -> Option<Report> {
        let NodeOrToken::Node(node) = node else {
            return None;
        };
        let apply = ast::Apply::cast(node.clone())?;
        let source = github::source_of(&apply)?;
        if github::in_nixpkgs() {
            return None;
        }
        // not nixpkgs' own package against itself
        let candidates: Vec<&Packaged> = github::packaged()
            .get(&source.repo)?
            .iter()
            .filter(|p| !scripts::current_file_ends_with(&p.file))
            .collect();
        // the same package: named alike (or unnamed, and the repository has one)
        let mut attrs: Vec<&str> = candidates.iter().map(|p| p.attr.as_str()).collect();
        attrs.sort_unstable();
        attrs.dedup();
        // `python3Packages.foo` for `buildPythonPackage { pname = "foo"; }`
        let python = source
            .builder
            .as_deref()
            .is_some_and(|b| b.contains("Python"));
        let named = |p: &&&Packaged| {
            let (set, name) = p.attr.rsplit_once('.').unwrap_or(("", p.attr.as_str()));
            source.pname.as_deref() == Some(name) && (set == "python3Packages") == python
        };
        let same = candidates.iter().find(named).or_else(|| {
            (source.pname.is_none() && attrs.len() == 1)
                .then(|| candidates.first())
                .flatten()
        });
        let Some(best) = same else {
            // a monorepo, or a component of it: say what's there
            if attrs.is_empty() {
                return None;
            }
            let list = attrs
                .iter()
                .take(5)
                .map(|a| format!("`pkgs.{a}`"))
                .collect::<Vec<_>>()
                .join(", ");
            return Some(
                self.report().severity(Severity::Hint).diagnostic_with_help(
                    node.text_range(),
                    format!("nixpkgs builds packages from `{}`: {list}", source.repo),
                    "If one of them is what this builds, use it instead of building it here; `overrideAttrs` changes it while keeping nixpkgs' build.".to_string(),
                ),
            );
        };
        let attr = &best.attr;
        let theirs = best.version.as_deref();
        let at = node.text_range();
        let known = |v: Option<&str>| v.map_or_else(String::new, |v| format!(" {v}"));
        let (severity, message, help) = match (source.built, source.version.as_deref(), theirs) {
            // same or newer in nixpkgs
            (true, Some(ours), Some(nix)) => match github::compare_versions(nix, ours) {
                Ordering::Less => return None,
                ord => (
                    Severity::Warn,
                    format!(
                        "`{}` is in nixpkgs as `pkgs.{attr}` ({nix}{})",
                        source.repo,
                        if ord == Ordering::Equal {
                            ", the same version"
                        } else {
                            ", newer"
                        }
                    ),
                    format!(
                        "Use `pkgs.{attr}` instead of building it; to change it, `pkgs.{attr}.overrideAttrs` keeps nixpkgs' build. Versions are from the nixpkgs statix was built with: check yours with `nix eval nixpkgs#{attr}.version`."
                    ),
                ),
            },
            // a commit: can't compare
            (true, _, _) => (
                Severity::Hint,
                format!(
                    "`{}` is in nixpkgs as `pkgs.{attr}`{}",
                    source.repo,
                    known(theirs)
                ),
                format!(
                    "If that version works for you, use `pkgs.{attr}`; to build another revision, `pkgs.{attr}.overrideAttrs (old: {{ src = ...; }})` keeps nixpkgs' build."
                ),
            ),
            // just the source
            (false, _, _) => (
                Severity::Hint,
                format!(
                    "`{}` is the source of `pkgs.{attr}`{}",
                    source.repo,
                    known(theirs)
                ),
                format!(
                    "If that version works for you, `pkgs.{attr}.src` is this source, already fetched and in the binary cache."
                ),
            ),
        };
        Some(
            self.report()
                .severity(severity)
                .diagnostic_with_help(at, message, help),
        )
    }
}
