use super::*;

fn first_string(src: &str) -> ast::Str {
    rnix::Root::parse(src)
        .tree()
        .syntax()
        .descendants()
        .filter_map(ast::Str::cast)
        .find(|s| s.syntax().text().to_string().contains("RUN"))
        .unwrap()
}

fn attrs(d: &Declared) -> Vec<&str> {
    d.packages.iter().map(|p| p.attr.as_str()).collect()
}

#[test]
fn shell_application_inputs_and_platforms() {
    let s = first_string(
        "pkgs.writeShellApplication { name = \"x\"; runtimeInputs = [ pkgs.jq curl (lib.getBin pkgs.gnused) ] ++ lib.optionals pkgs.stdenv.isLinux [ pkgs.strace ]; meta.platforms = lib.platforms.unix; text = ''RUN''; }",
    );
    let d = declared(&s, Lang::Shell("bash")).unwrap();
    assert_eq!(attrs(&d), ["jq", "curl", "gnused", "strace"]);
    assert!(!d.incomplete);
    let none = |_: &str, _: &str| None;
    assert!(!d.packages[3].cond.allows("aarch64-darwin", &none));
    assert!(d.packages[3].cond.allows("x86_64-linux", &none));
    assert_eq!(d.place, "runtimeInputs");
}

#[test]
fn devenv_packages() {
    let s = first_string(
        "{ pkgs, lib, ... }: { packages = [ pkgs.git ] ++ lib.optionals pkgs.stdenv.isDarwin [ pkgs.darwin.trash ]; languages.rust.enable = true; scripts.a.exec = ''RUN''; scripts.b.exec = \"x\"; scripts.a.packages = [ pkgs.jq ]; }",
    );
    let d = declared(&s, Lang::Shell("bash")).unwrap();
    let a = attrs(&d);
    assert!(
        a.contains(&"git")
            && a.contains(&"darwin.trash")
            && a.contains(&"cargo")
            && a.contains(&"jq")
            && a.contains(&"coreutils"),
        "{a:?}"
    );
    assert!(d.commands.contains("b"));
    assert!(!d.incomplete);
}

#[test]
fn devenv_scripts_and_language_tools() {
    let s = first_string(
        "{ languages.javascript = { enable = true; pnpm.enable = true; }; scripts.frontend-check.exec = ''\n  cd \"$DEVENV_ROOT\" && exec pnpm check \"$@\"\n''; scripts.ci.exec = ''RUN frontend-check''; }",
    );
    let d = declared(&s, Lang::Shell("bash")).unwrap();
    assert!(
        attrs(&d).contains(&"pnpm") && attrs(&d).contains(&"nodejs"),
        "{:?}",
        attrs(&d)
    );
    assert!(d.commands.contains("frontend-check"));
}

#[test]
fn unknown_entries_make_it_incomplete() {
    let s = first_string("{ packages = myPackages; enterShell = ''RUN''; }");
    assert!(declared(&s, Lang::Shell("bash")).unwrap().incomplete);
}

#[test]
fn systemd_path() {
    let s = first_string("{ systemd.services.web = { path = [ pkgs.curl ]; script = ''RUN''; }; }");
    let d = declared(&s, Lang::Shell("bash")).unwrap();
    assert!(attrs(&d).contains(&"curl") && attrs(&d).contains(&"coreutils"));
    assert_eq!(d.platforms, Some(Cond::Platform("linux".into())));
}

#[test]
fn let_scoping() {
    let d = |src: &str| declared(&first_string(src), Lang::Shell("bash")).unwrap();
    // pkg = cfg.package: unknown, not pkgs.pkg
    let fdb = d(
        "{ config, pkgs, ... }: let cfg = config.services.x; pkg = cfg.package; in { systemd.services.x = { path = [ pkg pkgs.coreutils ]; script = ''RUN''; }; }",
    );
    assert!(fdb.incomplete);
    assert!(!attrs(&fdb).contains(&"pkg"));
    // let jq = pkgs.jq; with pkgs; callPackage arguments
    let known = d(
        "{ pkgs, curl, ... }: let j = pkgs.jq; in { systemd.services.x = { path = [ j curl ] ++ (with pkgs; [ ripgrep ]); script = ''RUN''; }; }",
    );
    assert!(!known.incomplete);
    assert!(
        ["jq", "curl", "ripgrep"]
            .iter()
            .all(|a| attrs(&known).contains(a))
    );
    // lexical bindings win over `with pkgs;`
    let shadowed = d(
        "{ pkgs, ... }: let git = pkgs.gitMinimal; in { systemd.services.x = { path = with pkgs; [ git ]; script = ''RUN''; }; }",
    );
    assert!(attrs(&shadowed).contains(&"gitMinimal"));
    // a function argument isn't a package
    let arg =
        d("{ pkgs, ... }: { systemd.services.x = { path = map (p: p) [ ]; script = ''RUN''; }; }");
    assert!(arg.incomplete);
}

#[test]
fn scripts_added_to_other_modules_services() {
    let d = declared(
        &first_string("{ systemd.services.postgresql-setup.postStart = ''RUN''; }"),
        Lang::Shell("bash"),
    )
    .unwrap();
    assert!(d.incomplete);
}

#[test]
fn conditions() {
    let parse = |src: &str| {
        let e = rnix::Root::parse(src).tree().expr().unwrap();
        cond_of(&e)
    };
    let none = |_: &str, _: &str| None;
    let c = parse("pkgs.stdenv.hostPlatform.isDarwin && !stdenv.isAarch64");
    assert_eq!(c.eval("x86_64-darwin", &none), Some(true));
    assert_eq!(c.eval("aarch64-darwin", &none), Some(false));
    let c = parse("builtins.elem pkgs.system [ \"x86_64-linux\" ]");
    assert_eq!(c.eval("aarch64-linux", &none), Some(false));
    assert_eq!(parse("config.foo").eval("x86_64-linux", &none), None);
}
