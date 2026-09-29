# statix

> Lints and suggestions for the Nix programming language.

Reluctantly forked from [oppiliappan/statix](https://github.com/oppiliappan/statix)
because I wanted to maintain and could not obtain some repository permission bits.
I still hope to unfork.

`statix check` highlights antipatterns in Nix code. `statix
fix` can fix several such occurrences.

For the time-being, `statix` works only with ASTs
produced by the `rnix-parser` crate and does not evaluate
any nix code (imports, attr sets etc.).

## Examples

```shell
$ statix check tests/c.nix
[manual_inherit_from] Warning: Assignment instead of inherit from
   ╭─[tests/c.nix:2:3]
   │
 2 │   mtl = pkgs.haskellPackages.mtl;
   ·   ───────────────┬───────────────
   ·                  ╰───────────────── This assignment is better written with inherit
───╯

$ statix fix --dry-run tests/c.nix
--- tests/c.nix
+++ tests/c.nix [fixed]
@@ -1,6 +1,6 @@
 let
-  mtl = pkgs.haskellPackages.mtl;
+  inherit (pkgs.haskellPackages) mtl;
 in
 null
```

## Installation

`statix` is available via a nix flake:

```shell
# build from source
nix build git+https://git.peppe.rs/languages/statix
./result/bin/statix --help

# statix also provides a flake app
nix run git+https://git.peppe.rs/languages/statix -- --help

# save time on builds using cachix
cachix use statix
```

Install from nixpkgs:

```shell
nix run nixpkgs#statix -- help
```

Install with [brew/linuxbrew](https://brew.sh)

```bash
brew install statix
```

## No releases

Even though some releases were made,
no more are intended.
Instead, this project is meant to be used from HEAD.

## Usage

Basic usage is as simple as:

```shell
# recursively finds nix files and raises lints
statix check /path/to/dir

# ignore generated files, such as Cargo.nix
statix check /path/to/dir -i Cargo.nix

# ignore more than one file
statix check /path/to/dir -i a.nix b.nix c.nix

# ignore an entire directory
statix check /path/to/dir -i .direnv

# statix respects your .gitignore if it exists
# run statix in "unrestricted" mode, to disable that
statix check /path/to/dir -u

# see `statix -h` for a full list of options
```

Certain lints have suggestions. Apply suggestions back to
the source with:

```shell
statix fix /path/to/file

# show diff, do not write to file
statix fix --dry-run /path/to/file
```

`statix` supports a variety of output formats; standard,
json and errfmt:

```shell
statix check /path/to/dir -o json
statix check /path/to/dir -o errfmt # singleline, easy to integrate with vim
statix check /path/to/dir -o agent  # markdown with context and fix instructions, for coding agents
```

### Only what changed

```shell
statix check a.nix b.nix scripts/x.sh  # several targets; a script checks the .nix files referring to it
statix check --staged                  # staged files (git)
statix check --changed                 # changed since HEAD, incl. unstaged and untracked (git)
statix check --changed origin/main     # changed since a branch point
```

`statix fix` takes the same options.

Results are cached between runs in `.statix-cache/` at the repository root
(next to `.git`; `$STATIX_CACHE_DIR` overrides). It ignores itself for git
and stores paths relative to the root, so a copy of the repository, e.g. a
copy-on-write clone for a new worktree, starts with a warm cache. A file
that had no findings is skipped until it or a script it refers to changes,
and checker results are reused per script. The cache is dropped when
statix, the enabled lints, `statix.toml` or the `shellcheck`/`ruff`
versions change. Turn it off with `--no-cache` or `STATIX_NO_CACHE=1`.

On all of nixpkgs (44k files, 12 cores): 22 s the first time, 6 s again
(files with findings are re-checked), 0.3 s for one changed file, 0.14 s
for `--staged`. `--changed` also lists untracked files, which costs git
about a second on a repository that size.

### Scripts in and next to Nix code

Lints check scripts with [ShellCheck](https://www.shellcheck.net),
[ruff](https://docs.astral.sh/ruff), jq and gawk (install them, or point
`STATIX_SHELLCHECK` / `STATIX_RUFF` at them; without them the lints do
nothing):

- `shellcheck`: shell scripts written as Nix strings: devenv `scripts`,
  `tasks`, `processes`, `enterShell`, `enterTest`; nixpkgs
  `writeShellScript`, `writeShellApplication`, `writers.writeBash` and
  friends, `runCommand`, stdenv phases and hooks (`buildPhase`,
  `postInstall`, `shellHook`...); NixOS `systemd.services.*.script`,
  `preStart`... and `system.activationScripts`; and `let` bindings used
  as or interpolated into those
- `ruff`: Python scripts written as Nix strings: `writers.writePython3`,
  `writeScript` with a Python shebang, devenv scripts with a Python
  `package`
- `script_file`: `.sh`, `.bash` and `.py` files (or files with such a
  shebang) that Nix code refers to: `./deploy.sh`, `builtins.readFile
./x.sh`, `"${./x.sh}"`, `updateScript = ./update.sh`, setup hooks, and
  files devenv scripts run by relative path (`python scripts/x.py`)

Findings are reported at their position in the `.nix` file (for
`script_file`, at the reference, with the script's own line and column).

`statix fix` applies the checkers' fixes: ShellCheck's own plus built-in
ones for SC2045, SC2115, SC2155 and SC2162, and ruff's safe fixes. In Nix
strings they are escaped for `''...''` and `"..."` strings and leave `${...}`
interpolations alone; every fix is re-parsed and checked to render exactly
the intended script before it's applied. Referenced script files are fixed
in place (`--dry-run` shows the diff).

Everything else needs a decision. `-o agent` lists it with the
surrounding source, a hint for common codes, a link to the rule's docs
and the Nix escaping rules, so a coding agent can finish the job:

```shell
statix fix . && statix check -o agent .
```

In `-o json` every diagnostic has `fixable` (whether `statix fix`
handles it), `help` when it doesn't, and `external` (file, line, column)
for findings in referenced scripts.

### As a git hook in a devenv project

`devenv.yaml` (devenv's git hooks need the `git-hooks` input too):

```yaml
inputs:
  git-hooks:
    url: github:cachix/git-hooks.nix
    inputs:
      nixpkgs:
        follows: nixpkgs
  statix:
    url: github:onnimonni/statix # or pin a release tag: github:onnimonni/statix/<tag>
```

or add them with:

```shell
devenv inputs add git-hooks github:cachix/git-hooks.nix --follows nixpkgs
devenv inputs add statix github:onnimonni/statix
```

`devenv.nix`:

```nix
{ pkgs, inputs, ... }:
let
  # statix with shellcheck and ruff for the script lints
  statix = inputs.statix.packages.${pkgs.stdenv.system}.statix-scripts;
in
{
  # prebuilt binaries, pushed by CI
  cachix.pull = [ "onnimonni" ];

  git-hooks.hooks.statix = {
    enable = true;
    package = statix;
    # Fix what can be fixed in the staged files, then fail on anything left
    # or changed, so the fixes get reviewed and staged. A staged script
    # (.sh/.py) is checked through the .nix files that refer to it.
    entry = toString (
      pkgs.writeShellScript "statix-hook" ''
        ${statix}/bin/statix fix --staged
        ${statix}/bin/statix check --staged
      ''
    );
    # run when one of these is staged
    files = "\\.(nix|sh|bash|py)$";
  };
}
```

`devenv shell` installs the hook. On `git commit` it runs on the staged
files only (`--staged`), so its cost grows with the change, not the
repository. When it
fixes something the commit stops: review the changes, `git add` them and
commit again.

Run it on the whole repository with `prek run statix --all-files` (or
`pre-commit run statix --all-files`), or directly with `statix fix . &&
statix check .`. Give a coding agent what's left with
`statix check -o agent .`.

Binaries without Nix (statix only; install `shellcheck` and `ruff`
separately for the script lints) are attached to
[releases](https://github.com/onnimonni/statix/releases).

### Undeclared commands

`undeclared_command` parses shell scripts (with
[brush-parser](https://github.com/reubeno/brush), following
[resholve](https://github.com/abathur/resholve)'s rules for builtins,
functions, aliases and commands that run other commands like `sudo`,
`xargs`, `find -exec`) and reports commands that won't be found:

- commands not declared where the script runs: devenv `packages`,
  `scripts.<name>.packages`, enabled `languages`/`services`/modules and
  other `scripts` (a devenv script's name is a command in the others);
  `writeShellApplication` `runtimeInputs`; NixOS systemd `path` and its
  default path. Common host tools (`rm`, `mkdir`, `cat`, `cp`, `mv`, `grep`, `ln`, `chown`,
  `touch`, `which`, `locale`, `dirname`, `sudo`, `mktemp`, `chmod`, `sort`,
  `tail`, `head`, `cut`, `find`, `wc`, `sleep`, `tee`, `stat`, `basename`,
  `date`, `hostname`, `stat`, `install`) are always available, except GNU-only
  options of the host's tool when macOS is checked: `stat -c`/`-f`...,
  `install -D`/`-t`/`-T`/`-Z`/`--long`. `sed -i` with the host's sed is reported when macOS is
  checked (BSD sed needs `-i ''`, which GNU sed rejects; `-i.bak` works on
  both). When declarations can't be
  known (`imports`, computed lists) nothing is reported.
- packages not available on a checked system: x86_64-linux,
  aarch64-linux and the system statix runs on, from
  [nix-index-database](https://github.com/nix-community/nix-index-database)
  (`statix-scripts` includes it; point `STATIX_PROGRAMS` at
  `system=file.tsv:...` otherwise, or statix falls back to a small
  built-in table). Platform conditions are read from Nix:
  `lib.optionals stdenv.isDarwin [...]`, `lib.mkIf`, `if`,
  `pkgs.system == ...`, `meta.platforms`, NixOS modules (Linux).
- `${pkgs.jq}/bin/jqq` and `lib.getExe' pkgs.jq "jqq"`: the package has
  no such program.
- absolute host paths (`/run/current-system/sw/bin/x`) and missing
  relative ones (`./x.sh` next to `devenv.nix`).

Commands checked with `command -v`, `type`, `hash`, `which` or `[ -x ./x ]`
are optional. Directives in the script (shellcheck style, before the first
command for the whole script, otherwise for the next command):

```bash
# statix platforms=darwin          # only runs on macOS
pbcopy < out.txt
# statix provided=osascript,open   # comes from the host
# statix disable=undeclared_command
```

Platforms are `darwin`, `linux`, `aarch64`, `x86_64`, `unix`, `all` or a
system like `aarch64-darwin`. The same `# statix platforms=...` comment
works before a package in a Nix list. In `statix.toml`:

```toml
systems = ["x86_64-linux", "aarch64-darwin"]  # systems to check
provided = ["docker"]                          # commands the host provides
```

`STATIX_SYSTEMS=x86_64-linux,aarch64-darwin` overrides `systems`.

### Programs inside shell scripts and NixOS tests

`embedded_code` checks programs written inline in shell scripts: `jq`
filters (compiled by jq without running them), `awk` programs (parsed by
gawk) and `python -c` code (syntax errors and undefined names, with ruff).
Programs with `${...}` or shell variables in them are skipped.

NixOS test scripts (`testScript` next to `nodes`) are checked by `ruff`
with the test driver's names defined: `start_all`, `subtest`, `machine`,
one variable per node (`nodes.web-server` is `web_server`) and the rest.
When the nodes aren't written out, or the script pulls in code with
`${...}`, undefined names aren't reported.

### Structured files written as text

`structured_text_file` reports JSON, YAML, TOML and INI files written as
text (`pkgs.writeText "config.json" ''...''`, `environment.etc."x.toml".text`,
devenv `files."x.yaml".text`, `writeTextFile`, `builtins.toFile`) and points
to generating them from a Nix value: `(pkgs.formats.json { }).generate`,
devenv `files."x.json".json = { ... }`. Generated files are always well
formed and interpolated values are quoted. Text that's already generated
(`builtins.toJSON`), one static line, and meson `--cross-file`s are fine.
Shell scripts writing such files with text known before they run
(`cat > x.json <<'EOF'`, `echo '...' > x.yaml`) are reported too; runtime
text (`$VAR`, `$(...)`) and appends (`>>`) aren't.

### Shorter shell idioms

`shell_idiom` teaches shorter commands for common sequences in shell
scripts. `mkdir -p`, `cp`, `chmod`, `chown`/`chgrp` of one file are one
`install`:

```bash
mkdir -p $out/bin              # install -Dm755 tool $out/bin/tool
cp tool $out/bin/tool
chmod 755 $out/bin/tool
```

Other idioms, exact ones fixed by `statix fix`, the rest hints:

- build phases: missing `runHook preInstall`/`postInstall` (exact),
  `substituteInPlace --replace` → `--replace-fail`, `sed -i 's/a/b/'` →
  `substituteInPlace`, `installBin`, `installManPage`,
  `installShellCompletion`, `mkdir -p` before `makeWrapper`/`install -D`
  (exact), `set -euo pipefail` (already set), `HOME=$(mktemp -d)` →
  `$TMPDIR`, `--prefix PATH : ${x}/bin` → `lib.makeBinPath`
- `mkdir -p a; mkdir -p b` → `mkdir -p a b`, `rm -f x; ln -s y x` →
  `ln -sfn y x`, `$(cat f)` → `$(<f)` (bash), `egrep`/`fgrep` → `grep -E`/`-F`
  (exact)
- `cat f | cmd` → `cmd < f`, `echo x | tee f >/dev/null` → `echo x > f`
  (exact), `grep >/dev/null` → `grep -q`, `grep | wc -l` → `grep -c`,
  `grep | head -1` → `grep -m1`, `sort | uniq` → `sort -u`,
  `find -exec rm {} \;` → `-delete`, `[ -e f ] && rm f` → `rm -f f`,
  `[ ! -d d ] && mkdir d` → `mkdir -p d`, `for f in $(ls d)` → `d/*`,
  `ls | grep`, `echo $(cmd)`, `$(which x)` → `command -v`, `cd d; ...; cd ..`
  and `pushd`/`popd` → subshell

With an explicit numeric mode the `install` idiom is a warning that `statix fix` rewrites.
Without one it's a hint (shown, doesn't fail `statix check`): `install`
sets mode 755 where `cp` keeps the source's, so add `-m 644` for data
files. `-D` and `-t` are only suggested where GNU coreutils run the script
(build phases, devenv, NixOS).

### Configuration

Ignore lints and fixes by creating a `statix.toml` file at
your project root:

```
# within statix.toml
disabled = [
  "empty_pattern"
]
```

`statix` automatically discovers the configuration file by
traversing parents of the current directory and looking for
a `statix.toml` file. Alternatively, you can pass the path
to the `statix.toml` file on the command line with the
`--config` flag (available on `statix check` and `statix
fix`).

The available lints are (see `statix list` for an updated
list):

```
bool_comparison
empty_let_in
manual_inherit
manual_inherit_from
legacy_let_syntax
collapsible_let_in
eta_reduction
useless_parens
empty_pattern
redundant_pattern_bind
unquoted_uri
empty_inherit
deprecated_to_path
bool_simplification
useless_has_attr
repeated_keys
empty_list_concat
devenv_exec_shebang
devenv_pre_commit
hardcoded_store_path
impure_host_path
shellcheck
ruff
script_file
undeclared_command
structured_text_file
shell_idiom
embedded_code
```

All lints are enabled by default. Generate a minimal config
with `statix dump > statix.toml`.
