// Copyright 2020 ZomboDB, LLC <zombodb@gmail.com>. All rights reserved. Use of this source code is
// governed by the MIT license that can be found in the LICENSE file.

extern crate build_deps;

use bindgen::callbacks::MacroParsingBehavior;
use pgx_utils::pg_config::{PgConfig, PgConfigSelector, Pgx};
use pgx_utils::{exit_with_error, handle_result, prefix_path};
use quote::quote;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::PathBuf;
use std::process::{Command, Output};
use syn::Item;

#[derive(Debug)]
struct IgnoredMacros(HashSet<String>);

impl IgnoredMacros {
    fn default() -> Self {
        // these cause duplicate definition problems on linux
        // see: https://github.com/rust-lang/rust-bindgen/issues/687
        IgnoredMacros(
            vec![
                "FP_INFINITE".into(),
                "FP_NAN".into(),
                "FP_NORMAL".into(),
                "FP_SUBNORMAL".into(),
                "FP_ZERO".into(),
                "IPPORT_RESERVED".into(),
            ]
            .into_iter()
            .collect(),
        )
    }
}

impl bindgen::callbacks::ParseCallbacks for IgnoredMacros {
    fn will_parse_macro(&self, name: &str) -> MacroParsingBehavior {
        if self.0.contains(name) {
            bindgen::callbacks::MacroParsingBehavior::Ignore
        } else {
            bindgen::callbacks::MacroParsingBehavior::Default
        }
    }
}

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    if std::env::var("DOCS_RS").unwrap_or("false".into()) == "1" {
        return Ok(());
    }

    // dump the environment for debugging if asked
    if std::env::var("PGX_BUILD_VERBOSE").unwrap_or("false".to_string()) == "true" {
        for (k, v) in std::env::vars() {
            eprintln!("{}={}", k, v);
        }
    }

    println!("cargo:rerun-if-env-changed=PGX_PG_SYS_SKIP_BINDING_REWRITE");

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut src_dir = manifest_dir.clone();
    src_dir.push("src");
    let mut shim_src = manifest_dir.clone();
    shim_src.push("cshim");
    let mut shim_dst = out_dir.clone();
    shim_dst.push("cshim");

    eprintln!("manifest_dir={}", manifest_dir.display());
    eprintln!("shim_src={}", shim_src.display());
    eprintln!("shim_dst={}", shim_dst.display());

    let pgx = Pgx::from_config()?;

    build_deps::rerun_if_changed_paths(&Pgx::config_toml()?.display().to_string()).unwrap();
    build_deps::rerun_if_changed_paths("include/*").unwrap();
    build_deps::rerun_if_changed_paths("cshim/pgx-cshim.c").unwrap();
    build_deps::rerun_if_changed_paths("cshim/Makefile").unwrap();

    let pg_configs = pgx
        .iter(PgConfigSelector::All)
        .map(|v| v.expect("invalid pg_config"))
        .collect::<Vec<_>>();
    pg_configs.par_iter().for_each(|pg_config| {
        let major_version = handle_result!(
            pg_config.major_version(),
            "could not determine major version"
        );
        let mut include_h = manifest_dir.clone();
        include_h.push("include");
        include_h.push(format!("pg{}.h", major_version));

        let bindgen_output = handle_result!(
            run_bindgen(&pg_config, &include_h),
            format!("bindgen failed for pg{}", major_version)
        );

        let rewritten_items = handle_result!(
            rewrite_items(&bindgen_output),
            format!("failed to rewrite items for pg{}", major_version)
        );

        let oids = handle_result!(
            extract_oids(&bindgen_output),
            format!("unable to generate oids for pg{}", major_version)
        );

        let bindings_files =
            if std::env::var("PGX_PG_SYS_SKIP_BINDING_REWRITE").unwrap_or("false".into()) != "1" {
                vec![out_dir.clone(), src_dir.clone()]
            } else {
                vec![out_dir.clone()]
            };
        for mut bindings_file in bindings_files {
            bindings_file.push(&format!("pg{}.rs", major_version));
            handle_result!(
                write_rs_file(
                    rewritten_items.clone(),
                    &bindings_file,
                    quote! {
                        use crate as pg_sys;
                        use pgx_macros::*;
                        use crate::PgNode;
                    }
                ),
                format!(
                    "Unable to write bindings file for pg{} to `{}`",
                    major_version,
                    bindings_file.display()
                )
            );

            let mut oids_file = out_dir.clone();
            oids_file.push(&format!("pg{}_oids.rs", major_version));
            handle_result!(
                write_rs_file(oids.clone(), &oids_file, quote! {}),
                format!(
                    "Unable to write oids file for pg{} to `{}`",
                    major_version,
                    oids_file.display()
                )
            );
        }
    });

    // compile the cshim for each binding
    for pg_config in pg_configs {
        build_shim(&shim_src, &shim_dst, &pg_config)?;
    }

    Ok(())
}

