//! GitHub sources in Nix code (`fetchFromGitHub`, github.com URLs), and an
//! index of the ones nixpkgs packages, built by parsing nixpkgs' files.

use crate::{scripts::context::fn_name, utils};
use rnix::{
    SyntaxKind, SyntaxNode,
    ast::{self, AttrpathValue, Expr, HasEntry as _},
};
use rowan::ast::AstNode as _;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// A GitHub source and the version of what's built from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// `owner/repo`, lower case.
    pub repo: String,
    /// The derivation's `version`, or a version-like `tag`/`rev`.
    pub version: Option<String>,
    /// The derivation's `pname`.
    pub pname: Option<String>,
    /// Whether a builder (`mkDerivation`, `buildGoModule`...) uses it.
    pub built: bool,
    /// The builder is inside another one (a helper derivation).
    pub nested: bool,
    /// The builder's name (`buildPythonPackage`...).
    pub builder: Option<String>,
}

/// Builders whose argument set has `src` and `version`.
fn is_builder(name: &str) -> bool {
    name == "mkDerivation"
        || name.starts_with("build")
        || matches!(name, "mixRelease" | "mkYarnPackage" | "mkDerivationNoCC")
}

/// A binding named `name` in scope at `node`: `let`, the attribute set it's
/// in, or `finalAttrs.name` / `self.name` of the derivation.
fn lookup(node: &SyntaxNode, name: &str) -> Option<Expr> {
    for scope in node.ancestors().skip(1) {
        let entries: Vec<AttrpathValue> = match scope.kind() {
            SyntaxKind::NODE_LET_IN => ast::LetIn::cast(scope.clone())?.attrpath_values().collect(),
            SyntaxKind::NODE_ATTR_SET => ast::AttrSet::cast(scope.clone())?
                .attrpath_values()
                .collect(),
            _ => continue,
        };
        for apv in entries {
            let keys: Vec<_> = apv.attrpath()?.attrs().collect();
            if let [key] = keys.as_slice()
                && utils::attr_name(key).as_deref() == Some(name)
            {
                return apv.value();
            }
        }
    }
    None
}

/// The string `e` evaluates to, when it's made of literals and bindings in
/// scope (`"v${version}"`, `pname`, `finalAttrs.version`).
fn static_str(e: &Expr, depth: usize) -> Option<String> {
    if depth > 6 {
        return None;
    }
    match e {
        Expr::Paren(p) => static_str(&p.expr()?, depth + 1),
        Expr::Str(s) => {
            let mut out = String::new();
            for part in s.normalized_parts() {
                match part {
                    ast::InterpolPart::Literal(t) => out.push_str(&t),
                    ast::InterpolPart::Interpolation(i) => {
                        out.push_str(&static_str(&i.expr()?, depth + 1)?);
                    }
                }
            }
            Some(out)
        }
        Expr::Ident(i) => {
            let name = i.syntax().text().to_string();
            static_str(&lookup(i.syntax(), &name)?, depth + 1)
        }
        // `finalAttrs.version`, `self.pname`
        Expr::Select(sel) => {
            let base = sel.expr()?;
            let attrs: Vec<_> = sel.attrpath()?.attrs().collect();
            let [attr] = attrs.as_slice() else {
                return None;
            };
            if !matches!(
                base.syntax().text().to_string().as_str(),
                "finalAttrs" | "self" | "final" | "attrs"
            ) {
                return None;
            }
            let name = utils::attr_name(attr)?;
            static_str(&lookup(sel.syntax(), &name)?, depth + 1)
        }
        _ => None,
    }
}

/// `v1.2.3`, `1.2.3`, `release-1.2`: the version; `None` for commit hashes.
fn version_of_ref(r: &str) -> Option<String> {
    let v = r
        .trim_start_matches("refs/tags/")
        .trim_start_matches(|c: char| c.is_ascii_alphabetic() || c == '-' || c == '_');
    let looks_like_version = v.starts_with(|c: char| c.is_ascii_digit())
        && v.contains('.')
        && !(r.len() == 40 && r.bytes().all(|b| b.is_ascii_hexdigit()));
    looks_like_version.then(|| v.to_string())
}

