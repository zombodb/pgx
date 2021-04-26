// Copyright 2020 ZomboDB, LLC <zombodb@gmail.com>. All rights reserved. Use of this source code is
// governed by the MIT license that can be found in the LICENSE file.

use crate::commands::get::get_property;
use pgx_utils::operator_common::*;
use pgx_utils::{
    categorize_type, exit_with_error, get_named_capture, handle_result, CategorizedType,
    ExternArgs, FunctionArgs,
};
use proc_macro2::{Ident, Span, TokenTree};
use quote::quote;
use std::borrow::BorrowMut;
use std::collections::{BTreeSet, HashSet};
use std::fs::DirEntry;
use std::io::{BufRead, Write};
use std::ops::Deref;
use std::path::PathBuf;
use std::result::Result;
use std::str::FromStr;
use syn::spanned::Spanned;
use syn::{Attribute, FnArg, Item, ItemFn, Pat, ReturnType, Type};

#[derive(Debug)]
enum OperatorOptions {
    Name(String),
    Commutator(String),
    Negator(String),
    Restrict(String),
    Join(String),
    Hashes,
    Merges,
}

#[derive(Debug)]
enum CategorizedAttribute {
    PgGuard(Span),
    PgTest(Span, BTreeSet<ExternArgs>, Vec<FunctionArgs>),
    RustTest(Span),
    PgExtern(Span, BTreeSet<ExternArgs>, Vec<FunctionArgs>),
    PgOperator(
        Span,
        BTreeSet<ExternArgs>,
        Vec<FunctionArgs>,
        Vec<OperatorOptions>,
    ),
    Sql(Vec<String>),
    SqlFunctionName(String),
    SqlFunctionArgs(String),
    Other(Vec<(Span, String)>),
}

#[derive(Debug, Eq, PartialEq, Hash)]
enum DeriveMacros {
    PostgresType,
    PostgresEq,
    PostgresOrd,
    PostgresHash,
}

pub(crate) fn generate_schema(features: &[&str]) -> Result<(), std::io::Error> {
    let mut features: HashSet<String> = features.into_iter().map(|s| s.to_string()).collect();
    if let Ok(pg_features) = std::env::var("PGX_BUILD_FEATURES") {
        features.extend(pg_features.split(' ').map(|s| s.to_string()))
    }
    let path = PathBuf::from_str("./src").unwrap();
    let files = find_rs_files(&path, Vec::new());
    let default_schema = get_property("schema").unwrap_or_else(|| "public".to_string());

    delete_generated_sql();

    let mut created = Vec::new();
    files.iter().for_each(|f: &DirEntry| {
        let statemets = generate_sql(f, default_schema.clone(), &features);
        let (did_write, filename) = write_sql_file(f, statemets);

        // strip the leading ./sql/ from the filenames we generated
        let mut filename = filename.display().to_string();
        filename = filename.trim_start_matches("./sql/").to_string();

        if did_write {
            created.push(filename);
        }
    });

    process_schema_load_order(created);

    Ok(())
}

fn process_schema_load_order(mut created: Vec<String>) {
    let filename = PathBuf::from_str("./sql/load-order.txt").unwrap();
    let mut load_order = read_load_order(&filename);

    // keep in load oder only those files that a) aren't generated or b) are generated that we just created
    // ie, remove those that are flagged as generated but aren't valid anymore
    load_order.retain(|v| {
        !v.ends_with(".generated.sql") || (v.ends_with(".generated.sql") && created.contains(v))
    });

    // remove everything from created that is already in load order
    created.retain(|v| !load_order.contains(v));

    // append whatever is left in created to load_order as they're new files
    created.sort();
    load_order.append(&mut created);

    // rewrite the load_order file
    let mut file = handle_result!(
        std::fs::File::create(&filename),
        format!("failed to create {}", filename.display())
    );
    load_order.iter().for_each(|v| {
        let v = v.trim_start_matches("./sql/");

        handle_result!(
            file.write_all(v.as_bytes()),
            format!("failed to write to {}", filename.display())
        );
        handle_result!(
            file.write_all(&[b'\n']),
            format!("failed to write to {}", filename.display())
        );
    });
}

pub(crate) fn read_load_order(filename: &PathBuf) -> Vec<String> {
    let mut load_order = Vec::new();

    if let Ok(file) = std::fs::File::open(&filename) {
        let reader = std::io::BufReader::new(file);
        for (_, line) in reader.lines().enumerate() {
            load_order.push(line.unwrap());
        }
    }

    load_order
}

fn write_sql_file(f: &DirEntry, statements: Vec<String>) -> (bool, PathBuf) {
    let filename = make_sql_filename(f);

    if statements.is_empty() {
        // delete existing sql file if it exists
        if filename.exists() {
            handle_result!(
                std::fs::remove_file(&filename),
                format!("failed to delete {}", filename.display())
            );
        }

        (false, filename)
    } else {
        // write the statements out to the sql file
        let mut file = handle_result!(
            std::fs::File::create(&filename),
            format!("failed to create {}", filename.display())
        );
        for statement in statements {
            handle_result!(
                file.write_all(statement.as_bytes()),
                format!("failed to write to {}", filename.display())
            );
            handle_result!(
                file.write_all(&[b'\n']),
                format!("failed to write to {}", filename.display())
            );
        }

        (true, filename)
    }
}

fn make_sql_filename(f: &DirEntry) -> PathBuf {
    PathBuf::from_str(make_sql_filename_from_string(f.path().display().to_string()).as_str())
        .unwrap()
}

fn make_sql_filename_from_string(mut sql_filename: String) -> String {
    sql_filename = sql_filename.trim_start_matches("./src/").to_string();
    sql_filename = sql_filename.trim_start_matches("./sql/").to_string();
    sql_filename = sql_filename.trim_end_matches(".rs").to_string();
    sql_filename = sql_filename.replace("/", "_");
    sql_filename.insert_str(0, "./sql/");
    sql_filename.push_str(".generated.sql");

    sql_filename
}

fn find_rs_files(path: &PathBuf, mut files: Vec<DirEntry>) -> Vec<DirEntry> {
    if path.display().to_string().contains("/target/") {
        // ignore the target/ directory
        return files;
    }

    for f in std::fs::read_dir(path).unwrap() {
        if let Ok(f) = f {
            let filename = f.file_name().into_string().unwrap();

            if f.metadata().unwrap().is_dir() {
                // recurse
                files = find_rs_files(&f.path(), files);
            } else if filename.ends_with(".rs") {
                files.push(f);
            }
        }
    }

    files.sort_by(|a, b| {
        a.file_name()
            .to_str()
            .unwrap()
            .cmp(b.file_name().to_str().unwrap())
    });
    files
}

