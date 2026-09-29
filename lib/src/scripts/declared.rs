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

mod tables;

use tables::{DEVENV_FUNCTIONS, LANGUAGES, MODULES, NIXOS_OPTIONS, SERVICES, STDENV, SYSTEMD_PATH};

/// `pkgs.jq` / `jq` / `lib.getBin pkgs.jq` / `(python3.withPackages f)` ->
/// attribute path; `None` when it isn't a package reference we understand.
pub fn package_attr(expr: &Expr) -> Option<String> {
    package_attr_at(expr, 0)
}

/// What an identifier refers to.
enum Binding {
    /// `let x = <expr>;` (or `rec { x = <expr>; }`)
    Value(Expr),
    /// A package: `with pkgs;`, a callPackage argument (`{ jq, ... }:`), or
    /// free in the file.
    Package,
    /// A function argument or anything else we can't follow.
    Unknown,
}

/// Resolve identifier `name` at `node` with Nix scoping: lexical bindings
/// (`let`, `rec`, function arguments) win over `with`.
fn resolve(node: &SyntaxNode, name: &str) -> Binding {
    let mut with_pkgs = false;
    let mut with_other = false;
    let mut child = node.clone();
    for scope in node.ancestors().skip(1) {
        let bindings = match scope.kind() {
            SyntaxKind::NODE_LET_IN => Some(scope.clone()),
            SyntaxKind::NODE_ATTR_SET
                if ast::AttrSet::cast(scope.clone()).is_some_and(|a| a.rec_token().is_some()) =>
            {
                Some(scope.clone())
            }
            _ => None,
        };
        if let Some(set) = bindings {
            for entry in set.children() {
                if let Some(apv) = AttrpathValue::cast(entry.clone()) {
                    let keys: Vec<_> = apv.attrpath().into_iter().flat_map(|p| p.attrs()).collect();
                    if let [key] = keys.as_slice()
                        && utils::attr_name(key).as_deref() == Some(name)
                    {
                        return apv.value().map_or(Binding::Unknown, Binding::Value);
                    }
                } else if let Some(inherit) = ast::Inherit::cast(entry) {
                    let inherits_name = inherit
                        .attrs()
                        .any(|a| utils::attr_name(&a).as_deref() == Some(name));
                    if inherits_name {
                        return match inherit.from() {
                            // inherit (pkgs) jq;
                            Some(from)
                                if from.expr().is_some_and(|e| e.syntax().text() == "pkgs") =>
                            {
                                Binding::Package
                            }
                            // inherit x; refers to the outer x
                            None => resolve(&set, name),
                            Some(_) => Binding::Unknown,
                        };
                    }
                }
            }
        }
        if let Some(with) = ast::With::cast(scope.clone())
            && with.body().is_some_and(|b| b.syntax() == &child)
        {
            if with
                .namespace()
                .is_some_and(|n| n.syntax().text() == "pkgs")
            {
                with_pkgs = true;
            } else {
                with_other = true;
            }
        }
        if let Some(lambda) = ast::Lambda::cast(scope.clone()) {
            let outermost = !scope
                .ancestors()
                .skip(1)
                .any(|a| a.kind() == SyntaxKind::NODE_LAMBDA);
            match lambda.param() {
                Some(ast::Param::IdentParam(p)) if p.syntax().text() == name => {
                    return Binding::Unknown;
                }
                Some(ast::Param::Pattern(pat)) => {
                    let bound = pat
                        .pat_entries()
                        .any(|e| e.ident().is_some_and(|i| i.syntax().text() == name))
                        || pat
                            .pat_bind()
                            .and_then(|b| b.ident())
                            .is_some_and(|i| i.syntax().text() == name);
                    if bound {
                        // `{ jq, ... }:` of a callPackage file
                        return if outermost {
                            Binding::Package
                        } else {
                            Binding::Unknown
                        };
                    }
                }
                _ => {}
            }
        }
        child = scope;
    }
    if with_pkgs {
        Binding::Package
    } else if with_other {
        Binding::Unknown
    } else {
        Binding::Package
    }
}

