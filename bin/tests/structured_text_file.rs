mod _utils;

use macros::generate_tests;

generate_tests! {
    rule: structured_text_file,
    expressions: [
        "pkgs.writeText \"config.json\" ''\n  { \"port\": ${toString port} }\n''",
        "{ environment.etc.\"app/config.toml\".text = ''\n  name = \"${name}\"\n  port = 80\n''; }",
        "{ files.\"settings.yaml\".text = ''\n  a: 1\n  b: 2\n''; }",
        "pkgs.writeTextFile { name = \"x.ini\"; text = ''\n  [main]\n  key = ${value}\n''; }",
        // allowed
        "pkgs.writeText \"config.json\" (builtins.toJSON { port = 80; })",
        "{ environment.etc.\"consul.d/dummy.json\".text = \"{ }\"; }",
        "pkgs.writeText \"notes.txt\" ''\n  a\n  b\n''",
        "\"--cross-file=${writeText \"cross.ini\" ''\n  [binaries]\n  gpg = '${gnupg}/bin/gpg'\n''}\"",
        "pkgs.writeText \"docs.yaml\" ''\n  ---\n  ${lib.concatMapStringsSep \"\\n---\\n\" builtins.toJSON docs}\n''",
    ],
}
