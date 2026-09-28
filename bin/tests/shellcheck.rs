mod _utils;

use macros::generate_tests;

generate_tests! {
    rule: shellcheck,
    expressions: [
        "{ scripts.hello.exec = ''\n  echo $1\n''; }",
        r#"{ processes.web.exec = "cd $HOME && serve"; }"#,
        "{ enterShell = ''\n  echo ''${GREETING}\n''; }",
        r#"{ tasks."app:run".exec = "${pkgs.hello}/bin/hello $ARGS"; }"#,
        "{ packages = [ (pkgs.writeShellScriptBin \"list\" ''\n  for f in $(ls *.txt); do echo \"$f\"; done\n'') ]; }",
        "pkgs.writeShellApplication { name = \"x\"; text = ''\n  rm -rf $DIR/*\n''; }",
        // not shell
        "{ scripts.py = { package = pkgs.python3; exec = ''\n  print($1)\n''; }; }",
        "{ description = \"echo $1\"; }",
        // clean
        "{ scripts.ok.exec = ''\n  echo \"$1\"\n''; }",
    ],
}
