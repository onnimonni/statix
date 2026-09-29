//! Shorter idioms for common command sequences in shell scripts.
//!
//! `mkdir -p d; cp src d/x; chmod 755 d/x; chown u d/x` is one
//! `install -Dm755 -o u src d/x`.

use super::{
    commands::{Commands, RedirectKind, Simple},
    nixstr::PLACEHOLDER,
};

mod install;
mod pipes;
mod sequences;
mod single;

use install::install;
use pipes::{pipe_idioms, tee_to_null, useless_cat};
use sequences::{cd_and_back, guarded, pushd_popd, sequence_idioms};
use single::{ls_loop, more_simple_idioms, renamed_command, simple_idioms};

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

fn parent(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map_or("", |(p, _)| p)
}

/// Whether a word is exactly one word after expansion: static without glob
/// characters, or double quoted (`"$f"`). `$files` may be none or several.
fn one_word(text: Option<&str>, raw: &str) -> bool {
    match text {
        Some(t) => !t.contains(['*', '?', '[']),
        None => raw.starts_with('"') && raw.ends_with('"') && !raw.contains(['*', '?', '[']),
    }
}

/// Whether words in `text[a..b]` and `text[c..d]` can swap places in a fix:
/// `${...}` placeholders are put back in order, so not when both have one.
fn can_reorder(text: &str, a: (usize, usize), b: (usize, usize)) -> bool {
    let has = |(s, e): (usize, usize)| text.get(s..e).is_none_or(|t| t.contains(PLACEHOLDER));
    !(has(a) && has(b))
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

#[cfg(test)]
mod tests;
