mod _utils;

use macros::generate_tests;

generate_tests! {
    rule: shell_idiom,
    expressions: [
        // exact: statix fix rewrites it
        "stdenv.mkDerivation { installPhase = ''\n  mkdir -p $out/bin\n  cp tool $out/bin/tool\n  chmod 755 $out/bin/tool\n''; }",
        "{ systemd.services.x.preStart = ''\n  cp ${cfg.package}/bin/helper /run/x/helper\n  chgrp x /run/x/helper\n  chmod 750 /run/x/helper\n''; }",
        // a hint: install sets mode 755 where cp keeps the source's
        "stdenv.mkDerivation { installPhase = ''\n  mkdir -p $out/share/doc\n  cp README.md NEWS $out/share/doc\n''; }",
        // codex review: hints only, nothing rewritten
        "stdenv.mkDerivation { installPhase = ''\n  runHook \"preInstall\"\n  cp ${src} dst; chown ${owner} dst; chmod 755 dst\n  cp src existingDir; chmod 700 existingDir\n  cat Cargo.toml | read value\n  rm -f dst && ln -s src dst\n  runHook \"postInstall\"\n''; }",
        // codex review: not reported
        "{ enterShell = ''\n  cp \"x; echo INJECTED\" d/; chmod 755 \"d/x; echo INJECTED\"\n  cat input | > output sort\n  v=$(cat Cargo.*)\n  mkdir -p foo; install -Dm644 src foobar/file\n  mkdir -p a; mkdir -p */b\n  mkdir -p blocked/child && mkdir -p later\n  grep() { echo custom; }; egrep workspace Cargo.toml\n  sh -c 'v=$(cat file)'\n''; }",
        // allowed
        "stdenv.mkDerivation { installPhase = ''\n  cp -r dist $out\n  chmod 755 $out\n''; }",
        "pkgs.writeShellScript \"x\" ''\n  mkdir -p d\n  cp x d/\n''",
    ],
}
