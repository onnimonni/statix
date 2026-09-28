//! What a script is allowed to run: the packages and commands its context
//! declares (`writeShellApplication` `runtimeInputs`, devenv `packages`,
//! systemd `path`...), each with the platforms it's declared on.

use super::{Lang, context::fn_name, programs::platform_matches};
use crate::utils;
use rnix::{
    SyntaxKind, SyntaxNode, TextRange,
    ast::{self, AttrpathValue, BinOpKind, Expr, HasEntry as _, UnaryOpKind},
};
use rowan::ast::AstNode as _;
use std::collections::HashMap;

/// A platform condition, evaluated per system as true, false or unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cond {
    True,
    Unknown,
    /// `darwin`, `linux`, `aarch64`, `x86_64` or a system string
    Platform(String),
    /// `lib.meta.availableOn hostPlatform pkgs.X`
    AvailableOn(String),
    Not(Box<Cond>),
    And(Box<Cond>, Box<Cond>),
    Or(Box<Cond>, Box<Cond>),
}

impl Cond {
    #[must_use]
    pub fn and(self, other: Cond) -> Cond {
        match (self, other) {
            (Cond::True, c) | (c, Cond::True) => c,
            (a, b) => Cond::And(Box::new(a), Box::new(b)),
        }
    }

    /// `Some(bool)` when decided for `system`, `None` when unknown.
    /// `available(system, attr)` answers `lib.meta.availableOn`.
    pub fn eval(
        &self,
        system: &str,
        available: &dyn Fn(&str, &str) -> Option<bool>,
    ) -> Option<bool> {
        match self {
            Cond::True => Some(true),
            Cond::Unknown => None,
            Cond::Platform(p) => platform_matches(p, system),
            Cond::AvailableOn(attr) => available(system, attr),
            Cond::Not(c) => c.eval(system, available).map(|b| !b),
            Cond::And(a, b) => match (a.eval(system, available), b.eval(system, available)) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            },
            Cond::Or(a, b) => match (a.eval(system, available), b.eval(system, available)) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            },
        }
    }

    /// Whether it may hold on `system` (unknown counts as yes).
    pub fn allows(&self, system: &str, available: &dyn Fn(&str, &str) -> Option<bool>) -> bool {
        self.eval(system, available) != Some(false)
    }

    /// `platforms=a,b` directive values; `None` if a value is unknown.
    #[must_use]
    pub fn from_platforms(values: &[&str]) -> Option<Cond> {
        let mut out: Option<Cond> = None;
        for v in values {
            platform_matches(v, "x86_64-linux")?;
            let c = Cond::Platform((*v).to_string());
            out = Some(match out {
                None => c,
                Some(prev) => Cond::Or(Box::new(prev), Box::new(c)),
            });
        }
        out
    }
}

/// A package declared for the script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// Attribute path without `pkgs.`, e.g. `jq`, `python3Packages.foo`.
    pub attr: String,
    pub cond: Cond,
    pub at: TextRange,
}

#[derive(Debug, Clone, Default)]
pub struct Declared {
    /// Where to declare more, for messages: `runtimeInputs`, `packages`, `path`.
    pub place: &'static str,
    pub packages: Vec<Package>,
    /// Commands declared directly (devenv script names, ...).
    /// Shared by all scripts of a file (cheap to clone).
    pub commands: std::rc::Rc<std::collections::HashSet<String>>,
    /// Platforms the script runs on (systemd: Linux, `meta.platforms`).
    pub platforms: Option<Cond>,
    /// Some declarations couldn't be read (unknown expressions, imports):
    /// commands that look undeclared may be declared there.
    pub incomplete: bool,
    /// Relative paths in the script are relative to this project root.
    pub devenv: bool,
}

/// Tools of the stdenv every devenv shell is built from.
const STDENV: &[&str] = &[
    "coreutils",
    "findutils",
    "diffutils",
    "gnused",
    "gnugrep",
    "gawk",
    "gnutar",
    "gzip",
    "bzip2",
    "xz",
    "gnumake",
    "bash",
    "gnupatch",
    "file",
];

/// systemd services' default `path` (with `enableDefaultPath`).
const SYSTEMD_PATH: &[&str] = &["coreutils", "findutils", "gnugrep", "gnused", "systemd"];