fn write_rs_file(
    code: proc_macro2::TokenStream,
    file: &PathBuf,
    header: proc_macro2::TokenStream,
) -> Result<(), std::io::Error> {
    let contents = quote! {
        #header
        #code
    };

    std::fs::write(&file, contents.to_string())?;
    rust_fmt(&file)
}

/// Given a token stream representing a file, apply a series of transformations to munge
/// the bindgen generated code with some postgres specific enhancements
fn rewrite_items(
    file: &syn::File,
) -> Result<proc_macro2::TokenStream, Box<dyn Error + Send + Sync>> {
    let items = apply_pg_guard(&file.items)?;
    let pgnode_impls = impl_pg_node(&items)?;

    let mut stream = proc_macro2::TokenStream::new();
    for item in items.into_iter().chain(pgnode_impls.into_iter()) {
        stream.extend(quote! { #item });
    }

    Ok(stream)
}

/// Find all the constants that represent Postgres type OID values.
///
/// These are constants of type `u32` whose name ends in the string "OID"
fn extract_oids(code: &syn::File) -> Result<proc_macro2::TokenStream, Box<dyn Error>> {
    let mut enum_variants = proc_macro2::TokenStream::new();
    let mut from_impl = proc_macro2::TokenStream::new();
    for item in &code.items {
        match item {
            Item::Const(c) => {
                let ident = &c.ident;
                let name = ident.to_string();
                let ty = &c.ty;
                let ty = quote! {#ty}.to_string();

                if ty == "u32" && name.ends_with("OID") && name != "HEAP_HASOID" {
                    enum_variants.extend(quote! {#ident = crate::#ident as isize, });
                    from_impl.extend(quote! {crate::#ident => Some(crate::PgBuiltInOids::#ident), })
                }
            }
            _ => {}
        }
    }

    Ok(quote! {
        #[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
        pub enum PgBuiltInOids {
            #enum_variants
        }

        impl PgBuiltInOids {
            pub fn from(oid: crate::Oid) -> Option<PgBuiltInOids> {
                match oid {
                    #from_impl
                    _ => None,
                }
            }
        }
    })
}

/// Implement our `PgNode` marker trait for `pg_sys::Node` and its "subclasses"
fn impl_pg_node(items: &Vec<syn::Item>) -> Result<Vec<syn::Item>, Box<dyn Error + Send + Sync>> {
    let mut pgnode_impls = Vec::new();

    // we scope must of the computation so we can borrow `items` and then
    // extend it at the very end.
    let struct_graph: StructGraph = StructGraph::from(&items[..]);

    // collect all the structs with `NodeTag` as their first member,
    // these will serve as roots in our forest of `Node`s
    let mut root_node_structs = Vec::new();
    for descriptor in struct_graph.descriptors.iter() {
        // grab the first field, if any
        let first_field = match &descriptor.struct_.fields {
            syn::Fields::Named(fields) => {
                if let Some(first_field) = fields.named.first() {
                    first_field
                } else {
                    continue;
                }
            }
            syn::Fields::Unnamed(fields) => {
                if let Some(first_field) = fields.unnamed.first() {
                    first_field
                } else {
                    continue;
                }
            }
            _ => continue,
        };

        // grab the type name of the first field
        let ty_name = if let syn::Type::Path(p) = &first_field.ty {
            if let Some(last_segment) = p.path.segments.last() {
                last_segment.ident.to_string()
            } else {
                continue;
            }
        } else {
            continue;
        };

        if ty_name == "NodeTag" {
            root_node_structs.push(descriptor);
        }
    }

    // the set of types which subclass `Node` according to postgres' object system
    let mut node_set = HashSet::new();
    // fill in any children of the roots with a recursive DFS
    // (we are not operating on user input, so it is ok to just
    //  use direct recursion rather than an explicit stack).
    for root in root_node_structs.into_iter() {
        dfs_find_nodes(root, &struct_graph, &mut node_set);
    }

    // now we can finally iterate the Nodes and emit out Display impl
    for node_struct in node_set.into_iter() {
        let struct_name = &node_struct.struct_.ident;

        // impl the PgNode trait for all nodes
        pgnode_impls.push((
            struct_name.to_string(),
            syn::parse2(quote! {
                impl pg_sys::PgNode for #struct_name {
                    type NodeType = #struct_name;
                }
            })?,
        ));

        // impl Rust's Display trait for all nodes
        pgnode_impls.push((
            struct_name.to_string(),
            syn::parse2(quote! {
                impl std::fmt::Display for #struct_name {
                    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(f, "{}", crate::node_to_string_for_display(self.as_node_ptr() as *mut crate::Node))
                    }
                }
            })?,
        ));
    }

    // sort the pgnode impls by their struct name so that we have a consistent ordering in our bindings output
    pgnode_impls.sort_by(|(a_name, _), (b_name, _)| a_name.cmp(b_name));

    // pluck out the syn::Item field and return as a Vec
    Ok(pgnode_impls.into_iter().map(|(_, item)| item).collect())
}

/// Given a root node, dfs_find_nodes adds all its children nodes to `node_set`.
fn dfs_find_nodes<'graph>(
    node: &'graph StructDescriptor<'graph>,
    graph: &'graph StructGraph<'graph>,
    node_set: &mut HashSet<StructDescriptor<'graph>>,
) {
    node_set.insert(node.clone());

    for child in node.children(graph) {
        if node_set.contains(child) {
            continue;
        }
        dfs_find_nodes(child, graph, node_set);
    }
}

/// A graph describing the inheritance relationships between different nodes
/// according to postgres' object system.
///
/// NOTE: the borrowed lifetime on a StructGraph should also ensure that the offsets
///       it stores into the underlying items struct are always correct.
#[derive(Clone, Debug)]
struct StructGraph<'a> {
    /// A table mapping struct names to their offset in the descriptor table
    name_tab: HashMap<String, usize>,
    /// A table mapping offsets into the underlying items table to offsets in the descriptor table
    item_offset_tab: Vec<Option<usize>>,
    /// A table of struct descriptors
    descriptors: Vec<StructDescriptor<'a>>,
}

impl<'a> From<&'a [syn::Item]> for StructGraph<'a> {
    fn from(items: &'a [syn::Item]) -> StructGraph<'a> {
        let mut descriptors = Vec::new();

        // a table mapping struct names to their offset in `descriptors`
        let mut name_tab: HashMap<String, usize> = HashMap::new();
        let mut item_offset_tab: Vec<Option<usize>> = vec![None; items.len()];
        for (i, item) in items.iter().enumerate() {
            if let &syn::Item::Struct(struct_) = &item {
                let next_offset = descriptors.len();
                descriptors.push(StructDescriptor {
                    struct_,
                    items_offset: i,
                    parent: None,
                    children: Vec::new(),
                });
                name_tab.insert(struct_.ident.to_string(), next_offset);
                item_offset_tab[i] = Some(next_offset);
            }
        }

        for item in items.iter() {
            // grab the first field if it is struct
            let (id, first_field) = match &item {
                &syn::Item::Struct(syn::ItemStruct {
                    ident: id,
                    fields: syn::Fields::Named(fields),
                    ..
                }) => {
                    if let Some(first_field) = fields.named.first() {
                        (id.to_string(), first_field)
                    } else {
                        continue;
                    }
                }
                &syn::Item::Struct(syn::ItemStruct {
                    ident: id,
                    fields: syn::Fields::Unnamed(fields),
                    ..
                }) => {
                    if let Some(first_field) = fields.unnamed.first() {
                        (id.to_string(), first_field)
                    } else {
                        continue;
                    }
                }
                _ => continue,
            };

            if let syn::Type::Path(p) = &first_field.ty {
                // We should be guaranteed that just extracting the last path
                // segment is ok because these structs are all from the same module.
                // (also, they are all generated from C code, so collisions should be
                //  impossible anyway thanks to C's single shared namespace).
                if let Some(last_segment) = p.path.segments.last() {
                    if let Some(parent_offset) = name_tab.get(&last_segment.ident.to_string()) {
                        // establish the 2-way link
                        let child_offset = name_tab[&id];
                        descriptors[child_offset].parent = Some(*parent_offset);
                        descriptors[*parent_offset].children.push(child_offset);
                    }
                }
            }
        }

        StructGraph {
            name_tab,
            item_offset_tab,
            descriptors,
        }
    }
}

impl<'a> StructDescriptor<'a> {
    /// children returns an iterator over the children of this node in the graph
    fn children(&'a self, graph: &'a StructGraph) -> StructDescriptorChildren {
        StructDescriptorChildren {
            offset: 0,
            descriptor: self,
            graph,
        }
    }
}

/// An iterator over a StructDescriptor's children
struct StructDescriptorChildren<'a> {
    offset: usize,
    descriptor: &'a StructDescriptor<'a>,
    graph: &'a StructGraph<'a>,
}

impl<'a> std::iter::Iterator for StructDescriptorChildren<'a> {
    type Item = &'a StructDescriptor<'a>;
    fn next(&mut self) -> Option<&'a StructDescriptor<'a>> {
        if self.offset >= self.descriptor.children.len() {
            None
        } else {
            let ret = Some(&self.graph.descriptors[self.descriptor.children[self.offset]]);
            self.offset += 1;
            ret
        }
    }
}

/// A node a StructGraph
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct StructDescriptor<'a> {
    /// A reference to the underlying struct syntax node
    struct_: &'a syn::ItemStruct,
    /// An offset into the items slice that was used to construct the struct graph that
    /// this StructDescriptor is a part of
    items_offset: usize,
    /// The offset of the "parent" (first member) struct (if any).
    parent: Option<usize>,
    /// The offsets of the "children" structs (if any).
    children: Vec<usize>,
}

