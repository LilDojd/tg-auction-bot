{
  pkgs,
  ...
}:
{
  env.DATABASE_URL = "postgresql://localhost:5432/postgres";
  env.SQLX_OFFLINE = true;

  languages.rust = {
    enable = true;
    channel = "nightly";
    mold.enable = if pkgs.stdenv.isLinux then true else false;

    components = [
      "rustc"
      "cargo"
      "clippy"
      "rustfmt"
      "rust-analyzer"
    ];
  };

  dotenv.enable = true;
  dotenv.filename = [
    ".env"
  ];

  services = {
    postgres = {
      enable = true;
      listen_addresses = "*";
    };
  };
  packages = [
    pkgs.git
    pkgs.basedpyright
    pkgs.cargo-nextest
    pkgs.cargo-expand
    pkgs.cargo-edit
    pkgs.sqlx-cli
    pkgs.cargo-machete
    pkgs.cargo-deny
    pkgs.cargo-autoinherit
    pkgs.openssl
  ];
}