/// Packages devenv `languages.<name>.enable` adds.
const LANGUAGES: &[(&str, &[&str])] = &[
    ("rust", &["cargo", "rustc", "rustfmt", "clippy"]),
    ("javascript", &["nodejs"]),
    ("typescript", &["typescript"]),
    ("python", &["python3"]),
    ("go", &["go"]),
    ("ruby", &["ruby"]),
    ("php", &["php"]),
    ("java", &["jdk"]),
    ("elixir", &["elixir"]),
    ("erlang", &["erlang"]),
    ("zig", &["zig"]),
    ("deno", &["deno"]),
    ("terraform", &["terraform"]),
    ("opentofu", &["opentofu"]),
    ("nix", &["nil"]),
    ("c", &["gcc"]),
    ("cplusplus", &["gcc"]),
    ("haskell", &["ghc", "cabal-install"]),
    ("ocaml", &["ocaml", "dune_3"]),
    ("lua", &["lua"]),
    ("perl", &["perl"]),
    ("kotlin", &["kotlin"]),
    ("scala", &["scala"]),
    ("dotnet", &["dotnet-sdk"]),
    ("swift", &["swift"]),
    ("julia", &["julia"]),
    ("r", &["R"]),
];

/// Packages devenv `services.<name>.enable` adds.
const SERVICES: &[(&str, &[&str])] = &[
    ("postgres", &["postgresql"]),
    ("mysql", &["mariadb"]),
    ("redis", &["redis"]),
    ("mongodb", &["mongodb"]),
    ("minio", &["minio", "minio-client"]),
    ("elasticsearch", &["elasticsearch"]),
    ("caddy", &["caddy"]),
    ("nginx", &["nginx"]),
];

/// Packages top-level devenv modules add with `<module>.enable`.
const MODULES: &[(&str, &[&str])] = &[
    ("treefmt", &["treefmt"]),
    ("git-hooks", &["prek"]),
    ("pre-commit", &["pre-commit"]),
    ("cachix", &["cachix"]),
];

/// Shell functions devenv defines for its scripts (`enterTest`).
const DEVENV_FUNCTIONS: &[&str] = &["wait_for_port", "wait_for_processes"];

/// `pkgs.jq` / `jq` / `lib.getBin pkgs.jq` / `(python3.withPackages f)` ->
/// attribute path; `None` when it isn't a package reference we understand.
pub fn package_attr(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Paren(p) => package_attr(&p.expr()?),
        Expr::Ident(i) => Some(i.syntax().text().to_string()),
        Expr::Select(s) => {
            if s.default_expr().is_some() {
                return None;
            }
            let base = s.expr()?;
            let attrs: Vec<String> = s
                .attrpath()?
                .attrs()
                .map(|a| utils::attr_name(&a))
                .collect::<Option<_>>()?;
            match &base {
                Expr::Ident(i) if i.syntax().text() == "pkgs" => Some(attrs.join(".")),
                Expr::Ident(i) if i.syntax().text() == "config" || i.syntax().text() == "self" => {
                    None
                }
                Expr::Ident(i) => Some(format!("{}.{}", i.syntax().text(), attrs.join("."))),
                _ => None,
            }
        }
        Expr::Apply(a) => {
            let f = a.lambda()?;
            let name = fn_name(&f)?;
            match name.as_str() {
                "getBin" | "getOutput" | "getExe" | "getDev" => package_attr(&a.argument()?),
                // pkgs.python3.withPackages (ps: ...) -> python3
                "withPackages" | "override" | "overrideAttrs" => match f {
                    Expr::Select(s) => package_attr(&s.expr()?),
                    _ => None,
                },
                _ => None,
            }
        }
        _ => None,
    }
}

fn last_key(apv: &AttrpathValue) -> Option<String> {
    apv.attrpath()?
        .attrs()
        .last()
        .and_then(|a| utils::attr_name(&a))
}

