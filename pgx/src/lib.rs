// Copyright 2020 ZomboDB, LLC <zombodb@gmail.com>. All rights reserved. Use of this source code is
// governed by the MIT license that can be found in the LICENSE file.

//! `pgx` is a framework for creating Postgres extensions in 100% Rust
//!
//! ## Example
//!
//! ```rust,no_run
//! use pgx::*;
//!
//! pg_module_magic!();
//!
//! // Convert the input string to lowercase and return
//! #[pg_extern]
//! fn my_to_lowercase(input: &'static str) -> String {
//!     input.to_lowercase()
//! }
//!
//! ```
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::cast_ptr_alignment)]
extern crate pgx_macros;

extern crate num_traits;

#[macro_use]
extern crate bitflags;

// expose our various derive macros
pub use pgx_macros::*;

pub mod callbacks;
pub mod datum;
pub mod enum_helper;
pub mod fcinfo;
pub mod guc;
pub mod hooks;
pub mod htup;
pub mod inoutfuncs;
pub mod itemptr;
pub mod list;
#[macro_use]
pub mod log;
pub mod atomics;
pub mod bgworkers;
pub mod lwlock;
pub mod memcxt;
pub mod misc;
pub mod namespace;
pub mod nodes;
pub mod pgbox;
pub mod rel;
pub mod shmem;
pub mod spi;
pub mod stringinfo;
pub mod trigger_support;
pub mod tupdesc;
pub mod varlena;
pub mod wrappers;
pub mod xid;

#[doc(hidden)]
pub use inventory;
#[doc(hidden)]
pub use once_cell;

pub use atomics::*;
pub use callbacks::*;
pub use datum::*;
pub use enum_helper::*;
pub use fcinfo::*;
pub use guc::*;
pub use hooks::*;
pub use htup::*;
pub use inoutfuncs::*;
pub use itemptr::*;
pub use list::*;
pub use log::*;
pub use lwlock::*;
pub use memcxt::*;
pub use namespace::*;
pub use nodes::*;
pub use pgbox::*;
pub use rel::*;
pub use shmem::*;
pub use spi::*;
pub use stringinfo::*;
pub use trigger_support::*;
pub use tupdesc::*;
pub use varlena::*;
pub use wrappers::*;
pub use xid::*;

pub use pgx_pg_sys as pg_sys; // the module only, not its contents
pub use pgx_pg_sys::submodules::*;
pub use pgx_pg_sys::PgBuiltInOids; // reexport this so it looks like it comes from here

use core::any::TypeId;
use once_cell::sync::Lazy;
use std::collections::HashMap;

/// Top-level initialization function.  This is called automatically by the `pg_module_magic!()`
/// macro and need not be called directly
#[allow(unused)]
pub fn initialize() {
    register_pg_guard_panic_handler();
}

pub fn map_unsized_type<T: 'static + ?Sized>(map: &mut HashMap<TypeId, String>, sql: &str) {
    let single_sql = sql.to_string();

    map.insert(*<T as WithTypeIds>::ITEM_ID, single_sql.clone());
}

pub fn map_type<T: 'static>(map: &mut HashMap<TypeId, String>, sql: &str) {
    let single_sql = sql.to_string();
    let set_sql = format!("{}[]", single_sql);

    map.insert(*<T as WithTypeIds>::ITEM_ID, single_sql.clone());

    if let Some(id) = *WithSizedTypeIds::<T>::OPTION_ID {
        map.insert(id, single_sql.clone());
    }
    if let Some(id) = *WithSizedTypeIds::<T>::VEC_ID {
        map.insert(id, set_sql.clone());
    }
    if let Some(id) = *WithSizedTypeIds::<T>::VEC_OPTION_ID {
        map.insert(id, set_sql.clone());
    }
    
    if let Some(id) = *WithArrayTypeIds::<T>::ARRAY_ID {
        map.insert(id, set_sql.clone());
    }
    if let Some(id) = *WithArrayTypeIds::<T>::OPTION_ARRAY_ID {
        map.insert(id, set_sql.clone());
    }

    if let Some(id) = *WithArrayTypeIds::<T>::VARLENA_ID {
        map.insert(id, set_sql.clone());
    }
}

