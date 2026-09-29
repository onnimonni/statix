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
         langchain-ai/langchain\tpython3Packages.langchain-core\t1.2.0\tpkgs/development/python-modules/langchain-core/default.nix\tlangchain-core\t\n\
         0ldsk00l/nestopia\tnestopia-ue\t1.53.2\tpkgs/by-name/ne/nestopia-ue/package.nix\tnestopia\t\n\
         deltachat/deltachat-tauri\tdeltachat-tauri\t2.60.0\tpkgs/by-name/de/deltachat-tauri/package.nix\tdeltachat-tauri\tbroken\n",
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
            "4:100:I:35:[packaged_in_nixpkgs] nixpkgs builds packages from `langchain-ai/langchain`: `pkgs.python3Packages.langchain-core`",
        ]
    );
}

#[test]
fn codex_review() {
    let out = check(
        r#"{ pkgs, ... }: let org = "my-fork"; in {
  a = pkgs.buildGoModule { pname = "dexter"; version = "0.7.2"; org = "remoteoss"; src = pkgs.fetchFromGitHub { owner = org; repo = "dexter"; tag = "v0.7.2"; hash = ""; }; };
  b = pkgs.buildGoModule { pname = "dexter"; version = "0.7.2"; src = ./.; patches = [ (pkgs.fetchurl { url = "https://github.com/remoteoss/dexter/commit/abc.patch"; hash = ""; }) ]; };
  c = pkgs.buildGoModule { pname = "dexter"; version = "0.7.2"; src = pkgs.fetchFromGitHub { owner = "remoteoss"; repo = "dexter"; rev = "0123456789abcdef0123456789abcdef01234567"; hash = ""; }; };
  d = pkgs.stdenv.mkDerivation { pname = "nestopia"; version = "1.53.2"; src = pkgs.fetchFromGitHub { owner = "0ldsk00l"; repo = "nestopia"; tag = "1.53.2"; hash = ""; }; };
  e = pkgs.rustPlatform.buildRustPackage { pname = "deltachat-tauri"; version = "2.59.0"; src = pkgs.fetchFromGitHub { owner = "deltachat"; repo = "deltachat-tauri"; tag = "v2.59.0"; hash = ""; }; };
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        [
            "4:71:I:35:[packaged_in_nixpkgs] `remoteoss/dexter` is in nixpkgs as `pkgs.dexter` 0.7.2",
            "5:80:W:35:[packaged_in_nixpkgs] `0ldsk00l/nestopia` is in nixpkgs as `pkgs.nestopia-ue` (1.53.2, the same version)",
            "6:97:I:35:[packaged_in_nixpkgs] `deltachat/deltachat-tauri` is in nixpkgs as `pkgs.deltachat-tauri` 2.60.0 (broken there)",
        ]
    );
}

#[test]
fn fable_review() {
    let out = check(
        r#"{ pkgs, ... }: {
  # an override is nixpkgs' package already
  a = pkgs.dexter.overrideAttrs (o: { src = pkgs.fetchFromGitHub { owner = "remoteoss"; repo = "dexter"; tag = "v0.7.2"; hash = ""; }; });
  # the source through a let
  b = let src = pkgs.fetchFromGitHub { owner = "remoteoss"; repo = "dexter"; tag = "v0.7.2"; hash = ""; }; in pkgs.buildGoModule { pname = "dexter"; version = "0.7.2"; inherit src; };
  # silenced
  # statix disable=packaged_in_nixpkgs
  c = pkgs.buildGoModule { pname = "dexter"; version = "0.7.2"; src = pkgs.fetchFromGitHub { owner = "remoteoss"; repo = "dexter"; tag = "v0.7.2"; hash = ""; }; };
  # older in nixpkgs, broken there: nothing
  d = pkgs.rustPlatform.buildRustPackage { pname = "deltachat-tauri"; version = "3.0.0"; src = pkgs.fetchFromGitHub { owner = "deltachat"; repo = "deltachat-tauri"; tag = "v3.0.0"; hash = ""; }; };
  # a builder without pname: not a package set's attribute
  e = pkgs.tree-sitter.buildGrammar { language = "x"; version = "1.0"; src = pkgs.fetchFromGitHub { owner = "langchain-ai"; repo = "langchain"; tag = "v1.0"; hash = ""; }; };
  # an issue attachment isn't the repository
  f = pkgs.fetchzip { url = "https://github.com/remoteoss/dexter/files/123/x.zip"; hash = ""; };
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        [
            "5:17:W:35:[packaged_in_nixpkgs] `remoteoss/dexter` is in nixpkgs as `pkgs.dexter` (0.7.2, the same version)",
            // a hint listing what's there, not a wrong suggestion
            "12:78:I:35:[packaged_in_nixpkgs] nixpkgs builds packages from `langchain-ai/langchain`: `pkgs.python3Packages.langchain-core`",
        ]
    );
}