/// Platform condition of a Nix expression.
#[must_use]
pub fn cond_of(expr: &Expr) -> Cond {
    match expr {
        Expr::Paren(p) => p.expr().map_or(Cond::Unknown, |e| cond_of(&e)),
        Expr::UnaryOp(u) if u.operator() == Some(UnaryOpKind::Invert) => {
            Cond::Not(Box::new(u.expr().map_or(Cond::Unknown, |e| cond_of(&e))))
        }
        Expr::BinOp(b) => {
            let (Some(lhs), Some(rhs)) = (b.lhs(), b.rhs()) else {
                return Cond::Unknown;
            };
            match b.operator() {
                Some(BinOpKind::And) => Cond::And(Box::new(cond_of(&lhs)), Box::new(cond_of(&rhs))),
                Some(BinOpKind::Or) => Cond::Or(Box::new(cond_of(&lhs)), Box::new(cond_of(&rhs))),
                Some(op @ (BinOpKind::Equal | BinOpKind::NotEqual)) => {
                    let system = [(&lhs, &rhs), (&rhs, &lhs)]
                        .into_iter()
                        .find_map(|(a, b)| is_system(a).then(|| string(b)).flatten());
                    match (system, op) {
                        (Some(s), BinOpKind::Equal) => Cond::Platform(s),
                        (Some(s), _) => Cond::Not(Box::new(Cond::Platform(s))),
                        _ => Cond::Unknown,
                    }
                }
                _ => Cond::Unknown,
            }
        }
        Expr::Select(_) | Expr::Ident(_) => {
            let text = expr.syntax().text().to_string();
            let last = text.rsplit('.').next().unwrap_or(&text);
            match last {
                "isDarwin" => Cond::Platform("darwin".into()),
                "isLinux" => Cond::Platform("linux".into()),
                "isAarch64" => Cond::Platform("aarch64".into()),
                "isx86_64" => Cond::Platform("x86_64".into()),
                _ => Cond::Unknown,
            }
        }
        Expr::Apply(a) => {
            let (Some(f), args) = flatten(a) else {
                return Cond::Unknown;
            };
            match (fn_name(&f).as_deref(), args.as_slice()) {
                // builtins.elem pkgs.system [ "a" "b" ]
                (Some("elem"), [x, Expr::List(l)]) if is_system(x) => l
                    .items()
                    .filter_map(|i| string(&i))
                    .map(Cond::Platform)
                    .reduce(|a, b| Cond::Or(Box::new(a), Box::new(b)))
                    .unwrap_or(Cond::Unknown),
                (Some("availableOn"), [_, pkg]) => {
                    package_attr(pkg).map_or(Cond::Unknown, Cond::AvailableOn)
                }
                _ => Cond::Unknown,
            }
        }
        _ => Cond::Unknown,
    }
}

fn is_system(e: &Expr) -> bool {
    let text = e.syntax().text().to_string();
    text.ends_with(".system") || text == "system"
}

fn string(e: &Expr) -> Option<String> {
    match e {
        Expr::Str(s) => utils::attr_name(&ast::Attr::Str(s.clone())),
        _ => None,
    }
}

fn flatten(a: &ast::Apply) -> (Option<Expr>, Vec<Expr>) {
    let mut args = Vec::new();
    let mut cur = Expr::Apply(a.clone());
    while let Expr::Apply(ap) = &cur {
        args.extend(ap.argument());
        match ap.lambda() {
            Some(l) => cur = l,
            None => return (None, args),
        }
    }
    args.reverse();
    (Some(cur), args)
}

/// `# statix platforms=...` in the Nix comments right before `node`.
fn comment_platforms(node: &SyntaxNode) -> Option<Cond> {
    let mut token = node.first_token()?.prev_token();
    while let Some(t) = token {
        match t.kind() {
            SyntaxKind::TOKEN_WHITESPACE => {}
            SyntaxKind::TOKEN_COMMENT => {
                let d = super::directives::parse_line(t.text())?;
                return d.platforms.and_then(|p| {
                    Cond::from_platforms(&p.iter().map(String::as_str).collect::<Vec<_>>())
                });
            }
            _ => return None,
        }
        token = t.prev_token();
    }
    None
}

