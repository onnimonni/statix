//! Options of commands that run other commands (`sudo`, `env`, `xargs`...).

use super::{Arg, Source, Walker};

/// Leading `-xyz` flags (letters) and the arguments after them.
pub(super) fn split_flags(args: &[Arg]) -> (String, &[Arg]) {
    let mut flags = String::new();
    let mut i = 0;
    while let Some(text) = args.get(i).and_then(|a| a.text.as_deref()) {
        if text == "--" {
            i += 1;
            break;
        }
        match text.strip_prefix('-') {
            Some(f) if !f.is_empty() && !f.starts_with('-') => flags.push_str(f),
            _ => break,
        }
        i += 1;
    }
    (flags, &args[i..])
}

/// Arguments after the options: `value_short`/`value_long` options take a
/// value. Any other option is taken as a flag without value. `None` when an
/// argument is dynamic before the command is found.
pub(super) fn skip_options<'a>(
    args: &'a [Arg],
    value_short: &[char],
    value_long: &[&str],
) -> Option<&'a [Arg]> {
    let mut i = 0;
    while i < args.len() {
        let text = args[i].text.as_deref()?;
        if text == "--" {
            return Some(&args[i + 1..]);
        }
        if let Some(long) = text.strip_prefix("--") {
            if !long.contains('=') && value_long.contains(&long) {
                i += 1;
            }
        } else if let Some(short) = text.strip_prefix('-').filter(|s| !s.is_empty()) {
            // -abc: a value option consumes the rest, or the next argument
            for (j, c) in short.char_indices() {
                if value_short.contains(&c) {
                    if j + c.len_utf8() == short.len() {
                        i += 1;
                    }
                    break;
                }
            }
        } else {
            return Some(&args[i..]);
        }
        i += 1;
    }
    // `exec -a` without its value
    Some(args.get(i..).unwrap_or_default())
}

/// Options of a command that runs another command.
struct Execer {
    /// Short options taking a value.
    value_short: &'static [char],
    /// Long options taking a value (when not given as `--opt=value`).
    value_long: &'static [&'static str],
    /// All other known options (flags); an unknown option means we can't
    /// tell where the command starts.
    flags_short: &'static [char],
    flags_long: &'static [&'static str],
    /// Positional arguments before the command (timeout's duration).
    positionals: usize,
    /// Skip `NAME=value` arguments before the command (env, sudo).
    assignments: bool,
    /// Options that mean no command is run (sudo -v, -l...).
    no_command: &'static [char],
}

const EXECERS: &[(&str, Execer)] = &[
    (
        "sudo",
        Execer {
            value_short: &['C', 'D', 'g', 'h', 'p', 'R', 'U', 'T', 'u'],
            value_long: &[
                "close-from",
                "chdir",
                "group",
                "host",
                "prompt",
                "chroot",
                "other-user",
                "command-timeout",
                "user",
            ],
            flags_short: &['A', 'b', 'E', 'H', 'i', 'k', 'n', 'P', 'S', 's'],
            flags_long: &[
                "askpass",
                "background",
                "preserve-env",
                "set-home",
                "login",
                "reset-timestamp",
                "non-interactive",
                "preserve-groups",
                "stdin",
                "shell",
            ],
            positionals: 0,
            assignments: true,
            no_command: &['e', 'K', 'l', 'V', 'v', 'h'],
        },
    ),
    (
        "doas",
        Execer {
            value_short: &['C', 'u'],
            value_long: &[],
            flags_short: &['n', 's'],
            flags_long: &[],
            positionals: 0,
            assignments: false,
            no_command: &['L'],
        },
    ),
    (
        "env",
        Execer {
            value_short: &['u', 'C', 'P'],
            value_long: &["unset", "chdir"],
            flags_short: &['i', '0', 'v'],
            flags_long: &["ignore-environment", "null", "debug"],
            positionals: 0,
            assignments: true,
            no_command: &[],
        },
    ),
    (
        "xargs",
        Execer {
            value_short: &['a', 'd', 'E', 'I', 'J', 'L', 'n', 'P', 's'],
            value_long: &[
                "arg-file",
                "delimiter",
                "max-lines",
                "max-args",
                "max-procs",
                "max-chars",
                "process-slot-var",
            ],
            flags_short: &['0', 'o', 'p', 'r', 't', 'x'],
            flags_long: &[
                "null",
                "open-tty",
                "interactive",
                "no-run-if-empty",
                "verbose",
                "exit",
            ],
            positionals: 0,
            assignments: false,
            no_command: &[],
        },
    ),
    (
        "nice",
        Execer {
            value_short: &['n'],
            value_long: &["adjustment"],
            flags_short: &[],
            flags_long: &[],
            positionals: 0,
            assignments: false,
            no_command: &[],
        },
    ),
    (
        "nohup",
        Execer {
            value_short: &[],
            value_long: &[],
            flags_short: &[],
            flags_long: &[],
            positionals: 0,
            assignments: false,
            no_command: &[],
        },
    ),
    (
        "timeout",
        Execer {
            value_short: &['s', 'k'],
            value_long: &["signal", "kill-after"],
            flags_short: &['v'],
            flags_long: &["preserve-status", "foreground", "verbose"],
            positionals: 1,
            assignments: false,
            no_command: &[],
        },
    ),
    (
        "stdbuf",
        Execer {
            value_short: &['i', 'o', 'e'],
            value_long: &["input", "output", "error"],
            flags_short: &[],
            flags_long: &[],
            positionals: 0,
            assignments: false,
            no_command: &[],
        },
    ),
    (
        "setsid",
        Execer {
            value_short: &[],
            value_long: &[],
            flags_short: &['c', 'f', 'w'],
            flags_long: &["ctty", "fork", "wait"],
            positionals: 0,
            assignments: false,
            no_command: &[],
        },
    ),
    (
        "chroot",
        Execer {
            value_short: &[],
            value_long: &["userspec", "groups"],
            flags_short: &[],
            flags_long: &["skip-chdir"],
            positionals: 1,
            assignments: false,
            no_command: &[],
        },
    ),
    (
        "time",
        Execer {
            value_short: &['o', 'f'],
            value_long: &["output", "format"],
            flags_short: &['a', 'p', 'v', 'q'],
            flags_long: &["append", "portability", "verbose", "quiet"],
            positionals: 0,
            assignments: false,
            no_command: &[],
        },
    ),
];

