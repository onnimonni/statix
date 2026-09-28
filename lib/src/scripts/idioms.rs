//! Shorter idioms for common command sequences in shell scripts.
//!
//! `mkdir -p d; cp src d/x; chmod 755 d/x; chown u d/x` is one
//! `install -Dm755 -o u src d/x`.

use super::{
    commands::{Commands, RedirectKind, Simple},
    nixstr::PLACEHOLDER,
};

/// `install` without `-m` sets mode 755 where `cp` keeps the source's.
const MODE_NOTE: &str = "`install` sets mode 755 unless given `-m`, where `cp` keeps the source's mode: add `-m 644` for data files.";

/// A sequence of commands with a shorter equivalent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Idiom {
    /// Byte range of the commands in the script.
    pub start: usize,
    pub end: usize,
    /// The replacement: literal text and script words (byte ranges), so it
    /// can be rendered from the script or from the Nix source.
    pub replacement: Vec<Piece>,
    /// What can be shorter, e.g. "`mkdir -p` + `cp` can be one `install`".
    pub message: String,
    /// Caveat for suggestions that aren't exact.
    pub note: Option<&'static str>,
    /// Whether the replacement does exactly the same, words in the same
    /// order. Without `chmod`, `install` sets mode 755 where `cp` keeps the
    /// source's mode.
    pub exact: bool,
}

/// Part of a replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Text(String),
    /// A word of the script, as written.
    Word(usize, usize),
}

impl Idiom {
    /// The replacement with words taken from `word(start, end)`.
    pub fn render(&self, word: impl Fn(usize, usize) -> Option<String>) -> Option<String> {
        self.replacement
            .iter()
            .map(|p| match p {
                Piece::Text(t) => Some(t.clone()),
                Piece::Word(s, e) => word(*s, *e),
            })
            .collect()
    }
}

/// Where a script runs.
#[derive(Debug, Clone, Copy, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Context {
    /// GNU coreutils: `install -D`, `-t`.
    pub gnu: bool,
    /// A stdenv build phase or hook: setup.sh functions (`substituteInPlace`,
    /// `installManPage`...) exist.
    pub build: bool,
    /// bash (not POSIX sh): `$(<file)`.
    pub bash: bool,
    /// A stdenv phase body (`installPhase`...): runs with `set -eu -o pipefail`.
    pub phase: bool,
}

/// Idioms in `commands` of the script `text` running in `ctx`.
#[must_use]
pub fn idioms_in(text: &str, commands: &Commands, ctx: Context) -> Vec<Idiom> {
    let mut out = Vec::new();
    for stages in &commands.pipelines {
        out.extend(useless_cat(text, stages, &commands.defined));
        out.extend(tee_to_null(text, stages));
        out.extend(pipe_idioms(stages));
    }
    for c in &commands.simples {
        out.extend(simple_idioms(text, c, ctx));
        out.extend(more_simple_idioms(text, c, ctx));
    }
    for u in &commands.uses {
        if !commands.defined.contains("grep") {
            out.extend(renamed_command(text, u));
        }
    }
    for &(start, end) in &commands.ls_loops {
        out.extend(ls_loop(text, start, end));
    }
    for seq in &commands.sequences {
        out.extend(cd_and_back(seq));
        out.extend(guarded(seq));
        out.extend(pushd_popd(seq));
        out.extend(sequence_idioms(text, seq, ctx));
        let mut i = 0;
        while i < seq.len() {
            // `mkdir -p d` just before: `d` is a directory
            let prior_dir = i.checked_sub(1).and_then(|p| mkdir_p(text, &seq[p]));
            match install(text, &seq[i..], ctx, prior_dir.as_deref()) {
                Some((idiom, used)) => {
                    out.push(idiom);
                    i += used;
                }
                None => i += 1,
            }
        }
    }
    out.sort_by_key(|i| i.start);
    out
}

/// A hint without a ready replacement.
fn hint(start: usize, end: usize, message: String, note: &'static str) -> Idiom {
    Idiom {
        start,
        end,
        replacement: Vec::new(),
        message,
        note: Some(note),
        exact: false,
    }
}

/// `[text](start, words...)`: a replacement of literal text and words.
fn pieces(parts: impl IntoIterator<Item = Piece>) -> Vec<Piece> {
    parts.into_iter().collect()
}

/// Words `from..` of `c`, each preceded by a space.
fn rest(c: &Simple, from: usize) -> Vec<Piece> {
    c.words
        .iter()
        .skip(from)
        .flat_map(|w| [Piece::Text(" ".into()), Piece::Word(w.1, w.2)])
        .collect()
}

