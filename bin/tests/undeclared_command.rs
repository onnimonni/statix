use std::{fs, path::Path, process::Command};

/// `statix check -o errfmt` on `devenv.nix` with `nix` as content, checking
/// x86_64-linux and aarch64-darwin against a small fake nix-index.
fn check(nix: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("devenv.nix"), nix).unwrap();
    fs::create_dir(root.join("scripts")).unwrap();
    fs::write(root.join("scripts/run.sh"), "echo ok\n").unwrap();

    let linux = root.join("linux.tsv");
    let darwin = root.join("darwin.tsv");
    let common = "jq\tbin\tjq\ncurl\tbin\tcurl\nripgrep\tout\trg\ncoreutils\tout\tls\ncoreutils\tout\tcat\ncoreutils\tout\tuname\ncoreutils\tout\tchroot\ncoreutils\tout\tstat\ncoreutils\tout\tinstall\npnpm\tout\tpnpm\nnodejs\tout\tnode\ngit\tout\tgit\ngnused\tout\tsed\n";
    fs::write(
        &linux,
        format!("{common}strace\tout\tstrace\nxclip\tout\txclip\n"),
    )
    .unwrap();
    fs::write(&darwin, common).unwrap();

    run(root, &linux, &darwin)
}

fn run(root: &Path, linux: &Path, darwin: &Path) -> String {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_statix"))
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("STATIX_NO_CACHE", "1")
        .env("STATIX_SYSTEMS", "x86_64-linux,aarch64-darwin")
        .env(
            "STATIX_PROGRAMS",
            format!(
                "x86_64-linux={}:aarch64-darwin={}",
                linux.display(),
                darwin.display()
            ),
        )
        .args(["check", "-o", "errfmt", "devenv.nix"])
        .current_dir(root)
        .output()
        .unwrap();
    String::from_utf8(strip_ansi_escapes::strip(output.stdout))
        .unwrap()
        .lines()
        .filter(|l| l.contains("[undeclared_command]"))
        .map(|l| l.split_once('>').map_or(l, |(_, rest)| rest).to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn undeclared_command_in_devenv_script() {
    let out = check(
        "{ pkgs, ... }: {\n  packages = [ pkgs.curl ];\n  scripts.x.exec = ''\n    curl -s x | jq .\n  '';\n}\n",
    );
    assert_eq!(out, "4:17:W:31:[undeclared_command] `jq` isn't declared");
}

#[test]
fn declared_scripts_languages_and_stdenv() {
    // pnpm comes from languages.javascript.pnpm, frontend-check is a script,
    // ls/cat come with every devenv shell
    let out = check(
        r#"{ pkgs, ... }: {
  languages.javascript = { enable = true; pnpm.enable = true; };
  scripts.frontend-check.exec = ''
    cd "$DEVENV_ROOT" && exec pnpm check "$@"
  '';
  scripts.ci.exec = ''
    ls | cat
    frontend-check
  '';
}
"#,
    );
    assert_eq!(out, "");
}

#[test]
fn platform_specific_packages() {
    let out = check(
        r"{ pkgs, lib, ... }: {
  packages = [ pkgs.strace ] ++ lib.optionals pkgs.stdenv.isLinux [ pkgs.xclip ];
  enterShell = ''
    strace -f true
    if [[ $(uname) == Darwin ]]; then
      # statix platforms=darwin
      pbcopy < x
    else
      # statix platforms=linux
      xclip -selection clipboard < x
    fi
  '';
}
",
    );
    assert_eq!(
        out,
        "4:5:W:31:[undeclared_command] `strace` (`pkgs.strace`) isn't available on aarch64-darwin"
    );
}

#[test]
fn probes_directives_and_paths() {
    let out = check(
        r"{ pkgs, ... }: {
  scripts.x.exec = ''
    # statix provided=osascript
    if command -v rg >/dev/null; then rg foo; fi
    osascript -e 'x'
    ./scripts/run.sh
    ./scripts/missing.sh
    /run/current-system/sw/bin/foo
    ${pkgs.jq}/bin/jqq .
    ${pkgs.curl}/bin/curl x
    # statix plattforms=darwin
    true
  '';
}
",
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        [
            "7:5:W:31:[undeclared_command] `./scripts/missing.sh` doesn't exist",
            "8:5:W:31:[undeclared_command] `/run/current-system/sw/bin/foo` depends on the host system",
            "9:5:W:31:[undeclared_command] `pkgs.jq` has no program `jqq`",
            "11:5:W:31:[undeclared_command] Unknown statix directive `plattforms=darwin`",
        ]
    );
}

