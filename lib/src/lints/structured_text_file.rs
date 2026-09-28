use crate::{Metadata, Report, Rule, scripts::context::fn_name, utils};

use macros::lint;
use rnix::{
    NodeOrToken, SyntaxElement, SyntaxKind,
    ast::{self, AttrpathValue, Expr, HasEntry as _},
};
use rowan::ast::AstNode as _;

/// ## What it does
/// Checks for JSON, YAML, TOML and INI files written as text: `pkgs.writeText
/// "config.json" ''...''`, `environment.etc."app.toml".text = ''...''`,
/// devenv `files."x.yaml".text`, `builtins.toFile`, `writeTextFile`.
///
/// ## Why is this bad?
/// Hand written structured text is easy to break (a trailing comma,
/// indentation, quoting an interpolated value) and nothing checks it until
/// the program reading it fails. Nix values serialized by a generator are
/// always well formed, and interpolated values are escaped.
///
/// ## Example
///
/// ```nix
/// pkgs.writeText "config.json" ''
///   { "port": ${toString port} }
/// ''
/// ```
///
/// Generate it from a Nix value:
///
/// ```nix
/// (pkgs.formats.json { }).generate "config.json" { inherit port; }
/// ```
#[lint(
    name = "structured_text_file",
    note = "Structured file written as text",
    code = 32,
    match_with = SyntaxKind::NODE_STRING
)]
struct StructuredTextFile;

/// File extension -> format name in `pkgs.formats`.
const FORMATS: &[(&str, &str)] = &[
    ("json", "json"),
    ("yaml", "yaml"),
    ("yml", "yaml"),
    ("toml", "toml"),
    ("ini", "ini"),
];

fn format_of(file: &str) -> Option<&'static str> {
    let ext = file.rsplit_once('.')?.1.to_ascii_lowercase();
    FORMATS.iter().find(|(e, _)| *e == ext).map(|(_, f)| *f)
}

/// Static text of a string literal (`None` with interpolations).
fn static_text(e: &Expr) -> Option<String> {
    let Expr::Str(s) = e else { return None };
    utils::attr_name(&ast::Attr::Str(s.clone()))
}

/// Where the text goes.
enum Place {
    /// writeText "x.json" <s>, builtins.toFile, writeTextDir
    Writer { function: String, file: String },
    /// writeTextFile { name/destination = "x.json"; text = <s>; }
    TextFile { file: String },
    /// environment.etc."x.json".text, home.file..., devenv files."x.json".text
    Option { path: Vec<String> },
}

fn place_of(s: &ast::Str) -> Option<Place> {
    let node = s.syntax();
    let parent = node.parent()?;
    // f "x.json" <s>
    if let Some(apply) = ast::Apply::cast(parent.clone())
        && apply.argument().is_some_and(|a| a.syntax() == node)
    {
        let Expr::Apply(name_arg) = apply.lambda()? else {
            return None;
        };
        let function = fn_name(&name_arg.lambda()?)?;
        if !matches!(function.as_str(), "writeText" | "toFile" | "writeTextDir") {
            return None;
        }
        let file = static_text(&name_arg.argument()?)?;
        return Some(Place::Writer { function, file });
    }
    let apv = AttrpathValue::cast(parent)?;
    let keys: Vec<String> = apv
        .attrpath()?
        .attrs()
        .map(|a| utils::attr_name(&a))
        .collect::<Option<_>>()?;
    if keys.last().map(String::as_str) != Some("text") {
        return None;
    }
    // writeTextFile { name = "x.json"; text = <s>; }
    if keys.len() == 1
        && let Some(set) = apv.syntax().parent().and_then(ast::AttrSet::cast)
        && let Some(apply) = set.syntax().parent().and_then(ast::Apply::cast)
        && apply.lambda().and_then(|f| fn_name(&f)).as_deref() == Some("writeTextFile")
    {
        let file = set
            .attrpath_values()
            .filter(|a| {
                a.attrpath()
                    .and_then(|p| p.attrs().next())
                    .and_then(|k| utils::attr_name(&k))
                    .is_some_and(|k| k == "destination" || k == "name")
            })
            .filter_map(|a| static_text(&a.value()?))
            .find(|f| format_of(f).is_some())?;
        return Some(Place::TextFile { file });
    }
    let path = utils::enclosing_attrpath(apv.syntax())?;
    Some(Place::Option { path })
}

/// Whether the text is worth generating: it has literal text beyond YAML
/// document separators, and either interpolations or more than one line
/// (`"{ }"`, `"managed: content\n"` are fine as they are).
fn is_hand_written(s: &ast::Str) -> bool {
    let mut literal = String::new();
    let mut interpolated = false;
    for part in s.normalized_parts() {
        match part {
            ast::InterpolPart::Literal(text) => literal.push_str(&text),
            ast::InterpolPart::Interpolation(_) => interpolated = true,
        }
    }
    // `${concatMapStringsSep "\n---\n" toJSON docs}` between `---`
    let content = literal
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && *l != "---");
    let lines = content.count();
    lines > 0 && (interpolated || lines > 1)
}

/// `"--cross-file=${writeText "cross.ini" ...}"`: meson machine files look
/// like INI but have their own syntax.
fn is_meson_machine_file(s: &ast::Str) -> bool {
    s.syntax()
        .ancestors()
        .skip(1)
        .filter(|a| a.kind() == SyntaxKind::NODE_STRING)
        .any(|a| {
            let text = a.text().to_string();
            text.contains("--cross-file") || text.contains("--native-file")
        })
}

impl Rule for StructuredTextFile {
    fn validate(&self, node: &SyntaxElement) -> Option<Report> {
        let NodeOrToken::Node(node) = node else {
            return None;
        };
        let s = ast::Str::cast(node.clone())?;
        if !is_hand_written(&s) || is_meson_machine_file(&s) {
            return None;
        }
        let place = place_of(&s)?;
        let (file, help) = match &place {
            Place::Writer { function, file } => {
                let format = format_of(file)?;
                let writer = match function.as_str() {
                    "toFile" => format!("builtins.toFile \"{file}\" (builtins.toJSON {{ ... }})"),
                    _ => format!("(pkgs.formats.{format} {{ }}).generate \"{file}\" {{ ... }}"),
                };
                (
                    file.clone(),
                    format!("Generate the file from a Nix value: `{writer}`."),
                )
            }
            Place::TextFile { file } => {
                let format = format_of(file)?;
                (
                    file.clone(),
                    format!(
                        "Generate the file from a Nix value: `(pkgs.formats.{format} {{ }}).generate \"{file}\" {{ ... }}`."
                    ),
                )
            }
            Place::Option { path } => {
                // ... ."x.json".text
                let file = path.get(path.len().checked_sub(2)?)?;
                let format = format_of(file)?;
                let help = if path.first().map(String::as_str) == Some("files") {
                    format!(
                        "devenv writes it from a Nix value: `files.\"{file}\".{format} = {{ ... }};`."
                    )
                } else {
                    format!(
                        "Set `source` to a generated file instead of `text`: `source = (pkgs.formats.{format} {{ }}).generate \"{}\" {{ ... }};`.",
                        file.rsplit('/').next().unwrap_or(file)
                    )
                };
                (file.clone(), help)
            }
        };
        let format = format_of(&file)?;
        Some(self.report().diagnostic_with_help(
            node.text_range(),
            format!("`{file}` is written as {} text", format.to_ascii_uppercase()),
            format!(
                "{help} Interpolated values are then quoted and escaped, and the file is always well formed. `pkgs.formats` has json, yaml, toml and ini; `lib.generators` has more."
            ),
        ))
    }
}
