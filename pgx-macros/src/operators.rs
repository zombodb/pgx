use pgx_utils::{operator_common::*, pg_inventory};
use quote::ToTokens;
use syn::DeriveInput;

pub(crate) fn impl_postgres_eq(ast: DeriveInput) -> proc_macro2::TokenStream {
    let mut stream = proc_macro2::TokenStream::new();

    stream.extend(eq(&ast.ident));
    stream.extend(ne(&ast.ident));

    stream
}

pub(crate) fn impl_postgres_ord(ast: DeriveInput) -> proc_macro2::TokenStream {
    let mut stream = proc_macro2::TokenStream::new();

    stream.extend(lt(&ast.ident));
    stream.extend(gt(&ast.ident));
    stream.extend(le(&ast.ident));
    stream.extend(ge(&ast.ident));
    stream.extend(cmp(&ast.ident));

    pg_inventory::PostgresOrd::new(ast.ident.clone()).to_tokens(&mut stream);

    stream
}

pub(crate) fn impl_postgres_hash(ast: DeriveInput) -> proc_macro2::TokenStream {
    let mut stream = proc_macro2::TokenStream::new();

    stream.extend(hash(&ast.ident));

    pg_inventory::PostgresHash::new(ast.ident.clone()).to_tokens(&mut stream);

    stream
}
