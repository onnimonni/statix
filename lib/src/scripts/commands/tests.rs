use super::*;

#[test]
fn nested_positions_stay_in_bounds() {
    for script in ["eval $'\u{e9}'", "eval \"a\\\"b\"; bash -c $'x\\ty'"] {
        let c = commands(script, "bash");
        assert!(
            c.uses
                .iter()
                .all(|u| u.end <= script.len() && script.is_char_boundary(u.start))
        );
    }
    let c = commands(
        "cd x; source ./lib.sh; cat <<EOF\n# statix x=y\nEOF\n",
        "bash",
    );
    assert!(c.changes_dir && c.sources && !c.data.is_empty());
}

fn names(script: &str) -> Vec<String> {
    commands(script, "bash")
        .uses
        .into_iter()
        .map(|u| u.name)
        .collect()
}

#[test]
fn resholve_cases() {
    let cases: &[(&str, &[&str])] = &[
        ("HOME=oops LC_ALL=c file heh", &["file"]),
        (
            "env LC_ALL=c HOME=y find /x -name find -exec file {} + -executable",
            &["env", "find", "file"],
        ),
        (
            "builtin builtin command command xargs xargs find . -exec xargs find {} +",
            &["xargs", "xargs", "find", "xargs", "find"],
        ),
        ("f(){ echo \"$(file x)\"; }; f file", &["file"]),
        ("exec find file", &["find"]),
        ("echo w | xargs file", &["xargs", "file"]),
        (
            "find . -print0 | xargs -0 -r mv -t x",
            &["find", "xargs", "mv"],
        ),
        ("timeout -s SIGTERM 1 ls -la .", &["timeout", "ls"]),
        (
            "alias find=\"find -H\"; find; \\find; command find",
            &["find", "find"],
        ),
        ("x(){ command which \"$@\"; }", &["which"]),
        ("which foo; which(){ file \"$@\"; }", &["file"]),
        ("exec >&2; <hehe", &[]),
        ("coproc file", &["file"]),
        ("$@; \"$1\"; $0; $CMD", &[]),
        ("eval echo blah; eval \"jq . x\"", &["jq"]),
        ("trap 'rm -rf \"$d\"' EXIT", &["rm"]),
        ("bash -c 'curl -s x | jq .'", &["bash", "curl", "jq"]),
        ("sudo -u root rm -rf /x", &["sudo", "rm"]),
        ("sudo -v", &["sudo"]),
        ("nice -n 5 make", &["nice", "make"]),
        (
            "x=$(git rev-parse HEAD); echo \"${x:-$(date)}\"",
            &["git", "date"],
        ),
        // runners that are functions don't run their arguments
        ("sudo() { echo ok; }; sudo imaginary", &[]),
        ("timeout -- 5 ls", &["timeout", "ls"]),
        ("nice -5 ls", &["nice", "ls"]),
        ("$'j\\x71' .", &[]),
        ("for ((i=0; i<1; i++)); do jq .; done", &["jq"]),
        ("exec -a", &[]),
        ("env \u{e9}", &["env", "\u{e9}"]),
        (
            "sudo() { :; }; command sudo x; exec sudo y",
            &["sudo", "x", "sudo", "y"],
        ),
        (
            "cat <(sort a) | while read -r l; do tr a b <<< \"$l\"; done",
            &["cat", "sort", "tr"],
        ),
        (
            "if [[ $(uname) == Darwin ]]; then pbcopy; else xclip; fi",
            &["uname", "pbcopy", "xclip"],
        ),
        (
            "__nix_interp__/bin/jq . x; /usr/bin/env python3 x; ./run.sh",
            &[
                "__nix_interp__/bin/jq",
                "/usr/bin/env",
                "python3",
                "./run.sh",
            ],
        ),
        ("strace -f make", &["strace"]),
        ("timeout --unknown-flag 1 ls", &["timeout"]),
    ];
    for (script, expected) in cases {
        assert_eq!(names(script), *expected, "{script}");
    }
}

#[test]
fn static_writes() {
    let c = commands(
        "cat > a.json <<'EOF'\n{ \"a\": 1 }\nEOF\ncat > \"$out/b.yaml\" <<EOF\nb: $HOME\nEOF\ncat >> c.toml <<EOF\nc = 1\nEOF\necho '{}' > d.json\necho \"$x\" > e.json\ncat <<EOF > f.toml\nf = 1\nEOF\n",
        "bash",
    );
    let files: Vec<&str> = c.writes.iter().map(|w| w.file.as_str()).collect();
    assert_eq!(files, ["a.json", "d.json", "f.toml"]);
    assert_eq!(c.writes[0].text, "{ \"a\": 1 }\n");
}

#[test]
fn probes_are_optional() {
    let c = commands(
        "if command -v jq >/dev/null; then jq .; fi; type -p rg; which fd; hash bat; [ -f ./a.sh ] && ./a.sh; [[ -x ./b ]]",
        "bash",
    );
    let probed: Vec<&str> = {
        let mut v: Vec<&str> = c.probed.iter().map(String::as_str).collect();
        v.sort_unstable();
        v
    };
    assert_eq!(probed, ["./a.sh", "./b", "bat", "fd", "jq", "rg"]);
    assert!(c.uses.iter().any(|u| u.name == "jq"));
}

#[test]
fn positions_are_bytes_in_the_script() {
    let script = "echo ä; jq .\nx=$(curl -s y)";
    let c = commands(script, "bash");
    let jq = c.uses.iter().find(|u| u.name == "jq").unwrap();
    assert_eq!(&script[jq.start..jq.end], "jq");
    let curl = c.uses.iter().find(|u| u.name == "curl").unwrap();
    assert_eq!(&script[curl.start..curl.end], "curl");
}

#[test]
fn dash_builtins() {
    assert_eq!(commands("local x; mapfile y", "sh").uses.len(), 1);
}

#[test]
fn broken_script_is_incomplete() {
    assert!(commands("if then fi (", "bash").incomplete);
}