/// `owner/repo` of a github.com URL.
fn repo_of_url(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let mut parts = rest.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    let repo = repo.trim_end_matches(".git");
    (!owner.is_empty() && !repo.is_empty()).then(|| format!("{owner}/{repo}").to_lowercase())
}

/// The GitHub source `apply` fetches, if it's `fetchFromGitHub { owner;
/// repo; ... }` or `fetchurl`/`fetchzip`/`fetchgit` of a github.com URL.
#[must_use]
pub fn source_of(apply: &ast::Apply) -> Option<Source> {
    let name = fn_name(&apply.lambda()?)?;
    let Expr::AttrSet(args) = apply.argument()? else {
        return None;
    };
    let get = |key: &str| {
        args.attrpath_values().find_map(|a| {
            let keys: Vec<_> = a.attrpath()?.attrs().collect();
            match keys.as_slice() {
                [k] if utils::attr_name(k).as_deref() == Some(key) => a.value(),
                _ => None,
            }
        })
    };
    let (repo, reference) = match name.as_str() {
        "fetchFromGitHub" => {
            let owner = static_str(&get("owner")?, 0)?;
            let repo = static_str(&get("repo")?, 0)?;
            let reference = get("tag")
                .or_else(|| get("rev"))
                .and_then(|e| static_str(&e, 0));
            (format!("{owner}/{repo}").to_lowercase(), reference)
        }
        "fetchurl" | "fetchzip" | "fetchgit" | "fetchTarball" => {
            let url = static_str(&get("url")?, 0)?;
            // `.../releases/download/v1.2.3/x.tar.gz`, `.../archive/v1.2.3.tar.gz`
            let reference = url
                .split("/releases/download/")
                .nth(1)
                .and_then(|r| r.split('/').next())
                .or_else(|| {
                    url.split("/archive/")
                        .nth(1)
                        .map(|r| r.trim_end_matches(".tar.gz").trim_end_matches(".zip"))
                })
                .map(String::from);
            (repo_of_url(&url)?, reference)
        }
        _ => return None,
    };
    // the derivation it's the `src` of, and whether that's inside another
    let is_build = |a: &ast::Apply| {
        let mut f = a.lambda();
        while let Some(Expr::Apply(inner)) = f {
            f = inner.lambda();
        }
        f.and_then(|f| fn_name(&f)).is_some_and(|n| is_builder(&n))
    };
    let mut builders = apply
        .syntax()
        .ancestors()
        .skip(1)
        .filter_map(ast::Apply::cast)
        .filter(is_build);
    let builder = builders.next();
    let nested = builders.next().is_some();
    let builder_name = builder.as_ref().and_then(|b| {
        let mut f = b.lambda();
        while let Some(Expr::Apply(inner)) = f {
            f = inner.lambda();
        }
        fn_name(&f?)
    });
    let field = |key: &str| {
        let mut arg = builder.as_ref()?.argument()?;
        while let Expr::Paren(p) = &arg {
            arg = p.expr()?;
        }
        let set = match arg {
            Expr::AttrSet(set) => set,
            // `mkDerivation (finalAttrs: { ... })`
            Expr::Lambda(l) => match l.body()? {
                Expr::AttrSet(set) => set,
                _ => return None,
            },
            _ => return None,
        };
        set.attrpath_values().find_map(|a| {
            let keys: Vec<_> = a.attrpath()?.attrs().collect();
            match keys.as_slice() {
                [k] if utils::attr_name(k).as_deref() == Some(key) => static_str(&a.value()?, 0),
                _ => None,
            }
        })
    };
    let version = field("version").or_else(|| reference.as_deref().and_then(version_of_ref));
    Some(Source {
        repo,
        version,
        pname: field("pname"),
        built: builder.is_some(),
        nested,
        builder: builder_name,
    })
}

/// A package nixpkgs builds from a GitHub repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packaged {
    pub attr: String,
    pub version: Option<String>,
    /// The file in nixpkgs defining it (relative), to not report nixpkgs
    /// against itself.
    pub file: String,
}