fn package_attr_at(expr: &Expr, depth: usize) -> Option<String> {
    if depth > 8 {
        return None;
    }
    match expr {
        Expr::Paren(p) => package_attr_at(&p.expr()?, depth + 1),
        Expr::Ident(i) => {
            let name = i.syntax().text().to_string();
            match resolve(i.syntax(), &name) {
                // pkg = cfg.package;
                Binding::Value(v) => package_attr_at(&v, depth + 1),
                Binding::Package => Some(name),
                Binding::Unknown => None,
            }
        }
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
            let Expr::Ident(i) = &base else { return None };
            let name = i.syntax().text().to_string();
            match name.as_str() {
                "pkgs" => Some(attrs.join(".")),
                "config" | "self" | "lib" | "cfg" => None,
                // python3Packages.foo, pp.foo with pp = pkgs.python3Packages
                _ => match resolve(i.syntax(), &name) {
                    Binding::Package => Some(format!("{name}.{}", attrs.join("."))),
                    Binding::Value(v) if v.syntax().text() == "pkgs" => Some(attrs.join(".")),
                    Binding::Value(v) => {
                        package_attr_at(&v, depth + 1).map(|b| format!("{b}.{}", attrs.join(".")))
                    }
                    Binding::Unknown => None,
                },
            }
        }
        Expr::Apply(a) => {
            let f = a.lambda()?;
            let name = fn_name(&f)?;
            match name.as_str() {
                "getBin" | "getOutput" | "getExe" | "getDev" => {
                    package_attr_at(&a.argument()?, depth + 1)
                }
                // pkgs.python3.withPackages (ps: ...) -> python3
                "withPackages" | "override" | "overrideAttrs" => match f {
                    Expr::Select(s) => package_attr_at(&s.expr()?, depth + 1),
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
    // whether this file defines the service, or only adds scripts to one
    // another module defines (`systemd.services.postgresql.postStart`)
    let mut defines = false;
    for (path, apv) in bindings(root).iter() {
        // other modules may add to the service's `path`
        if path.first().is_some_and(|p| p == "imports") {
            out.incomplete = true;
        }
        let in_service = path.len() > service.len()
            && path
                .iter()
                .zip(service)
                .all(|(a, b)| a == b || a == "*" || b == "*");
        if in_service
            && !matches!(
                path[service.len()].as_str(),
                "script" | "preStart" | "postStart" | "preStop" | "postStop" | "reload"
            )
        {
            defines = true;
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
    if !defines {
        out.incomplete = true;
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
            // meta = { platforms = ...; }
            ["meta"] => {
                let platforms = match apv.value() {
                    Some(Expr::AttrSet(meta)) => meta
                        .attrpath_values()
                        .find(|m| last_key(m).as_deref() == Some("platforms")),
                    _ => None,
                };
                if let Some(p) = platforms {
                    out.platforms = p.value().map(|v| meta_platforms(&v));
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

/// Whether `node` is (in) the `runScript` or `profile` of a `buildFHSEnv`
/// call: it runs inside the FHS sandbox, where `/usr`, `/opt`... are the
/// sandbox's and commands come from `targetPkgs`/`multiPkgs`.
#[must_use]
pub fn in_fhs_env(node: &SyntaxNode) -> bool {
    node.ancestors().filter_map(AttrpathValue::cast).any(|apv| {
        matches!(last_key(&apv).as_deref(), Some("runScript" | "profile"))
            && apv
                .syntax()
                .parent()
                .and_then(|set| set.parent())
                .and_then(ast::Apply::cast)
                .and_then(|a| a.lambda())
                .and_then(|f| fn_name(&f))
                .is_some_and(|f| f.starts_with("buildFHSEnv") || f.starts_with("buildFHSUserEnv"))
    })
}

/// Platforms every script in the file of `node` runs on: Linux in a NixOS
/// module or test; else the package's `meta.platforms` when the file has
/// exactly one.
pub fn file_platforms(node: &SyntaxNode) -> Option<Cond> {
    let root = root_of(node);
    let nixos = bindings(&root).iter().any(|(path, _)| {
        // config.* is stripped already; options.systemd... too
        let path = path.strip_prefix(&["options".to_string()]).unwrap_or(path);
        path.first()
            .is_some_and(|p| NIXOS_OPTIONS.contains(&p.as_str()))
    });
    if nixos {
        return Some(Cond::Platform("linux".into()));
    }
    // update scripts run on the maintainer's machine, not the package's
    let in_update_script = node
        .ancestors()
        .filter_map(AttrpathValue::cast)
        .any(|a| last_key(&a).as_deref() == Some("updateScript"));
    if in_update_script {
        return None;
    }
    let mut metas = root
        .descendants()
        .filter_map(AttrpathValue::cast)
        .filter(|apv| {
            // meta.platforms = ..., meta = { platforms = ...; }
            let in_meta = |a: &AttrpathValue| {
                a.attrpath().is_some_and(|p| {
                    p.attrs()
                        .any(|k| utils::attr_name(&k).as_deref() == Some("meta"))
                })
            };
            last_key(apv).as_deref() == Some("platforms")
                && apv
                    .syntax()
                    .ancestors()
                    .filter_map(AttrpathValue::cast)
                    .any(|a| in_meta(&a))
        });
    let only = metas.next()?;
    if metas.next().is_some() {
        return None;
    }
    only.value().map(|v| meta_platforms(&v))
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
mod tests;
