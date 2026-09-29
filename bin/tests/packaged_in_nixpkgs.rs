use std::{fs, process::Command};

/// `statix check -o errfmt` on `devenv.nix` with a small nixpkgs index.
fn check(nix: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("devenv.nix"), nix).unwrap();
    let index = root.join("github.tsv");
    fs::write(
        &index,
        "remoteoss/dexter\tdexter\t0.7.2\tpkgs/by-name/de/dexter/package.nix\n\
         jqlang/jq\tjq\t1.8.2\tpkgs/by-name/jq/jq/package.nix\n\
         virustotal/yara-x\tyara-x\t1.20.0\tpkgs/by-name/ya/yara-x/package.nix\n\
         virustotal/yara-x\tpython3Packages.yara-x\t1.20.0\tpkgs/development/python-modules/yara-x/default.nix\n\
         langchain-ai/langchain\tlangchain-core\t1.2.0\tpkgs/development/python-modules/langchain-core/default.nix\n",
    )
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_statix"))
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("STATIX_NO_CACHE", "1")
        .env("STATIX_NIXPKGS_GITHUB", &index)
        .args(["check", "-o", "errfmt", "devenv.nix"])
        .current_dir(root)
        .output()
        .unwrap();
    String::from_utf8(strip_ansi_escapes::strip(output.stdout))
        .unwrap()
        .lines()
        .filter(|l| l.contains("[packaged_in_nixpkgs]"))
        .map(|l| l.split_once('>').map_or(l, |(_, rest)| rest).to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn same_or_newer_in_nixpkgs() {
    let out = check(
        r#"{ pkgs, ... }: {
  packages = [
    (pkgs.buildGoModule (finalAttrs: {
      pname = "dexter";
      version = "0.7.2";
      src = pkgs.fetchFromGitHub { owner = "remoteoss"; repo = "dexter"; tag = "v${finalAttrs.version}"; hash = ""; };
      vendorHash = "";
    }))
    (pkgs.python3Packages.buildPythonPackage {
      pname = "yara-x";
      version = "1.0.0";
      src = pkgs.fetchFromGitHub { owner = "VirusTotal"; repo = "yara-x"; rev = "v1.0.0"; hash = ""; };
    })
  ];
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        [
            "6:13:W:35:[packaged_in_nixpkgs] `remoteoss/dexter` is in nixpkgs as `pkgs.dexter` (0.7.2, the same version)",
            "12:13:W:35:[packaged_in_nixpkgs] `virustotal/yara-x` is in nixpkgs as `pkgs.python3Packages.yara-x` (1.20.0, newer)",
        ]
    );
}

#[test]
fn hints_and_nothing() {
    let out = check(
        r#"{ pkgs, ... }: {
  a = pkgs.buildGoModule { pname = "dexter"; version = "9.0.0"; src = pkgs.fetchFromGitHub { owner = "remoteoss"; repo = "dexter"; rev = "v9.0.0"; hash = ""; }; };
  b = pkgs.fetchFromGitHub { owner = "jqlang"; repo = "jq"; rev = "0123456789abcdef0123456789abcdef01234567"; hash = ""; };
  c = pkgs.python3Packages.buildPythonPackage { pname = "langchain-openai"; version = "1.0"; src = pkgs.fetchFromGitHub { owner = "langchain-ai"; repo = "langchain"; rev = "v1.0"; hash = ""; }; };
  d = pkgs.buildNpmPackage { pname = "petal-components-mcp"; version = "0.1.0"; src = pkgs.fetchFromGitHub { owner = "petalframework"; repo = "petal-components-mcp"; rev = "v0.1.0"; hash = ""; }; };
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        [
            "3:7:I:35:[packaged_in_nixpkgs] `jqlang/jq` is the source of `pkgs.jq` 1.8.2",
            "4:100:I:35:[packaged_in_nixpkgs] nixpkgs builds packages from `langchain-ai/langchain`: `pkgs.langchain-core`",
        ]
    );
}