/// `pkgs/by-name/de/dexter/package.nix`: `dexter`.
fn by_name_attr(rel: &str) -> Option<String> {
    let rest = rel.strip_prefix("pkgs/by-name/")?;
    let mut parts = rest.split('/');
    let (_, name, file) = (parts.next()?, parts.next()?, parts.next()?);
    (file == "package.nix").then(|| name.to_string())
}

/// `pkgs/development/python-modules/foo/default.nix`: `python3Packages.foo`.
fn package_set_attr(rel: &str) -> Option<String> {
    let sets = [
        ("pkgs/development/python-modules/", "python3Packages"),
        ("pkgs/development/ocaml-modules/", "ocamlPackages"),
        ("pkgs/development/lua-modules/", "luaPackages"),
        ("pkgs/development/perl-modules/", "perlPackages"),
        ("pkgs/development/php-packages/", "phpPackages"),
    ];
    sets.iter().find_map(|(dir, set)| {
        let name = rel.strip_prefix(dir)?.split('/').next()?;
        Some(format!("{set}.{name}"))
    })
}

/// All GitHub sources of packages in the nixpkgs checkout at `root`, as
/// `owner/repo<TAB>attr<TAB>version<TAB>file` lines (attribute from
/// `pkgs/by-name`, else the derivation's `pname`).
#[must_use]
pub fn index_nixpkgs(root: &Path) -> Vec<String> {
    use rayon::prelude::*;
    let files: Vec<PathBuf> = walk(&root.join("pkgs"));
    let mut lines: Vec<String> = files
        .par_iter()
        .flat_map_iter(|path| {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            let text = std::fs::read_to_string(path).unwrap_or_default();
            if !text.contains("github") && !text.contains("GitHub") {
                return Vec::new();
            }
            let parsed = rnix::Root::parse(&text).tree();
            let by_name = by_name_attr(&rel);
            parsed
                .syntax()
                .descendants()
                .filter_map(ast::Apply::cast)
                .filter_map(|a| source_of(&a))
                .filter(|s| s.built)
                .filter_map(|s| {
                    // the file's package, or a helper derivation in it
                    let main = !s.nested
                        && (by_name.is_none()
                            || s.pname
                                .as_deref()
                                .is_none_or(|p| by_name.as_deref() == Some(p)));
                    let attr = if main {
                        by_name
                            .clone()
                            .or_else(|| package_set_attr(&rel))
                            .or(s.pname.clone())?
                    } else {
                        s.pname.clone()?
                    };
                    Some(format!(
                        "{}\t{attr}\t{}\t{rel}",
                        s.repo,
                        s.version.unwrap_or_default()
                    ))
                })
                .collect::<Vec<_>>()
        })
        .collect();
    lines.sort();
    lines.dedup();
    lines
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "nix") {
                out.push(p);
            }
        }
    }
    out
}

/// Whether the file being linted is a package of nixpkgs itself (in the
/// index): nixpkgs isn't told to use its own packages.
#[must_use]
pub fn in_nixpkgs() -> bool {
    static FILES: OnceLock<std::collections::HashSet<String>> = OnceLock::new();
    let files = FILES.get_or_init(|| {
        packaged()
            .values()
            .flatten()
            .map(|p| p.file.clone())
            .collect()
    });
    let Some(path) = crate::scripts::current_file() else {
        return false;
    };
    let path = path.to_string_lossy();
    path.match_indices("pkgs/")
        .any(|(i, _)| files.contains(&path[i..]))
}

