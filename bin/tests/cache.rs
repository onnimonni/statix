use std::{fs, path::Path, process::Command};

/// statix with its cache in `cache`, and no user-level checker config.
fn statix(dir: &Path, cache: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_statix"))
        .env("HOME", cache)
        .env("XDG_CONFIG_HOME", cache)
        .env("STATIX_CACHE_DIR", cache)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8(strip_ansi_escapes::strip(output.stdout)).unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success());
}

fn project() -> (tempfile::TempDir, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("scripts")).unwrap();
    fs::write(root.join("scripts/ok.sh"), "echo \"ok\"\n").unwrap();
    fs::write(
        root.join("clean.nix"),
        "{ scripts.ok.exec = ./scripts/ok.sh; }\n",
    )
    .unwrap();
    fs::write(root.join("dirty.nix"), "{ enterShell = \"cd $HOME\"; }\n").unwrap();
    (dir, tempfile::tempdir().unwrap())
}

#[test]
fn warm_run_gives_the_same_findings() {
    let (dir, cache) = project();
    let cold = statix(dir.path(), cache.path(), &["check", "-o", "errfmt"]);
    let warm = statix(dir.path(), cache.path(), &["check", "-o", "errfmt"]);
    assert!(cold.contains("dirty.nix"), "{cold}");
    assert_eq!(cold, warm);
    assert!(
        fs::read_dir(cache.path())
            .unwrap()
            .any(|e| { e.unwrap().path().extension().is_some_and(|x| x == "json") })
    );
}

#[test]
fn clean_file_is_rechecked_when_its_script_changes() {
    let (dir, cache) = project();
    let root = dir.path();
    let first = statix(root, cache.path(), &["check", "-o", "errfmt"]);
    assert!(!first.contains("clean.nix"), "{first}");
    fs::write(root.join("scripts/ok.sh"), "cd $HOME\n").unwrap();
    let second = statix(root, cache.path(), &["check", "-o", "errfmt"]);
    assert!(
        second.contains("clean.nix>1:21:W:30:[script_file] ./scripts/ok.sh:1:1: SC2164"),
        "{second}"
    );
}

#[test]
fn changed_file_is_rechecked() {
    let (dir, cache) = project();
    let root = dir.path();
    statix(root, cache.path(), &["check", "-o", "errfmt"]);
    fs::write(root.join("clean.nix"), "{ enterShell = \"echo $1\"; }\n").unwrap();
    let out = statix(root, cache.path(), &["check", "-o", "errfmt"]);
    assert!(
        out.contains("clean.nix>1:22:I:28:[shellcheck] SC2086"),
        "{out}"
    );
}

#[test]
fn changed_and_staged_select_files_with_git() {
    let (dir, cache) = project();
    let root = dir.path();
    git(root, &["init", "-q"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "init"]);

    // nothing changed: nothing to check
    assert_eq!(
        statix(root, cache.path(), &["check", "-o", "errfmt", "--changed"]),
        ""
    );

    fs::write(root.join("new.nix"), "{ enterShell = \"cd $HOME\"; }\n").unwrap();
    fs::write(root.join("scripts/ok.sh"), "cd $HOME\n").unwrap();
    let changed = statix(root, cache.path(), &["check", "-o", "errfmt", "--changed"]);
    assert!(changed.contains("new.nix"), "{changed}");
    assert!(changed.contains("clean.nix"), "{changed}"); // refers to the changed script
    assert!(!changed.contains("dirty.nix"), "{changed}");

    git(root, &["add", "new.nix"]);
    let staged = statix(root, cache.path(), &["check", "-o", "errfmt", "--staged"]);
    assert!(staged.contains("new.nix"), "{staged}");
    assert!(!staged.contains("clean.nix"), "{staged}");
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn cache_lives_in_the_repository_and_survives_a_copy() {
    let (project_dir, home) = project();
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join("repo");
    copy_dir(project_dir.path(), &root);
    git(&root, &["init", "-q"]);

    // counts shellcheck runs
    let log = home.path().join("shellcheck.log");
    let wrapper = home.path().join("shellcheck-wrapper");
    fs::write(
        &wrapper,
        format!(
            // `--version` is how statix checks whether cached results are still valid
            "#!/bin/sh\n[ \"$1\" = --version ] || echo run >> '{}'\nexec shellcheck \"$@\"\n",
            log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(
        &wrapper,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let run = |dir: &Path| {
        let output = Command::new(env!("CARGO_BIN_EXE_statix"))
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path())
            .env_remove("STATIX_CACHE_DIR")
            .env("STATIX_SHELLCHECK", &wrapper)
            .args(["check", "-o", "errfmt"])
            .current_dir(dir)
            .output()
            .unwrap();
        String::from_utf8(strip_ansi_escapes::strip(output.stdout)).unwrap()
    };
    let runs = || fs::read_to_string(&log).map_or(0, |l| l.lines().count());

    let first = run(&root);
    assert!(first.contains("dirty.nix"), "{first}");
    assert!(runs() > 0);
    assert_eq!(
        fs::read_to_string(root.join(".statix-cache/.gitignore")).unwrap(),
        "# statix cache\n*\n"
    );

    // a copy (like a new worktree) starts warm: no shellcheck runs at all
    let copy = work.path().join("copy");
    copy_dir(&root, &copy);
    let before = runs();
    let second = run(&copy);
    assert_eq!(first, second);
    assert_eq!(runs(), before, "shellcheck ran in the copy");
}
