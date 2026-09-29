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

/// A GitHub source and what's built from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// `owner/repo`, lower case.
    pub repo: String,
    /// The derivation's `version`, or a version-like `tag`/`rev`.
    pub version: Option<String>,
    /// The derivation's `pname`.
    pub pname: Option<String>,
    /// Whether it's the `src` of a builder (`mkDerivation`, `buildGoModule`...).
    pub built: bool,
    /// The builder is inside another one (a helper derivation).
    pub nested: bool,
    /// The builder's name (`buildPythonPackage`...).
    pub builder: Option<String>,
    /// Pinned to a commit (not a tag or release).
    pub pinned: bool,
    /// Why it may not be usable: `broken`, `unfree`, `platforms`.
    pub flags: Vec<&'static str>,
}

/// Builders whose argument set has `src` and `version`.
fn is_builder(name: &str) -> bool {
    name == "mkDerivation"
        || name.starts_with("build")
        || matches!(name, "mixRelease" | "mkYarnPackage" | "mkDerivationNoCC")
}

/// Whether lambda `l` binds `name`.
fn binds(l: &ast::Lambda, name: &str) -> bool {
    match l.param() {
        Some(ast::Param::IdentParam(p)) => p.syntax().text() == name,
        Some(ast::Param::Pattern(pat)) => {
            pat.pat_entries()
                .any(|e| e.ident().is_some_and(|i| i.syntax().text() == name))
                || pat
                    .pat_bind()
                    .and_then(|b| b.ident())
                    .is_some_and(|i| i.syntax().text() == name)
        }
        None => false,
    }
}

/// A name in a set of bindings.
enum Bound {
    Value(Expr),
    /// There, but can't be followed (`inherit`, no value).
    Unknown,
    NotHere,
}

/// `name` in the bindings of `set` (a `let` or attribute set).
fn binding(set: &SyntaxNode, name: &str) -> Bound {
    for entry in set.children() {
        if let Some(apv) = AttrpathValue::cast(entry.clone()) {
            let keys: Vec<_> = apv.attrpath().into_iter().flat_map(|p| p.attrs()).collect();
            if let [key] = keys.as_slice()
                && utils::attr_name(key).as_deref() == Some(name)
            {
                return apv.value().map_or(Bound::Unknown, Bound::Value);
            }
        } else if let Some(inherit) = ast::Inherit::cast(entry)
            && inherit
                .attrs()
                .any(|a| utils::attr_name(&a).as_deref() == Some(name))
        {
            return Bound::Unknown;
        }
    }
    Bound::NotHere
}

/// Identifier `name` at `node` with Nix scoping: `let` and `rec` bindings,
/// shadowed by function arguments; `None` when unknown.
fn lookup(node: &SyntaxNode, name: &str) -> Option<Expr> {
    for scope in node.ancestors().skip(1) {
        let bindings = match scope.kind() {
            SyntaxKind::NODE_LET_IN => true,
            SyntaxKind::NODE_ATTR_SET => {
                ast::AttrSet::cast(scope.clone()).is_some_and(|s| s.rec_token().is_some())
            }
            SyntaxKind::NODE_LAMBDA
                if ast::Lambda::cast(scope.clone()).is_some_and(|l| binds(&l, name)) =>
            {
                return None;
            }
            _ => false,
        };
        if bindings {
            match binding(&scope, name) {
                Bound::Value(v) => return Some(v),
                Bound::Unknown => return None,
                Bound::NotHere => {}
            }
        }
    }
    None
}

/// `finalAttrs.x`: `x` in the attribute set the lambda binding `finalAttrs`
/// returns (`mkDerivation (finalAttrs: { x = ...; })`).
fn final_attr(sel: &ast::Select, base: &str, attr: &str) -> Option<Expr> {
    let lambda = sel
        .syntax()
        .ancestors()
        .filter_map(ast::Lambda::cast)
        .find(
            |l| matches!(l.param(), Some(ast::Param::IdentParam(p)) if p.syntax().text() == base),
        )?;
    let mut body = lambda.body()?;
    while let Expr::Paren(p) = &body {
        body = p.expr()?;
    }
    let Expr::AttrSet(set) = body else {
        return None;
    };
    match binding(set.syntax(), attr) {
        Bound::Value(v) => Some(v),
        _ => None,
    }
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
        // `finalAttrs.version`
        Expr::Select(sel) => {
            let base = sel.expr()?.syntax().text().to_string();
            let attrs: Vec<_> = sel.attrpath()?.attrs().collect();
            let [attr] = attrs.as_slice() else {
                return None;
            };
            let name = utils::attr_name(attr)?;
            static_str(&final_attr(sel, &base, &name)?, depth + 1)
        }
        _ => None,
    }
}