/// The command `program` (a command that runs another) runs, or `None` when
/// it runs none or we can't tell (unknown option, dynamic argument).
pub(super) fn runs_command<'a>(
    program: &str,
    args: &'a [Arg],
    _depth: usize,
    walker: &mut Walker,
    src: &Source,
) -> Option<&'a [Arg]> {
    // flock [opts] file -c 'script' | flock [opts] file cmd args
    if program == "flock" {
        let rest = skip_known(
            args,
            &Execer {
                value_short: &['w', 'E'],
                value_long: &["timeout", "conflict-exit-code"],
                flags_short: &['s', 'x', 'u', 'n', 'o', 'F', 'e'],
                flags_long: &[
                    "shared",
                    "exclusive",
                    "unlock",
                    "nonblock",
                    "no-fork",
                    "close",
                    "verbose",
                ],
                positionals: 1,
                assignments: false,
                no_command: &[],
            },
        )?;
        if rest.first().and_then(|a| a.text.as_deref()) == Some("-c") {
            walker.nested(rest.get(1)?, src);
            return None;
        }
        return Some(rest);
    }
    let (_, execer) = EXECERS.iter().find(|(name, _)| *name == program)?;
    skip_known(args, execer)
}

fn skip_known<'a>(args: &'a [Arg], execer: &Execer) -> Option<&'a [Arg]> {
    let mut i = 0;
    let mut positionals = execer.positionals;
    while i < args.len() {
        let text = args[i].text.as_deref()?;
        if text == "--" {
            // `timeout -- 5 cmd`: positionals come after `--` too
            i += 1 + positionals;
            break;
        }
        let digits = text.strip_prefix('-').unwrap_or_default();
        let numeric = !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
        if numeric && !digits.starts_with(execer.flags_short) {
            // `nice -5 cmd`
        } else if let Some(long) = text.strip_prefix("--") {
            let name = long.split('=').next().unwrap_or(long);
            if execer.value_long.contains(&name) {
                if !long.contains('=') {
                    i += 1;
                }
            } else if !execer.flags_long.contains(&name) {
                return None;
            }
        } else if let Some(short) = text.strip_prefix('-').filter(|s| !s.is_empty()) {
            for (j, c) in short.char_indices() {
                if execer.no_command.contains(&c) {
                    return None;
                }
                if execer.value_short.contains(&c) {
                    if j + c.len_utf8() == short.len() {
                        i += 1;
                    }
                    break;
                }
                if !execer.flags_short.contains(&c) {
                    return None;
                }
            }
        } else if execer.assignments && text.contains('=') && !text.starts_with('=') {
            // NAME=value
        } else if positionals > 0 {
            positionals -= 1;
        } else {
            break;
        }
        i += 1;
    }
    let rest = &args[i.min(args.len())..];
    (!rest.is_empty()).then_some(rest)
}