/// Packages in a list-like expression (`[ ... ]`, `++`, `lib.optionals c [...]`,
/// `with pkgs; [...]`, `if`), each with its condition.
fn collect(expr: &Expr, cond: &Cond, out: &mut Declared) {
    match expr {
        Expr::Paren(p) => {
            if let Some(e) = p.expr() {
                collect(&e, cond, out);
            }
        }
        Expr::With(w) => {
            if let Some(body) = w.body() {
                collect(&body, cond, out);
            }
        }
        Expr::List(l) => {
            for item in l.items() {
                let item_cond = comment_platforms(item.syntax())
                    .map_or_else(|| cond.clone(), |c| cond.clone().and(c));
                match package_attr(&item) {
                    Some(attr) => out.packages.push(Package {
                        attr,
                        cond: item_cond,
                        at: item.syntax().text_range(),
                    }),
                    None => out.incomplete = true,
                }
            }
        }
        Expr::BinOp(b) if b.operator() == Some(BinOpKind::Concat) => {
            for side in [b.lhs(), b.rhs()].into_iter().flatten() {
                collect(&side, cond, out);
            }
        }
        Expr::IfElse(i) => {
            let c = i.condition().map_or(Cond::Unknown, |c| cond_of(&c));
            if let Some(body) = i.body() {
                collect(&body, &cond.clone().and(c.clone()), out);
            }
            if let Some(body) = i.else_body() {
                collect(&body, &cond.clone().and(Cond::Not(Box::new(c))), out);
            }
        }
        Expr::Apply(a) => {
            let (Some(f), args) = flatten(a) else {
                out.incomplete = true;
                return;
            };
            match (fn_name(&f).as_deref(), args.as_slice()) {
                (Some("optionals" | "mkIf"), [c, list]) => {
                    collect(list, &cond.clone().and(cond_of(c)), out);
                }
                (Some("optional"), [c, item]) => {
                    let c = cond.clone().and(cond_of(c));
                    match package_attr(item) {
                        Some(attr) => out.packages.push(Package {
                            attr,
                            cond: c,
                            at: item.syntax().text_range(),
                        }),
                        None => out.incomplete = true,
                    }
                }
                _ => out.incomplete = true,
            }
        }
        _ => out.incomplete = true,
    }
}

/// Conditions of the `lib.mkIf` / `lib.optionalAttrs` / `if` around `node`.
fn ancestor_cond(node: &SyntaxNode) -> Cond {
    let mut cond = Cond::True;
    let mut child = node.clone();
    for parent in node.ancestors().skip(1) {
        if let Some(a) = ast::Apply::cast(parent.clone())
            && let (Some(f), args) = flatten(&a)
            && matches!(fn_name(&f).as_deref(), Some("mkIf" | "optionalAttrs"))
            && let [c, body] = args.as_slice()
            && body
                .syntax()
                .text_range()
                .contains_range(child.text_range())
        {
            cond = cond.and(cond_of(c));
        }
        if let Some(i) = ast::IfElse::cast(parent.clone())
            && let Some(c) = i.condition()
        {
            let c = cond_of(&c);
            if i.body()
                .is_some_and(|b| b.syntax().text_range().contains_range(child.text_range()))
            {
                cond = cond.and(c);
            } else if i
                .else_body()
                .is_some_and(|b| b.syntax().text_range().contains_range(child.text_range()))
            {
                cond = cond.and(Cond::Not(Box::new(c)));
            }
        }
        child = parent;
    }
    cond
}

fn root_of(node: &SyntaxNode) -> SyntaxNode {
    node.ancestors().last().unwrap_or_else(|| node.clone())
}

/// All bindings in the file with their full attribute path (`config.` removed).
type Bindings = std::rc::Rc<Vec<(Vec<String>, AttrpathValue)>>;

fn bindings(root: &SyntaxNode) -> Bindings {
    // every script in a file looks at the same bindings: keep the last file's
    thread_local! {
        static LAST: std::cell::RefCell<Option<(SyntaxNode, Bindings)>> =
            const { std::cell::RefCell::new(None) };
    }
    LAST.with(|last| {
        if let Some((node, b)) = &*last.borrow()
            && node == root
        {
            return b.clone();
        }
        let b: Bindings = std::rc::Rc::new(collect_bindings(root));
        *last.borrow_mut() = Some((root.clone(), b.clone()));
        b
    })
}