/// `v1.2.3`, `1.2.3`, `release-1.2`: the version; `None` for commit hashes.
fn version_of_ref(r: &str) -> Option<String> {
    let v = r
        .trim_start_matches("refs/tags/")
        .trim_start_matches(|c: char| c.is_ascii_alphabetic() || c == '-' || c == '_');
    let hash = r.len() >= 7 && r.bytes().all(|b| b.is_ascii_hexdigit());
    (v.starts_with(|c: char| c.is_ascii_digit()) && v.contains('.') && !hash).then(|| v.to_string())
}

/// `owner/repo` of a github.com URL of the repository's code: an archive, a
/// release download (not an issue attachment or a raw file).
fn repo_of_url(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let mut parts = rest.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    if !matches!(parts.next(), Some("archive" | "releases")) {
        return None;
    }
    let repo = repo.trim_end_matches(".git");
    (!owner.is_empty() && !repo.is_empty()).then(|| format!("{owner}/{repo}").to_lowercase())
}

fn single_key(apv: &AttrpathValue) -> Option<String> {
    let keys: Vec<_> = apv.attrpath()?.attrs().collect();
    match keys.as_slice() {
        [k] => utils::attr_name(k),
        _ => None,
    }
}

/// The attribute set a builder is called with (`f { }`, `f (x: { })`).
fn builder_set(builder: &ast::Apply) -> Option<ast::AttrSet> {
    let mut arg = builder.argument()?;
    loop {
        match arg {
            Expr::Paren(p) => arg = p.expr()?,
            Expr::Lambda(l) => arg = l.body()?,
            // `finalAttrs: let ... in { ... }`
            Expr::LetIn(l) => arg = l.body()?,
            Expr::AttrSet(set) => return Some(set),
            _ => return None,
        }
    }
}

/// Why nixpkgs' package may not be usable, from its `meta`: `broken` (any
/// value but `false`), an unfree license, a platform list.
fn meta_flags(set: &ast::AttrSet) -> Vec<&'static str> {
    let mut text = String::new();
    for apv in set.attrpath_values() {
        let first = apv
            .attrpath()
            .and_then(|p| p.attrs().next())
            .and_then(|a| utils::attr_name(&a));
        if first.as_deref() == Some("meta") {
            text.push_str(&apv.syntax().to_string());
        }
    }
    let mut out = Vec::new();
    if text.contains("broken") && !text.contains("broken = false") {
        out.push("broken");
    }
    if text.contains("unfree") {
        out.push("unfree");
    }
    // limited to one OS family: may not be there for the user's system
    let (linux, darwin) = (
        text.contains("platforms.linux"),
        text.contains("platforms.darwin"),
    );
    if linux != darwin {
        out.push(if linux { "Linux-only" } else { "Darwin-only" });
    }
    out
}

/// The builder a function application is (`buildGoModule { }`,
/// `stdenv.mkDerivation (finalAttrs: { })`).
fn is_build(a: &ast::Apply) -> bool {
    let mut f = a.lambda();
    while let Some(Expr::Apply(inner)) = f {
        f = inner.lambda();
    }
    f.and_then(|f| fn_name(&f)).is_some_and(|n| is_builder(&n))
}

fn builder_name(b: &ast::Apply) -> Option<String> {
    let mut f = b.lambda();
    while let Some(Expr::Apply(inner)) = f {
        f = inner.lambda();
    }
    fn_name(&f?)
}

