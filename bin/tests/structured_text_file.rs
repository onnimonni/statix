mod _utils;

use macros::generate_tests;

generate_tests! {
    rule: structured_text_file,
    expressions: [
        "pkgs.writeText \"config.json\" ''\n  { \"port\": ${toString port} }\n''",
        "{ environment.etc.\"app/config.toml\".text = ''\n  name = \"${name}\"\n  port = 80\n''; }",
        "{ files.\"settings.yaml\".text = ''\n  a: 1\n  b: 2\n''; }",
        "pkgs.writeTextFile { name = \"x.ini\"; text = ''\n  [main]\n  key = ${value}\n''; }",
        "{ scripts.setup.exec = ''\n  cat > config.json <<'EOF'\n  { \"port\": ${toString port},\n    \"host\": \"localhost\" }\n  EOF\n''; }",
        "pkgs.runCommand \"x\" { } ''\n  mkdir -p $out\n  cat > $out/settings.toml <<EOF\n  [main]\n  path = \"${pkgs.hello}\"\n  EOF\n''",
        // allowed
        "{ systemd.services.x.script = ''\n  cat > /var/lib/x/local.yaml <<EOF\n  ${lib.optionalString (f != null) ''\n    secrets:\n      x: '$(cat ${f})'\n  ''}\n  listen: 80\n  EOF\n''; }",
        "pkgs.runCommand \"x\" { } ''\n  echo '${builtins.toJSON versions}' > $out/installed.json\n''",
        "pkgs.runCommand \"x\" { } ''\n  cat > $out/settings.toml <<EOF\n  [main]\n  home = \"$HOME\"\n  EOF\n''",
        "pkgs.runCommand \"x\" { } ''\n  cat >> pyproject.toml <<EOF\n  [tool.x]\n  a = 1\n  EOF\n''",
        "pkgs.writeText \"config.json\" (builtins.toJSON { port = 80; })",
        "{ environment.etc.\"consul.d/dummy.json\".text = \"{ }\"; }",
        "pkgs.writeText \"notes.txt\" ''\n  a\n  b\n''",
        "\"--cross-file=${writeText \"cross.ini\" ''\n  [binaries]\n  gpg = '${gnupg}/bin/gpg'\n''}\"",
        "pkgs.writeText \"docs.yaml\" ''\n  ---\n  ${lib.concatMapStringsSep \"\\n---\\n\" builtins.toJSON docs}\n''",
    ],
}