/// Whether `inherit` somewhere in `root` binds one of `names`: declarations
/// we can't follow.
fn inherits(root: &SyntaxNode, names: &[&str]) -> bool {
    root.descendants()
        .filter_map(ast::Inherit::cast)
        .flat_map(|i| i.attrs())
        .filter_map(|a| utils::attr_name(&a))
        .any(|n| names.contains(&n.as_str()))
}

fn collect_bindings(root: &SyntaxNode) -> Vec<(Vec<String>, AttrpathValue)> {
    root.descendants()
        .filter_map(AttrpathValue::cast)
        .filter_map(|apv| {
            let path = utils::enclosing_attrpath(apv.syntax())?;
            let path = match path.first().map(String::as_str) {
                Some("config") => path[1..].to_vec(),
                _ => path,
            };
            Some((path, apv))
        })
        .collect()
}

/// Condition of `x.enable = <value>`: `None` when it's `false`.
fn enabled(apv: &AttrpathValue) -> Option<Cond> {
    let value = apv.value()?;
    let cond = ancestor_cond(apv.syntax());
    match value.syntax().text().to_string().as_str() {
        "true" => Some(cond),
        "false" => None,
        // enable = pkgs.stdenv.isLinux;
        _ => Some(cond.and(cond_of(&value))),
    }
}

fn add_packages(attrs: &[&str], cond: &Cond, out: &mut Declared) {
    out.packages.extend(attrs.iter().map(|attr| Package {
        attr: (*attr).to_string(),
        cond: cond.clone(),
        at: TextRange::default(),
    }));
}

fn add_package_list(apv: &AttrpathValue, out: &mut Declared) {
    let cond = ancestor_cond(apv.syntax());
    match apv.value() {
        Some(value) => collect(&value, &cond, out),
        None => out.incomplete = true,
    }
}

/// devenv: `packages`, `scripts.<n>.packages`, script names, stdenv, enabled
/// languages and services.
fn devenv(script: &SyntaxNode, name: Option<&str>) -> Declared {
    type PerScript = std::rc::Rc<HashMap<String, Vec<AttrpathValue>>>;
    // the same for every script in the file: keep the last file's
    thread_local! {
        static LAST: std::cell::RefCell<Option<(SyntaxNode, Declared, PerScript)>> =
            const { std::cell::RefCell::new(None) };
    }
    let root = root_of(script);
    let (mut out, per_script) = LAST.with(|last| {
        if let Some((node, d, p)) = &*last.borrow()
            && *node == root
        {
            return (d.clone(), p.clone());
        }
        let (d, p) = devenv_file(&root);
        let p: PerScript = std::rc::Rc::new(p);
        *last.borrow_mut() = Some((root.clone(), d.clone(), p.clone()));
        (d, p)
    });
    // lib.mkIf pkgs.stdenv.isDarwin { enterTest = ...; }
    let cond = ancestor_cond(script);
    if cond != Cond::True {
        out.platforms = Some(cond);
    }
    for apv in name.and_then(|n| per_script.get(n)).into_iter().flatten() {
        add_package_list(apv, &mut out);
    }
    out
}