/// Given a specific postgres version, `run_bindgen` generates bindings for the given
/// postgres version and returns them as a token stream.
fn run_bindgen(
    pg_config: &PgConfig,
    include_h: &PathBuf,
) -> Result<syn::File, Box<dyn Error + Send + Sync>> {
    let major_version = pg_config.major_version()?;
    eprintln!("Generating bindings for pg{}", major_version);
    let includedir_server = pg_config.includedir_server()?;
    let bindings = bindgen::Builder::default()
        .header(include_h.display().to_string())
        .clang_arg(&format!("-I{}", includedir_server.display()))
        .parse_callbacks(Box::new(IgnoredMacros::default()))
        .blocklist_function("varsize_any") // pgx converts the VARSIZE_ANY macro, so we don't want to also have this function, which is in heaptuple.c
        .blocklist_function("query_tree_walker")
        .blocklist_function("expression_tree_walker")
        .blocklist_function("sigsetjmp")
        .blocklist_function("siglongjmp")
        .blocklist_function("pg_re_throw")
        .size_t_is_usize(true)
        .rustfmt_bindings(false)
        .derive_debug(true)
        .derive_copy(true) // necessary to avoid __BindgenUnionField usages -- I don't understand why?
        .derive_default(true)
        .derive_eq(false)
        .derive_partialeq(false)
        .derive_hash(false)
        .derive_ord(false)
        .derive_partialord(false)
        .layout_tests(false)
        .generate()
        .unwrap_or_else(|e| {
            panic!(
                "Unable to generate bindings for pg{}: {:?}",
                major_version, e
            )
        });

    syn::parse_file(bindings.to_string().as_str()).map_err(|e| From::from(e))
}