fn delete_generated_sql() {
    let path = PathBuf::from_str("./sql").unwrap();
    if !path.exists() {
        exit_with_error!(
            "The path `{}` does not exist.  Try running `{}` to fix this.",
            path.display().to_string().yellow(),
            "cargo pgx schema".green()
        )
    }
    for f in std::fs::read_dir(&path).unwrap() {
        if let Ok(f) = f {
            let filename = f.file_name().into_string().unwrap();

            if f.metadata().unwrap().is_file() && filename.ends_with(".generated.sql") {
                handle_result!(
                    std::fs::remove_file(f.path()),
                    format!("failed to delete {}", filename)
                );
            }
        }
    }
}

fn parse_extern_args(att: &Attribute) -> BTreeSet<ExternArgs> {
    pgx_utils::parse_extern_attributes(att.tokens.clone())
        .into_iter()
        .collect()
}

fn generate_sql(
    rs_file: &DirEntry,
    default_schema: String,
    features: &HashSet<String>,
) -> Vec<String> {
    let mut sql = Vec::new();
    let file = std::fs::read_to_string(rs_file.path()).unwrap();
    let ast = syn::parse_file(file.as_str()).unwrap();

    let mut schema_stack = Vec::new();

    schema_stack.push(default_schema.clone());
    walk_items(
        rs_file,
        &mut sql,
        ast.items,
        &mut schema_stack,
        &default_schema,
        features,
    );

    sql
}