/// devenv declarations of the whole file, and `scripts.<name>.packages`.
fn devenv_file(root: &SyntaxNode) -> (Declared, HashMap<String, Vec<AttrpathValue>>) {
    let mut per_script: HashMap<String, Vec<AttrpathValue>> = HashMap::new();
    let mut out = Declared {
        place: "packages",
        devenv: true,
        ..Declared::default()
    };
    for attr in STDENV {
        out.packages.push(Package {
            attr: (*attr).to_string(),
            cond: Cond::True,
            at: TextRange::default(),
        });
    }
    let commands = std::rc::Rc::make_mut(&mut out.commands);
    commands.insert("devenv".into());
    commands.extend(DEVENV_FUNCTIONS.iter().map(|f| (*f).to_string()));
    out.incomplete |= inherits(root, &["packages"]);
    for (path, apv) in bindings(root).iter() {
        let p: Vec<&str> = path.iter().map(String::as_str).collect();
        match p.as_slice() {
            // languages.vala.package = pkgs.vala;
            ["packages"] | ["languages", _, "package"] => add_package_list(apv, &mut out),
            ["scripts", n, rest @ ..] => {
                if rest == ["packages"] {
                    per_script
                        .entry((*n).to_string())
                        .or_default()
                        .push(apv.clone());
                }
                std::rc::Rc::make_mut(&mut out.commands).insert((*n).to_string());
            }
            ["imports"] => out.incomplete = true,
            ["languages", lang, "enable"] => {
                if let Some(cond) = enabled(apv) {
                    let attrs = LANGUAGES
                        .iter()
                        .find(|(l, _)| l == lang)
                        .map_or_else(|| vec![*lang], |(_, a)| a.to_vec());
                    add_packages(&attrs, &cond, &mut out);
                }
            }
            // languages.javascript.pnpm.enable = true, ...
            ["languages", _, tool, "enable"] => {
                if let Some(cond) = enabled(apv) {
                    let attr = match *tool {
                        "npm" => "nodejs",
                        t => t,
                    };
                    add_packages(&[attr], &cond, &mut out);
                }
            }
            ["services", service, "enable"] => {
                if let Some(cond) = enabled(apv) {
                    let attrs = SERVICES
                        .iter()
                        .find(|(s, _)| s == service)
                        .map_or_else(|| vec![*service], |(_, a)| a.to_vec());
                    add_packages(&attrs, &cond, &mut out);
                }
            }
            [module, "enable"] => {
                if let (Some(cond), Some((_, attrs))) =
                    (enabled(apv), MODULES.iter().find(|(m, _)| m == module))
                {
                    add_packages(attrs, &cond, &mut out);
                }
            }
            _ => {}
        }
    }
    (out, per_script)
}

/// systemd services: their `path` and the default path; Linux only.
fn systemd(service: &[String], root: &SyntaxNode) -> Declared {
    let mut out = Declared {
        place: "path",
        platforms: Some(Cond::Platform("linux".into())),
        // initrd services run with the initrd's own PATH
        incomplete: service.first().is_some_and(|s| s == "boot") || inherits(root, &["path"]),
        ..Declared::default()
    };
    let mut default_path = true;
    for (path, apv) in bindings(root).iter() {
        // other modules may add to the service's `path`
        if path.first().is_some_and(|p| p == "imports") {
            out.incomplete = true;
        }
        let matches_service = path.len() == service.len() + 1
            && path
                .iter()
                .zip(service)
                .all(|(a, b)| a == b || a == "*" || b == "*");
        if !matches_service {
            continue;
        }
        match path.last().map(String::as_str) {
            Some("path") => add_package_list(apv, &mut out),
            Some("enableDefaultPath")
                if apv.value().is_some_and(|v| v.syntax().text() == "false") =>
            {
                default_path = false;
            }
            _ => {}
        }
    }
    if default_path {
        out.packages.extend(SYSTEMD_PATH.iter().map(|attr| Package {
            attr: (*attr).to_string(),
            cond: Cond::True,
            at: TextRange::default(),
        }));
    }
    out
}