fn build_shim(
    shim_src: &PathBuf,
    shim_dst: &PathBuf,
    pg_config: &PgConfig,
) -> Result<(), std::io::Error> {
    let major_version = pg_config.major_version()?;
    let mut libpgx_cshim: PathBuf = shim_dst.clone();

    libpgx_cshim.push(format!("libpgx-cshim-{}.a", major_version));

    eprintln!("libpgx_cshim={}", libpgx_cshim.display());
    // then build the shim for the version feature currently being built
    build_shim_for_version(&shim_src, &shim_dst, pg_config)?;

    // no matter what, tell rustc to link to the library that was built for the feature we're currently building
    let envvar_name = format!("CARGO_FEATURE_PG{}", major_version);
    if std::env::var(envvar_name).is_ok() {
        println!("cargo:rustc-link-search={}", shim_dst.display());
        println!("cargo:rustc-link-lib=static=pgx-cshim-{}", major_version);
    }

    Ok(())
}

fn build_shim_for_version(
    shim_src: &PathBuf,
    shim_dst: &PathBuf,
    pg_config: &PgConfig,
) -> Result<(), std::io::Error> {
    let path_env = prefix_path(pg_config.parent_path());
    let major_version = pg_config.major_version()?;

    eprintln!("PATH for build_shim={}", path_env);
    eprintln!("shim_src={}", shim_src.display());
    eprintln!("shim_dst={}", shim_dst.display());

    std::fs::create_dir_all(shim_dst).unwrap();

    if !std::path::Path::new(&format!("{}/Makefile", shim_dst.display())).exists() {
        std::fs::copy(
            format!("{}/Makefile", shim_src.display()),
            format!("{}/Makefile", shim_dst.display()),
        )
        .unwrap();
    }

    if !std::path::Path::new(&format!("{}/pgx-cshim.c", shim_dst.display())).exists() {
        std::fs::copy(
            format!("{}/pgx-cshim.c", shim_src.display()),
            format!("{}/pgx-cshim.c", shim_dst.display()),
        )
        .unwrap();
    }

    let rc = run_command(
        Command::new("make")
            .arg("clean")
            .arg(&format!("libpgx-cshim-{}.a", major_version))
            .env("PG_TARGET_VERSION", format!("{}", major_version))
            .env("PATH", path_env)
            .current_dir(shim_dst),
        &format!("shim for PG v{}", major_version),
    )?;

    if rc.status.code().unwrap() != 0 {
        panic!("failed to make pgx-cshim for v{}", major_version);
    }

    Ok(())
}