pub static DEFAULT_TYPEID_SQL_MAPPING: Lazy<HashMap<TypeId, String>> = Lazy::new(|| {
    let mut m = HashMap::new();

    map_unsized_type::<str>(&mut m, "text");
    map_type::<&str>(&mut m, "text");
    map_type::<String>(&mut m, "text");
    map_type::<&std::ffi::CStr>(&mut m, "cstring");
    map_type::<()>(&mut m, "void");
    map_type::<i8>(&mut m, "\"char\"");
    map_type::<i16>(&mut m, "smallint");
    map_type::<i32>(&mut m, "integer");
    map_type::<i64>(&mut m, "bigint");
    map_type::<bool>(&mut m, "bool");
    map_type::<char>(&mut m, "varchar");
    map_type::<f32>(&mut m, "real");
    map_type::<f64>(&mut m, "double precision");
    map_type::<datum::JsonB>(&mut m, "jsonb");
    map_type::<datum::Json>(&mut m, "json");
    map_type::<pgx_pg_sys::ItemPointerData>(&mut m, "tid");
    map_type::<pgx_pg_sys::Point>(&mut m, "point");
    map_type::<pgx_pg_sys::BOX>(&mut m, "box");
    map_type::<Date>(&mut m, "date");
    map_type::<Time>(&mut m, "time");
    map_type::<Timestamp>(&mut m, "timestamp");
    map_type::<TimeWithTimeZone>(&mut m, "time with time zone");
    map_type::<pgx_pg_sys::PlannerInfo>(&mut m, "internal");
    map_type::<datum::Numeric>(&mut m, "numeric");
    map_type::<pg_sys::Oid>(&mut m, "oid");
    map_type::<datum::AnyElement>(&mut m, "anyelement");
    map_type::<datum::Inet>(&mut m, "inet");

    // Bytea is a special case...
    m.insert(TypeId::of::<&[u8]>(), String::from("bytea"));
    m.insert(TypeId::of::<Option<&[u8]>>(), String::from("bytea"));
    m.insert(TypeId::of::<Vec<u8>>(), String::from("bytea"));
    m.insert(TypeId::of::<Option<Vec<u8>>>(), String::from("bytea"));

    m
});