/// `writeShellApplication { runtimeInputs = [...]; meta.platforms = ...; }`
fn shell_application(set: &ast::AttrSet) -> Declared {
    let mut out = Declared {
        place: "runtimeInputs",
        incomplete: set
            .inherits()
            .flat_map(|i| i.attrs())
            .filter_map(|a| utils::attr_name(&a))
            .any(|n| n == "runtimeInputs"),
        ..Declared::default()
    };
    for apv in set.attrpath_values() {
        let keys: Vec<String> = apv
            .attrpath()
            .map(|p| p.attrs().filter_map(|a| utils::attr_name(&a)).collect())
            .unwrap_or_default();
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        match keys.as_slice() {
            ["runtimeInputs"] => add_package_list(&apv, &mut out),
            ["meta", "platforms"] => out.platforms = apv.value().map(|v| meta_platforms(&v)),
            ["meta"] => {
                if let Some(Expr::AttrSet(meta)) = apv.value() {
                    for m in meta.attrpath_values() {
                        if last_key(&m).as_deref() == Some("platforms") {
                            out.platforms = m.value().map(|v| meta_platforms(&v));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// `lib.platforms.darwin`, `[ "x86_64-linux" ]`, `a ++ b`.
fn meta_platforms(e: &Expr) -> Cond {
    match e {
        Expr::Select(s) => s
            .attrpath()
            .and_then(|p| p.attrs().last())
            .and_then(|a| utils::attr_name(&a))
            .filter(|name| platform_matches(name, "x86_64-linux").is_some())
            .map_or(Cond::Unknown, Cond::Platform),
        Expr::List(l) => l
            .items()
            .filter_map(|i| string(&i))
            .map(Cond::Platform)
            .reduce(|a, b| Cond::Or(Box::new(a), Box::new(b)))
            .unwrap_or(Cond::Unknown),
        Expr::BinOp(b) if b.operator() == Some(BinOpKind::Concat) => match (b.lhs(), b.rhs()) {
            (Some(l), Some(r)) => {
                Cond::Or(Box::new(meta_platforms(&l)), Box::new(meta_platforms(&r)))
            }
            _ => Cond::Unknown,
        },
        Expr::Paren(p) => p.expr().map_or(Cond::Unknown, |e| meta_platforms(&e)),
        _ => Cond::Unknown,
    }
}

/// `systemd.services.<n>.script` / `systemd.user.services.<n>.preStart`...
fn is_systemd_script(path: &[&str]) -> bool {
    let n = path.len();
    n >= 4
        && matches!(
            path[n - 1],
            "script" | "preStart" | "postStart" | "preStop" | "postStop" | "reload"
        )
        && path[n - 3] == "services"
        && (path[n - 4] == "systemd"
            || (n >= 5 && path[n - 4] == "user" && path[n - 5] == "systemd"))
}

/// Top-level options only NixOS has (devenv has `services`, `env`...).
const NIXOS_OPTIONS: &[&str] = &[
    "systemd",
    "boot",
    "security",
    "networking",
    "fileSystems",
    "hardware",
    "virtualisation",
    // NixOS VM tests
    "nodes",
    "testScript",
];

/// Platforms every script in the file of `node` runs on: Linux in a NixOS
/// module.
pub fn file_platforms(node: &SyntaxNode) -> Option<Cond> {
    bindings(&root_of(node))
        .iter()
        .any(|(path, _)| {
            // config.* is stripped already; options.systemd... too
            let path = path.strip_prefix(&["options".to_string()]).unwrap_or(path);
            path.first()
                .is_some_and(|p| NIXOS_OPTIONS.contains(&p.as_str()))
        })
        .then(|| Cond::Platform("linux".into()))
}

/// Declarations for the shell script in `s`, or `None` when its context
/// doesn't declare dependencies (writeShellScript, stdenv phases...).
#[must_use]
pub fn declared(s: &ast::Str, lang: Lang) -> Option<Declared> {
    if !matches!(lang, Lang::Shell(_)) {
        return None;
    }
    let node = s.syntax();
    // writeShellApplication { text = <s>; }
    if let Some(apv) = AttrpathValue::cast(node.parent()?)
        && last_key(&apv).as_deref() == Some("text")
        && let Some(set) = apv.syntax().parent().and_then(ast::AttrSet::cast)
        && let Some(apply) = set.syntax().parent().and_then(ast::Apply::cast)
        && apply.lambda().and_then(|f| fn_name(&f)).as_deref() == Some("writeShellApplication")
    {
        let mut out = shell_application(&set);
        if let Some(linux) = file_platforms(node) {
            out.platforms = Some(out.platforms.map_or(linux.clone(), |c| c.and(linux)));
        }
        return Some(out);
    }

    let apv = node.ancestors().find_map(AttrpathValue::cast)?;
    let path = utils::enclosing_attrpath(apv.syntax())?;
    let path: Vec<String> = match path.first().map(String::as_str) {
        Some("config") => path[1..].to_vec(),
        _ => path,
    };
    let p: Vec<&str> = path.iter().map(String::as_str).collect();
    match p.as_slice() {
        ["scripts", name, "exec"] => Some(devenv(node, Some(name))),
        ["tasks" | "processes", _, "exec" | "status"] | ["enterShell" | "enterTest"] => {
            Some(devenv(node, None))
        }
        _ if is_systemd_script(&p) => Some(systemd(&path[..path.len() - 1], &root_of(node))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_string(src: &str) -> ast::Str {
        rnix::Root::parse(src)
            .tree()
            .syntax()
            .descendants()
            .filter_map(ast::Str::cast)
            .find(|s| s.syntax().text().to_string().contains("RUN"))
            .unwrap()
    }

    fn attrs(d: &Declared) -> Vec<&str> {
        d.packages.iter().map(|p| p.attr.as_str()).collect()
    }

    #[test]
    fn shell_application_inputs_and_platforms() {
        let s = first_string(
            "pkgs.writeShellApplication { name = \"x\"; runtimeInputs = [ pkgs.jq curl (lib.getBin pkgs.gnused) ] ++ lib.optionals pkgs.stdenv.isLinux [ pkgs.strace ]; meta.platforms = lib.platforms.unix; text = ''RUN''; }",
        );
        let d = declared(&s, Lang::Shell("bash")).unwrap();
        assert_eq!(attrs(&d), ["jq", "curl", "gnused", "strace"]);
        assert!(!d.incomplete);
        let none = |_: &str, _: &str| None;
        assert!(!d.packages[3].cond.allows("aarch64-darwin", &none));
        assert!(d.packages[3].cond.allows("x86_64-linux", &none));
        assert_eq!(d.place, "runtimeInputs");
    }

    #[test]
    fn devenv_packages() {
        let s = first_string(
            "{ pkgs, lib, ... }: { packages = [ pkgs.git ] ++ lib.optionals pkgs.stdenv.isDarwin [ pkgs.darwin.trash ]; languages.rust.enable = true; scripts.a.exec = ''RUN''; scripts.b.exec = \"x\"; scripts.a.packages = [ pkgs.jq ]; }",
        );
        let d = declared(&s, Lang::Shell("bash")).unwrap();
        let a = attrs(&d);
        assert!(
            a.contains(&"git")
                && a.contains(&"darwin.trash")
                && a.contains(&"cargo")
                && a.contains(&"jq")
                && a.contains(&"coreutils"),
            "{a:?}"
        );
        assert!(d.commands.contains("b"));
        assert!(!d.incomplete);
    }

    #[test]
    fn devenv_scripts_and_language_tools() {
        let s = first_string(
            "{ languages.javascript = { enable = true; pnpm.enable = true; }; scripts.frontend-check.exec = ''\n  cd \"$DEVENV_ROOT\" && exec pnpm check \"$@\"\n''; scripts.ci.exec = ''RUN frontend-check''; }",
        );
        let d = declared(&s, Lang::Shell("bash")).unwrap();
        assert!(
            attrs(&d).contains(&"pnpm") && attrs(&d).contains(&"nodejs"),
            "{:?}",
            attrs(&d)
        );
        assert!(d.commands.contains("frontend-check"));
    }

    #[test]
    fn unknown_entries_make_it_incomplete() {
        let s = first_string("{ packages = myPackages; enterShell = ''RUN''; }");
        assert!(declared(&s, Lang::Shell("bash")).unwrap().incomplete);
    }

    #[test]
    fn systemd_path() {
        let s =
            first_string("{ systemd.services.web = { path = [ pkgs.curl ]; script = ''RUN''; }; }");
        let d = declared(&s, Lang::Shell("bash")).unwrap();
        assert!(attrs(&d).contains(&"curl") && attrs(&d).contains(&"coreutils"));
        assert_eq!(d.platforms, Some(Cond::Platform("linux".into())));
    }

    #[test]
    fn conditions() {
        let parse = |src: &str| {
            let e = rnix::Root::parse(src).tree().expr().unwrap();
            cond_of(&e)
        };
        let none = |_: &str, _: &str| None;
        let c = parse("pkgs.stdenv.hostPlatform.isDarwin && !stdenv.isAarch64");
        assert_eq!(c.eval("x86_64-darwin", &none), Some(true));
        assert_eq!(c.eval("aarch64-darwin", &none), Some(false));
        let c = parse("builtins.elem pkgs.system [ \"x86_64-linux\" ]");
        assert_eq!(c.eval("aarch64-linux", &none), Some(false));
        assert_eq!(parse("config.foo").eval("x86_64-linux", &none), None);
    }
}
