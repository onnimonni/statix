//! Which programs nixpkgs packages provide, per system.
//!
//! Data comes from nix-index-database's per-system `-small` indexes, which
//! the statix Nix package converts to TSV files (`attr\toutput\tprogram`) and
//! passes as `STATIX_PROGRAMS=system=path:system=path`. Without them a small
//! built-in table is used.

use std::{collections::HashMap, sync::OnceLock};

/// Systems nix-index-database publishes indexes for.
pub const INDEXED_SYSTEMS: &[&str] = &["x86_64-linux", "aarch64-linux", "aarch64-darwin"];

/// Packages whose programs aren't just their own name.
const FALLBACK: &[(&str, &[&str])] = &[
    (
        "coreutils",
        &[
            "[",
            "b2sum",
            "base32",
            "base64",
            "basename",
            "basenc",
            "cat",
            "chcon",
            "chgrp",
            "chmod",
            "chown",
            "chroot",
            "cksum",
            "comm",
            "cp",
            "csplit",
            "cut",
            "date",
            "dd",
            "df",
            "dir",
            "dircolors",
            "dirname",
            "du",
            "echo",
            "env",
            "expand",
            "expr",
            "factor",
            "false",
            "fmt",
            "fold",
            "groups",
            "head",
            "hostid",
            "id",
            "install",
            "join",
            "link",
            "ln",
            "logname",
            "ls",
            "md5sum",
            "mkdir",
            "mkfifo",
            "mknod",
            "mktemp",
            "mv",
            "nice",
            "nl",
            "nohup",
            "nproc",
            "numfmt",
            "od",
            "paste",
            "pathchk",
            "pinky",
            "pr",
            "printenv",
            "printf",
            "ptx",
            "pwd",
            "readlink",
            "realpath",
            "rm",
            "rmdir",
            "runcon",
            "seq",
            "sha1sum",
            "sha224sum",
            "sha256sum",
            "sha384sum",
            "sha512sum",
            "shred",
            "shuf",
            "sleep",
            "sort",
            "split",
            "stat",
            "stdbuf",
            "stty",
            "sum",
            "sync",
            "tac",
            "tail",
            "tee",
            "test",
            "timeout",
            "touch",
            "tr",
            "true",
            "truncate",
            "tsort",
            "tty",
            "uname",
            "unexpand",
            "uniq",
            "unlink",
            "users",
            "vdir",
            "wc",
            "who",
            "whoami",
            "yes",
        ],
    ),
    ("findutils", &["find", "xargs", "locate", "updatedb"]),
    ("gnugrep", &["grep", "egrep", "fgrep"]),
    ("gnused", &["sed"]),
    ("gawk", &["awk", "gawk"]),
    ("diffutils", &["diff", "cmp", "diff3", "sdiff"]),
    ("gnutar", &["tar"]),
    ("gzip", &["gzip", "gunzip", "zcat", "zgrep"]),
    ("bzip2", &["bzip2", "bunzip2", "bzcat"]),
    ("xz", &["xz", "unxz", "xzcat", "lzma", "unlzma"]),
    ("gnumake", &["make"]),
    ("bash", &["bash", "sh"]),
    ("bashInteractive", &["bash", "sh"]),
    ("gnupatch", &["patch"]),
    ("patch", &["patch"]),
    ("ripgrep", &["rg"]),
    ("fd", &["fd"]),
    ("nodejs", &["node", "npm", "npx", "corepack"]),
    ("python3", &["python3", "python"]),
    (
        "openssh",
        &[
            "ssh",
            "scp",
            "sftp",
            "ssh-keygen",
            "ssh-add",
            "ssh-agent",
            "ssh-copy-id",
        ],
    ),
    (
        "procps",
        &[
            "ps", "top", "pgrep", "pkill", "free", "uptime", "watch", "kill", "pidof",
        ],
    ),
    (
        "util-linux",
        &[
            "mount", "umount", "lsblk", "blkid", "fdisk", "script", "setsid", "flock", "column",
            "logger", "uuidgen", "kill", "rename", "hexdump", "getopt",
        ],
    ),
    ("iproute2", &["ip", "ss", "tc", "bridge"]),
    (
        "inetutils",
        &["hostname", "ping", "telnet", "ftp", "traceroute", "whois"],
    ),
    ("gettext", &["gettext", "envsubst", "msgfmt", "ngettext"]),
    (
        "systemd",
        &[
            "systemctl",
            "journalctl",
            "loginctl",
            "systemd-run",
            "busctl",
        ],
    ),
    ("git", &["git"]),
    (
        "nix",
        &[
            "nix",
            "nix-build",
            "nix-shell",
            "nix-store",
            "nix-env",
            "nix-instantiate",
            "nix-prefetch-url",
            "nix-collect-garbage",
            "nix-channel",
            "nix-hash",
        ],
    ),
    ("file", &["file"]),
    ("which", &["which"]),
    ("unzip", &["unzip"]),
    ("zip", &["zip"]),
    (
        "moreutils",
        &["sponge", "ts", "chronic", "parallel", "vipe", "ifne", "pee"],
    ),
];