/// The GitHub source `apply` fetches, if it's `fetchFromGitHub { owner;
/// repo; ... }` or `fetchurl`/`fetchzip` of a github.com URL, and either a
/// builder's `src` or (`fetchFromGitHub` only) not in a builder at all.
/// Patches, `passthru` and other fetches inside a derivation: `None`.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn source_of(apply: &ast::Apply) -> Option<Source> {
    let name = fn_name(&apply.lambda()?)?;
    let Expr::AttrSet(args) = apply.argument()? else {
        return None;
    };
    let get = |key: &str| {
        args.attrpath_values()
            .find(|a| single_key(a).as_deref() == Some(key))
            .and_then(|a| a.value())
    };
    let (repo, reference) = match name.as_str() {
        "fetchFromGitHub" => {
            let owner = static_str(&get("owner")?, 0)?;
            let repo = static_str(&get("repo")?, 0)?;
            // a `rev`/`tag` that can't be resolved: unknown, like a commit
            let reference = get("tag")
                .or_else(|| get("rev"))
                .map(|e| static_str(&e, 0).unwrap_or_default());
            (format!("{owner}/{repo}").to_lowercase(), reference)
        }
        "fetchurl" | "fetchzip" => {
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
    // `pkg.overrideAttrs (o: { src = ...; })`: already nixpkgs' package
    let overriding = apply
        .syntax()
        .ancestors()
        .skip(1)
        .filter_map(ast::Apply::cast)
        .any(|a| {
            let mut f = a.lambda();
            while let Some(Expr::Apply(inner)) = f {
                f = inner.lambda();
            }
            f.and_then(|f| fn_name(&f)).is_some_and(|n| {
                matches!(
                    n.as_str(),
                    "overrideAttrs" | "override" | "overrideDerivation"
                )
            })
        });
    if overriding {
        return None;
    }
    let builders: Vec<ast::Apply> = apply
        .syntax()
        .ancestors()
        .skip(1)
        .filter_map(ast::Apply::cast)
        .filter(is_build)
        .collect();
    let binding_of = apply
        .syntax()
        .ancestors()
        .skip(1)
        .find_map(AttrpathValue::cast);
    // the builder whose `src = <this>;` it is
    let src_of = binding_of
        .clone()
        .filter(|apv| single_key(apv).as_deref() == Some("src"))
        .and_then(|apv| {
            let set = apv.syntax().parent()?;
            builders
                .iter()
                .find(|b| builder_set(b).is_some_and(|s| s.syntax() == &set))
                .cloned()
        })
        // `let src = fetch...; in mkDerivation { inherit src; }`
        .or_else(|| {
            let apv = binding_of.as_ref()?;
            let let_in = ast::LetIn::cast(apv.syntax().parent()?)?;
            let name = single_key(apv)?;
            let_in
                .syntax()
                .descendants()
                .filter_map(ast::Apply::cast)
                .filter(is_build)
                .find(|b| {
                    builder_set(b).is_some_and(|set| {
                        set.syntax().children().any(|entry| {
                            ast::Inherit::cast(entry.clone()).is_some_and(|i| {
                                i.from().is_none()
                                    && name == "src"
                                    && i.attrs()
                                        .any(|a| utils::attr_name(&a).as_deref() == Some("src"))
                            }) || AttrpathValue::cast(entry).is_some_and(|a| {
                                single_key(&a).as_deref() == Some("src")
                                    && a.value()
                                        .is_some_and(|v| v.syntax().text() == name.as_str())
                            })
                        })
                    })
                })
        });
    let builder = match (src_of, builders.is_empty()) {
        (Some(b), _) => Some(b),
        // not in a derivation: a source fetched for itself
        (None, true) if name == "fetchFromGitHub" => None,
        // a patch, `passthru.x`, a test fixture...
        _ => return None,
    };
    let set = builder.as_ref().and_then(builder_set);
    let field = |key: &str| {
        set.as_ref()?
            .attrpath_values()
            .find(|a| single_key(a).as_deref() == Some(key))
            .and_then(|a| static_str(&a.value()?, 0))
    };
    let pinned = reference
        .as_deref()
        .is_some_and(|r| version_of_ref(r).is_none());
    let version = field("version").or_else(|| reference.as_deref().and_then(version_of_ref));
    let nested = builder.as_ref().is_some_and(|b| {
        b.syntax()
            .ancestors()
            .skip(1)
            .filter_map(ast::Apply::cast)
            .any(|a| is_build(&a))
    });
    Some(Source {
        repo,
        version,
        pname: field("pname"),
        built: builder.is_some(),
        nested,
        builder: builder.as_ref().and_then(builder_name),
        pinned,
        flags: set.as_ref().map(meta_flags).unwrap_or_default(),
    })
}

/// A package nixpkgs builds from a GitHub repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packaged {
    /// Attribute path in `pkgs`.
    pub attr: String,
    pub version: Option<String>,
    /// The file in nixpkgs defining it (relative).
    pub file: String,
    pub pname: Option<String>,
    /// `broken`, `unfree`, `platforms`.
    pub flags: Vec<String>,
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
        let rest = rel.strip_prefix(dir)?;
        let (name, file) = rest.split_once('/')?;
        (file == "default.nix").then(|| format!("{set}.{name}"))
    })
}

