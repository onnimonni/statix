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
in
{
  perSystem =
    { pkgs, lib, ... }:
    {
      packages = {
        default = pkgs.statix;
        inherit (pkgs) statix;
        # statix with shellcheck and ruff for the script lints
        statix-scripts = pkgs.symlinkJoin {
          name = "statix-scripts-${pkgs.statix.version}";
          paths = [ pkgs.statix ];
          nativeBuildInputs = [ pkgs.makeWrapper ];
          postBuild = ''
            wrapProgram $out/bin/statix --prefix PATH : ${
              lib.makeBinPath [
                pkgs.shellcheck
                pkgs.ruff
              ]
            }
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