/// Commands the operating system provides (not from nixpkgs).
pub const OS_PROVIDED: &[(&str, &[&str])] = &[
    (
        "darwin",
        &[
            "pbcopy",
            "pbpaste",
            "osascript",
            "open",
            "defaults",
            "security",
            "launchctl",
            "sw_vers",
            "xcrun",
            "plutil",
            "hdiutil",
            "diskutil",
            "codesign",
            "ditto",
        ],
    ),
    ("linux", &["systemctl", "loginctl"]),
];

/// program -> outputs having it
type Outputs = HashMap<String, Vec<String>>;

#[derive(Default)]
struct Index {
    /// attribute -> programs
    programs: HashMap<String, Outputs>,
    providers: HashMap<String, Vec<String>>,
}

impl Index {
    /// `jq` or `jq.bin` (an output of `jq`): the programs and the output.
    fn entry<'a>(&self, attr: &'a str) -> Option<(&Outputs, Option<&'a str>)> {
        if let Some(programs) = self.programs.get(attr) {
            return Some((programs, None));
        }
        let (base, output) = attr.rsplit_once('.')?;
        let programs = self.programs.get(base)?;
        programs
            .values()
            .any(|outputs| outputs.iter().any(|o| o == output))
            .then_some((programs, Some(output)))
    }

    /// Whether `attr` has `program`. Without an output (`pkgs.jq`) any output
    /// counts: which ones are on PATH (`outputsToInstall`) isn't in the index.
    fn has(&self, attr: &str, program: &str) -> Option<bool> {
        let (programs, output) = self.entry(attr)?;
        let outputs = programs.get(program);
        Some(match output {
            None => outputs.is_some(),
            Some(o) => outputs.is_some_and(|outs| outs.iter().any(|x| x == o)),
        })
    }
}

#[derive(Default)]
pub struct Programs {
    indexes: HashMap<String, Index>,
}

/// The system statix runs on, as a Nix system string.
#[must_use]
pub fn current_system() -> Option<String> {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        _ => return None,
    };
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        _ => return None,
    };
    Some(format!("{arch}-{os}"))
}

fn load(spec: &str) -> Programs {
    use rayon::prelude::*;
    let parts: Vec<(&str, &str)> = spec.split(':').filter_map(|p| p.split_once('=')).collect();
    let indexes = parts
        .par_iter()
        .filter_map(|(system, path)| {
            let text = std::fs::read_to_string(path).ok()?;
            Some(((*system).to_string(), parse_index(&text)))
        })
        .collect();
    Programs { indexes }
}