/// The index `STATIX_NIXPKGS_GITHUB` points at: `owner/repo` -> packages.
#[must_use]
pub fn packaged() -> &'static HashMap<String, Vec<Packaged>> {
    static INDEX: OnceLock<HashMap<String, Vec<Packaged>>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut out: HashMap<String, Vec<Packaged>> = HashMap::new();
        let Some(text) =
            std::env::var_os("STATIX_NIXPKGS_GITHUB").and_then(|p| std::fs::read_to_string(p).ok())
        else {
            return out;
        };
        for line in text.lines() {
            let mut cols = line.split('\t');
            let (Some(repo), Some(attr), Some(version), Some(file)) =
                (cols.next(), cols.next(), cols.next(), cols.next())
            else {
                continue;
            };
            out.entry(repo.to_string()).or_default().push(Packaged {
                attr: attr.to_string(),
                version: (!version.is_empty()).then(|| version.to_string()),
                file: file.to_string(),
            });
        }
        out
    })
}

/// `builtins.compareVersions`: split into numbers and other parts.
#[must_use]
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    fn parts(v: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        for c in v.chars() {
            let boundary = !cur.is_empty()
                && cur.chars().last().is_some_and(|l| l.is_ascii_digit()) != c.is_ascii_digit();
            if c == '.' || c == '-' || boundary {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                if c == '.' || c == '-' {
                    continue;
                }
            }
            cur.push(c);
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out
    }
    // Nix: `pre` is older than anything, a missing part older than the rest
    fn component(x: &str, y: &str) -> std::cmp::Ordering {
        use std::cmp::Ordering::{Equal, Greater, Less};
        match (x, y) {
            _ if x == y => Equal,
            ("pre", _) => Less,
            (_, "pre") => Greater,
            ("", _) => Less,
            (_, "") => Greater,
            _ => match (x.parse::<u64>(), y.parse::<u64>()) {
                (Ok(n), Ok(m)) => n.cmp(&m),
                (Ok(_), Err(_)) => Greater,
                (Err(_), Ok(_)) => Less,
                _ => x.cmp(y),
            },
        }
    }
    let (pa, pb) = (parts(a), parts(b));
    (0..pa.len().max(pb.len()))
        .map(|i| {
            component(
                pa.get(i).map_or("", String::as_str),
                pb.get(i).map_or("", String::as_str),
            )
        })
        .find(|o| o.is_ne())
        .unwrap_or(std::cmp::Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(src: &str) -> Vec<Source> {
        rnix::Root::parse(src)
            .tree()
            .syntax()
            .descendants()
            .filter_map(ast::Apply::cast)
            .filter_map(|a| source_of(&a))
            .collect()
    }

    #[test]
    fn finds_sources() {
        let s = sources(
            r#"stdenv.mkDerivation (finalAttrs: {
  pname = "dexter";
  version = "0.7.2";
  src = fetchFromGitHub { owner = "remoteoss"; repo = finalAttrs.pname; tag = "v${finalAttrs.version}"; hash = ""; };
})"#,
        );
        assert_eq!(
            s,
            [Source {
                repo: "remoteoss/dexter".into(),
                version: Some("0.7.2".into()),
                pname: Some("dexter".into()),
                built: true,
                nested: false,
                builder: Some("mkDerivation".into())
            }]
        );
        let bare = sources(r#"pkgs.fetchFromGitHub { owner = "x"; repo = "y"; rev = "v1.2.3"; }"#);
        assert_eq!(bare[0].version.as_deref(), Some("1.2.3"));
        assert!(!bare[0].built);
        let commit = sources(
            r#"fetchFromGitHub { owner = "x"; repo = "y"; rev = "d575611ed0d0cdcace5022e1c87733b698643909"; }"#,
        );
        assert_eq!(commit[0].version, None);
        let url = sources(
            r#"fetchurl { url = "https://github.com/Foo/Bar/releases/download/v2.0/bar.tar.gz"; }"#,
        );
        assert_eq!(
            (url[0].repo.as_str(), url[0].version.as_deref()),
            ("foo/bar", Some("2.0"))
        );
    }

    #[test]
    fn versions() {
        use std::cmp::Ordering::*;
        assert_eq!(compare_versions("0.7.2", "0.7.10"), Less);
        assert_eq!(compare_versions("1.0", "1.0"), Equal);
        assert_eq!(compare_versions("1.0.1", "1.0"), Greater);
        assert_eq!(compare_versions("1.0pre1", "1.0"), Less);
    }
}