#[allow(clippy::cognitive_complexity)]
fn walk_items(
    rs_file: &DirEntry,
    all_sql: &mut Vec<String>,
    items: Vec<Item>,
    schema_stack: &mut Vec<String>,
    default_schema: &str,
    features: &HashSet<String>,
) {
    let mut sql = Vec::new();
    let mut postgres_enums = Vec::new();
    let mut postgres_types = Vec::new();
    let mut operator_sql = Vec::new();
    let current_schema = schema_stack
        .last()
        .expect("couldn't determine the current schema")
        .clone();
    for item in items {
        if !is_active(&item, features) {
            continue;
        }
        if let Item::Mod(module) = item {
            module.attrs;
            if let Some((_, items)) = module.content {
                let schema = module.ident.to_string();
                schema_stack.push(schema.clone());
                sql.push(format!(
                    "CREATE SCHEMA IF NOT EXISTS {};",
                    schema
                ));
                walk_items(
                    rs_file,
                    &mut sql,
                    items,
                    schema_stack,
                    default_schema,
                    features,
                );
                schema_stack.pop();
            }
        } else if let Item::Struct(strct) = item {
            let mut derives = HashSet::<DeriveMacros>::new();

            for a in &strct.attrs {
                let string = quote! {#a}.to_string();

                if string.contains("PostgresType") {
                    derives.insert(DeriveMacros::PostgresType);
                }
                if string.contains("PostgresEq") {
                    derives.insert(DeriveMacros::PostgresEq);
                }
                if string.contains("PostgresOrd") {
                    derives.insert(DeriveMacros::PostgresOrd);
                }
                if string.contains("PostgresHash") {
                    derives.insert(DeriveMacros::PostgresHash);
                }
            }

            if derives.contains(&DeriveMacros::PostgresType) {
                let name = strct.ident.to_string().to_lowercase();
                postgres_types.push(format!(
                    "CREATE TYPE {};",
                    qualify_name(&current_schema, &name)
                ));
                postgres_types.push(format!("CREATE OR REPLACE FUNCTION {qualified_name}_in(cstring) RETURNS {qualified_name} IMMUTABLE STRICT PARALLEL SAFE LANGUAGE C AS 'MODULE_PATHNAME', '{name}_in_wrapper';", qualified_name = qualify_name(&current_schema, &name), name = name));
                postgres_types.push(format!("CREATE OR REPLACE FUNCTION {qualified_name}_out({qualified_name}) RETURNS cstring IMMUTABLE STRICT PARALLEL SAFE LANGUAGE C AS 'MODULE_PATHNAME', '{name}_out_wrapper';", qualified_name = qualify_name(&current_schema, &name), name = name));
                postgres_types.push(format!(
                    "CREATE TYPE {qualified_name} (
                                INTERNALLENGTH = variable,
                                INPUT = {qualified_name}_in,
                                OUTPUT = {qualified_name}_out,
                                STORAGE = extended
                            );",
                    qualified_name = qualify_name(&current_schema, &name)
                ));
            }

            if derives.contains(&DeriveMacros::PostgresEq) {
                walk_items(
                    rs_file,
                    &mut operator_sql,
                    vec![parse_item(eq(&strct.ident))],
                    schema_stack,
                    default_schema,
                    features,
                );
                walk_items(
                    rs_file,
                    &mut operator_sql,
                    vec![parse_item(ne(&strct.ident))],
                    schema_stack,
                    default_schema,
                    features,
                );
            }

            if derives.contains(&DeriveMacros::PostgresOrd) {
                walk_items(
                    rs_file,
                    &mut operator_sql,
                    vec![parse_item(lt(&strct.ident))],
                    schema_stack,
                    default_schema,
                    features,
                );
                walk_items(
                    rs_file,
                    &mut operator_sql,
                    vec![parse_item(gt(&strct.ident))],
                    schema_stack,
                    default_schema,
                    features,
                );
                walk_items(
                    rs_file,
                    &mut operator_sql,
                    vec![parse_item(le(&strct.ident))],
                    schema_stack,
                    default_schema,
                    features,
                );
                walk_items(
                    rs_file,
                    &mut operator_sql,
                    vec![parse_item(ge(&strct.ident))],
                    schema_stack,
                    default_schema,
                    features,
                );
                walk_items(
                    rs_file,
                    &mut operator_sql,
                    vec![parse_item(cmp(&strct.ident))],
                    schema_stack,
                    default_schema,
                    features,
                );
            }

            if derives.contains(&DeriveMacros::PostgresHash) {
                walk_items(
                    rs_file,
                    &mut operator_sql,
                    vec![parse_item(hash(&strct.ident))],
                    schema_stack,
                    default_schema,
                    features,
                );

                let type_name = &strct.ident.to_string().to_lowercase();
                operator_sql.push(format!(
                    "CREATE OPERATOR FAMILY {}_hash_ops USING hash;",
                    type_name
                ));
                operator_sql.push(format!(
                    "CREATE OPERATOR CLASS {type_name}_hash_ops DEFAULT FOR TYPE {type_name} USING hash FAMILY {type_name}_hash_ops AS
                        OPERATOR    1   =  ({type_name}, {type_name}),
                        FUNCTION    1   {type_name}_hash({type_name});",
                    type_name = type_name
                ));
            }

            if derives.contains(&DeriveMacros::PostgresEq)
                && derives.contains(&DeriveMacros::PostgresOrd)
            {
                let type_name = &strct.ident.to_string().to_lowercase();
                operator_sql.push(format!(
                    "CREATE OPERATOR FAMILY {}_btree_ops USING btree;",
                    type_name
                ));
                operator_sql.push(format!(
                    "CREATE OPERATOR CLASS {type_name}_btree_ops DEFAULT FOR TYPE {type_name} USING btree FAMILY {type_name}_btree_ops AS
                      OPERATOR 1 < ,
                      OPERATOR 2 <= ,
                      OPERATOR 3 = ,
                      OPERATOR 4 >= ,
                      OPERATOR 5 > ,
                      FUNCTION 1 {type_name}_cmp({type_name}, {type_name});",
                    type_name = type_name
                ));
            }
        } else if let Item::Enum(enm) = item {
            let mut found_postgres_enum = false;
            for a in enm.attrs {
                let string = quote! {#a}.to_string();

                if string.contains("PostgresEnum") {
                    found_postgres_enum = true;
                }
            }

            if found_postgres_enum {
                let name = enm.ident.to_string().to_lowercase();
                postgres_enums.push(format!(
                    "CREATE TYPE {qualified_name} AS ENUM (",
                    qualified_name = qualify_name(&current_schema, &name)
                ));

                for (idx, d) in enm.variants.iter().enumerate() {
                    let mut line = String::new();
                    line.push_str(&format!("'{}'", d.ident.to_string()));
                    if idx < enm.variants.len() - 1 {
                        line.push(',');
                    }

                    postgres_enums.push(line);
                }
                postgres_enums.push(");".to_string());
            }
        } else if let Item::Macro(makro) = item {
            let name = match makro.mac.path.get_ident() {
                Some(ident) => ident.to_string(),
                None => "".to_string(),
            };

            if name.ends_with("extension_sql") {
                let string = makro.mac.tokens.to_string();
                let string = string.trim();

                if !string.starts_with("r#\"") || !string.ends_with("\"#") {
                    exit_with_error!("extension_sql!{{}} value isn't ia raw string");
                }

                // remove the raw string quotes
                let string = string.trim_start_matches("r#\"");
                let string = string.trim_end_matches("\"#");

                // trim off leading/trailing new lines, but preserve other whitespace
                let string = string.trim_start_matches('\n');
                let string = string.trim_end_matches('\n');

                // and remember this sql block
                sql.push(string.to_string());
            }
        } else if let Item::Fn(func) = item {
            let attributes = collect_attributes(rs_file, &func.sig.ident, &func.attrs);
            let is_test_mode = std::env::var("PGX_TEST_MODE_VERSION").is_ok();
            let mut function_sql = Vec::new();
            let sql_func_args = extract_funcargs_attribute(&attributes);

            for attribute in attributes {
                match attribute {
                    // only generate CREATE FUNCTION statements for #[pg_test] functions
                    // if we're in test mode, which is controlled by the PGX_TEST_MODE
                    // environment variable
                    CategorizedAttribute::PgTest(span, _, funcargs) if is_test_mode => {
                        if let (Some(statement), _, _) = make_create_function_statement(
                            &func,
                            None,
                            rs_file,
                            None,
                            &current_schema,
                            schema_stack,
                            &funcargs,
                        ) {
                            function_sql.push(location_comment(rs_file, &span));
                            function_sql.push(statement);
                        }
                    }

                    // for #[pg_extern] attributes, we only want to programmatically generate
                    // a CREATE FUNCTION statement if we don't already have some
                    CategorizedAttribute::PgExtern(span, extern_args, funcargs)
                        if function_sql.is_empty() =>
                    {
                        if let (Some(statement), _, _) = make_create_function_statement(
                            &func,
                            Some(extern_args),
                            rs_file,
                            sql_func_args.clone(),
                            &current_schema,
                            schema_stack,
                            &funcargs,
                        ) {
                            function_sql.push(location_comment(rs_file, &span));
                            function_sql.push(statement);
                        }
                    }

                    // for #[pg_operator] we do the same as above for #[pg_extern], but we also
                    // generate the CREATE OPERATOR sql too
                    CategorizedAttribute::PgOperator(span, args, funcargs, options)
                        if function_sql.is_empty() =>
                    {
                        if let (Some(statement), Some(func_name), Some(type_names)) =
                            make_create_function_statement(
                                &func,
                                Some(args),
                                rs_file,
                                sql_func_args.clone(),
                                &current_schema,
                                schema_stack,
                                &funcargs,
                            )
                        {
                            if type_names.len() > 2 {
                                exit_with_error!(
                                    "#[pg_operator] only supports functions with 1 or 2 arguments"
                                )
                            }

                            function_sql.push(location_comment(rs_file, &span));
                            function_sql.push(statement);

                            let mut name = None;

                            for option in &options {
                                match option {
                                    OperatorOptions::Name(n) => name = Some(n),
                                    _ => {}
                                }
                            }

                            if name.is_none() {
                                exit_with_error!(
                                    "#[pg_operator] requires the #[opname( <opname> )] macro"
                                )
                            }

                            let mut sql = String::new();
                            sql.push_str("CREATE OPERATOR ");
                            sql.push_str(&qualify_name(&current_schema, &name.unwrap()));
                            sql.push_str(" (");
                            sql.push_str("\n   PROCEDURE=");
                            sql.push_str(&qualify_name(&current_schema, &func_name));
                            if type_names.len() == 1 {
                                sql.push_str(",\n   RIGHTARG=");
                                sql.push_str(type_names.get(0).unwrap());
                            } else if type_names.len() == 2 {
                                sql.push_str(",\n   LEFTARG=");
                                sql.push_str(type_names.get(0).unwrap());
                                sql.push_str(",\n   RIGHTARG=");
                                sql.push_str(type_names.get(1).unwrap());
                            }

                            for option in options {
                                match option {
                                    OperatorOptions::Commutator(c) => {
                                        sql.push_str(",\n   COMMUTATOR = ");
                                        sql.push_str(&c);
                                    }
                                    OperatorOptions::Negator(n) => {
                                        sql.push_str(",\n   NEGATOR = ");
                                        sql.push_str(&n);
                                    }
                                    OperatorOptions::Restrict(r) => {
                                        sql.push_str(",\n   RESTRICT = ");
                                        sql.push_str(&r);
                                    }
                                    OperatorOptions::Join(j) => {
                                        sql.push_str(",\n   JOIN = ");
                                        sql.push_str(&j);
                                    }
                                    OperatorOptions::Hashes => sql.push_str(",\n   HASHES"),
                                    OperatorOptions::Merges => sql.push_str(",\n   MERGES"),
                                    _ => {}
                                }
                            }

                            sql.push_str("\n);");

                            operator_sql.push(sql);
                        }
                    }

                    // it's user-provided SQL from doc comment blocks
                    CategorizedAttribute::Sql(mut sql_lines) => function_sql.append(&mut sql_lines),

                    // we don't care about other attributes
                    _ => {}
                }
            }

            sql.append(&mut function_sql);
        }
    }

    if !sql.is_empty() {
        if current_schema != default_schema {
            // pg_catalog is a reserved schema name that we can't even try to create
            if current_schema != "pg_catalog" {
                all_sql.push(format!(
                    "CREATE SCHEMA IF NOT EXISTS {};",
                    quote_ident_string(current_schema.clone())
                ));
            }
        }
    }

    // types (with their in/out functions) go first
    all_sql.append(&mut postgres_enums);
    all_sql.append(&mut postgres_types);

    // then general sql (mostly just functions)
    all_sql.append(&mut sql);

    // and finally then operators
    all_sql.append(&mut operator_sql);
}

fn is_active(item: &syn::Item, features: &HashSet<String>) -> bool {
    for attr in item_attrs(item) {
        if !attr.path.is_ident("cfg") {
            continue;
        }

        let meta = match attr.parse_meta() {
            Ok(syn::Meta::List(meta)) => meta,
            _ => continue,
        };

        if !is_active_inner(meta.nested.iter(), features, true) {
            return false;
        }

        fn is_active_inner<'a>(
            metas: impl Iterator<Item = &'a syn::NestedMeta>,
            features: &HashSet<String>,
            is_any: bool,
        ) -> bool {
            let mut active = !is_any;
            for meta in metas {
                let meta = match meta {
                    syn::NestedMeta::Meta(meta) => meta,
                    syn::NestedMeta::Lit(_) => return true,
                };

                match meta {
                    // cannot tell, just continue
                    syn::Meta::Path(_) => continue,
                    syn::Meta::NameValue(inner) => {
                        if !inner.path.is_ident("feature") {
                            continue; // cannot tell, just continue
                        }
                        match &inner.lit {
                            syn::Lit::Str(s) => match (features.contains(&*s.value()), is_any) {
                                (true, true) => active |= true,
                                (true, false) => active &= true,
                                (false, true) => active |= false,
                                (false, false) => active &= false,
                            },
                            _ => continue,
                        }
                    }
                    // if we find a list, there can only be one element
                    syn::Meta::List(list) => {
                        if list.path.is_ident("not") {
                            return !is_active_inner(list.nested.iter(), features, is_any);
                        }

                        if list.path.is_ident("any") {
                            return is_active_inner(list.nested.iter(), features, true);
                        }

                        if list.path.is_ident("all") {
                            return is_active_inner(list.nested.iter(), features, false);
                        }
                    }
                }
            }
            active
        }
    }
    true
}

