use rnix::{SyntaxKind, SyntaxNode, TextRange};

pub fn with_preceeding_whitespace(node: &SyntaxNode) -> TextRange {
    let start = node.prev_sibling_or_token().map_or_else(
        || node.text_range().start(),
        |t| {
            if t.kind() == SyntaxKind::TOKEN_WHITESPACE {
                t.text_range().start()
            } else {
                t.text_range().end()
            }
        },
    );
    let end = node.text_range().end();
    TextRange::new(start, end)
}

/// Static name of an attribute (`foo`, `"foo"`), `None` for dynamic keys.
pub fn attr_name(attr: &rnix::ast::Attr) -> Option<String> {
    use rnix::ast::{Attr, InterpolPart};
    use rowan::ast::AstNode as _;
    match attr {
        Attr::Ident(ident) => Some(ident.syntax().text().to_string()),
        Attr::Str(s) => s
            .normalized_parts()
            .into_iter()
            .map(|part| match part {
                InterpolPart::Literal(lit) => Some(lit),
                InterpolPart::Interpolation(_) => None,
            })
            .collect(),
        Attr::Dynamic(_) => None,
    }
}

/// Keys of the attribute sets enclosing `node`, outermost first, e.g. for `x` in
/// `{ a = { b.c = x; }; }` this is `["a", "b", "c"]`. Returns `None` when a key
/// is dynamic or `node` is (inside) a `let` binding, since then it is not an
/// attribute path of the enclosing module.
pub fn enclosing_attrpath(node: &SyntaxNode) -> Option<Vec<String>> {
    use rnix::ast::AttrpathValue;
    use rowan::ast::AstNode as _;
    let mut keys = Vec::new();
    for ancestor in node.ancestors() {
        let Some(apv) = AttrpathValue::cast(ancestor) else {
            continue;
        };
        if apv.syntax().parent()?.kind() != SyntaxKind::NODE_ATTR_SET {
            return None;
        }
        let names: Option<Vec<String>> = apv.attrpath()?.attrs().map(|a| attr_name(&a)).collect();
        keys.splice(0..0, names?);
    }
    Some(keys)
}

const SHELL_PACKAGES: &[&str] = &["bash", "bashInteractive"];

/// Is there a sibling `package = pkgs.<not bash>;` next to this `exec`?
pub fn has_non_shell_package(exec: &rnix::ast::AttrpathValue) -> bool {
    use rnix::ast::HasEntry as _;
    use rowan::ast::AstNode as _;
    let Some(exec_keys) = exec
        .attrpath()
        .map(|p| p.attrs().filter_map(|a| attr_name(&a)).collect::<Vec<_>>())
    else {
        return false;
    };
    let Some(set) = exec.syntax().parent().and_then(rnix::ast::AttrSet::cast) else {
        return false;
    };
    let mut package_keys = exec_keys;
    package_keys.pop();
    package_keys.push("package".into());

    set.attrpath_values().any(|sibling| {
        let keys: Vec<_> = sibling
            .attrpath()
            .map(|p| p.attrs().filter_map(|a| attr_name(&a)).collect())
            .unwrap_or_default();
        let Some(value) = sibling.value() else {
            return false;
        };
        let value = value.syntax().to_string();
        let last = value.trim().rsplit('.').next().unwrap_or_default();
        keys == package_keys && !SHELL_PACKAGES.contains(&last)
    })
}
