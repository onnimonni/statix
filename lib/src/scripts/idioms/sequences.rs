//! Sequences: `mkdir -p` merges, `cd`/`pushd` there and back, guards.

use super::{Context, Idiom, Piece, Simple, key, mkdir_p, only_separators, parent};

/// The file these create with its directory (`install -D`, makeWrapper).
fn creates_parent<'a>(text: &'a str, c: &Simple) -> Option<&'a str> {
    let n = c.words.len();
    let raw = |k: usize| c.words.get(k).and_then(|w| text.get(w.1..w.2));
    match c.word(0)? {
        "makeWrapper" if n >= 3 => raw(2),
        "install"
            if c.words.iter().any(|w| {
                w.0.as_deref().is_some_and(|t| {
                    t.starts_with("-D")
                        || t.starts_with('-') && !t.starts_with("--") && t.contains('D')
                })
            }) =>
        {
            raw(n - 1)
        }
        _ => None,
    }
}

/// Sequences: `mkdir -p d` before a command that creates `d` itself,
/// `mkdir -p a; mkdir -p b`, `rm -f x; ln -s y x`, `set -e` in a phase.
#[allow(clippy::too_many_lines)]
pub(super) fn sequence_idioms(text: &str, seq: &[Simple], ctx: Context) -> Vec<Idiom> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < seq.len() {
        let (first, second) = (&seq[i], &seq[i + 1]);
        let raw = |c: &Simple, k: usize| c.words.get(k).and_then(|w| text.get(w.1..w.2));
        let mkdir = mkdir_p(text, first);
        // `mkdir -p $out/bin; makeWrapper x $out/bin/y`: makeWrapper and
        // `install -D` create the directory
        if let Some(dir) = &mkdir
            && ctx.gnu
            && let Some(target) = creates_parent(text, second)
            && key(target).trim_end_matches('/') != dir.trim_end_matches('/')
            && {
                let (p, d) = (parent(&key(target)).to_string(), dir.trim_end_matches('/'));
                p == d || p.starts_with(&format!("{d}/"))
            }
            // nothing else uses the directory
            && !seq[i + 2..].iter().any(|c| text.get(c.start()..c.end()).is_some_and(|t| t.contains(dir.as_str())))
            && only_separators(text, &seq[i..i + 2])
        {
            out.push(Idiom {
                start: first.start(),
                end: second.end(),
                replacement: vec![Piece::Word(second.start(), second.end())],
                message: format!(
                    "`{}` creates the directory: the `mkdir -p` isn't needed",
                    second.word(0).unwrap_or("it")
                ),
                note: None,
                exact: true,
            });
            i += 2;
            continue;
        }
        // `mkdir -p a; mkdir -p b`: `mkdir -p a b`
        // static paths only (globs expand before the first one exists), and
        // not `&&` (the second shouldn't run when the first fails)
        let mergeable = |c: &Simple| {
            mkdir_p(text, c).is_some()
                && !c.after_and
                // `$out/a` is fine; globs and braces expand
                && c.words.get(2).is_some_and(|w| {
                    text.get(w.1..w.2)
                        .is_some_and(|t| !t.contains(['*', '?', '[', '{']) || t.starts_with("${") && !t[2..].contains(['*', '?', '[']))
                })
        };
        if mergeable(first) && mergeable(second) && only_separators(text, &seq[i..i + 2]) {
            let mut j = i + 1;
            while j + 1 < seq.len()
                && mergeable(&seq[j + 1])
                && only_separators(text, &seq[j..j + 2])
            {
                j += 1;
            }
            let mut r = vec![Piece::Text("mkdir -p".into())];
            for c in &seq[i..=j] {
                r.push(Piece::Text(" ".into()));
                r.push(Piece::Word(c.words[2].1, c.words[2].2));
            }
            out.push(Idiom {
                start: first.start(),
                end: seq[j].end(),
                replacement: r,
                message: "`mkdir -p` takes several directories".into(),
                note: None,
                exact: true,
            });
            i = j + 1;
            continue;
        }
        // `rm -f x; ln -s y x`: `ln -sfn y x`
        if first.word(0) == Some("rm")
            && first.word(1) == Some("-f")
            && first.words.len() == 3
            && second.word(0) == Some("ln")
            && second.word(1) == Some("-s")
            && second.words.len() == 4
            && raw(first, 2).map(key) == raw(second, 3).map(key)
            && only_separators(text, &seq[i..i + 2])
        {
            out.push(Idiom {
                start: first.start(),
                end: second.end(),
                replacement: vec![
                    Piece::Text("ln -sfn ".into()),
                    Piece::Word(second.words[2].1, second.words[2].2),
                    Piece::Text(" ".into()),
                    Piece::Word(second.words[3].1, second.words[3].2),
                ],
                message: "`rm -f` then `ln -s`: `ln -sfn` replaces the link".into(),
                note: Some("Unlike `rm -f`, `ln -sfn` doesn't fail when the target is a directory: it links inside it."),
                exact: false,
            });
            i += 2;
            continue;
        }
        i += 1;
    }
    // `set -e` / `set -euo pipefail` first in a stdenv phase
    // first, or right after `runHook preInstall`
    let set_at = match seq.first() {
        Some(c) if c.word(0) == Some("runHook") => seq.get(1),
        c => c,
    };
    if ctx.phase
        && let Some(first) = set_at
        && seq
            .first()
            .is_some_and(|c| c.start() == text.len() - text.trim_start().len())
        && first.word(0) == Some("set")
        && first.words[1..].iter().all(|w| {
            matches!(
                w.0.as_deref(),
                Some("-e" | "-u" | "-eu" | "-ue" | "-euo" | "-eo" | "-o" | "pipefail")
            )
        })
    {
        out.push(Idiom {
            start: first.start(),
            end: first.end(),
            replacement: Vec::new(),
            message: "stdenv phases already run with `set -eu -o pipefail`".into(),
            note: Some("Remove the line."),
            exact: false,
        });
    }
    out
}