fn item_attrs(item: &syn::Item) -> impl Iterator<Item = &syn::Attribute> {
    match item {
        syn::Item::Const(i) => i.attrs.iter(),
        syn::Item::Enum(i) => i.attrs.iter(),
        syn::Item::ExternCrate(i) => i.attrs.iter(),
        syn::Item::Fn(i) => i.attrs.iter(),
        syn::Item::ForeignMod(i) => i.attrs.iter(),
        syn::Item::Impl(i) => i.attrs.iter(),
        syn::Item::Macro(i) => i.attrs.iter(),
        syn::Item::Macro2(i) => i.attrs.iter(),
        syn::Item::Mod(i) => i.attrs.iter(),
        syn::Item::Static(i) => i.attrs.iter(),
        syn::Item::Struct(i) => i.attrs.iter(),
        syn::Item::Trait(i) => i.attrs.iter(),
        syn::Item::TraitAlias(i) => i.attrs.iter(),
        syn::Item::Type(i) => i.attrs.iter(),
        syn::Item::Union(i) => i.attrs.iter(),
        syn::Item::Use(i) => i.attrs.iter(),
        _ => [].iter(),
    }
}

fn qualify_name(schema: &str, name: &str) -> String {
    if "public" == schema {
        name.to_owned()
    } else {
        format!("{}.{}", schema, name)
    }
}