/// `attr<TAB>output<TAB>program` lines.
fn parse_index(text: &str) -> Index {
    let mut index = Index::default();
    for line in text.lines() {
        let mut cols = line.split('\t');
        let (Some(attr), Some(output), Some(program)) = (cols.next(), cols.next(), cols.next())
        else {
            continue;
        };
        index
            .programs
            .entry(attr.to_string())
            .or_default()
            .entry(program.to_string())
            .or_default()
            .push(output.to_string());
        let providers = index.providers.entry(program.to_string()).or_default();
        if !providers.iter().any(|p| p == attr) {
            providers.push(attr.to_string());
        }
    }
    index
}

/// The package data (loaded once).
#[must_use]
pub fn programs() -> &'static Programs {
    static PROGRAMS: OnceLock<Programs> = OnceLock::new();
    PROGRAMS.get_or_init(|| {
        std::env::var("STATIX_PROGRAMS")
            .map(|spec| load(&spec))
            .unwrap_or_default()
    })
}

/// Last attribute name: `python3Packages.foo` -> `foo`.
fn attr_name(attr: &str) -> &str {
    attr.rsplit('.').next().unwrap_or(attr)
}

impl Programs {
    /// Whether some index knows `attr`.
    #[must_use]
    pub fn knows(&self, attr: &str) -> bool {
        self.indexes.values().any(|i| i.entry(attr).is_some())
    }

    /// Whether `attr` provides `program` on `system`: `Some(true/false)` when
    /// known, `None` when unknown (no index for the system, or the index
    /// doesn't know the attribute).
    #[must_use]
    pub fn provides(&self, system: &str, attr: &str, program: &str) -> Option<bool> {
        let fallback = FALLBACK
            .iter()
            .find(|(a, _)| *a == attr_name(attr))
            .map(|(_, programs)| programs.contains(&program));
        match self.indexes.get(system) {
            Some(index) => match index.has(attr, program) {
                Some(has) => Some(has),
                // known on another system only: not available here
                None if self.knows(attr) => Some(false),
                None => fallback,
            },
            // no data for this system: the table, or "named like the program";
            // anything else is unknown (`custom = writeShellScriptBin "x"`)
            None => fallback.or_else(|| (attr_name(attr) == program).then_some(true)),
        }
    }

    /// Whether `attr` exists on `system`: `None` when unknown.
    #[must_use]
    pub fn available(&self, system: &str, attr: &str) -> Option<bool> {
        let index = self.indexes.get(system)?;
        if index.entry(attr).is_some() {
            Some(true)
        } else if self.knows(attr) {
            Some(false)
        } else {
            None
        }
    }

    /// Packages providing `program` on any system, best first: the one named
    /// like the program, then top-level attributes, then shorter names.
    #[must_use]
    pub fn providers(&self, program: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for index in self.indexes.values() {
            for attr in index.providers.get(program).into_iter().flatten() {
                if !out.contains(attr) {
                    out.push(attr.clone());
                }
            }
        }
        for (attr, programs) in FALLBACK {
            if programs.contains(&program) && !out.iter().any(|a| a == attr) {
                out.push((*attr).to_string());
            }
        }
        out.sort_by_key(|a| (a != program, a.contains('.'), a.len(), a.clone()));
        out
    }
}

/// Systems a platform name covers: `lib.platforms` names (`darwin`, `linux`,
/// `unix`, `all`, `aarch64`, `x86_64`) or a system string. `None` when the
/// name is unknown.
#[must_use]
pub fn platform_matches(name: &str, system: &str) -> Option<bool> {
    let (arch, os) = system.split_once('-')?;
    Some(match name {
        "all" | "unix" => true,
        "darwin" => os == "darwin",
        "linux" => os == "linux",
        "aarch64" => arch == "aarch64",
        "x86_64" => arch == "x86_64",
        s if s.contains('-') => s == system,
        _ => return None,
    })
}