/// The GitHub sources of packages in the nixpkgs checkout at `root` whose
/// attribute is known from the file's place (`pkgs/by-name/x/<attr>`,
/// package-set directories), as `owner/repo<TAB>attr<TAB>version<TAB>file
/// <TAB>pname<TAB>flags` lines. Helper derivations inside a package, and
/// packages defined elsewhere, aren't in it: their attribute isn't known.
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
            let Some(attr) = by_name_attr(&rel).or_else(|| package_set_attr(&rel)) else {
                return Vec::new();
            };
            let text = std::fs::read_to_string(path).unwrap_or_default();
            if !text.contains("github") && !text.contains("GitHub") {
                return Vec::new();
            }
            let parsed = rnix::Root::parse(&text).tree();
            parsed
                .syntax()
                .descendants()
                .filter_map(ast::Apply::cast)
                .filter_map(|a| source_of(&a))
                .filter(|s| s.built && !s.nested)
                .map(|s| {
                    format!(
                        "{}\t{attr}\t{}\t{rel}\t{}\t{}",
                        s.repo,
                        s.version.unwrap_or_default(),
                        s.pname.unwrap_or_default(),
                        s.flags.join(",")
                    )
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

/// Whether the file being linted is in a nixpkgs checkout (a parent has
/// `pkgs/top-level/all-packages.nix`): nixpkgs isn't told to use its own
/// packages.
#[must_use]
pub fn in_nixpkgs() -> bool {
    thread_local! {
        static LAST: std::cell::RefCell<Option<(PathBuf, bool)>> =
            const { std::cell::RefCell::new(None) };
    }
    let Some(path) = crate::scripts::current_file() else {
        return false;
    };
    LAST.with(|last| {
        if let Some((p, answer)) = &*last.borrow()
            && *p == path
        {
            return *answer;
        }
        let full = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let answer = full
            .ancestors()
            .skip(1)
            .any(|d| d.join("pkgs/top-level/all-packages.nix").is_file());
        *last.borrow_mut() = Some((path, answer));
        answer
    })
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
            let cols: Vec<&str> = line.split('\t').collect();
            let [repo, attr, version, file, rest @ ..] = cols.as_slice() else {
                continue;
            };
            let nonempty = |s: &&str| (!s.is_empty()).then(|| (*s).to_string());
            out.entry((*repo).to_string()).or_default().push(Packaged {
                attr: (*attr).to_string(),
                version: nonempty(version),
                file: (*file).to_string(),
                pname: rest.first().and_then(nonempty),
                flags: rest
                    .get(1)
                    .map(|f| {
                        f.split(',')
                            .filter(|x| !x.is_empty())
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default(),
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
  meta.broken = stdenv.isDarwin;
})"#,
        );
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].repo, "remoteoss/dexter");
        assert_eq!(s[0].version.as_deref(), Some("0.7.2"));
        assert_eq!(s[0].pname.as_deref(), Some("dexter"));
        assert!(s[0].built && !s[0].pinned);
        assert_eq!(s[0].flags, ["broken"]);
        let bare = sources(r#"pkgs.fetchFromGitHub { owner = "x"; repo = "y"; rev = "v1.2.3"; }"#);
        assert_eq!(bare[0].version.as_deref(), Some("1.2.3"));
        assert!(!bare[0].built);
        let commit = sources(
            r#"fetchFromGitHub { owner = "x"; repo = "y"; rev = "d575611ed0d0cdcace5022e1c87733b698643909"; }"#,
        );
        assert!(commit[0].pinned);
        let url = sources(
            r#"fetchurl { url = "https://github.com/Foo/Bar/releases/download/v2.0/bar.tar.gz"; }"#,
        );
        assert!(
            url.is_empty(),
            "a bare URL fetch (a patch, a file) isn't a source"
        );
    }

    #[test]
    fn scoping() {
        // `org` of the set isn't in scope (not `rec`): the let's is
        let fork = sources(
            r#"let org = "my-fork"; in buildGoModule { pname = "dexter"; version = "0.7.2"; org = "remoteoss"; src = fetchFromGitHub { owner = org; repo = "dexter"; tag = "v0.7.2"; }; }"#,
        );
        assert_eq!(fork[0].repo, "my-fork/dexter");
        // a function argument shadows: unknown
        let arg =
            sources(r#"let owner = "a"; in owner: fetchFromGitHub { inherit owner; repo = "x"; }"#);
        assert!(arg.is_empty());
        // patches and passthru aren't the source
        let aux = sources(
            r#"mkDerivation { pname = "x"; src = ./.; patches = [ (fetchurl { url = "https://github.com/a/b/commit/1.patch"; }) ]; passthru.roms = fetchFromGitHub { owner = "86box"; repo = "roms"; rev = "v1.0"; }; }"#,
        );
        assert!(aux.is_empty());
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