/// A macro for marking a library compatible with the Postgres extension framework.
///
/// This macro was initially inspired from the `pg_module` macro in https://github.com/thehydroimpulse/postgres-extension.rs
///
/// Shamelessly cribbed from https://github.com/bluejekyll/pg-extend-rs
#[macro_export]
macro_rules! pg_module_magic {
    () => {
        #[no_mangle]
        #[allow(non_snake_case)]
        #[allow(unused)]
        #[link_name = "Pg_magic_func"]
        pub extern "C" fn Pg_magic_func() -> &'static pgx::pg_sys::Pg_magic_struct {
            use pgx;
            use std::mem::size_of;
            use std::os::raw::c_int;

            #[cfg(not(feature = "pg13"))]
            const MY_MAGIC: pgx::pg_sys::Pg_magic_struct = pgx::pg_sys::Pg_magic_struct {
                len: size_of::<pgx::pg_sys::Pg_magic_struct>() as c_int,
                version: pgx::pg_sys::PG_VERSION_NUM as std::os::raw::c_int / 100,
                funcmaxargs: pgx::pg_sys::FUNC_MAX_ARGS as c_int,
                indexmaxkeys: pgx::pg_sys::INDEX_MAX_KEYS as c_int,
                namedatalen: pgx::pg_sys::NAMEDATALEN as c_int,
                float4byval: pgx::pg_sys::USE_FLOAT4_BYVAL as c_int,
                float8byval: pgx::pg_sys::USE_FLOAT8_BYVAL as c_int,
            };

            #[cfg(feature = "pg13")]
            const MY_MAGIC: pgx::pg_sys::Pg_magic_struct = pgx::pg_sys::Pg_magic_struct {
                len: size_of::<pgx::pg_sys::Pg_magic_struct>() as c_int,
                version: pgx::pg_sys::PG_VERSION_NUM as std::os::raw::c_int / 100,
                funcmaxargs: pgx::pg_sys::FUNC_MAX_ARGS as c_int,
                indexmaxkeys: pgx::pg_sys::INDEX_MAX_KEYS as c_int,
                namedatalen: pgx::pg_sys::NAMEDATALEN as c_int,
                float8byval: pgx::pg_sys::USE_FLOAT8_BYVAL as c_int,
            };

            // go ahead and register our panic handler since Postgres
            // calls this function first
            pgx::initialize();

            // return the magic
            &MY_MAGIC
        }

        pub use __pgx_internals::generate_sql;
        mod __pgx_internals {
            use ::core::{any::TypeId, convert::TryFrom};
            use ::pgx::datum::{Array, FromDatum, PgVarlena};
            use ::pgx_utils::pg_inventory::{once_cell::sync::Lazy, *};
            use ::std::collections::HashMap;

            static CONTROL_FILE: &str = include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/",
                env!("CARGO_CRATE_NAME"),
                ".control"
            ));

            #[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
            pub struct ExtensionSql(pub pgx_utils::pg_inventory::InventoryExtensionSql);
            inventory::collect!(ExtensionSql);

            #[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
            pub struct PostgresType(pub pgx_utils::pg_inventory::InventoryPostgresType);
            inventory::collect!(PostgresType);

            #[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
            pub struct PgExtern(pub pgx_utils::pg_inventory::InventoryPgExtern);
            inventory::collect!(PgExtern);

            #[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
            pub struct PostgresEnum(pub pgx_utils::pg_inventory::InventoryPostgresEnum);
            inventory::collect!(PostgresEnum);

            #[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
            pub struct PostgresHash(pub pgx_utils::pg_inventory::InventoryPostgresHash);
            inventory::collect!(PostgresHash);

            #[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
            pub struct PostgresOrd(pub pgx_utils::pg_inventory::InventoryPostgresOrd);
            inventory::collect!(PostgresOrd);

            #[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
            pub struct Schema(pub pgx_utils::pg_inventory::InventorySchema);
            inventory::collect!(Schema);

            pub fn generate_sql<'a>() -> pgx_utils::pg_inventory::eyre::Result<PgxSql<'a>> {
                use std::fmt::Write;
                let mut generated = PgxSql::build(
                    ControlFile::try_from(CONTROL_FILE)?,
                    (*pgx::DEFAULT_TYPEID_SQL_MAPPING)
                        .iter()
                        .map(|(x, y)| (x.clone(), y.clone())),
                    {
                        let mut set = inventory::iter::<Schema>().collect::<Vec<_>>();
                        set.sort();
                        set.into_iter().map(|x| &x.0)
                    },
                    {
                        let mut set = inventory::iter::<ExtensionSql>().collect::<Vec<_>>();
                        set.sort();
                        set.into_iter().map(|x| &x.0)
                    },
                    {
                        let mut set = inventory::iter::<PgExtern>().collect::<Vec<_>>();
                        set.sort();
                        set.into_iter().map(|x| &x.0)
                    },
                    {
                        let mut set = inventory::iter::<PostgresType>().collect::<Vec<_>>();
                        set.sort();
                        set.into_iter().map(|x| &x.0)
                    },
                    {
                        let mut set = inventory::iter::<PostgresEnum>().collect::<Vec<_>>();
                        set.sort();
                        set.into_iter().map(|x| &x.0)
                    },
                    {
                        let mut set = inventory::iter::<PostgresOrd>().collect::<Vec<_>>();
                        set.sort();
                        set.into_iter().map(|x| &x.0)
                    },
                    {
                        let mut set = inventory::iter::<PostgresHash>().collect::<Vec<_>>();
                        set.sort();
                        set.into_iter().map(|x| &x.0)
                    },
                )?;

                Ok(generated)
            }
        }
    };
}

#[macro_export]
macro_rules! pg_binary_magic {
    ($($prelude:ident)::*) => {
        fn main() -> ::pgx_utils::pg_inventory::color_eyre::Result<()> {
            use ::pgx_utils::pg_inventory::{
                tracing_error::ErrorLayer,
                tracing,
                tracing_subscriber::{self, util::SubscriberInitExt, layer::SubscriberExt, EnvFilter},
                color_eyre,
                eyre,
            };
            use std::env;
            use $($prelude :: )*generate_sql;

            // Initialize tracing with tracing-error.
            let fmt_layer = tracing_subscriber::fmt::layer().with_target(false);
            let filter_layer = EnvFilter::try_from_default_env()
                .or_else(|_| EnvFilter::try_new("info"))
                .unwrap();
            tracing_subscriber::registry()
                .with(filter_layer)
                .with(fmt_layer)
                .with(ErrorLayer::default())
                .init();

            color_eyre::install()?;

            let mut args = env::args().skip(1);
            let path = args.next().unwrap_or(concat!("./sql/", stringify!($($prelude :: )*), ".sql").into());
            let dot: Option<String> = args.next();
            if args.next().is_some() {
                return Err(eyre::eyre!("Only accepts two arguments, the destination path, and an optional (GraphViz) dot output path."));
            }

            tracing::info!(path = %path, "Writing SQL.");
            let sql = generate_sql()?;
            sql.to_file(path)?;
            if let Some(dot) = dot {
                sql.to_dot(dot)?;
            }
            Ok(())
        }
    };
}