/// `pushd d; cmd; popd`: `(cd d && cmd)`.
pub(super) fn pushd_popd(seq: &[Simple]) -> Vec<Idiom> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < seq.len() {
        if seq[i].word(0) != Some("pushd") || seq[i].words.len() != 2 {
            i += 1;
            continue;
        }
        let back = seq[i + 1..]
            .iter()
            .position(|c| matches!(c.word(0), Some("popd" | "pushd" | "cd")));
        let Some(back) = back.map(|b| i + 1 + b) else {
            break;
        };
        if seq[back].word(0) != Some("popd") || back == i + 1 {
            i = back;
            continue;
        }
        let dir = &seq[i].words[1];
        let mut r = vec![Piece::Text("(cd ".into()), Piece::Word(dir.1, dir.2)];
        for c in &seq[i + 1..back] {
            r.push(Piece::Text(" && ".into()));
            r.push(Piece::Word(c.start(), c.end()));
        }
        r.push(Piece::Text(")".into()));
        out.push(Idiom {
            start: seq[i].start(),
            end: seq[back].end(),
            replacement: r,
            message: "`pushd` there and `popd` back: use a subshell".into(),
            note: Some("The subshell returns to the directory even when a command fails, without `pushd`'s output; variables it sets don't survive it."),
            exact: false,
        });
        i = back + 1;
    }
    out
}

/// `[ -e f ] && rm f`: `rm -f f`; `[ ! -d d ] && mkdir d`: `mkdir -p d`.
pub(super) fn guarded(seq: &[Simple]) -> Vec<Idiom> {
    let mut out = Vec::new();
    for pair in seq.windows(2) {
        let [test, cmd] = pair else { continue };
        if !cmd.after_and {
            continue;
        }
        let words: Vec<Option<&str>> = test.words.iter().map(|w| w.0.as_deref()).collect();
        let (neg, op, path) = match words.as_slice() {
            [Some("[" | "test"), Some("!"), Some(op), Some(p), ..] => (true, *op, *p),
            [Some("[" | "test"), Some(op), Some(p), ..] => (false, *op, *p),
            _ => continue,
        };
        let target = cmd.words.last().and_then(|w| w.0.as_deref());
        let (replacement, message, note) = match (neg, op, cmd.word(0), cmd.words.len()) {
            (false, "-e" | "-f" | "-L" | "-h", Some("rm"), 2) if target == Some(path) => (
                "rm -f",
                "`rm` only if it exists: `rm -f`",
                "`rm -f` also removes a dangling symlink and doesn't fail when there's nothing to remove.",
            ),
            (true, "-d" | "-e", Some("mkdir"), 2) if target == Some(path) => (
                "mkdir -p",
                "`mkdir` only if it doesn't exist: `mkdir -p`",
                "`mkdir -p` also creates missing parents.",
            ),
            _ => continue,
        };
        let last = cmd.words.last().map(|w| (w.1, w.2));
        let Some((a, b)) = last else { continue };
        out.push(Idiom {
            start: test.start(),
            end: cmd.end(),
            replacement: vec![Piece::Text(format!("{replacement} ")), Piece::Word(a, b)],
            message: message.into(),
            note: Some(note),
            exact: false,
        });
    }
    out
}

/// `cd X; ...; cd ..` (or `cd -`): a subshell `(cd X && ...)` comes back
/// even when a command fails.
pub(super) fn cd_and_back(seq: &[Simple]) -> Vec<Idiom> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < seq.len() {
        let dir = match (seq[i].word(0), seq[i].words.as_slice()) {
            (Some("cd"), [_, (Some(d), _, _)]) if !d.starts_with('-') && !d.contains("..") => {
                d.clone()
            }
            _ => {
                i += 1;
                continue;
            }
        };
        let depth = dir
            .trim_end_matches('/')
            .split('/')
            .filter(|c| !c.is_empty() && *c != ".")
            .count();
        let back = seq[i + 1..].iter().position(|c| {
            // `cd -` from anywhere, `cd ../..` from a relative directory
            c.word(0) == Some("cd")
                && matches!(c.word(1), Some(b) if b == "-"
                    || !dir.starts_with('/')
                        && b.trim_end_matches('/').split('/').all(|p| p == "..")
                        && b.trim_end_matches('/').split('/').count() == depth)
        });
        let Some(back) = back.map(|b| i + 1 + b) else {
            i += 1;
            continue;
        };
        // another cd in between: where `cd ..` goes isn't clear
        if back == i + 1 || seq[i + 1..back].iter().any(|c| c.word(0) == Some("cd")) {
            i = back + 1;
            continue;
        }
        let mut pieces = vec![
            Piece::Text("(".into()),
            Piece::Word(seq[i].start(), seq[i].end()),
        ];
        for c in &seq[i + 1..back] {
            pieces.push(Piece::Text(" && ".into()));
            pieces.push(Piece::Word(c.start(), c.end()));
        }
        pieces.push(Piece::Text(")".into()));
        out.push(Idiom {
            start: seq[i].start(),
            end: seq[back].end(),
            replacement: pieces,
            message: "`cd` there and back: use a subshell".into(),
            note: Some("The subshell returns to the directory even when a command fails; variables it sets don't survive it."),
            exact: false,
        });
        i = back + 1;
    }
    out
}
