let
  path = ".github/workflows/release.yaml";
  # Nix builds pushed to the Cachix cache named by the `CACHIX_CACHE` variable, for every push to these branches and tags
  nixSystems = [
    "ubuntu-latest"
    "ubuntu-24.04-arm"
    "macos-latest"
  ];
  # Standalone binaries attached to GitHub releases for `v*` tags
  binaries = [
    {
      os = "ubuntu-latest";
      target = "x86_64-unknown-linux-musl";
    }
    {
      os = "ubuntu-24.04-arm";
      target = "aarch64-unknown-linux-musl";
    }
    {
      os = "macos-latest";
      target = "aarch64-apple-darwin";
    }
    {
      os = "macos-latest";
      target = "x86_64-apple-darwin";
    }
  ];
  # nix-index-database small indexes (programs in bin/sbin) for the systems
  # `undeclared_command` checks; converted to `attr<TAB>output<TAB>program`
  indexRelease = "2026-09-27-083517";
  indexHashes = {
    x86_64-linux = "sha256-9PU4T/8oicqb/qEhNU9hZ9Jh7gMIoZmKVc2jiRPHB3Q=";
    aarch64-linux = "sha256-1TqhPiO2vYf46Uju0IaIPfmIR6bYutVS9QKQxar4Mj0=";
    aarch64-darwin = "sha256-UIe55mh35D/TdE6VUqBndjJJZ7jC1eAvUkU8bVa5tao=";
  };
in
{
  perSystem =
    { pkgs, lib, ... }:
    let
      programIndex =
        system: hash:
        pkgs.runCommand "statix-programs-${system}"
          {
            nativeBuildInputs = [ pkgs.nix-index ];
            index = pkgs.fetchurl {
              url = "https://github.com/nix-community/nix-index-database/releases/download/${indexRelease}/index-${system}-small";
              inherit hash;
            };
          }
          ''
            mkdir db && ln -s $index db/files
            nix-locate --db db --at-root --regex '/s?bin/[^/]+$' \
              | awk '($3=="x"||$3=="s") && split($4,p,"/")==6 && (p[5]=="bin"||p[5]=="sbin") {
                  a=$1; o=a; sub(/\.[^.]*$/,"",a); sub(/.*\./,"",o); print a"\t"o"\t"p[6] }' \
              | sort -u > $out
            test -s $out
          '';
      # GitHub sources nixpkgs packages, for `packaged_in_nixpkgs`
      githubIndex = pkgs.runCommand "statix-nixpkgs-github" { } ''
        ${lib.getExe pkgs.statix} nixpkgs-index ${pkgs.path} > $out
        test -s $out
      '';
      programs = lib.concatStringsSep ":" (
        lib.mapAttrsToList (system: hash: "${system}=${programIndex system hash}") indexHashes
      );
    in
    {
      packages = {
        default = pkgs.statix;
        inherit (pkgs) statix;
        # statix with shellcheck, ruff and the program indexes for the script lints
        statix-scripts = pkgs.symlinkJoin {
          name = "statix-scripts-${pkgs.statix.version}";
          paths = [ pkgs.statix ];
          nativeBuildInputs = [ pkgs.makeWrapper ];
          postBuild = ''
            wrapProgram $out/bin/statix --prefix PATH : ${
              lib.makeBinPath [
                pkgs.shellcheck
                pkgs.ruff
                pkgs.jq
                pkgs.gawk
              ]
            } --set-default STATIX_PROGRAMS ${lib.escapeShellArg programs} \
              --set-default STATIX_NIXPKGS_GITHUB ${githubIndex}
          '';
          meta.mainProgram = "statix";
        };
      };

      files.file.${path}.source = pkgs.writers.writeJSON "gh-actions-workflow-release.yaml" {
        name = "Release";
        on = {
          push = {
            branches = [ "devenv" ];
            tags = [ "v*" ];
          };
          workflow_dispatch = { };
        };
        jobs = {
          cachix = {
            "if" = "\${{ vars.CACHIX_CACHE != '' }}";
            strategy.matrix.os = nixSystems;
            runs-on = "\${{ matrix.os }}";
            steps = [
              { uses = "actions/checkout@v5"; }
              { uses = "cachix/install-nix-action@master"; }
              {
                uses = "cachix/cachix-action@master";
                "with" = {
                  name = "\${{ vars.CACHIX_CACHE }}";
                  authToken = "\${{ secrets.CACHIX_AUTH_TOKEN }}";
                };
              }
              {
                run = "nix build --accept-flake-config --print-build-logs .#statix .#statix-scripts";
              }
            ];
          };
          binaries = {
            "if" = "startsWith(github.ref, 'refs/tags/v')";
            strategy.matrix.include = binaries;
            runs-on = "\${{ matrix.os }}";
            steps = [
              { uses = "actions/checkout@v5"; }
              {
                uses = "dtolnay/rust-toolchain@stable";
                "with".targets = "\${{ matrix.target }}";
              }
              {
                "if" = "runner.os == 'Linux'";
                run = "sudo apt-get update && sudo apt-get install -y musl-tools";
              }
              {
                run = ''
                  cargo build --release --locked --bin statix --target ''${{ matrix.target }}
                  tar -czf statix-''${{ matrix.target }}.tar.gz -C target/''${{ matrix.target }}/release statix
                '';
              }
              {
                uses = "actions/upload-artifact@v4";
                "with" = {
                  name = "statix-\${{ matrix.target }}";
                  path = "statix-\${{ matrix.target }}.tar.gz";
                };
              }
            ];
          };
          release = {
            needs = "binaries";
            runs-on = "ubuntu-latest";
            permissions.contents = "write";
            steps = [
              {
                uses = "actions/download-artifact@v4";
                "with".merge-multiple = true;
              }
              {
                env.GH_TOKEN = "\${{ github.token }}";
                run = ''
                  sha256sum statix-*.tar.gz > SHA256SUMS
                  gh release create "$GITHUB_REF_NAME" --repo "$GITHUB_REPOSITORY" --generate-notes statix-*.tar.gz SHA256SUMS
                '';
              }
            ];
          };
        };
      };

      treefmt.settings.global.excludes = [ path ];
    };
}