#[test]
fn write_shell_application_and_systemd() {
    let out = check(
        r#"{ pkgs, ... }: {
  a = pkgs.writeShellApplication {
    name = "a";
    runtimeInputs = [ pkgs.curl ];
    text = ''
      curl x | rg y
    '';
  };
  systemd.services.web = {
    path = [ pkgs.git ];
    script = ''
      git pull && ls && jq .
    '';
  };
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        [
            "6:16:W:31:[undeclared_command] `rg` isn't declared",
            "12:25:W:31:[undeclared_command] `jq` isn't declared",
        ]
    );
}

#[test]
fn unknown_declarations_are_not_reported() {
    let out = check(
        "{ pkgs, ... }: {\n  packages = myPackages;\n  enterShell = ''\n    jq .\n  '';\n}\n",
    );
    assert_eq!(out, "");
}

#[test]
fn codex_review_false_positives() {
    // unknown local packages, inherited inputs, snippets, sourced files,
    // cd, heredoc data: nothing to report
    let out = check(
        r#"{ pkgs, lib, runtimeInputs, ... }:
let
  custom = pkgs.writeShellScriptBin "hello" "echo hi";
  setup = ''
    greet() { echo hi; }
  '';
in {
  packages = [ custom ];
  scripts.a.exec = ''
    hello
    cd scripts
    ./run-elsewhere.sh
    cat <<'EOF'
    # statix hello=world
    EOF
  '';
  scripts.b.exec = ''
    ${setup}
    greet
  '';
  x = pkgs.writeShellApplication {
    name = "x";
    inherit runtimeInputs;
    text = "jq .";
  };
}
"#,
    );
    assert_eq!(out, "");
}

#[test]
fn codex_review_missed_findings() {
    let out = check(
        r#"{ pkgs, ... }: {
  # statix platforms=bsd
  a = pkgs.writeShellScript "a" ''
    /run/current-system/sw/bin/foo
    "${pkgs.jq}/bin/jqq" .
    for ((i=0; i<1; i++)); do x=1; done
  '';
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        [
            "2:3:W:31:[undeclared_command] Unknown statix directive `platforms=bsd`",
            "4:5:W:31:[undeclared_command] `/run/current-system/sw/bin/foo` depends on the host system",
            "5:5:W:31:[undeclared_command] `pkgs.jq` has no program `jqq`",
        ]
    );
}

#[test]
fn sed_in_place_on_darwin() {
    // devenv shells have GNU sed; writeShellApplication uses the host's
    let out = check(
        r#"{ pkgs, ... }: {
  scripts.a.exec = "sed -i 's/a/b/' x";
  b = pkgs.writeShellApplication {
    name = "b";
    text = ''
      sed -i 's/a/b/' x
      grep x y | touch z
    '';
  };
  c = pkgs.writeShellApplication {
    name = "c";
    text = "sed -n p x; sed -i.bak 's/a/b/' x";
  };
  d = pkgs.writeShellApplication {
    name = "d";
    runtimeInputs = [ pkgs.gnused ];
    text = "sed -i 's/a/b/' x";
  };
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        [
            "6:7:W:31:[undeclared_command] `sed -i` isn't portable: macOS sed needs `-i ''`, which GNU sed rejects",
            "12:13:W:31:[undeclared_command] `sed` isn't declared",
        ]
    );
}

#[test]
fn runtime_inputs_platforms_and_chroot() {
    // strace is Linux only: so is a script with it in runtimeInputs
    let out = check(
        r#"{ pkgs, ... }: {
  a = pkgs.writeShellApplication {
    name = "a";
    runtimeInputs = [ pkgs.strace pkgs.coreutils ];
    text = ''
      strace -f true
      chroot /tmp/root /busybox uname
    '';
  };
}
"#,
    );
    assert_eq!(out, "");
}

#[test]
fn gnu_only_options_of_host_tools() {
    let out = check(
        r#"{ pkgs, ... }: {
  a = pkgs.writeShellApplication {
    name = "a";
    text = ''
      stat -c %s x
      install -Dm644 a b
      install -m 644 a b
      stat -L x
    '';
  };
  b = pkgs.writeShellApplication {
    name = "b";
    runtimeInputs = [ pkgs.coreutils ];
    text = "stat -c %s x; install -D a b";
  };
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        [
            "5:7:W:31:[undeclared_command] `stat -c` isn't portable: GNU and macOS stat have different options",
            "6:7:W:31:[undeclared_command] `install -Dm644` isn't portable: macOS install lacks GNU's `-D`, `-t`, `-T`, `-Z` and long options",
        ]
    );
}
