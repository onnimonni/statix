//! Pipelines: `cat f | cmd`, `echo | tee`, `grep | wc -l`, `sort | uniq`...

use super::{
    Idiom, Piece, RedirectKind, Simple, basename, can_reorder, hint, one_word, pieces, rest,
};

/// `grep | wc -l`, `sort | uniq`, `ls | grep`.
pub(super) fn pipe_idioms(stages: &[Simple]) -> Option<Idiom> {
    fn words(c: &Simple) -> Vec<Option<&str>> {
        c.words.iter().map(|w| w.0.as_deref()).collect()
    }
    let [a, b] = stages else { return None };
    if a.other || b.other || !a.redirects.is_empty() {
        return None;
    }
    let start = a.start();
    let end = b.span.1;
    match (words(a).as_slice(), words(b).as_slice()) {
        // one input (stdin or one file): `grep -c` counts per file
        ([Some("grep"), args @ ..], [Some("wc"), Some("-l")])
            if args
                .iter()
                .all(|w| w.is_some_and(|t| !t.contains(['*', '?', '['])))
                && args
                    .iter()
                    .filter(|w| !w.is_some_and(|t| t.starts_with('-')))
                    .count()
                    <= 2
                && !args.iter().any(|w| {
                    matches!(
                        w,
                        Some(
                            "-c" | "-o"
                                | "--count"
                                | "--only-matching"
                                | "-r"
                                | "-R"
                                | "--recursive"
                                | "-e"
                                | "-f"
                        )
                    )
                }) =>
        {
            let mut r = pieces([Piece::Text("grep -c".into())]);
            r.extend(rest(a, 1));
            Some(Idiom {
                start,
                end: b.end(),
                replacement: r,
                message: "`grep | wc -l` counts matching lines: `grep -c`".into(),
                note: Some("With several files `grep -c` prints a count per file."),
                exact: false,
            })
        }
        (
            [Some("grep"), args @ ..],
            [Some("head"), Some("-1" | "-n1")] | [Some("head"), Some("-n"), Some("1")],
        ) if !args.iter().any(|w| matches!(w, Some("-c" | "-l" | "-L"))) => {
            let mut r = pieces([Piece::Text("grep -m1".into())]);
            r.extend(rest(a, 1));
            Some(Idiom {
                start,
                end: b.end(),
                replacement: r,
                message: "`grep | head -1`: `grep -m1` stops at the first match".into(),
                note: Some("With several files `grep -m1` prints the first match of each."),
                exact: false,
            })
        }
        // `sort -k2 | uniq`: `-u` would dedup by key
        ([Some("sort"), sort_args @ ..], [Some("uniq")])
            if sort_args
                .iter()
                .all(|w| w.is_some_and(|t| !t.starts_with('-') || t == "-r")) =>
        {
            let mut r = pieces([Piece::Text("sort -u".into())]);
            r.extend(rest(a, 1));
            Some(Idiom {
                start,
                end: b.end(),
                replacement: r,
                message: "`sort | uniq` is `sort -u`".into(),
                note: Some(
                    "`sort -u` compares lines with the locale's collation, `uniq` byte by byte: set `LC_ALL=C` if that matters.",
                ),
                exact: false,
            })
        }
        ([Some("ls"), ..], [Some("grep"), ..]) => Some(hint(
            start,
            end,
            "`ls | grep` to find files: use a glob".into(),
            "`ls` output isn't meant to be parsed; a glob like `for f in dir/*.ext` handles every file name.",
        )),
        _ => None,
    }
}

/// `cat FILE | cmd` is `cmd < FILE`.
/// Programs that behave the same reading a file on stdin as from a pipe
/// (external: no shell state to lose by leaving the pipeline's subshell).
const STDIN_FILTERS: &[&str] = &[
    "grep",
    "sed",
    "awk",
    "gawk",
    "jq",
    "yq",
    "sort",
    "uniq",
    "head",
    "tail",
    "wc",
    "tr",
    "cut",
    "xz",
    "gzip",
    "gunzip",
    "bzip2",
    "zstd",
    "base64",
    "sha256sum",
    "md5sum",
    "cpio",
    "tar",
    "nixfmt",
];

pub(super) fn useless_cat(
    text: &str,
    stages: &[Simple],
    defined: &std::collections::HashSet<String>,
) -> Option<Idiom> {
    let [cat, next, ..] = stages else { return None };
    let file = match (cat.word(0), cat.words.as_slice()) {
        (Some("cat"), [_, (_, s, e)]) => (*s, *e),
        _ => return None,
    };
    let raw = text.get(file.0..file.1)?;
    if !cat.redirects.is_empty()
        || cat.other
        || next.other
        || raw.starts_with('-')
        || raw.contains(['*', '?', '['])
        // `cmd < other`: two inputs
        || next.redirects.iter().any(|r| r.kind == RedirectKind::Read || r.fd == Some(0))
    {
        return None;
    }
    Some(Idiom {
        start: cat.start(),
        end: next.span.1,
        replacement: vec![
            Piece::Word(next.span.0, next.span.1),
            Piece::Text(" < ".into()),
            Piece::Word(file.0, file.1),
        ],
        // `${pkgs.postgresql}/bin/psql`: psql
        message: format!(
            "`cat` only feeds `{}`: read the file directly",
            next.word(0).map_or("the command", basename)
        ),
        note: None,
        // builtins (`read`) would leave the pipeline's subshell
        // `< $files` is an ambiguous redirect unless it's one word
        exact: can_reorder(text, file, next.span)
            && one_word(cat.word(1), raw)
            && next
                .word(0)
                .is_some_and(|w| STDIN_FILTERS.contains(&basename(w)) && !defined.contains(w)),
    })
}

/// `echo X | tee FILE > /dev/null` is `echo X > FILE` (`tee -a`: `>>`).
pub(super) fn tee_to_null(text: &str, stages: &[Simple]) -> Option<Idiom> {
    let [echo, tee] = stages else { return None };
    // `printf -v` sets a variable
    if echo.word(1) == Some("-v") {
        return None;
    }
    if !matches!(echo.word(0), Some("echo" | "printf")) || !echo.redirects.is_empty() || echo.other
    {
        return None;
    }
    let (append, file) = match (tee.word(0), tee.word(1), tee.words.as_slice()) {
        (Some("tee"), Some("-a"), [_, _, (_, s, e)]) => (true, (*s, *e)),
        (Some("tee"), _, [_, (_, s, e)]) => (false, (*s, *e)),
        _ => return None,
    };
    if text.get(file.0..file.1)?.starts_with('-') || tee.other {
        return None;
    }
    // tee's own output goes nowhere
    let [r] = tee.redirects.as_slice() else {
        return None;
    };
    if r.kind != RedirectKind::Write
        || r.fd.is_some_and(|fd| fd != 1)
        || r.target.0.as_deref() != Some("/dev/null")
    {
        return None;
    }
    let op = if append { " >> " } else { " > " };
    Some(Idiom {
        start: echo.start(),
        end: tee.span.1,
        replacement: vec![
            Piece::Word(echo.span.0, echo.span.1),
            Piece::Text(op.into()),
            Piece::Word(file.0, file.1),
        ],
        message: format!(
            "`{}` into `tee` whose output is discarded is a redirection",
            echo.word(0).unwrap_or("echo")
        ),
        note: None,
        exact: true,
    })
}
