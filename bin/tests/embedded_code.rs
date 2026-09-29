mod _utils;

use macros::generate_tests;

generate_tests! {
    rule: embedded_code,
    expressions: [
        "{ scripts.x.exec = ''\n  jq '.items[] | .name,' data.json\n''; }",
        "{ scripts.x.exec = ''\n  n=$(awk -F: '{print $1' /etc/passwd)\n''; }",
        "{ scripts.x.exec = ''\n  python3 -c 'import sys; print(undefined_name)'\n''; }",
        // allowed
        "{ scripts.x.exec = ''\n  jq -n '$a + $b' --arg a 1 --argjson b 2\n  jq \"$filter\" x\n  jq '.a.${attr}' x\n''; }",
    ],
}
