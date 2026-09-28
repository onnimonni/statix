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
        // allowed
        "stdenv.mkDerivation { installPhase = ''\n  cp -r dist $out\n  chmod 755 $out\n''; }",
        "pkgs.writeShellScript \"x\" ''\n  mkdir -p d\n  cp x d/\n''",
    ],
}
