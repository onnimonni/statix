//! Shorter idioms for common command sequences in shell scripts.
//!
//! `mkdir -p d; cp src d/x; chmod 755 d/x; chown u d/x` is one
//! `install -Dm755 -o u src d/x`.

use super::commands::{Commands, Simple};

/// A sequence of commands with a shorter equivalent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Idiom {
    /// Byte range of the commands in the script.
    pub start: usize,
    pub end: usize,
    /// The replacement: literal text and script words (byte ranges), so it
    /// can be rendered from the script or from the Nix source.
    pub replacement: Vec<Piece>,
    /// The commands replaced, for the message (`mkdir -p` + `cp` + `chmod`).
    pub replaced: Vec<&'static str>,
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

/// Idioms in `commands` of the script `text`. `gnu`: GNU coreutils run it
/// (build phases, devenv, NixOS), so `install -D` and `-t` work.
#[must_use]
pub fn idioms(text: &str, commands: &Commands, gnu: bool) -> Vec<Idiom> {
    let mut out = Vec::new();
    for seq in &commands.sequences {
        let mut i = 0;
        while i < seq.len() {
            // `mkdir -p d` just before: `d` is a directory
            let prior_dir = i.checked_sub(1).and_then(|p| mkdir_p(text, &seq[p]));
            match install(text, &seq[i..], gnu, prior_dir.as_deref()) {
                Some((idiom, used)) => {
                    out.push(idiom);
                    i += used;
                }
                None => i += 1,
            }
        }
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
        // numeric modes only: `install -m +x` isn't `chmod +x`
        "chmod" if value.bytes().all(|b| b.is_ascii_digit()) => "chmod",
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
    gnu: bool,
    prior_dir: Option<&str>,
) -> Option<(Idiom, usize)> {
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
    while let Some((command, value, target)) = seq.get(i).and_then(|c| attribute(text, c)) {
        // `cp x dir; chmod 755 dir` is the directory's mode
        if file.as_deref() != Some(target.as_str()) && (into_dir || target != dst) {
            break;
        }
        match command {
            "chmod" if mode.is_none() => mode = Some(value),
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
            replaced,
            exact: mode.is_some() && !reordered && only_separators(text, commands),
        },
        i,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripts::commands::commands;

    fn suggest(script: &str, gnu: bool) -> Vec<(String, bool)> {
        idioms(script, &commands(script, "bash"), gnu)
            .into_iter()
            .map(|i| {
                let text = i.render(|s, e| script.get(s..e).map(String::from));
                (text.unwrap(), i.exact)
            })
            .collect()
    }

    #[test]
    fn install() {
        let s = |x: &str| x.to_string();
        assert_eq!(
            suggest(
                "mkdir -p $out/bin\ncp foo $out/bin/foo\nchmod 755 $out/bin/foo\n",
                true
            ),
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
                true
            )]
        );
        // BSD install has no -D: mkdir stays
        assert_eq!(
            suggest("mkdir -p d\ncp x d/x\nchmod 644 d/x\n", false),
            [(s("mkdir -p d && install -m 644 x d/x"), true)]
        );
        // a comment in between isn't lost by a fix
        assert_eq!(
            suggest("cp x y\n# make it executable\nchmod 755 y\n", true),
            [(s("install -m 755 x y"), false)]
        );
        // symbolic modes are relative to the file's mode
        assert!(suggest("cp x y\nchmod +x y\n", true).is_empty());
        // unrelated or not worth it
        assert!(suggest("mkdir -p a\ncp x b/x\n", true).is_empty());
        assert!(suggest("cp -r x y\nchmod 755 y\n", true).is_empty());
        assert!(suggest("mkdir -p d\ncp x d/\n", false).is_empty());
        assert!(suggest("cp x y\nchmod 755 z\n", true).is_empty());
        assert!(suggest("mkdir -p d\ncp x d\nchmod 700 d\n", false).is_empty());
        assert!(suggest("cp levels/* out\nchmod 644 out/*\n", true).is_empty());
    }
}
