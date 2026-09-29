//! `mkdir -p` + `cp` + `chmod`/`chown` is one `install`.

use super::{
    Context, Idiom, Piece, Simple, basename, is_option, key, mkdir_p, only_separators, parent,
    raw_words,
};

/// `install` without `-m` sets mode 755 where `cp` keeps the source's.
const MODE_NOTE: &str = "`install` sets mode 755 unless given `-m`, where `cp` keeps the source's mode: add `-m 644` for data files.";

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

/// `[mkdir -p D;] cp SRC... DST; [chmod|chown|chgrp ... DST]...` at the
/// start of `seq`: the idiom and how many commands it replaces.
#[allow(clippy::too_many_lines)]
pub(super) fn install(
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
