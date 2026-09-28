//! `# statix key=value ...` directives, shellcheck style:
//! `# statix platforms=darwin`, `# statix provided=pbcopy,osascript`,
//! `# statix disable=undeclared_command`. A directive on its own line applies
//! to the next command (simple or compound); before the first command it
//! applies to the whole script. A trailing `# reason` is allowed.

use super::programs::platform_matches;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Directive {
    pub platforms: Option<Vec<String>>,
    pub provided: Vec<String>,
    pub disable: Vec<String>,
    /// Unknown keys or values, for warnings.
    pub unknown: Vec<String>,
}

/// Parse a `# statix ...` comment line; `None` when it isn't one.
#[must_use]
pub fn parse_line(line: &str) -> Option<Directive> {
    let rest = line.trim_start().strip_prefix('#')?.trim_start();
    let rest = rest.strip_prefix("statix")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    // `# reason` after the directives
    let rest = rest.split(" #").next().unwrap_or(rest);
    let mut d = Directive::default();
    for item in rest.split_whitespace() {
        let Some((key, value)) = item.split_once('=') else {
            d.unknown.push(item.to_string());
            continue;
        };
        let values: Vec<String> = value
            .split(',')
            .filter(|v| !v.is_empty())
            .map(String::from)
            .collect();
        match key {
            "platforms" => {
                for v in &values {
                    if platform_matches(v, "x86_64-linux").is_none() {
                        d.unknown.push(format!("platforms={v}"));
                    }
                }
                d.platforms.get_or_insert_with(Vec::new).extend(
                    values
                        .into_iter()
                        .filter(|v| platform_matches(v, "x86_64-linux").is_some()),
                );
            }
            "provided" => d.provided.extend(values),
            "disable" => {
                for v in &values {
                    if v != "all" && !crate::LINTS.iter().any(|l| l.name() == v) {
                        d.unknown.push(format!("disable={v}"));
                    }
                }
                d.disable.extend(values);
            }
            _ => d.unknown.push(item.to_string()),
        }
    }
    Some(d)
}

/// A directive found in a script, and the byte range it covers.
#[derive(Debug, Clone)]
pub struct Scoped {
    pub directive: Directive,
    /// Byte range of the comment.
    pub at: (usize, usize),
    /// Byte range of script text it applies to.
    pub scope: (usize, usize),
}

/// Directives in `text`, scoped with the command `spans` (outermost first
/// for equal starts). Lines inside `data` (here-documents, multi-line
/// strings) aren't comments.
#[must_use]
pub fn scoped(text: &str, spans: &[(usize, usize)], data: &[(usize, usize)]) -> Vec<Scoped> {
    let first_command = spans.iter().map(|s| s.0).min().unwrap_or(usize::MAX);
    let mut out = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        if data.iter().any(|&(a, b)| a <= start && start < b) {
            continue;
        }
        let Some(directive) = parse_line(line) else {
            continue;
        };
        let end = start + line.trim_end().len();
        let scope = if start < first_command {
            (0, text.len())
        } else {
            spans
                .iter()
                .filter(|s| s.0 >= end)
                .min_by_key(|s| (s.0, std::cmp::Reverse(s.1)))
                .copied()
                .unwrap_or((end, end))
        };
        out.push(Scoped {
            directive,
            at: (start, end),
            scope,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let d = parse_line("  # statix provided=pbcopy platforms=darwin # macOS only").unwrap();
        assert_eq!(d.provided, ["pbcopy"]);
        assert_eq!(d.platforms, Some(vec!["darwin".to_string()]));
        assert!(d.unknown.is_empty());
        assert!(parse_line("# statixfoo").is_none());
        assert!(parse_line("# not statix").is_none());
        let bad = parse_line("# statix platform=darwin platforms=bsd").unwrap();
        assert_eq!(bad.unknown, ["platform=darwin", "platforms=bsd"]);
    }

    #[test]
    fn scopes() {
        let text = "# statix platforms=linux\necho a\n# statix provided=pbcopy\nif x; then\n  pbcopy\nfi\n";
        // spans: `echo a` and the `if` block (and its inner commands)
        let echo = (25, 31);
        let if_start = text.find("if x").unwrap();
        let if_block = (if_start, text.len() - 1);
        let pb = text.find("pbcopy\n").unwrap();
        let spans = [echo, if_block, (pb, pb + 6)];
        let d = scoped(text, &spans, &[]);
        assert_eq!(d[0].scope, (0, text.len()));
        assert_eq!(d[1].scope, if_block);
    }
}