fn make_create_function_statement(
    func: &ItemFn,
    mut extern_args: Option<BTreeSet<ExternArgs>>,
    rs_file: &DirEntry,
    sql_func_arg: Option<String>,
    schema: &str,
    _schema_stack: &Vec<String>, // don't use this, but seems to be a good thing to keep around for the future
    funcargs: &Vec<FunctionArgs>,
) -> (Option<String>, Option<String>, Option<Vec<String>>) {
    let exported_func_name = format!("{}_wrapper", func.sig.ident.to_string());

    let has_option_arg = func_args_have_option(func, rs_file);
    let attributes = collect_attributes(rs_file, &func.sig.ident, &func.attrs);
    let mut sql_func_name =
        extract_funcname_attribute(&attributes).unwrap_or_else(|| quote_ident(&func.sig.ident));
    let mut sql_argument_type_names = Vec::new();

    let mut statement = String::new();

    if let Some(sql_func_arg) = sql_func_arg {
        statement.push_str(sql_func_arg.as_str());
    } else {
        // function arguments
        statement.push('(');
        let mut had_none = false;
        let mut i = 0;
        for arg in &func.sig.inputs {
            match arg {
                FnArg::Receiver(_) => {
                    exit_with_error!("functions that take 'self' are not supported")
                }
                FnArg::Typed(ty) => match translate_type(rs_file, &ty.ty) {
                    Some((type_name, _, default_value, variadic)) => {
                        sql_argument_type_names.push(type_name.to_string());

                        if i > 0 {
                            statement.push_str(", ");
                        }

                        statement.push_str(&arg_name(arg));
                        statement.push(' ');
                        if variadic {
                            statement.push_str("VARIADIC ");
                        }
                        statement.push_str(&type_name);

                        if let Some(mut default_value) = default_value {
                            default_value = default_value.trim_start_matches('"').to_string();
                            default_value = default_value.trim_end_matches('"').to_string();
                            default_value = default_value.trim().to_string();
                            default_value = default_value.trim_start_matches('\'').to_string();
                            default_value = default_value.trim_end_matches('\'').to_string();

                            // quote default values, except those that match certain patterns
                            if !default_value.eq_ignore_ascii_case("NULL")
                                && !default_value.to_uppercase().starts_with("ARRAY")
                                && !default_value.contains("(")
                            {
                                default_value = format!("'{}'", default_value);
                            }

                            statement.push_str(&format!(" DEFAULT {}", default_value));
                        }

                        i += 1;
                    }
                    None => had_none = true,
                },
            }
        }

        if had_none && i == 0 {
            let span = &func.span();
            eprintln!(
                "{}:{}:{}: Could not generate function for {} at  -- it contains only pg_sys::FunctionCallData as its only argument",
                rs_file.path().display(),
                span.start().line,
                span.start().column,
                quote_ident(&func.sig.ident),
            );
            return (None, None, None);
        }
        statement.push(')');
    }

    if !has_option_arg {
        // there were no Option<T> arguments, so the function can be declared STRICT
        if let Some(extern_args) = extern_args.borrow_mut() {
            extern_args.insert(ExternArgs::Strict);
        }
    }

    // append RETURNS clause
    match match &func.sig.output {
        ReturnType::Default => Some(("void".to_string(), false, None, false)),
        ReturnType::Type(_, ty) => translate_type(rs_file, ty),
    } {
        Some((return_type, _is_option, _, _)) => {
            statement.push_str(&format!(" RETURNS {}", return_type))
        }
        None => exit_with_error!(
            "could not determine return type for function: {}",
            func.sig.ident
        ),
    }

    let mut custom_schema = None;
    // modifiers
    if let Some(extern_args) = extern_args {
        for extern_arg in extern_args {
            match extern_arg {
                ExternArgs::Immutable => statement.push_str(" IMMUTABLE"),
                ExternArgs::Strict => statement.push_str(" STRICT"),
                ExternArgs::Stable => statement.push_str(" STABLE"),
                ExternArgs::Volatile => statement.push_str(" VOLATILE"),
                ExternArgs::Raw => { /* noop */ }
                ExternArgs::ParallelSafe => statement.push_str(" PARALLEL SAFE"),
                ExternArgs::ParallelUnsafe => statement.push_str(" PARALLEL UNSAFE"),
                ExternArgs::ParallelRestricted => statement.push_str(" PARALLEL RESTRICTED"),
                ExternArgs::Error(_) => { /* noop */ }
                ExternArgs::NoGuard => {}
                ExternArgs::Schema(s) => custom_schema = Some(s),
                ExternArgs::Name(n) => sql_func_name = n,
            }
        }
    }

    statement = format!(
        "CREATE OR REPLACE FUNCTION {}{}",
        qualify_name(
            custom_schema.as_ref().map(|s| &**s).unwrap_or(schema),
            &sql_func_name
        ),
        statement
    );

    let mut search_path = String::new();
    for arg in funcargs {
        match arg {
            FunctionArgs::SearchPath(sp) => search_path.push_str(&sp),
        }
    }

    if !search_path.is_empty() {
        statement.push_str(&format!(" SET search_path TO {}", search_path));
    }

    statement.push_str(&format!(
        " LANGUAGE c AS 'MODULE_PATHNAME', '{}';",
        exported_func_name
    ));

    (
        Some(statement),
        Some(sql_func_name),
        Some(sql_argument_type_names),
    )
}

fn func_args_have_option(func: &ItemFn, rs_file: &DirEntry) -> bool {
    for arg in &func.sig.inputs {
        if let FnArg::Typed(ty) = arg {
            if let Some((_, is_option, _, _)) = translate_type(rs_file, &ty.ty) {
                if is_option {
                    return true;
                }
            }
        }
    }

    false
}

fn quote_ident(ident: &Ident) -> String {
    quote_ident_string(ident.to_string())
}

fn quote_ident_string(ident: String) -> String {
    let mut quoted = String::new();

    quoted.push('"');
    quoted.push_str(ident.replace("\"", "\"\"").as_str());
    quoted.push('"');

    quoted
}

fn arg_name(arg: &FnArg) -> String {
    if let FnArg::Typed(ty) = arg {
        if let Pat::Ident(ident) = ty.pat.deref() {
            return quote_ident(&ident.ident);
        }

        exit_with_error!("Can't figure out argument name");
    }

    exit_with_error!("functions that take 'self' are not supported")
}

