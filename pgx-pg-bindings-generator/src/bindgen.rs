use pgx_utils::pg_config::PgConfig;
use std::path::PathBuf;

pub struct PgBindingsGenerator<'a> {
    header: &'a PathBuf,
    pg_config: &'a PgConfig,
}

impl<'a> PgBindingsGenerator<'a> {
    pub fn new(header: &'a PathBuf, pg_config: &'a PgConfig) -> Self {
        PgBindingsGenerator { header, pg_config }
    }

    pub fn generate(self) -> Result<bindgen::Bindings, std::io::Error> {
        bindgen::Builder::default()
            .header(self.header.display().to_string())
            .clang_arg(&format!(
                "-I{}",
                self.pg_config.includedir_server()?.display()
            ))
            .blacklist_function("varsize_any") // pgx converts the VARSIZE_ANY macro, so we don't want to also have this function, which is in heaptuple.c
            .blacklist_function("query_tree_walker")
            .blacklist_function("expression_tree_walker")
            .blacklist_function("sigsetjmp")
            .blacklist_function("siglongjmp")
            .blacklist_function("pg_re_throw")
            .blacklist_function("palloc")
            .blacklist_function("palloc0")
            .blacklist_function("repalloc")
            .blacklist_function("pfree")
            .size_t_is_usize(true)
            .default_enum_style(bindgen::EnumVariation::Rust {
                non_exhaustive: false,
            })
            .default_macro_constant_type(bindgen::MacroTypeVariation::Signed)
            .rustfmt_bindings(true)
            .derive_debug(false)
            .derive_copy(false)
            .derive_default(false)
            .derive_eq(false)
            .derive_partialeq(false)
            .derive_hash(false)
            .derive_ord(false)
            .derive_partialord(false)
            .layout_tests(false)
            .generate()
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bindgen failed"))
    }
}
