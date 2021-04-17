{ lib, rustPlatform, fetchFromGitHub, postgresql_10, postgresql_11, postgresql_12, postgresql_13, pkg-config, openssl, rustfmt, llvmPackages, }:

rustPlatform.buildRustPackage rec {
  pname = "cargo-pgx";
  version = "0.1.20";

  src = ../.;

  cargoSha256 = "1HA7RFw+WKWERf4W7uHhmdnEne2XDib9ijnqf3lxKcQ=";
  cargoBuildFlags = [ "--package" "cargo-pgx" ];
  cargoCheckFlags = [ "--package" "cargo-pgx" ];
  cargoTestFlags = [ "--package" "cargo-pgx" ];

  /*
  TODO: pgx currently has to modify the postgres directory during development,
          so we can't get tricky and use the existing PostgreSQL packages.

  PGX_HOME = "./pgx";
  postBuild = ''
    cargo run --package cargo-pgx --bin cargo-pgx -- pgx init \
    --pg10 ${postgresql_10}/bin/pg_config \
    --pg11 ${postgresql_11}/bin/pg_config \
    --pg12 ${postgresql_12}/bin/pg_config \
    --pg13 ${postgresql_13}/bin/pg_config
  '';
    preFixup = ''
    mv $(pwd)/pgx $out/pgx
    ${makeWrapper} $out/bin/cargo-pgx $out/bin/cargo-pgx-wrapped --set PGX_HOME $out/pgx
  '';
  */

  nativeBuildInputs = [
    pkg-config
  ];
  buildInputs = [
    openssl
  ];

  LIBCLANG_PATH="${llvmPackages.libclang}/lib";

  meta = with lib; {
    description = "Build PostgreSQL extensions with Rust.";
    homepage = "https://github.com/zombodb/pgx";
    license = with licenses; [ mit ];
    maintainers = with maintainers; [ hoverbear ];
  };
}
