mod _utils;

use macros::generate_tests;

generate_tests! {
    rule: ruff,
    expressions: [
        "pkgs.writers.writePython3Bin \"hello\" { } ''\n  import os\n  print(\"hello\")\n''",
        "{ scripts.py = { package = pkgs.python312; exec = ''\n  import sys\n  print(${toString port})\n''; }; }",
        "pkgs.writeScript \"x\" ''\n  #!${pkgs.python3}/bin/python3\n  x == 1\n''",
        // NixOS test: driver globals and machines are defined
        "{ nodes.web-server = { }; testScript = ''\n  import os\n  start_all()\n  web_server.wait_for_unit(\"nginx\")\n  with subtest(\"x\"):\n    machine.succeed(\"true\")\n  undefined_helper()\n''; }",
        // codex review: two tests in one file, a renamed let binding
        "{ a = { nodes.server = {}; testScript = ''machine.start()''; }; b = { nodes.client = {}; testScript = ''machine.start()''; }; }",
        "let myTest = ''\n  start_all()\n  machine.succeed(\"true\")\n''; in { nodes.server = {}; testScript = myTest; }",
        // fable review: machine names like the driver's
        "{ nodes.\"2nd-host\" = {}; nodes.\"web.example\" = {}; testScript = ''\n  _nd_host.succeed(\"true\")\n  web_example.succeed(\"true\")\n  machine.succeed(\"true\")\n''; }",
                // not a NixOS test's script
        "{ system.build.testScript = ''\n  set -euxo pipefail\n  curl -sSf http://127.0.0.1:8000/\n''; }",
        // clean
        "pkgs.writers.writePython3 \"ok\" { } ''\n  print(\"ok\")\n''",
    ],
}
