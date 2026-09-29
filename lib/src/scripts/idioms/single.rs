//! Single commands: `egrep`, `grep > /dev/null`, `find -exec rm`, `sed -i`...

use super::{
    Context, Idiom, PLACEHOLDER, Piece, Simple, basename, hint, key, one_word, parent, pieces, rest,
};

/// `egrep`/`fgrep` are obsolete names of `grep -E`/`grep -F`.
pub(super) fn renamed_command(text: &str, u: &crate::scripts::commands::Use) -> Option<Idiom> {
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

/// `grep ... > /dev/null`, `find ... -exec rm {} \;`, `echo $(cmd)`,
/// `sed -i 's/a/b/'`, copies into man and completion directories.
#[allow(clippy::too_many_lines)]
pub(super) fn simple_idioms(text: &str, c: &Simple, ctx: Context) -> Option<Idiom> {
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
                    r.kind == crate::scripts::commands::RedirectKind::Write
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
            // `${...}` in the text: shown as the placeholder otherwise
            if script.contains(PLACEHOLDER) {
                return None;
            }
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

/// `$(cat f)`, `substituteInPlace --replace`, `installBin`,
/// `--prefix PATH : ${x}/bin`.
#[allow(clippy::too_many_lines)]
pub(super) fn more_simple_idioms(text: &str, c: &Simple, ctx: Context) -> Option<Idiom> {
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
                || !one_word(c.word(1), file)
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
        _ => None,
    }
}

/// `for f in $(ls dir)`: `for f in dir/*`.
pub(super) fn ls_loop(text: &str, start: usize, end: usize) -> Option<Idiom> {
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