fn translate_type(filename: &DirEntry, ty: &Type) -> Option<(String, bool, Option<String>, bool)> {
    let rust_type;
    let span;
    let mut subtype = None;
    let mut default_value = None;
    let mut variadic = false;

    match ty.deref() {
        Type::Path(path) => match categorize_type(ty) {
            CategorizedType::OptionalIterator(types) => {
                rust_type = "Iterator".to_string();
                span = path.span();
                subtype = Some(types);
            }
            CategorizedType::Default => {
                rust_type = format!("{}", quote! {#path});
                span = path.span();
            }
            _ => exit_with_error!("found unexpected path type: {:?}", ty),
        },
        Type::Reference(tref) => {
            let elem = &tref.elem;
            rust_type = format!("{}", quote! {&#elem});
            span = tref.span();
        }
        Type::Macro(makro) => {
            let as_string = format!("{}", quote!(#ty));

            match deconstruct_macro(&as_string) {
                Some((rt, dv, v)) => {
                    rust_type = rt;
                    default_value = dv;
                    variadic = v;
                    span = makro.span();
                }
                None => exit_with_error!("unrecognized macro in argument list: {}", as_string),
            }
        }

        // for the "unit" type:  ()
        Type::Tuple(tuple) if tuple.elems.is_empty() => {
            rust_type = format!("{}", quote!(#tuple));
            span = tuple.span();
        }

        Type::ImplTrait(_) | Type::Tuple(_) => match categorize_type(ty) {
            CategorizedType::Default => {
                exit_with_error!("{:?} isn't an 'impl Trait' type or a Rust Tuple", ty)
            }
            CategorizedType::Iterator(types)
            | CategorizedType::OptionalIterator(types)
            | CategorizedType::Tuple(types) => {
                rust_type = "Iterator".to_string();
                span = ty.span();
                subtype = Some(types);
            }
        },

        other => exit_with_error!("Unsupported type: {:?}", other),
    }

    translate_type_string(
        rust_type,
        filename,
        &span,
        0,
        default_value,
        variadic,
        subtype,
    )
}

fn deconstruct_macro(as_string: &str) -> Option<(String, Option<String>, bool)> {
    if as_string.starts_with("default !") {
        let regexp = regex::Regex::new(r#"default ! \((?P<type>.*?)\s*, (?P<value>.*)\)"#).unwrap();

        let default_value =
            Some(get_named_capture(&regexp, "value", as_string).expect("no default value"));

        let rust_type =
            get_named_capture(&regexp, "type", as_string).expect("no type name in default ");
        Some((rust_type, default_value, false))
    } else if as_string.starts_with("variadic !") {
        let regexp = regex::Regex::new(r#"variadic ! \((?P<type>.*?)\)"#).unwrap();

        let rust_type =
            get_named_capture(&regexp, "type", as_string).expect("no type name in default ");
        Some((rust_type, None, true))
    } else {
        None
    }
}

fn translate_type_string(
    rust_type: String,
    filename: &DirEntry,
    span: &proc_macro2::Span,
    depth: i32,
    mut default_value: Option<String>,
    mut variadic: bool,
    subtypes: Option<Vec<String>>,
) -> Option<(String, bool, Option<String>, bool)> {
    match rust_type.as_str() {
        "()" => Some(("void".to_string(), false, default_value, variadic)),
        "i8" => Some(("\"char\"".to_string(), false, default_value, variadic)),
        "i16" => Some(("smallint".to_string(), false, default_value, variadic)),
        "i32" => Some(("integer".to_string(), false, default_value, variadic)),
        "i64" => Some(("bigint".to_string(), false, default_value, variadic)),
        "bool" => Some(("bool".to_string(), false, default_value, variadic)),
        "char" => Some(("varchar".to_string(), false, default_value, variadic)), // a Rust "char" is 4 bytes, so we need to represent that as a varchar
        "f32" => Some(("real".to_string(), false, default_value, variadic)),
        "f64" => Some((
            "double precision".to_string(),
            false,
            default_value,
            variadic,
        )),
        "PgRelation" => Some(("regclass".to_string(), false, default_value, variadic)),
        "Numeric" => Some(("numeric".to_string(), false, default_value, variadic)),
        "Inet" => Some(("inet".to_string(), false, default_value, variadic)),
        "Json" => Some(("json".to_string(), false, default_value, variadic)),
        "Jsonb" => Some(("jsonb".to_string(), false, default_value, variadic)),
        "& str" | "& 'static str" | "&'static str" | "String" | "& 'static String" | "& String" => {
            Some(("text".to_string(), false, default_value, variadic))
        }
        "& [u8]" | "&[u8]" | "& 'static [u8]" | "&'static [u8]" | "Vec < u8 >" => {
            Some(("bytea".to_string(), false, default_value, variadic))
        }
        "& std :: ffi :: CStr" => Some(("cstring".to_string(), false, default_value, variadic)),
        "AnyElement" => Some(("anyelement".to_string(), false, default_value, variadic)),
        "AnyArray" => Some(("anyarray".to_string(), false, default_value, variadic)),
        "time :: Time" | "Time" => Some(("time".to_string(), false, default_value, variadic)),
        "pgx :: TimeWithTimeZone" | "TimeWithTimeZone" => Some((
            "time with time zone".to_string(),
            false,
            default_value,
            variadic,
        )),
        "time :: Date" | "Date" => Some(("date".to_string(), false, default_value, variadic)),
        "time :: PrimitiveDateTime" | "PrimitiveDateTime" | "pgx :: Timestamp" | "Timestamp" => {
            Some((
                "timestamp without time zone".to_string(),
                false,
                default_value,
                variadic,
            ))
        }
        "time :: OffsetDateTime"
        | "OffsetDateTime"
        | "pgx :: TimestampWithTimeZone"
        | "TimestampWithTimeZone" => Some((
            "timestamp with time zone".to_string(),
            false,
            default_value,
            variadic,
        )),
        "pg_sys :: BOX" | "BOX" => Some(("box".to_string(), false, default_value, variadic)),
        "pg_sys :: Point" | "Point" => Some(("point".to_string(), false, default_value, variadic)),
        "pg_sys :: Oid" => Some(("oid".to_string(), false, default_value, variadic)),
        "pg_sys :: ItemPointerData" => Some(("tid".to_string(), false, default_value, variadic)),
        "pg_sys :: FunctionCallInfo" => None,
        "pg_sys :: IndexAmRoutine" => Some((
            "index_am_handler".to_string(),
            false,
            default_value,
            variadic,
        )),
        "Iterator"
            if subtypes.is_some()
                && subtypes.as_ref().expect("no iterator subtypes").len() == 1 =>
        {
            let translated = translate_type_string(
                subtypes
                    .expect("no subtypes")
                    .pop()
                    .expect("failed to pop first subtype"),
                filename,
                span,
                depth + 1,
                default_value.clone(),
                variadic,
                None,
            )
            .unwrap();
            Some((
                format!("SETOF {}", translated.0),
                false,
                default_value,
                variadic,
            ))
        }
        "Iterator" => {
            let mut composite_def = String::new();
            for ty in subtypes
                .expect("no iterator subtypes where len != 1")
                .into_iter()
            {
                let parsed_type_with_name =
                    proc_macro2::TokenStream::from_str(&ty).expect("failed to parse type name");
                let mut iter = parsed_type_with_name.into_iter();
                let (name, ty) = if let Some(tree) = iter.next() {
                    match tree {
                        TokenTree::Ident(_ident) if ty.starts_with("name !") => {
                            let _bang = iter.next();
                            let open_paren = iter.next();
                            match open_paren.unwrap() {
                                TokenTree::Group(group) => {
                                    let mut iter = group.stream().into_iter();

                                    let name = iter.next();
                                    let _comma = iter.next();
                                    let mut ty = proc_macro2::TokenStream::new();
                                    for tree in iter {
                                        match tree {
                                            TokenTree::Punct(punc) => {
                                                if punc.as_char() != ')' {
                                                    ty.extend(quote! {#punc})
                                                }
                                            }
                                            other => ty.extend(quote! {#other}),
                                        }
                                    }

                                    (name.unwrap().to_string(), ty.to_string())
                                }
                                _ => exit_with_error!("malformed name!() macro"),
                            }
                        }
                        _ => exit_with_error!(
                            "No name!() macro specified for tuple member of type: {} at {}",
                            ty,
                            location_comment(filename, span)
                        ),
                    }
                } else {
                    exit_with_error!("malformed name!() macro")
                };

                let translated = translate_type_string(
                    ty,
                    filename,
                    span,
                    depth + 1,
                    default_value.clone(),
                    variadic,
                    None,
                )
                .unwrap();

                if !composite_def.is_empty() {
                    composite_def.push_str(", ");
                }
                if !name.starts_with('"') {
                    composite_def.push('"');
                }
                composite_def.push_str(&name);
                if !name.ends_with('"') {
                    composite_def.push('"');
                }

                composite_def.push(' ');
                composite_def.push_str(&translated.0);
            }

            Some((
                format!("TABLE ({})", composite_def),
                false,
                default_value,
                variadic,
            ))
        }
        _array if rust_type.starts_with("Array <") | rust_type.starts_with("Vec <") => {
            let rc = translate_type_string(
                extract_type(&rust_type),
                filename,
                span,
                depth + 1,
                default_value.clone(),
                variadic,
                subtypes,
            );
            let mut type_string = rc.unwrap().0;
            type_string.push_str("[]");
            Some((type_string, false, default_value, variadic))
        }
        _array if rust_type.starts_with("VariadicArray <") => {
            let rc = translate_type_string(
                extract_type(&rust_type),
                filename,
                span,
                depth + 1,
                default_value.clone(),
                true,
                subtypes,
            );
            let mut type_string = rc.unwrap().0;
            type_string.push_str("[]");
            Some((type_string, false, default_value, true))
        }
        _internal if rust_type.starts_with("Internal <") => {
            Some(("internal".to_string(), false, default_value, variadic))
        }
        _boxed if rust_type.starts_with("PgBox <") => translate_type_string(
            extract_type(&rust_type),
            filename,
            span,
            depth + 1,
            default_value,
            variadic,
            subtypes,
        ),
        _option if rust_type.starts_with("Option <") => {
            let mut extraced_type = extract_type(&rust_type);
            if let Some((rt, dv, v)) = deconstruct_macro(&extraced_type) {
                extraced_type = rt;
                default_value = dv;
                variadic = v;
            }

            let rc = translate_type_string(
                extraced_type,
                filename,
                span,
                depth + 1,
                default_value.clone(),
                variadic,
                subtypes,
            );
            let type_string = rc.unwrap().0;
            Some((type_string, true, default_value, variadic))
        }
        _pgvarlena
            if rust_type.starts_with("PgVarlena <")
                || rust_type.starts_with("pgx :: PgVarlena <") =>
        {
            let mut extraced_type = extract_type(&rust_type);
            if let Some((rt, dv, v)) = deconstruct_macro(&extraced_type) {
                extraced_type = rt;
                default_value = dv;
                variadic = v;
            }

            let rc = translate_type_string(
                extraced_type,
                filename,
                span,
                depth + 1,
                default_value.clone(),
                variadic,
                subtypes,
            );
            let type_string = rc.unwrap().0;
            Some((type_string, true, default_value, variadic))
        }
        mut unknown => {
            if std::env::var("DEBUG").is_ok() {
                eprintln!(
                    "{}:{}:{}: Unrecognized type: {}",
                    filename.path().display(),
                    span.start().line,
                    span.start().column,
                    unknown,
                );
            }

            unknown = unknown.trim_start_matches("&");
            unknown = unknown.trim_start_matches("pg_sys :: ");
            let unknown = match unknown.find('<') {
                Some(idx) => unknown[0..idx].trim(),
                None => unknown,
            };
            let parts: Vec<_> = unknown.split("::").collect();
            if parts.len() == 1 {
                return Some((unknown.trim().to_string(), false, default_value, variadic));
            }

            let (schema, name) = (parts[parts.len() - 2], parts[parts.len() - 1]);
            let qualified_name = format!("{}.{}", schema.trim(), name.trim());
            return Some((qualified_name.to_string(), false, default_value, variadic));
        }
    }
}

fn extract_type(type_name: &str) -> String {
    let re = regex::Regex::new(r#"\w+ <(.*)>.*"#).unwrap();
    let capture = re
        .captures(type_name)
        .unwrap_or_else(|| exit_with_error!("no type capture against: {}", type_name))
        .get(1);
    capture.unwrap().as_str().to_string().trim().to_string()
}

fn extract_funcargs_attribute(attrs: &[CategorizedAttribute]) -> Option<String> {
    for a in attrs {
        if let CategorizedAttribute::SqlFunctionArgs(sql) = a {
            return Some(sql.clone());
        }
    }

    None
}

fn extract_funcname_attribute(attrs: &[CategorizedAttribute]) -> Option<String> {
    for a in attrs {
        if let CategorizedAttribute::SqlFunctionName(sql) = a {
            return Some(quote_ident_string(sql.clone()));
        }
    }

    None
}

fn collect_attributes(
    rs_file: &DirEntry,
    ident: &Ident,
    attrs: &[Attribute],
) -> Vec<CategorizedAttribute> {
    let mut categorized_attributes = Vec::new();
    let mut other_attributes = Vec::new();
    let mut operator = None;
    let mut function = None;

    let mut i = 0;
    while i < attrs.len() {
        let a = attrs.get(i).unwrap();
        let span = a.span();
        let as_string = quote! {#a}.to_string();
        if as_string == "# [pg_guard]" {
            categorized_attributes.push(CategorizedAttribute::PgGuard(span));
        } else if as_string.starts_with("# [pg_extern") {
            function = Some(CategorizedAttribute::PgExtern(
                span,
                parse_extern_args(&a),
                Vec::new(),
            ));
        } else if as_string.starts_with("# [pg_operator") {
            operator = Some(CategorizedAttribute::PgOperator(
                span,
                parse_extern_args(&a),
                Vec::new(),
                Vec::new(),
            ));
        } else if as_string.starts_with("# [opname") {
            if let Some(CategorizedAttribute::PgOperator(_, _, _, options)) = operator.as_mut() {
                options.push(OperatorOptions::Name(extract_single_arg(a.tokens.clone())));
            }
        } else if as_string.starts_with("# [commutator") {
            if let Some(CategorizedAttribute::PgOperator(_, _, _, options)) = operator.as_mut() {
                options.push(OperatorOptions::Commutator(extract_single_arg(
                    a.tokens.clone(),
                )));
            }
        } else if as_string.starts_with("# [negator") {
            if let Some(CategorizedAttribute::PgOperator(_, _, _, options)) = operator.as_mut() {
                options.push(OperatorOptions::Negator(extract_single_arg(
                    a.tokens.clone(),
                )));
            }
        } else if as_string.starts_with("# [restrict") {
            if let Some(CategorizedAttribute::PgOperator(_, _, _, options)) = operator.as_mut() {
                options.push(OperatorOptions::Restrict(extract_single_arg(
                    a.tokens.clone(),
                )));
            }
        } else if as_string.starts_with("# [join") {
            if let Some(CategorizedAttribute::PgOperator(_, _, _, options)) = operator.as_mut() {
                options.push(OperatorOptions::Join(extract_single_arg(a.tokens.clone())));
            }
        } else if as_string.eq("# [hashes]") {
            if let Some(CategorizedAttribute::PgOperator(_, _, _, options)) = operator.as_mut() {
                options.push(OperatorOptions::Hashes);
            }
        } else if as_string.eq("# [merges]") {
            if let Some(CategorizedAttribute::PgOperator(_, _, _, options)) = operator.as_mut() {
                options.push(OperatorOptions::Merges);
            }
        } else if as_string.starts_with("# [search_path") {
            if let Some(CategorizedAttribute::PgExtern(_, _, funcargs)) = function.as_mut() {
                funcargs.push(FunctionArgs::SearchPath(extract_single_arg(
                    a.tokens.clone(),
                )));
            }

            if let Some(CategorizedAttribute::PgTest(_, _, funcargs)) = function.as_mut() {
                funcargs.push(FunctionArgs::SearchPath(extract_single_arg(
                    a.tokens.clone(),
                )));
            }

            if let Some(CategorizedAttribute::PgOperator(_, _, funcargs, _)) = operator.as_mut() {
                funcargs.push(FunctionArgs::SearchPath(extract_single_arg(
                    a.tokens.clone(),
                )));
            }
        } else if as_string.starts_with("# [pg_test") {
            function = Some(CategorizedAttribute::PgTest(
                span,
                parse_extern_args(&a),
                Vec::new(),
            ));
        } else if as_string == "# [test]" {
            categorized_attributes.push(CategorizedAttribute::RustTest(span));
        } else if as_string == "# [doc = \" ```funcname\"]" {
            let (new_i, mut sql_statements) = collect_doc(
                rs_file,
                ident,
                attrs,
                &mut other_attributes,
                i,
                a,
                span,
                false,
            );

            if sql_statements.len() == 1 {
                categorized_attributes.push(CategorizedAttribute::SqlFunctionName(
                    sql_statements.pop().unwrap(),
                ));
            } else if sql_statements.is_empty() {
                exit_with_error!("Found no lines for ```funcname");
            } else {
                exit_with_error!("Found more than 1 line for ```funcname");
            }

            i = new_i;
        } else if as_string == "# [doc = \" ```funcargs\"]" {
            let (new_i, mut sql_statements) = collect_doc(
                rs_file,
                ident,
                attrs,
                &mut other_attributes,
                i,
                a,
                span,
                false,
            );

            if sql_statements.len() == 1 {
                categorized_attributes.push(CategorizedAttribute::SqlFunctionArgs(
                    sql_statements.pop().unwrap(),
                ));
            } else if sql_statements.is_empty() {
                exit_with_error!("Found no lines for ```funcargs");
            } else {
                exit_with_error!("Found more than 1 line for ```funcargs");
            }

            i = new_i;
        } else if as_string == "# [doc = \" ```sql\"]" {
            let (new_i, sql_statements) = collect_doc(
                rs_file,
                ident,
                attrs,
                &mut other_attributes,
                i,
                a,
                span,
                true,
            );

            if !sql_statements.is_empty() {
                categorized_attributes.push(CategorizedAttribute::Sql(sql_statements));
            }

            i = new_i;
        } else {
            other_attributes.push((span, as_string));
        }

        i += 1;
    }

    if let Some(operator) = operator {
        categorized_attributes.push(operator);
    }

    if let Some(function) = function {
        categorized_attributes.push(function);
    }

    if !other_attributes.is_empty() {
        categorized_attributes.push(CategorizedAttribute::Other(other_attributes));
    }

    categorized_attributes
}

fn extract_single_arg(attr: proc_macro2::TokenStream) -> String {
    let mut itr = attr.into_iter();
    let mut arg = String::new();
    while let Some(t) = itr.next() {
        match t {
            token => arg.push_str(&token.to_string()),
        }
    }
    let mut arg: String = arg.trim_start_matches('(').trim_end_matches(')').into();
    arg.retain(|c| !c.is_whitespace());
    arg
}

#[allow(clippy::too_many_arguments)]
fn collect_doc(
    rs_file: &DirEntry,
    ident: &Ident,
    attrs: &[Attribute],
    other_attributes: &mut Vec<(Span, String)>,
    mut i: usize,
    a: &Attribute,
    span: Span,
    track_location: bool,
) -> (usize, Vec<String>) {
    let mut sql_statements = Vec::new();

    // run forward saving each line as an sql statement until we find ```

    if track_location {
        sql_statements.push(location_comment(rs_file, &a.span()));
    }

    i += 1;
    while i < attrs.len() {
        let a = attrs.get(i).unwrap();
        let as_string = quote! {#a}.to_string();

        if as_string == "# [doc = \" ```\"]" {
            // we found the end to this ```sql block of documentation
            break;
        } else if as_string.starts_with("# [doc = \"") {
            // it's a doc line within the sql block
            let as_string = as_string.trim_start_matches("# [doc = \"");
            let as_string = as_string.trim_end_matches("\"]");
            let as_string = as_string.trim();
            let as_string = unescape::unescape(as_string)
                .unwrap_or_else(|| exit_with_error!("Improperly escaped:\n{}", as_string));

            // do variable substitution in the sql statement
            let as_string = as_string.replace("@FUNCTION_NAME@", &format!("{}_wrapper", ident));

            // and remember it, along with its original source location
            sql_statements.push(as_string);
        } else {
            // it's not a doc line, so add it to other_attributes and get out
            other_attributes.push((span, as_string));
            break;
        }

        i += 1;
    }
    (i, sql_statements)
}

fn location_comment(rs_file: &DirEntry, span: &Span) -> String {
    format!(
        "-- {}:{}:{}",
        rs_file.path().display(),
        span.start().line,
        span.start().column,
    )
}

fn parse_item(stream: proc_macro2::TokenStream) -> Item {
    handle_result!(
        syn::parse_str(&stream.to_string()),
        format!("failed to parse Item from:\n{}", stream.to_string())
    )
}