/// Whether the operating system of `system` provides `program`.
#[must_use]
pub fn os_provides(system: &str, program: &str) -> bool {
    OS_PROVIDED.iter().any(|(platform, programs)| {
        platform_matches(platform, system) == Some(true) && programs.contains(&program)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs() {
        let index = parse_index("jq\tbin\tjq\njq\tdev\tjq-config\n");
        assert_eq!(index.has("jq", "jq"), Some(true));
        assert_eq!(index.has("jq.bin", "jq"), Some(true));
        assert_eq!(index.has("jq.dev", "jq"), Some(false));
        assert_eq!(index.has("jq.doc", "jq"), None);
        assert_eq!(index.has("custom", "jq"), None);
    }

    #[test]
    fn fallback_without_index() {
        let p = Programs::default();
        assert_eq!(p.provides("x86_64-linux", "coreutils", "ls"), Some(true));
        assert_eq!(p.provides("x86_64-linux", "coreutils", "jq"), Some(false));
        assert_eq!(p.provides("x86_64-linux", "pkgs.jq", "jq"), Some(true));
        assert_eq!(p.providers("rg"), ["ripgrep"]);
    }

    #[test]
    fn index() {
        let dir = tempfile::tempdir().unwrap();
        let linux = dir.path().join("linux.tsv");
        let darwin = dir.path().join("darwin.tsv");
        std::fs::write(
            &linux,
            "jq\tbin\tjq\nstrace\tout\tstrace\nripgrep\tout\trg\n",
        )
        .unwrap();
        std::fs::write(&darwin, "jq\tbin\tjq\nripgrep\tout\trg\n").unwrap();
        let p = load(&format!(
            "x86_64-linux={}:aarch64-darwin={}",
            linux.display(),
            darwin.display()
        ));
        assert_eq!(p.provides("x86_64-linux", "strace", "strace"), Some(true));
        assert_eq!(
            p.provides("aarch64-darwin", "strace", "strace"),
            Some(false)
        );
        assert_eq!(p.available("aarch64-darwin", "strace"), Some(false));
        assert_eq!(p.provides("x86_64-linux", "jq", "jqq"), Some(false));
        assert_eq!(p.provides("x86_64-linux", "unknown-pkg", "x"), None);
        assert_eq!(p.providers("rg"), ["ripgrep"]);
    }

    #[test]
    fn platforms() {
        assert_eq!(platform_matches("darwin", "aarch64-darwin"), Some(true));
        assert_eq!(platform_matches("linux", "aarch64-darwin"), Some(false));
        assert_eq!(platform_matches("x86_64-linux", "x86_64-linux"), Some(true));
        assert_eq!(platform_matches("bsd", "x86_64-linux"), None);
        assert!(os_provides("aarch64-darwin", "pbcopy"));
        assert!(!os_provides("x86_64-linux", "pbcopy"));
    }
}

/// Settings from `statix.toml`.
#[derive(Debug, Default, Clone)]
pub struct Settings {
    /// Systems to check commands on (default: the Linux systems with an
    /// index, and the system statix runs on).
    pub systems: Option<Vec<String>>,
    /// Commands the environment provides everywhere.
    pub provided: Vec<String>,
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();

/// Set once, before linting.
pub fn set_settings(settings: Settings) {
    let _ = SETTINGS.set(settings);
}

#[must_use]
pub fn settings() -> Settings {
    SETTINGS.get().cloned().unwrap_or_default()
}

/// Systems commands are checked on: `STATIX_SYSTEMS` (comma separated),
/// `systems` in statix.toml, or the Linux systems plus the running one.
#[must_use]
pub fn checked_systems() -> Vec<String> {
    if let Ok(systems) = std::env::var("STATIX_SYSTEMS") {
        return systems
            .split(',')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
    }
    if let Some(systems) = settings().systems {
        return systems;
    }
    let mut systems: Vec<String> = INDEXED_SYSTEMS
        .iter()
        .filter(|s| s.ends_with("-linux"))
        .map(|s| (*s).to_string())
        .collect();
    if let Some(current) = current_system()
        && !systems.contains(&current)
    {
        systems.push(current);
    }
    systems
}
