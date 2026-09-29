//! Packages devenv, NixOS and stdenv provide to scripts.

/// Tools of the stdenv every devenv shell is built from.
pub(super) const STDENV: &[&str] = &[
    "coreutils",
    "findutils",
    "diffutils",
    "gnused",
    "gnugrep",
    "gawk",
    "gnutar",
    "gzip",
    "bzip2",
    "xz",
    "gnumake",
    "bash",
    "gnupatch",
    "file",
];

/// systemd services' default `path` (with `enableDefaultPath`).
pub(super) const SYSTEMD_PATH: &[&str] =
    &["coreutils", "findutils", "gnugrep", "gnused", "systemd"];

/// Packages devenv `languages.<name>.enable` adds.
pub(super) const LANGUAGES: &[(&str, &[&str])] = &[
    ("rust", &["cargo", "rustc", "rustfmt", "clippy"]),
    ("javascript", &["nodejs"]),
    ("typescript", &["typescript"]),
    ("python", &["python3"]),
    ("go", &["go"]),
    ("ruby", &["ruby"]),
    ("php", &["php"]),
    ("java", &["jdk"]),
    ("elixir", &["elixir"]),
    ("erlang", &["erlang"]),
    ("zig", &["zig"]),
    ("deno", &["deno"]),
    ("terraform", &["terraform"]),
    ("opentofu", &["opentofu"]),
    ("nix", &["nil"]),
    ("c", &["gcc"]),
    ("cplusplus", &["gcc"]),
    ("haskell", &["ghc", "cabal-install"]),
    ("ocaml", &["ocaml", "dune_3"]),
    ("lua", &["lua"]),
    ("perl", &["perl"]),
    ("kotlin", &["kotlin"]),
    ("scala", &["scala"]),
    ("dotnet", &["dotnet-sdk"]),
    ("swift", &["swift"]),
    ("julia", &["julia"]),
    ("r", &["R"]),
];

/// Packages devenv `services.<name>.enable` adds.
pub(super) const SERVICES: &[(&str, &[&str])] = &[
    ("postgres", &["postgresql"]),
    ("mysql", &["mariadb"]),
    ("redis", &["redis"]),
    ("mongodb", &["mongodb"]),
    ("minio", &["minio", "minio-client"]),
    ("elasticsearch", &["elasticsearch"]),
    ("caddy", &["caddy"]),
    ("nginx", &["nginx"]),
];

/// Packages top-level devenv modules add with `<module>.enable`.
pub(super) const MODULES: &[(&str, &[&str])] = &[
    ("treefmt", &["treefmt"]),
    ("git-hooks", &["prek"]),
    ("pre-commit", &["pre-commit"]),
    ("cachix", &["cachix"]),
];

/// Shell functions devenv defines for its scripts (`enterTest`).
pub(super) const DEVENV_FUNCTIONS: &[&str] = &["wait_for_port", "wait_for_processes"];

/// Top-level options only NixOS has (devenv has `services`, `env`...).
pub(super) const NIXOS_OPTIONS: &[&str] = &[
    "systemd",
    "boot",
    "security",
    "networking",
    "fileSystems",
    "hardware",
    "virtualisation",
    // NixOS VM tests
    "nodes",
    "testScript",
];