/// `egrep`/`fgrep` are obsolete names of `grep -E`/`grep -F`.
fn renamed_command(text: &str, u: &super::commands::Use) -> Option<Idiom> {
    // `$(which x)`: `$(command -v x)`
    if u.name == "which" {
        let before = text.get(..u.start)?;
        if !(before.ends_with("$(") || before.ends_with('`')) {
            return None;
        }
        return Some(Idiom {
            start: u.start,
            end: u.end,
            replacement: vec![Piece::Text("command -v".into())],
            message: "`which` to find a command: `command -v`".into(),
            note: Some(
                "`command -v` is POSIX and built into the shell; `which` is an external program that may not be installed. It also finds functions and builtins.",
            ),
            exact: false,
        });
    }
    let new = match u.name.as_str() {
        "egrep" => "grep -E",
        "fgrep" => "grep -F",
        _ => return None,
    };
    // the word as written is the name (not `\egrep`, not a path)
    if text.get(u.start..u.end)? != u.name {
        return None;
    }
    Some(Idiom {
        start: u.start,
        end: u.end,
        replacement: vec![Piece::Text(new.into())],
        message: format!("`{}` is an obsolete name for `{new}`", u.name),
        note: None,
        exact: true,
    })
}

/// `grep | wc -l`, `sort | uniq`, `ls | grep`.
fn pipe_idioms(stages: &[Simple]) -> Option<Idiom> {
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
        ([Some("grep"), args @ ..], [Some("wc"), Some("-l")])
            if !args
                .iter()
                .any(|w| matches!(w, Some("-c" | "-o" | "--count" | "--only-matching"))) =>
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
        ([Some("sort"), ..], [Some("uniq")]) => {
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

/// `grep ... > /dev/null`, `find ... -exec rm {} \;`, `echo $(cmd)`,
/// `sed -i 's/a/b/'`, copies into man and completion directories.
#[allow(clippy::too_many_lines)]
fn simple_idioms(text: &str, c: &Simple, ctx: Context) -> Option<Idiom> {
    let word = |i: usize| c.word(i);
    let raw = |i: usize| c.words.get(i).and_then(|w| text.get(w.1..w.2));
    let n = c.words.len();
    match word(0)? {
        // `grep x > /dev/null`: `grep -q x`
        "grep"
            if !c.other
                && !c
                    .words
                    .iter()
                    .any(|w| matches!(w.0.as_deref(), Some("-q" | "--quiet" | "-s")))
                && c.redirects.len() == 1
                && c.redirects.iter().all(|r| {
                    r.kind == super::commands::RedirectKind::Write
                        && r.fd.is_none_or(|fd| fd == 1)
                        && r.target.0.as_deref() == Some("/dev/null")
                }) =>
        {
            let mut r = pieces([Piece::Text("grep -q".into())]);
            r.extend(rest(c, 1));
            Some(Idiom {
                start: c.start(),
                end: c.span.1,
                replacement: r,
                message: "`grep` with its output discarded: `grep -q`".into(),
                note: Some("`grep -q` stops at the first match."),
                exact: false,
            })
        }
        // `find ... -exec rm [-f] {} \;`: `-delete`
        "find" if n >= 5 => {
            let tail: Vec<Option<&str>> = c.words[n.saturating_sub(5)..]
                .iter()
                .map(|w| w.0.as_deref())
                .collect();
            let at = match tail.as_slice() {
                [_, Some("-exec"), Some("rm"), Some("{}"), Some(";" | "+")] => n - 4,
                [
                    Some("-exec"),
                    Some("rm"),
                    Some("-f"),
                    Some("{}"),
                    Some(";" | "+"),
                ] => n - 5,
                _ => return None,
            };
            let (s, _) = (c.words[at].1, 0);
            Some(Idiom {
                start: s,
                end: c.end(),
                replacement: vec![Piece::Text("-delete".into())],
                message: "`find -exec rm`: `find -delete`".into(),
                note: Some(
                    "`-delete` also removes empty directories that match, and implies `-depth`.",
                ),
                exact: false,
            })
        }
        // `echo $(cmd)`: `cmd`
        "echo" if n == 2 && c.redirects.is_empty() => {
            let arg = raw(1)?;
            let inner = arg
                .strip_prefix('"')
                .and_then(|a| a.strip_suffix('"'))
                .unwrap_or(arg)
                .strip_prefix("$(")?
                .strip_suffix(')')?;
            if inner.starts_with('(') || inner.contains(['(', ')', '`']) {
                return None;
            }
            Some(Idiom {
                start: c.start(),
                end: c.end(),
                replacement: vec![Piece::Text(inner.to_string())],
                message: "`echo $(cmd)` prints what `cmd` prints: run `cmd`".into(),
                note: Some(
                    "`echo` joins lines and splits words unless quoted, and hides `cmd`'s exit status.",
                ),
                exact: false,
            })
        }
        // `sed -i 's/a/b/g' f` in a build: `substituteInPlace`
        "sed" if ctx.build && n == 4 && word(1) == Some("-i") => {
            let script = word(2)?;
            // `s/a/b/`, `s|a|b|`, `s#a#b#g`
            let body = script.strip_prefix('s')?;
            let delim = body
                .chars()
                .next()
                .filter(|c| !c.is_alphanumeric() && *c != '\\')?;
            let [from, to, flags] = body[delim.len_utf8()..]
                .splitn(3, delim)
                .collect::<Vec<_>>()[..]
            else {
                return None;
            };
            let literal = |t: &str| {
                !t.is_empty()
                    && !t.contains([
                        '\\', '.', '*', '[', ']', '^', '$', '&', '\'', '+', '?', '(', ')', '{',
                        '}', '|',
                    ])
            };
            if !literal(from) || !(to.is_empty() || literal(to)) || !matches!(flags, "" | "g") {
                return None;
            }
            let file = c.words.get(3)?;
            Some(Idiom {
                start: c.start(),
                end: c.end(),
                replacement: vec![
                    Piece::Text("substituteInPlace ".into()),
                    Piece::Word(file.1, file.2),
                    Piece::Text(format!(" --replace-fail '{from}' '{to}'")),
                ],
                message: "`sed -i` with a literal replacement: `substituteInPlace`".into(),
                note: Some(
                    "`--replace-fail` fails the build when the text isn't there, so the patch can't silently stop applying; it replaces every occurrence, like `s///g`.",
                ),
                exact: false,
            })
        }
        // copies into man pages and shell completions in a build
        "cp" | "install" if ctx.build && n >= 3 => {
            let dst = raw(n - 1)?;
            let (helper, flag) = if dst.contains("share/man/man") {
                ("installManPage", "")
            } else if dst.contains("bash-completion/completions") {
                ("installShellCompletion", " --bash")
            } else if dst.contains("zsh/site-functions") {
                ("installShellCompletion", " --zsh")
            } else if dst.contains("fish/vendor_completions.d") {
                ("installShellCompletion", " --fish")
            } else {
                return None;
            };
            let srcs: Vec<&(Option<String>, usize, usize)> = c.words[1..n - 1]
                .iter()
                .filter(|w| !w.0.as_deref().is_some_and(|t| t.starts_with('-')))
                .collect();
            if srcs.is_empty()
                || c.words[1..n - 1].iter().any(|w| {
                    w.0.as_deref().is_some_and(|t| {
                        let mode = t.strip_prefix("-Dm").or_else(|| t.strip_prefix("-m"));
                        t.starts_with('-')
                            && t != "-D"
                            && t != "-m"
                            && !mode.is_some_and(|m| m.bytes().all(|b| b.is_ascii_digit()))
                    })
                })
            {
                return None;
            }
            // the helpers keep the file's name: not for `cp x.1 .../y.1`
            let named = !dst.ends_with('/')
                && !dst
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .is_some_and(|d| {
                        d.starts_with("man")
                            || d == "completions"
                            || d == "site-functions"
                            || d == "vendor_completions.d"
                    });
            if named && srcs.len() == 1 {
                let src = text.get(srcs[0].1..srcs[0].2)?;
                if basename(&key(src)) != basename(&key(dst)) {
                    return None;
                }
            }
            let mut r = pieces([Piece::Text(format!("{helper}{flag}"))]);
            for w in srcs.iter().filter(|w| {
                !w.0.as_deref()
                    .is_some_and(|t| t.bytes().all(|b| b.is_ascii_digit()))
            }) {
                r.push(Piece::Text(" ".into()));
                r.push(Piece::Word(w.1, w.2));
            }
            Some(Idiom {
                start: c.start(),
                end: c.end(),
                replacement: r,
                message: format!(
                    "Copying into `{}`: `{helper}`",
                    if helper == "installManPage" {
                        "share/man"
                    } else {
                        "completions"
                    }
                ),
                note: Some(
                    "Add `installShellFiles` to `nativeBuildInputs`. It picks the directory (and the `man` output), sets the mode and checks the file isn't empty.",
                ),
                exact: false,
            })
        }
        _ => None,
    }
}

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

fn parent(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map_or("", |(p, _)| p)
}

/// Sequences: `mkdir -p d` before a command that creates `d` itself,
/// `mkdir -p a; mkdir -p b`, `rm -f x; ln -s y x`, `set -e` in a phase.
#[allow(clippy::too_many_lines)]
fn sequence_idioms(text: &str, seq: &[Simple], ctx: Context) -> Vec<Idiom> {
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
                && c.words.get(2).is_some_and(|w| {
                    w.0.as_deref()
                        .is_some_and(|t| !t.contains(['*', '?', '[', '{']))
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
    if ctx.phase
        && let Some(first) = seq.first().or(None)
        && first.start() == text.len() - text.trim_start().len()
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
fn pushd_popd(seq: &[Simple]) -> Vec<Idiom> {
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

/// `$(cat f)`, `substituteInPlace --replace`, `installBin`,
/// `--prefix PATH : ${x}/bin`, `HOME=$(mktemp -d)`, `grep | head -1`.
#[allow(clippy::too_many_lines)]
fn more_simple_idioms(text: &str, c: &Simple, ctx: Context) -> Option<Idiom> {
    let raw = |k: usize| c.words.get(k).and_then(|w| text.get(w.1..w.2));
    let n = c.words.len();
    match c.word(0)? {
        // `$(cat f)` in bash: `$(<f)`
        "cat" if ctx.bash && n == 2 && c.redirects.is_empty() && !c.other => {
            let before = text.get(..c.start())?;
            let after = text.get(c.end()..)?;
            let file = raw(1)?;
            // one file: `$(<a*)` fails when a glob matches several
            if !before.ends_with("$(")
                || !after.starts_with(')')
                || file.starts_with('-')
                || c.word(1).is_none_or(|w| w.contains(['*', '?', '[']))
            {
                return None;
            }
            Some(Idiom {
                start: c.start(),
                end: c.end(),
                replacement: vec![
                    Piece::Text("<".into()),
                    Piece::Word(c.words[1].1, c.words[1].2),
                ],
                message: "`$(cat file)`: bash reads it without `cat` as `$(<file)`".into(),
                note: None,
                exact: true,
            })
        }
        // deprecated `--replace`
        "substituteInPlace" | "substitute" if ctx.build => {
            let w = c
                .words
                .iter()
                .find(|w| w.0.as_deref() == Some("--replace"))?;
            Some(Idiom {
                start: w.1,
                end: w.2,
                replacement: vec![Piece::Text("--replace-fail".into())],
                message: "`--replace` is deprecated: `--replace-fail` or `--replace-warn`".into(),
                note: Some(
                    "`--replace-fail` fails the build when the text isn't found, so a patch can't silently stop applying; use `--replace-warn` where it may be missing.",
                ),
                exact: false,
            })
        }
        // `install -Dm755 x $out/bin/x`: `installBin x`
        "install"
            if ctx.build
                && n == 4
                && matches!(c.word(1), Some("-Dm755" | "-Dm0755" | "-Dm555")) =>
        {
            let (src, dst) = (key(raw(2)?), key(raw(3)?));
            let dir = parent(&dst);
            if !(dir == "$out/bin" || dir == "${out}/bin") || basename(&src) != basename(&dst) {
                return None;
            }
            Some(Idiom {
                start: c.start(),
                end: c.end(),
                replacement: vec![
                    Piece::Text("installBin ".into()),
                    Piece::Word(c.words[2].1, c.words[2].2),
                ],
                message: "Installing a program into `$out/bin`: `installBin`".into(),
                note: Some(
                    "Add `installShellFiles` to `nativeBuildInputs`; `installBin` installs with mode 755 into `$out/bin`.",
                ),
                exact: false,
            })
        }
        // `wrapProgram ... --prefix PATH : ${x}/bin`: `lib.makeBinPath`
        "wrapProgram" | "makeWrapper" | "wrapProgramShell" => {
            let k = c.words.windows(4).position(|w| {
                w[0].0.as_deref() == Some("--prefix")
                    && w[1].0.as_deref() == Some("PATH")
                    && w[2].0.as_deref() == Some(":")
                    && text.get(w[3].1..w[3].2).is_some_and(|v| {
                        let v = v.trim_matches('"');
                        v.starts_with(PLACEHOLDER)
                            && v.ends_with("/bin")
                            && v.matches(PLACEHOLDER).count() == 1
                    })
            })?;
            let value = &c.words[k + 3];
            Some(hint(
                value.1,
                value.2,
                "A package's `bin` for `--prefix PATH`: `lib.makeBinPath`".into(),
                "`--prefix PATH : ${lib.makeBinPath [ pkg ]}` uses the package's `bin` output and takes a list.",
            ))
        }
        // `export HOME=$(mktemp -d)`: `$TMPDIR`
        "export" if ctx.build && n == 2 => {
            let w = raw(1)?;
            if !matches!(w, "HOME=$(mktemp -d)" | "HOME=\"$(mktemp -d)\"") {
                return None;
            }
            Some(Idiom {
                start: c.start(),
                end: c.end(),
                replacement: vec![Piece::Text("export HOME=$TMPDIR".into())],
                message: "A fresh `HOME` in a build: `$TMPDIR` is already one".into(),
                note: Some(
                    "Every build has its own `$TMPDIR`; keep `mktemp -d` if the build needs `HOME` empty and apart from the sources.",
                ),
                exact: false,
            })
        }
        // `cp -r x d` + later `chmod -R u+w d`: `--no-preserve=mode`
        _ => None,
    }
}

/// `for f in $(ls dir)`: `for f in dir/*`.
fn ls_loop(text: &str, start: usize, end: usize) -> Option<Idiom> {
    let word = text.get(start..end)?;
    let inner = word
        .strip_prefix("$(ls")
        .and_then(|w| w.strip_suffix(')'))
        .or_else(|| word.strip_prefix("`ls").and_then(|w| w.strip_suffix('`')))?
        .trim();
    // `$(ls)`, `$(ls dir)`: no options
    if inner.starts_with('-') || inner.contains([' ', '$', '(', '`', '*']) {
        return Some(hint(
            start,
            end,
            "`for ... in $(ls ...)`: use a glob".into(),
            "`ls` output splits file names with spaces; a glob (`dir/*`) doesn't.",
        ));
    }
    let glob = if inner.is_empty() {
        "*".to_string()
    } else {
        format!("{}/*", inner.trim_end_matches('/'))
    };
    Some(Idiom {
        start,
        end,
        replacement: vec![Piece::Text(glob)],
        message: "`for ... in $(ls ...)`: use a glob".into(),
        note: Some(
            "A glob keeps file names with spaces whole; the loop variable then has the directory prefix, and an empty directory leaves the pattern itself unless `nullglob` is set.",
        ),
        exact: false,
    })
}

/// `[ -e f ] && rm f`: `rm -f f`; `[ ! -d d ] && mkdir d`: `mkdir -p d`.
fn guarded(seq: &[Simple]) -> Vec<Idiom> {
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

/// Whether words in `text[a..b]` and `text[c..d]` can swap places in a fix:
/// `${...}` placeholders are put back in order, so not when both have one.
fn can_reorder(text: &str, a: (usize, usize), b: (usize, usize)) -> bool {
    let has = |(s, e): (usize, usize)| text.get(s..e).is_none_or(|t| t.contains(PLACEHOLDER));
    !(has(a) && has(b))
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

fn useless_cat(
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
        exact: can_reorder(text, file, next.span)
            && next
                .word(0)
                .is_some_and(|w| STDIN_FILTERS.contains(&basename(w)) && !defined.contains(w)),
    })
}

/// `echo X | tee FILE > /dev/null` is `echo X > FILE` (`tee -a`: `>>`).
fn tee_to_null(text: &str, stages: &[Simple]) -> Option<Idiom> {
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

/// `cd X; ...; cd ..` (or `cd -`): a subshell `(cd X && ...)` comes back
/// even when a command fails.
fn cd_and_back(seq: &[Simple]) -> Vec<Idiom> {
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

/// The command's words as written (quotes kept). `$out/bin` is dynamic but
/// the same text means the same path within a sequence; `$(...)` isn't.
fn raw_words<'a>(text: &'a str, c: &Simple) -> Option<Vec<&'a str>> {
    c.words
        .iter()
        .map(|(_, s, e)| {
            text.get(*s..*e)
                .filter(|w| !w.contains("$(") && !w.contains('`'))
        })
        .collect()
}

/// A word without quotes, for comparing paths (`"$out/bin"` is `$out/bin`).
fn key(raw: &str) -> String {
    raw.replace(['"', '\''], "")
}

fn is_option(w: &str) -> bool {
    w.starts_with('-')
}

/// `mkdir -p D`: D without quotes.
fn mkdir_p(text: &str, c: &Simple) -> Option<String> {
    let raw = raw_words(text, c)?;
    match (c.word(0), c.word(1), raw.as_slice()) {
        (Some("mkdir"), Some("-p"), [_, _, dir]) => Some(key(dir)),
        _ => None,
    }
}

/// `chmod MODE F`, `chown U[:G] F`, `chgrp G F`: (command, value, target).
fn attribute<'a>(text: &'a str, c: &Simple) -> Option<(&'static str, &'a str, String)> {
    let raw = raw_words(text, c)?;
    let [_, value, target] = raw.as_slice() else {
        return None;
    };
    if is_option(value) {
        return None;
    }
    let command = match c.word(0)? {
        // numeric modes; `+x` is suggested as 755 (not exact)
        "chmod"
            if value.bytes().all(|b| b.is_ascii_digit())
                || matches!(*value, "+x" | "a+x" | "u+x" | "ugo+x") =>
        {
            "chmod"
        }
        "chown" => "chown",
        "chgrp" => "chgrp",
        _ => return None,
    };
    Some((command, value, key(target)))
}

/// Whether only separators (`;`, `&&`, newlines) are between commands, so
/// replacing them all loses nothing (no comments).
fn only_separators(text: &str, cmds: &[Simple]) -> bool {
    cmds.windows(2).all(|p| {
        text.get(p[0].end()..p[1].start()).is_some_and(|between| {
            between
                .chars()
                .all(|c| c.is_whitespace() || c == ';' || c == '&')
        })
    })
}

/// Whether `chmod`/`chown` of the destination provably means the copied
/// file, not an existing directory `cp` copied into: the file is named
/// after a plain source name, or it's in a fresh `$out` directory the
/// sequence just created.
fn provably_file(
    ctx: Context,
    dst: &str,
    into_dir: bool,
    srcs: &[&str],
    mkdir: Option<&str>,
) -> bool {
    let plain = |w: &str| {
        !w.is_empty()
            && w.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
    };
    if into_dir {
        return srcs.len() == 1 && plain(basename(&key(srcs[0]))) && !srcs[0].contains(['"', '\'']);
    }
    let dst = dst.trim_end_matches('/');
    ctx.build
        && (dst.starts_with("$out/") || dst.starts_with("${out}/"))
        && mkdir.is_some_and(|d| parent(dst) == d.trim_end_matches('/'))
}

fn basename(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

/// `[mkdir -p D;] cp SRC... DST; [chmod|chown|chgrp ... DST]...` at the
/// start of `seq`: the idiom and how many commands it replaces.
#[allow(clippy::too_many_lines)]
fn install(
    text: &str,
    seq: &[Simple],
    ctx: Context,
    prior_dir: Option<&str>,
) -> Option<(Idiom, usize)> {
    let gnu = ctx.gnu;
    let mut i = 0;
    let mkdir = seq.first().and_then(|c| mkdir_p(text, c));
    if mkdir.is_some() {
        i += 1;
    }
    let cp = seq.get(i)?;
    if cp.word(0) != Some("cp") {
        return None;
    }
    let raw = raw_words(text, cp)?;
    let args = &raw[1..];
    // globs: which files `chmod` names isn't clear
    if args.len() < 2
        || args
            .iter()
            .any(|a| is_option(a) || a.contains(['*', '?', '[']))
    {
        return None;
    }
    let srcs = &args[..args.len() - 1];
    let dst_raw = args[args.len() - 1];
    let dst = key(dst_raw);
    let dst_dir = dst.trim_end_matches('/');
    // the mkdir must be for where cp copies to
    let mkdir = mkdir.filter(|dir| {
        let dir = dir.trim_end_matches('/');
        dst_dir == dir
            || dst_dir
                .rsplit_once('/')
                .is_some_and(|(parent, _)| parent == dir)
    });
    if mkdir.is_none() && i == 1 {
        return None;
    }
    // cp into a directory: `cp a b dir/`, `cp a dir` after `mkdir -p dir`
    let into_dir = srcs.len() > 1
        || dst.ends_with('/')
        || prior_dir.is_some_and(|d| d.trim_end_matches('/') == dst_dir)
        || mkdir
            .as_ref()
            .is_some_and(|dir| dir.trim_end_matches('/') == dst_dir);
    // the file chmod/chown must name
    let file = if into_dir {
        (srcs.len() == 1).then(|| format!("{dst_dir}/{}", basename(&key(srcs[0]))))
    } else {
        Some(dst.clone())
    };
    i += 1;

    let mut replaced = Vec::new();
    if mkdir.is_some() {
        replaced.push("mkdir -p");
    }
    replaced.push("cp");
    let (mut mode, mut owner, mut group) = (None, None, None);
    // `chmod +x` adds to the mode: `-m 755` is close, not the same
    let mut symbolic = false;
    while let Some((command, value, target)) = seq.get(i).and_then(|c| attribute(text, c)) {
        // `cp x dir; chmod 755 dir` is the directory's mode
        if file.as_deref() != Some(target.as_str()) && (into_dir || target != dst) {
            break;
        }
        match command {
            "chmod" if mode.is_none() => {
                symbolic = value.contains('x');
                mode = Some(if symbolic { "755" } else { value });
            }
            "chown" if owner.is_none() && group.is_none() => match value.split_once(':') {
                Some((u, g)) => {
                    owner = Some(u);
                    group = Some(g);
                }
                None => owner = Some(value),
            },
            "chgrp" if group.is_none() => group = Some(value),
            _ => break,
        }
        replaced.push(command);
        i += 1;
    }
    if replaced.len() < 2 {
        return None;
    }
    // without -D, `mkdir -p` stays: only worth it when something else goes
    let use_d = mkdir.is_some() && gnu;
    if mkdir.is_some() && !gnu && replaced.len() < 3 {
        return None;
    }

    let word = |c: &Simple, k: usize| c.words.get(k).map(|w| Piece::Word(w.1, w.2));
    let mut pieces: Vec<Piece> = Vec::new();
    if mkdir.is_some() && !use_d {
        // mkdir -p D && install ...
        pieces.push(Piece::Text("mkdir -p ".into()));
        pieces.push(word(&seq[0], 2)?);
        pieces.push(Piece::Text(" && ".into()));
    }
    let mut flags = String::new();
    match (use_d, mode) {
        // `-Dm755`
        (true, Some(m)) => flags.push_str(&[" -Dm", m].concat()),
        (true, None) => flags.push_str(" -D"),
        (false, Some(m)) => flags.push_str(&[" -m ", m].concat()),
        (false, None) => {}
    }
    for (flag, value) in [(" -o ", owner), (" -g ", group)] {
        if let Some(v) = value {
            flags.push_str(flag);
            flags.push_str(v);
        }
    }
    pieces.push(Piece::Text(format!("install{flags}")));
    let n = cp.words.len();
    // `-t DIR` reorders words: only for several files
    let reordered = into_dir && srcs.len() > 1 && gnu;
    if reordered {
        pieces.push(Piece::Text(format!(
            " -t {}",
            dst_raw.trim_end_matches('/')
        )));
    }
    for k in 1..n - 1 {
        pieces.push(Piece::Text(" ".into()));
        pieces.push(word(cp, k)?);
    }
    if !reordered {
        pieces.push(Piece::Text(" ".into()));
        pieces.push(word(cp, n - 1)?);
        // one file into a directory: name it, so `-D` creates the directory
        if into_dir && srcs.len() == 1 {
            // only plain names: `"x; y"` would turn into shell syntax
            let unquoted = key(srcs[0]);
            let name = basename(&unquoted);
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
                || srcs[0].contains(['"', '\''])
            {
                return None;
            }
            // `'/etc/x/'name` is fine shell
            let sep = if dst.ends_with('/') { "" } else { "/" };
            pieces.push(Piece::Text(format!("{sep}{}", basename(&key(srcs[0])))));
        }
    }
    let commands = &seq[..i];
    Some((
        Idiom {
            start: commands[0].start(),
            end: commands[i - 1].end(),
            replacement: pieces,
            message: format!("`{}` can be one `install`", replaced.join("` + `")),
            note: match (mode, symbolic) {
                (None, _) => Some(MODE_NOTE),
                (Some(_), true) => Some(
                    "`chmod +x` adds to the file's mode; `-m 755` sets it: check that's the mode you want.",
                ),
                _ => None,
            },
            exact: mode.is_some()
                && !symbolic
                && !reordered
                && only_separators(text, commands)
                && provably_file(ctx, &dst, into_dir, srcs, mkdir.as_deref()),
        },
        i,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripts::commands::commands;

    fn suggest(script: &str, gnu: bool) -> Vec<(String, bool)> {
        idioms_in(
            script,
            &commands(script, "bash"),
            Context {
                gnu,
                ..Context::default()
            },
        )
        .into_iter()
        .map(|i| {
            let text = i.render(|s, e| script.get(s..e).map(String::from));
            (text.unwrap(), i.exact)
        })
        .collect()
    }

    #[test]
    fn pipelines_and_cd() {
        let s = |x: &str| x.to_string();
        assert_eq!(
            suggest("cat data.json | jq -r .name > out\n", true),
            [(s("jq -r .name > out < data.json"), true)]
        );
        assert!(suggest("cat a b | sort\n", true).is_empty());
        assert!(suggest("cat x | sort < y\n", true).is_empty());
        assert_eq!(
            suggest("echo \"$v\" | tee -a log > /dev/null\n", true),
            [(s("echo \"$v\" >> log"), true)]
        );
        assert!(suggest("echo x | tee log\n", true).is_empty());
        assert!(suggest("echo x | sudo tee /etc/x > /dev/null\n", true).is_empty());
        assert_eq!(
            suggest("cd build\nmake\nmake install\ncd ..\n", true),
            [(s("(cd build && make && make install)"), false)]
        );
        assert_eq!(
            suggest("cd a/b; make; cd ../..", true),
            [(s("(cd a/b && make)"), false)]
        );
        assert!(suggest("cd a/b; make; cd ..", true).is_empty());
        assert_eq!(suggest("cd /tmp; make; cd -; cd x; cd ..", true).len(), 1);
    }

    fn suggest_build(script: &str) -> Vec<(String, bool)> {
        idioms_in(
            script,
            &commands(script, "bash"),
            Context {
                gnu: true,
                build: true,
                bash: true,
                phase: true,
            },
        )
        .into_iter()
        .map(|i| {
            let text = i.render(|s, e| script.get(s..e).map(String::from));
            (text.unwrap(), i.exact)
        })
        .collect()
    }

    #[test]
    fn more_idioms() {
        let s = |x: &str| x.to_string();
        assert_eq!(suggest("egrep -v x f", true), [(s("grep -E"), true)]);
        assert_eq!(
            suggest("if grep -w foo f > /dev/null; then :; fi", true),
            [(s("grep -q -w foo f"), false)]
        );
        assert!(suggest("grep foo f > /dev/null 2>&1", true).is_empty());
        assert_eq!(
            suggest("n=$(grep x f | wc -l)", true),
            [(s("grep -c x f"), false)]
        );
        assert_eq!(
            suggest("sort a | uniq > b", true),
            [(s("sort -u a"), false)]
        );
        assert_eq!(
            suggest(r"find . -name '*.o' -exec rm {} \;", true),
            [(s("-delete"), false)]
        );
        assert_eq!(
            suggest("[ -e out.log ] && rm out.log", true),
            [(s("rm -f out.log"), false)]
        );
        assert_eq!(
            suggest("[ ! -d build ] && mkdir build", true),
            [(s("mkdir -p build"), false)]
        );
        assert!(suggest("[ -e a ]; rm a", true).is_empty());
        assert_eq!(
            suggest("for f in $(ls patches); do echo $f; done", true),
            [(s("patches/*"), false)]
        );
        assert_eq!(suggest("x=$(which jq)", true), [(s("command -v"), false)]);
        assert_eq!(
            suggest("echo \"$(date +%s)\"", true),
            [(s("date +%s"), false)]
        );
        // build phases only
        assert!(suggest("sed -i 's/foo/bar/g' Makefile", true).is_empty());
        assert_eq!(
            suggest_build("sed -i 's/foo/bar/g' Makefile"),
            [(
                s("substituteInPlace Makefile --replace-fail 'foo' 'bar'"),
                false
            )]
        );
        assert!(suggest_build("sed -i 's/fo.o/bar/' Makefile").is_empty());
        assert_eq!(
            suggest_build("cp doc/tool.1 $out/share/man/man1/"),
            [(s("installManPage doc/tool.1"), false)]
        );
        assert_eq!(
            suggest_build("install -Dm644 comp.bash $out/share/bash-completion/completions/tool"),
            []
        );
        assert_eq!(
            suggest_build(
                "install -Dm644 tool.fish $out/share/fish/vendor_completions.d/tool.fish"
            ),
            [(s("installShellCompletion --fish tool.fish"), false)]
        );
    }

    #[test]
    fn install() {
        let s = |x: &str| x.to_string();
        assert_eq!(
            suggest(
                "mkdir -p $out/bin\ncp foo $out/bin/foo\nchmod 755 $out/bin/foo\n",
                true
            ),
            // outside a build `$out/bin/foo` could be an existing directory
            [(s("install -Dm755 foo $out/bin/foo"), false)]
        );
        assert_eq!(
            suggest_build("mkdir -p $out/bin\ncp foo $out/bin/foo\nchmod 755 $out/bin/foo\n"),
            [(s("install -Dm755 foo $out/bin/foo"), true)]
        );
        assert_eq!(
            suggest(
                "mkdir -p \"$out/share\" && cp a.txt b.txt \"$out/share\"",
                true
            ),
            [(s("install -D -t \"$out/share\" a.txt b.txt"), false)]
        );
        // one file into a directory: named, no -t
        assert_eq!(
            suggest(
                "mkdir -p $out/bin\ncp ./make $out/bin\nchmod 555 $out/bin/make\n",
                true
            ),
            [(s("install -Dm555 ./make $out/bin/make"), true)]
        );
        assert_eq!(
            suggest(
                "cp app /usr/local/bin/app; chmod 755 /usr/local/bin/app; chown root:wheel /usr/local/bin/app",
                false
            ),
            [(
                s("install -m 755 -o root -g wheel app /usr/local/bin/app"),
                false
            )]
        );
        // BSD install has no -D: mkdir stays
        assert_eq!(
            suggest("mkdir -p d\ncp x d/x\nchmod 644 d/x\n", false),
            [(s("mkdir -p d && install -m 644 x d/x"), false)]
        );
        // a comment in between isn't lost by a fix
        assert_eq!(
            suggest("cp x y\n# make it executable\nchmod 755 y\n", true),
            [(s("install -m 755 x y"), false)]
        );
        // `chmod +x` adds to the mode: a hint for 755
        assert_eq!(
            suggest("cp x y\nchmod +x y\n", true),
            [(s("install -m 755 x y"), false)]
        );
        assert!(suggest("cp x y\nchmod g+w y\n", true).is_empty());
        // unrelated or not worth it
        assert!(suggest("mkdir -p a\ncp x b/x\n", true).is_empty());
        assert!(suggest("cp -r x y\nchmod 755 y\n", true).is_empty());
        assert!(suggest("mkdir -p d\ncp x d/\n", false).is_empty());
        assert!(suggest("cp x y\nchmod 755 z\n", true).is_empty());
        assert!(suggest("mkdir -p d\ncp x d\nchmod 700 d\n", false).is_empty());
        assert!(suggest("cp levels/* out\nchmod 644 out/*\n", true).is_empty());
    }
}
