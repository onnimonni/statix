let
  path = ".github/workflows/check.yaml";
in
{
  perSystem =
    { pkgs, ... }:
    {
      files.file.${path}.source = pkgs.writers.writeJSON "gh-actions-workflow-check.yaml" {
        name = "Check";
        on = {
          pull_request = { };
          push = { };
          workflow_dispatch = { };
        };
        jobs = {
          check = {
            runs-on = "ubuntu-latest";
            steps = [
              { uses = "actions/checkout@v5"; }
              {
                uses = "cachix/install-nix-action@master";
                "with" = {
                  extra_nix_config = ''
                    # Kept out of the flake: its nixConfig made every `nix run` ask about each setting
                    abort-on-warn = true
                    allow-import-from-derivation = false
                    extra-substituters = https://onnimonni.cachix.org
                    extra-trusted-public-keys = onnimonni.cachix.org-1:bAPuRbTAiFMLNLoojt7KlqhQcpdeTN/OMIL22fP3LyM=
                    keep-env-derivations = true
                    keep-outputs = true
                  '';
                  github_access_token = "\${{ secrets.GITHUB_TOKEN }}";
                };
              }
              {
                uses = "nix-community/cache-nix-action@main";
                "with".primary-key = "nix-\${{ runner.os }}";
              }
              {
                run = "nix flake check --print-build-logs";
              }
            ];
          };
        };
      };

      treefmt.settings.global.excludes = [ path ];
    };
}
