use pgx_pg_bindings_generator::{PgBindingsGenerator, PgBindingsRewriter};
use pgx_utils::pg_config::Pgx;
use std::path::PathBuf;
use std::str::FromStr;
use syn::export::ToTokens;

fn main() -> Result<(), std::io::Error> {
    let input =
        PathBuf::from_str("/Users/e_ridge/_work/pgx/pgx-pg-sys/include/pg12.h").expect("bad path");
    let output = PathBuf::from_str("/Users/e_ridge/_work/pgx/pgx-pg-sys/src/pg12.rs")
        .expect("bad output path");
    let pgx = Pgx::from_config()?;
    let pg_config = pgx.get("pg12")?;
    let bindgen = PgBindingsGenerator::new(&input, pg_config);
    let bindings = bindgen.generate()?;
    let file = PgBindingsRewriter::new(bindings)
        .rewrite()
        .expect("failed to rewrite bindings");
    std::fs::write(output, file.to_token_stream().to_string())
}