fn run_command(mut command: &mut Command, version: &str) -> Result<Output, std::io::Error> {
    let mut dbg = String::new();

    command = command
        .env_remove("DEBUG")
        .env_remove("MAKEFLAGS")
        .env_remove("MAKELEVEL")
        .env_remove("MFLAGS")
        .env_remove("DYLD_FALLBACK_LIBRARY_PATH")
        .env_remove("OPT_LEVEL")
        .env_remove("TARGET")
        .env_remove("PROFILE")
        .env_remove("OUT_DIR")
        .env_remove("HOST")
        .env_remove("NUM_JOBS");

    eprintln!("[{}] {:?}", version, command);
    dbg.push_str(&format!("[{}] -------- {:?} -------- \n", version, command));

    let output = command.output()?;
    let rc = output.clone();

    if !output.stdout.is_empty() {
        for line in String::from_utf8(output.stdout).unwrap().lines() {
            if line.starts_with("cargo:") {
                dbg.push_str(&format!("{}\n", line));
            } else {
                dbg.push_str(&format!("[{}] [stdout] {}\n", version, line));
            }
        }
    }

    if !output.stderr.is_empty() {
        for line in String::from_utf8(output.stderr).unwrap().lines() {
            dbg.push_str(&format!("[{}] [stderr] {}\n", version, line));
        }
    }
    dbg.push_str(&format!(
        "[{}] /----------------------------------------\n",
        version
    ));

    eprintln!("{}", dbg);
    Ok(rc)
}

fn apply_pg_guard(items: &Vec<syn::Item>) -> Result<Vec<syn::Item>, Box<dyn Error + Send + Sync>> {
    let mut out = Vec::with_capacity(items.len());
    for item in items.into_iter() {
        match item {
            Item::ForeignMod(block) => {
                out.push(syn::parse2(quote! {
                    #[pg_guard]
                    #block
                })?);
            }
            _ => {
                out.push(item.clone());
            }
        }
    }

    Ok(out)
}

fn rust_fmt(path: &PathBuf) -> Result<(), std::io::Error> {
    run_command(
        Command::new("rustfmt").arg(path).current_dir("."),
        "[bindings_diff]",
    )?;

    Ok(())
}
