use super::*;
use crate::scripts::commands::commands;

fn suggest(script: &str, gnu: bool) -> Vec<(String, bool)> {
    idioms_in(
        script,
        &commands(script, "bash"),
        Context {
            gnu,
            ..Context::default()
        },
    )
    .into_iter()
    .map(|i| {
        let text = i.render(|s, e| script.get(s..e).map(String::from));
        (text.unwrap(), i.exact)
    })
    .collect()
}

#[test]
fn pipelines_and_cd() {
    let s = |x: &str| x.to_string();
    assert_eq!(
        suggest("cat data.json | jq -r .name > out\n", true),
        [(s("jq -r .name > out < data.json"), true)]
    );
    assert!(suggest("cat a b | sort\n", true).is_empty());
    assert!(suggest("cat x | sort < y\n", true).is_empty());
    assert_eq!(
        suggest("echo \"$v\" | tee -a log > /dev/null\n", true),
        [(s("echo \"$v\" >> log"), true)]
    );
    assert!(suggest("echo x | tee log\n", true).is_empty());
    assert!(suggest("echo x | sudo tee /etc/x > /dev/null\n", true).is_empty());
    assert_eq!(
        suggest("cd build\nmake\nmake install\ncd ..\n", true),
        [(s("(cd build && make && make install)"), false)]
    );
    assert_eq!(
        suggest("cd a/b; make; cd ../..", true),
        [(s("(cd a/b && make)"), false)]
    );
    assert!(suggest("cd a/b; make; cd ..", true).is_empty());
    assert_eq!(suggest("cd /tmp; make; cd -; cd x; cd ..", true).len(), 1);
}

fn suggest_build(script: &str) -> Vec<(String, bool)> {
    idioms_in(
        script,
        &commands(script, "bash"),
        Context {
            gnu: true,
            build: true,
            bash: true,
            phase: true,
        },
    )
    .into_iter()
    .map(|i| {
        let text = i.render(|s, e| script.get(s..e).map(String::from));
        (text.unwrap(), i.exact)
    })
    .collect()
}

#[test]
fn more_idioms() {
    let s = |x: &str| x.to_string();
    assert_eq!(suggest("egrep -v x f", true), [(s("grep -E"), true)]);
    assert_eq!(
        suggest("if grep -w foo f > /dev/null; then :; fi", true),
        [(s("grep -q -w foo f"), false)]
    );
    assert!(suggest("grep foo f > /dev/null 2>&1", true).is_empty());
    assert_eq!(
        suggest("n=$(grep x f | wc -l)", true),
        [(s("grep -c x f"), false)]
    );
    assert_eq!(
        suggest("sort a | uniq > b", true),
        [(s("sort -u a"), false)]
    );
    assert_eq!(
        suggest(r"find . -name '*.o' -exec rm {} \;", true),
        [(s("-delete"), false)]
    );
    assert_eq!(
        suggest("[ -e out.log ] && rm out.log", true),
        [(s("rm -f out.log"), false)]
    );
    assert_eq!(
        suggest("[ ! -d build ] && mkdir build", true),
        [(s("mkdir -p build"), false)]
    );
    assert!(suggest("[ -e a ]; rm a", true).is_empty());
    assert_eq!(
        suggest("for f in $(ls patches); do echo $f; done", true),
        [(s("patches/*"), false)]
    );
    assert_eq!(suggest("x=$(which jq)", true), [(s("command -v"), false)]);
    assert_eq!(
        suggest("echo \"$(date +%s)\"", true),
        [(s("date +%s"), false)]
    );
    // build phases only
    assert!(suggest("sed -i 's/foo/bar/g' Makefile", true).is_empty());
    assert_eq!(
        suggest_build("sed -i 's/foo/bar/g' Makefile"),
        [(
            s("substituteInPlace Makefile --replace-fail 'foo' 'bar'"),
            false
        )]
    );
    assert!(suggest_build("sed -i 's/fo.o/bar/' Makefile").is_empty());
    assert_eq!(
        suggest_build("cp doc/tool.1 $out/share/man/man1/"),
        [(s("installManPage doc/tool.1"), false)]
    );
    assert_eq!(
        suggest_build("install -Dm644 comp.bash $out/share/bash-completion/completions/tool"),
        []
    );
    assert_eq!(
        suggest_build("install -Dm644 tool.fish $out/share/fish/vendor_completions.d/tool.fish"),
        [(s("installShellCompletion --fish tool.fish"), false)]
    );
}

#[test]
fn install() {
    let s = |x: &str| x.to_string();
    assert_eq!(
        suggest(
            "mkdir -p $out/bin\ncp foo $out/bin/foo\nchmod 755 $out/bin/foo\n",
            true
        ),
        // outside a build `$out/bin/foo` could be an existing directory
        [(s("install -Dm755 foo $out/bin/foo"), false)]
    );
    assert_eq!(
        suggest_build("mkdir -p $out/bin\ncp foo $out/bin/foo\nchmod 755 $out/bin/foo\n"),
        [(s("install -Dm755 foo $out/bin/foo"), true)]
    );
    assert_eq!(
        suggest(
            "mkdir -p \"$out/share\" && cp a.txt b.txt \"$out/share\"",
            true
        ),
        [(s("install -D -t \"$out/share\" a.txt b.txt"), false)]
    );
    // one file into a directory: named, no -t
    assert_eq!(
        suggest(
            "mkdir -p $out/bin\ncp ./make $out/bin\nchmod 555 $out/bin/make\n",
            true
        ),
        [(s("install -Dm555 ./make $out/bin/make"), true)]
    );
    assert_eq!(
        suggest(
            "cp app /usr/local/bin/app; chmod 755 /usr/local/bin/app; chown root:wheel /usr/local/bin/app",
            false
        ),
        [(
            s("install -m 755 -o root -g wheel app /usr/local/bin/app"),
            false
        )]
    );
    // BSD install has no -D: mkdir stays
    assert_eq!(
        suggest("mkdir -p d\ncp x d/x\nchmod 644 d/x\n", false),
        [(s("mkdir -p d && install -m 644 x d/x"), false)]
    );
    // a comment in between isn't lost by a fix
    assert_eq!(
        suggest("cp x y\n# make it executable\nchmod 755 y\n", true),
        [(s("install -m 755 x y"), false)]
    );
    // `chmod +x` adds to the mode: a hint for 755
    assert_eq!(
        suggest("cp x y\nchmod +x y\n", true),
        [(s("install -m 755 x y"), false)]
    );
    assert!(suggest("cp x y\nchmod g+w y\n", true).is_empty());
    // unrelated or not worth it
    assert!(suggest("mkdir -p a\ncp x b/x\n", true).is_empty());
    assert!(suggest("cp -r x y\nchmod 755 y\n", true).is_empty());
    assert!(suggest("mkdir -p d\ncp x d/\n", false).is_empty());
    assert!(suggest("cp x y\nchmod 755 z\n", true).is_empty());
    assert!(suggest("mkdir -p d\ncp x d\nchmod 700 d\n", false).is_empty());
    assert!(suggest("cp levels/* out\nchmod 644 out/*\n", true).is_empty());
}
