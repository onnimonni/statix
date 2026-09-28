# AGENTS.md: requirements for this fork of statix

These requirements come from the maintainer (onnimonni) and from design reviews done with Codex and Fable. **They must be followed** when changing this repository. If a change needs to break one, ask the maintainer first.

## Repository and branches

- Development happens on the `devenv` branch (the fork's default branch). Releases are tagged from it.
- Pull requests to upstream (molybdenumsoftware/statix) come from their own branches, never from `devenv`, so development can continue. They must not contain fork-only things: the release workflow (`flake-parts/release.nix`, `.github/workflows/release.yaml`), the fork's version number, the README section about installing from this fork / Cachix, or this file.
- CI workflows are generated from Nix (`flake-parts/*.nix` via the `files` module): edit the Nix and run `write-files`, never the YAML.
- `nix flake check` (formatting, generated files, clippy pedantic, tests, dogfood) must pass before pushing.

## Releases and binaries

- Every push to `devenv` builds `packages.statix` and `packages.statix-scripts` (statix wrapped with shellcheck and ruff) for x86_64-linux, aarch64-linux and aarch64-darwin, and pushes them to Cachix. The cache name comes from the repository variable `CACHIX_CACHE` (never hardcode it); the token from the `CACHIX_AUTH_TOKEN` secret.
- `v*` tags attach standalone binaries (static musl on Linux, native on macOS arm64/x86_64) and SHA256SUMS to a GitHub release.
- Keep `bin/Cargo.toml`, `packages/statix.nix` and `Cargo.lock` versions in sync with the tag. Bump the version without re-resolving unrelated dependencies.

## Linting scripts in and next to Nix code

- One `statix` binary runs all checks: statix's own lints, the devenv lints, and the script lints. It may run external tools (`shellcheck`, `ruff`, `git`); `STATIX_SHELLCHECK` / `STATIX_RUFF` override them, and missing tools disable their lints instead of failing.
- Scripts are found in: devenv `scripts`/`tasks`/`processes`/`enterShell`/`enterTest`; `writeShellScript(Bin)`, `writeShellApplication`, `writers.write*`, `writeScript` with a shebang, `runCommand*`; stdenv phases and hooks; NixOS `systemd.services.*` scripts and `system.activationScripts`; `let` bindings used as or interpolated into those; and script files Nix refers to (`./x.sh`, `readFile`, `"${./x.sh}"`, setup hooks, paths devenv scripts run).
- Shell goes to shellcheck, Python to ruff. Findings are reported at their position in the `.nix` file (for referenced files: at the reference, with the file's own line and column).
- Fix automatically whatever can be fixed. Every fix to a Nix string must be escaped for `''...''` / `"..."` strings, must never cut into a `${...}` interpolation, and must re-parse and render exactly the intended script, or it isn't offered. Referenced script files are fixed in place.
- What can't be fixed must come with instructions for coding agents (`-o agent`): location, surrounding source, a fix hint, the rule's docs link and the Nix escaping rules. Diagnostics carry `fixable` and `help` in JSON.
- Drop findings that only exist because of the `${...}` placeholder, parse-error pointers and environment-provided variables in hooks/fragments.

## Performance

- Processing must be heavily parallel: files in parallel (rayon), checkers batched (many scripts per process) and cached by script text, `fix` in lockstep rounds across files. Never lint or fix files one at a time.
- Results are cached between runs in `.statix-cache/` at the repository root (next to `.git`), with paths relative to that root, so copy-on-write clones for new worktrees start warm. The directory ignores itself for git. The cache is dropped when statix, the enabled lints, the config or the checker versions change; it's written atomically and merged with concurrent runs.
- Only the changed files are processed when asked: several targets, `--staged`, `--changed [REF]`; a changed script processes the `.nix` files referring to it.
- Keep it fast enough for a git pre-commit hook. Reference numbers on all of nixpkgs (12 cores): check 22 s cold / 6 s warm, one file 0.3 s, `--staged` 0.14 s, fix ~40 s. Don't regress them.
- Validate big changes on a nixpkgs checkout: results identical to before (e.g. parallel vs sequential), and every file `statix fix` changed still parses (`nix-instantiate --parse`, `bash -n`, Python `ast.parse`).

## Documentation

- The README documents installing from this fork in a devenv project (`devenv.yaml` inputs incl. `git-hooks`, `cachix.pull`), the git hook (`statix fix --staged` then `statix check --staged`), the script lints, `-o agent`, the cache and `--staged`/`--changed`. Keep it updated with every user-visible change, and test snippets exactly as written.

## `undeclared_command` lint

Reports commands shell scripts in Nix call but don't declare, and commands/paths that don't exist.

- Written in Rust. Learn from resholve (command classification, commands that run other commands, directives); don't depend on resholve (it needs EOL Python 2.7). Prefer a pure Rust shell parser (brush-parser); switching to tree-sitter-bash (better error recovery, recommended by Codex) needs the maintainer's agreement.
- Classify like resholve: alias, keyword, builtin (per dialect: bash, sh, dash), function (defined anywhere in the script), external. Keep lookup modes (`command`, `builtin`, `exec`, `\cmd`). Walk every simple command incl. functions, pipelines, subshells, `$(...)`, backticks, `<(...)`, and parse `bash -c`, `trap` and `eval` string literals (resholve doesn't). Start with a small, test-backed table of commands that run other commands (sudo, env, xargs, find -exec, timeout, nice, nohup, ...); unknown option forms mean "don't know", not a guess.
- `command -v X`, `type -p X`, `which X`, `hash X` guards mean X is optional: never report it as undeclared or missing.
- `[ -f ./x ]`/`[[ -x ./x ]]` guards make paths optional. After `cd`/`pushd`, relative paths aren't checked. `source`/`.` or a bare `${snippet}` command means functions are unknown: don't report undeclared commands.
- Only whole scripts are checked for undeclared commands: pieces of `+` concatenations, `concatStrings*` and multi-item lists are fragments (command positions and functions unknown). Strings in `mkIf` conditions or other non-value positions aren't scripts.
- Files setting NixOS-only options (`systemd`, `boot`, `security`, `networking`, `fileSystems`, `hardware`, `virtualisation`, also under `options.`) are NixOS modules: their scripts run on Linux only.
- Scripts without declarations (writeShellScript, stdenv phases) still get the absolute-path and interpolated-program checks.
- Check absolute and relative command paths too: absolute paths outside `/nix/store` are host dependencies (except `/bin/sh`, `/usr/bin/env`); relative paths in devenv scripts must exist relative to the project root.
- Verify interpolated commands exist: `${pkgs.X}/bin/Y`, `${pkgs.X}/sbin/Y`, `${lib.getExe' pkgs.X "Y"}`. Don't guess `lib.getExe` (`meta.mainProgram` can name any binary).
- Declared commands per context: `writeShellApplication` `runtimeInputs` (its PATH is prepended, `inheritPath` defaults to true: report undeclared dependencies, not certain failures); devenv `packages`, `scripts.<n>.packages`, `scripts.*` names, stdenv tools, tools devenv modules add (languages, git-hooks, services, process managers) where known; NixOS systemd `path` plus its default path (bin and sbin, unless `enableDefaultPath = false`). When declarations can't be known (unknown expressions, imports), treat them as unknown and say so; never report a false positive to be safe. Record where each declaration came from. `rm`, `mkdir`, `cat`, `cp`, `mv`, `grep`, `ln`, `chown`, `touch`, `which`, `locale`, `dirname`, `sudo`, `mktemp`, `chmod`, `sort`, `tail`, `head`, `cut`, `find`, `wc`, `sleep`, `tee`, `stat`, `basename`, `date`, `hostname`, `stat`, `install` are always available, except GNU-only use of the host's tool when darwin is checked: `stat` with options other than `-L` (GNU `-c` vs BSD `-f`), `install -D/-t/-T/-Z`/long options: too common to report. `sed` isn't (GNU vs BSD): host `sed -i`/`--in-place`/`-Ei` is reported when darwin is checked; `-i.bak` is portable; devenv shells have GNU sed.
- Package to program data comes from nix-index-database's per-system `-small` indexes (x86_64-linux, aarch64-linux, aarch64-darwin), converted at build time; keep attribute, output and program separate. Check against the Linux systems **and the system statix runs on** (maintainer's requirement). Because that makes results differ per machine (Fable's concern), `statix.toml` can set `systems = [...]`, and the checked systems are part of the cache key. Report "not available" only for attributes the index knows on some system; never report missing on x86_64-darwin (no index).
- Platform-specific dependencies:
  - Infer from Nix: `lib.optional(s) stdenv.isDarwin`, `stdenv.hostPlatform.is*`, `lib.mkIf`, `if … then … else`, `lib.optionalAttrs`, `pkgs.system ==`, `builtins.elem pkgs.system [...]`, `lib.meta.availableOn`, `meta.platforms` of `writeShellApplication`, and context platforms (NixOS systemd = Linux, nix-darwin launchd = darwin). Evaluate conditions per system as true/false/unknown; unknown means declared.
  - Directives use shellcheck's form and scoping: `# statix platforms=darwin`, `# statix provided=pbcopy,osascript`, `# statix disable=undeclared_command`, combinable, with an optional trailing `# reason`. A directive on its own line applies to the next command (simple or compound); before the first command it applies to the whole script. Trailing same-line comments are not directives. Leading whitespace is allowed. The same grammar works in Nix comments before list elements.
  - Platform values: `lib.platforms` names (`darwin`, `linux`, `unix`, `all`, `aarch64`, `x86_64`) and full system strings.
  - Effective systems of a command = script platforms ∩ command platforms; it's declared if every effective system has a declaration, and existence is only checked on effective systems.
  - Unknown directive keys or values are warnings.
  - OS-provided commands (darwin: pbcopy, pbpaste, osascript, open, defaults, security, launchctl, sw_vers, xcrun, plutil, hdiutil, diskutil, codesign, ditto; linux: systemctl, loginctl) are provided automatically on those systems.
  - Messages are paste-ready: the exact `lib.optionals` wrapper or directive to add.
- Future idea, not v1: infer platforms from the scripts themselves (`uname`, `$OSTYPE`, `case $(uname)`), possibly with tree-sitter-bash ([#1](https://github.com/onnimonni/statix/issues/1)).
- Tests: resholve-derived snippets with expected commands, per-context declarations, interpolation and path cases, directives, Unicode offsets, malformed scripts, shared fragments.

## Working on this repository

- Ask Codex and Fable for design reviews of bigger features, and have Codex review implementations. Record decisions here.
- Keep tests hermetic: they set `HOME`/`XDG_CONFIG_HOME`, and use a temporary `STATIX_CACHE_DIR` or project directory, so user configuration and caches don't leak in.
