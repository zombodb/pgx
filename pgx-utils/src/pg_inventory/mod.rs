mod control_file;
mod extension_sql;
mod pg_extern;
mod pg_schema;
mod postgres_enum;
mod postgres_hash;
mod postgres_ord;
mod postgres_type;

pub use control_file::{ControlFile, ControlFileError};
pub use extension_sql::{
    ExtensionSql, ExtensionSqlFile, InventoryExtensionSql, InventoryExtensionSqlPositioningRef,
};
pub use pg_extern::{
    InventoryPgExtern, InventoryPgExternInput, InventoryPgExternReturn, InventoryPgOperator,
    PgExtern,
};
pub use pg_schema::{InventorySchema, Schema};
pub use postgres_enum::{InventoryPostgresEnum, PostgresEnum};
pub use postgres_hash::{InventoryPostgresHash, PostgresHash};
pub use postgres_ord::{InventoryPostgresOrd, PostgresOrd};
pub use postgres_type::{InventoryPostgresType, PostgresType};

// Reexports for the pgx extension inventory builders.
#[doc(hidden)]
pub use color_eyre;
#[doc(hidden)]
pub use eyre;
#[doc(hidden)]
pub use inventory;
#[doc(hidden)]
pub use once_cell;
#[doc(hidden)]
pub use tracing;
#[doc(hidden)]
pub use tracing_error;
#[doc(hidden)]
pub use tracing_subscriber;

use crate::ExternArgs;
use core::{any::TypeId, fmt::Debug};
use eyre::eyre as eyre_err;
use petgraph::{
    algo::toposort,
    dot::Dot,
    stable_graph::{NodeIndex, StableGraph},
};
use std::collections::HashMap;
use tracing::instrument;

#[derive(Debug, Clone)]
pub struct PgxSql<'a> {
    pub type_mappings: HashMap<TypeId, String>,
    pub control: ControlFile,
    pub graph: StableGraph<SqlGraphEntity<'a>, SqlGraphRelationship>,
    pub graph_root: NodeIndex,
    pub graph_bootstrap: Option<NodeIndex>,
    pub graph_finalize: Option<NodeIndex>,
    pub schemas: HashMap<&'a InventorySchema, NodeIndex>,
    pub extension_sqls: HashMap<&'a InventoryExtensionSql, NodeIndex>,
    pub externs: HashMap<&'a InventoryPgExtern, NodeIndex>,
    pub types: HashMap<&'a InventoryPostgresType, NodeIndex>,
    pub builtin_types: HashMap<&'a str, NodeIndex>,
    pub enums: HashMap<&'a InventoryPostgresEnum, NodeIndex>,
    pub ords: HashMap<&'a InventoryPostgresOrd, NodeIndex>,
    pub hashes: HashMap<&'a InventoryPostgresHash, NodeIndex>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum SqlGraphEntity<'a> {
    ExtensionRoot(ControlFile),
    Schema(&'a InventorySchema),
    CustomSql(&'a InventoryExtensionSql),
    Function(&'a InventoryPgExtern),
    Type(&'a InventoryPostgresType),
    BuiltinType(&'a str),
    Enum(&'a InventoryPostgresEnum),
    Ord(&'a InventoryPostgresOrd),
    Hash(&'a InventoryPostgresHash),
}
use SqlGraphEntity::*;

impl<'a> From<&'a InventorySchema> for SqlGraphEntity<'a> {
    fn from(item: &'a InventorySchema) -> Self {
        SqlGraphEntity::Schema(&item)
    }
}

impl<'a> From<&'a InventoryExtensionSql> for SqlGraphEntity<'a> {
    fn from(item: &'a InventoryExtensionSql) -> Self {
        SqlGraphEntity::CustomSql(&item)
    }
}

impl<'a> From<&'a InventoryPgExtern> for SqlGraphEntity<'a> {
    fn from(item: &'a InventoryPgExtern) -> Self {
        SqlGraphEntity::Function(&item)
    }
}

impl<'a> From<&'a InventoryPostgresType> for SqlGraphEntity<'a> {
    fn from(item: &'a InventoryPostgresType) -> Self {
        SqlGraphEntity::Type(&item)
    }
}

impl<'a> From<&'a InventoryPostgresEnum> for SqlGraphEntity<'a> {
    fn from(item: &'a InventoryPostgresEnum) -> Self {
        SqlGraphEntity::Enum(&item)
    }
}

impl<'a> From<&'a InventoryPostgresOrd> for SqlGraphEntity<'a> {
    fn from(item: &'a InventoryPostgresOrd) -> Self {
        SqlGraphEntity::Ord(&item)
    }
}

impl<'a> From<&'a InventoryPostgresHash> for SqlGraphEntity<'a> {
    fn from(item: &'a InventoryPostgresHash) -> Self {
        SqlGraphEntity::Hash(&item)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Eq, Ord)]
pub enum SqlGraphRelationship {
    RequiredBy,
    RequiredByArg,
    RequiredByReturn,
}

impl<'a> SqlGraphEntity<'a> {
    fn dot_format(&self) -> String {
        match self {
            Schema(item) => format!("mod {}", item.module_path.to_string()),
            CustomSql(item) => format!("sql {}", item.name.unwrap_or(item.full_path).to_string()),
            Function(item) => format!("fn {}", item.full_path.to_string(),),
            Type(item) => format!("type {}", item.full_path.to_string()),
            BuiltinType(item) => format!("internal type {}", item),
            Enum(item) => format!("enum {}", item.full_path.to_string()),
            Ord(item) => format!("ord {}", item.full_path.to_string()),
            Hash(item) => format!("hash {}", item.full_path.to_string()),
            ExtensionRoot(_control) => format!("ExtensionRoot"),
        }
    }
}

impl<'a> PgxSql<'a> {
    #[instrument(
        level = "debug",
        skip(
            control,
            type_mappings,
            schemas,
            extension_sqls,
            externs,
            types,
            enums,
            ords,
            hashes
        )
    )]
    pub fn build(
        control: ControlFile,
        type_mappings: impl Iterator<Item = (TypeId, String)>,
        schemas: impl Iterator<Item = &'a InventorySchema>,
        extension_sqls: impl Iterator<Item = &'a InventoryExtensionSql>,
        externs: impl Iterator<Item = &'a InventoryPgExtern>,
        types: impl Iterator<Item = &'a InventoryPostgresType>,
        enums: impl Iterator<Item = &'a InventoryPostgresEnum>,
        ords: impl Iterator<Item = &'a InventoryPostgresOrd>,
        hashes: impl Iterator<Item = &'a InventoryPostgresHash>,
    ) -> eyre::Result<Self> {
        let mut graph = StableGraph::new();

        let root = graph.add_node(SqlGraphEntity::ExtensionRoot(control.clone()));
        let mut bootstrap = None;
        let mut finalize = None;

        // The initial build phase.
        //
        // Notably, we do not set non-root edges here. We do that in a second step. This is
        // primarily because externs, types, operators, and the like tend to intertwine. If we tried
        // to do it here, we'd find ourselves trying to create edges to non-existing entities.
        let mut mapped_extension_sqls = HashMap::default();
        for item in extension_sqls {
            let index = graph.add_node(item.into());
            mapped_extension_sqls.insert(item, index);
            if item.bootstrap {
                if bootstrap.is_some() {
                    return Err(eyre_err!("Cannot have multiple `extension_sql!()` with `bootstrap` positioning."))
                }
                bootstrap = Some(index)
            }
            if item.finalize {
                if finalize.is_some() {
                    return Err(eyre_err!("Cannot have multiple `extension_sql!()` with `finalize` positioning."))
                }
                finalize = Some(index)
            }
        }
        for (item, index) in &mapped_extension_sqls {
            graph.add_edge(root, *index, SqlGraphRelationship::RequiredBy);
            if !item.bootstrap {
                if let Some(bootstrap) = bootstrap {
                    graph.add_edge(bootstrap, *index, SqlGraphRelationship::RequiredBy);
                }
            }
            if !item.finalize {
                if let Some(finalize) = finalize {
                    graph.add_edge(*index, finalize, SqlGraphRelationship::RequiredBy);
                }
            }
        }
        let mut mapped_schemas = HashMap::default();
        for item in schemas {
            let index = graph.add_node(item.into());
            mapped_schemas.insert(item, index);
            if let Some(bootstrap) = bootstrap {
                graph.add_edge(bootstrap, index, SqlGraphRelationship::RequiredBy);
            }
            if let Some(finalize) = finalize {
                graph.add_edge(index, finalize, SqlGraphRelationship::RequiredBy);
            }
        }
        let mut mapped_enums = HashMap::default();
        for item in enums {
            let index = graph.add_node(item.into());
            mapped_enums.insert(item, index);
            graph.add_edge(root, index, SqlGraphRelationship::RequiredBy);
            if let Some(bootstrap) = bootstrap {
                graph.add_edge(bootstrap, index, SqlGraphRelationship::RequiredBy);
            }
            if let Some(finalize) = finalize {
                graph.add_edge(index, finalize, SqlGraphRelationship::RequiredBy);
            }
        }
        let mut mapped_types = HashMap::default();
        for item in types {
            let index = graph.add_node(item.into());
            mapped_types.insert(item, index);
            graph.add_edge(root, index, SqlGraphRelationship::RequiredBy);
            if let Some(bootstrap) = bootstrap {
                graph.add_edge(bootstrap, index, SqlGraphRelationship::RequiredBy);
            }
            if let Some(finalize) = finalize {
                graph.add_edge(index, finalize, SqlGraphRelationship::RequiredBy);
            }
        }
        let mut mapped_externs = HashMap::default();
        let mut mapped_builtin_types = HashMap::default();
        for item in externs {
            let index = graph.add_node(item.into());
            graph.add_edge(root, index, SqlGraphRelationship::RequiredBy);
            if let Some(bootstrap) = bootstrap {
                graph.add_edge(bootstrap, index, SqlGraphRelationship::RequiredBy);
            }
            if let Some(finalize) = finalize {
                graph.add_edge(index, finalize, SqlGraphRelationship::RequiredBy);
            }

            mapped_externs.insert(item, index);

            for arg in &item.fn_args {
                let mut found = false;
                for (ty_item, &_ty_index) in &mapped_types {
                    if ty_item.id_matches(&arg.ty_id) {
                        found = true;
                        break;
                    }
                }
                for (ty_item, &_ty_index) in &mapped_enums {
                    if ty_item.id == arg.ty_id {
                        found = true;
                        break;
                    }
                }
                if !found {
                    mapped_builtin_types
                        .entry(arg.full_path)
                        .or_insert_with(|| {
                            graph.add_node(SqlGraphEntity::BuiltinType(arg.full_path))
                        });
                }
            }

            match &item.fn_return {
                InventoryPgExternReturn::None | InventoryPgExternReturn::Trigger => (),
                InventoryPgExternReturn::Type { id, full_path, .. }
                | InventoryPgExternReturn::SetOf { id, full_path, .. } => {
                    let mut found = false;
                    for (ty_item, &_ty_index) in &mapped_types {
                        if ty_item.id_matches(id) {
                            found = true;
                            break;
                        }
                    }
                    for (ty_item, &_ty_index) in &mapped_enums {
                        if ty_item.id == *id {
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        mapped_builtin_types.entry(full_path).or_insert_with(|| {
                            graph.add_node(SqlGraphEntity::BuiltinType(full_path))
                        });
                    }
                }
                InventoryPgExternReturn::Iterated(iterated_returns) => {
                    for iterated_return in iterated_returns {
                        let mut found = false;
                        for (ty_item, &_ty_index) in &mapped_types {
                            if ty_item.id_matches(&iterated_return.0) {
                                found = true;
                                break;
                            }
                        }
                        for (ty_item, &_ty_index) in &mapped_enums {
                            if ty_item.id == iterated_return.0 {
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            mapped_builtin_types
                                .entry(iterated_return.1)
                                .or_insert_with(|| {
                                    graph.add_node(SqlGraphEntity::BuiltinType(iterated_return.1))
                                });
                        }
                    }
                }
            }
        }
        let mut mapped_ords = HashMap::default();
        for item in ords {
            let index = graph.add_node(item.into());
            mapped_ords.insert(item, index);
            graph.add_edge(root, index, SqlGraphRelationship::RequiredBy);
            if let Some(bootstrap) = bootstrap {
                graph.add_edge(bootstrap, index, SqlGraphRelationship::RequiredBy);
            }
            if let Some(finalize) = finalize {
                graph.add_edge(index, finalize, SqlGraphRelationship::RequiredBy);
            }
        }
        let mut mapped_hashes = HashMap::default();
        for item in hashes {
            let index = graph.add_node(item.into());
            mapped_hashes.insert(item, index);
                        graph.add_edge(root, index, SqlGraphRelationship::RequiredBy);
            if let Some(bootstrap) = bootstrap {
                graph.add_edge(bootstrap, index, SqlGraphRelationship::RequiredBy);
            }
            if let Some(finalize) = finalize {
                graph.add_edge(index, finalize, SqlGraphRelationship::RequiredBy);
            }
        }

        let mut this = Self {
            type_mappings: type_mappings.collect(),
            control,
            schemas: mapped_schemas,
            extension_sqls: mapped_extension_sqls,
            externs: mapped_externs,
            types: mapped_types,
            builtin_types: mapped_builtin_types,
            enums: mapped_enums,
            ords: mapped_ords,
            hashes: mapped_hashes,
            graph: graph,
            graph_root: root,
            graph_bootstrap: bootstrap,
            graph_finalize: finalize,
        };

        // Now we can circle back and build up the edge sets.
        for (_item, &index) in &this.schemas {
            this.graph
                .add_edge(root, index, SqlGraphRelationship::RequiredBy);
        }
        for (item, &index) in &this.extension_sqls {
            for (schema_item, &schema_index) in &this.schemas {
                if item.module_path.starts_with(schema_item.module_path) {
                    tracing::trace!(from = ?item.identifier(), to = schema_item.module_path, "Adding ExtensionSQL after Schema edge.");
                    this.graph
                        .add_edge(schema_index, index, SqlGraphRelationship::RequiredBy);
                    break;
                }
            }
            for after in &item.after {
                match after {
                    InventoryExtensionSqlPositioningRef::FullPath(path) => {
                        for (other, other_index) in &this.types {
                            if other.full_path.ends_with(*path) {
                                tracing::trace!(from = ?item.identifier(), to = ?other.full_path, "Adding ExtensionSQL after Type edge.");
                                this.graph.add_edge(
                                    *other_index,
                                    index,
                                    SqlGraphRelationship::RequiredBy,
                                );
                                break;
                            }
                        }
                        for (other, other_index) in &this.enums {
                            if other.full_path.ends_with(*path) {
                                tracing::trace!(from = ?item.identifier(), to = ?other.full_path, "Adding ExtensionSQL after Enum edge.");
                                this.graph.add_edge(
                                    *other_index,
                                    index,
                                    SqlGraphRelationship::RequiredBy,
                                );
                                break;
                            }
                        }
                        for (other, other_index) in &this.externs {
                            if other.full_path.ends_with(*path) {
                                tracing::trace!(from = ?item.identifier(), to = ?other.full_path, "Adding ExtensionSQL after Extern edge.");
                                this.graph.add_edge(
                                    *other_index,
                                    index,
                                    SqlGraphRelationship::RequiredBy,
                                );
                                break;
                            }
                        }
                    }
                    InventoryExtensionSqlPositioningRef::Name(name) => {
                        for (other, other_index) in &this.extension_sqls {
                            if other.name == Some(name) {
                                tracing::trace!(from = ?item.identifier(), to = ?other.identifier(), "Adding ExtensionSQL after ExtensionSql edge.");
                                this.graph.add_edge(
                                    *other_index,
                                    index,
                                    SqlGraphRelationship::RequiredBy,
                                );
                                break;
                            }
                        }
                    }
                }
            }
            for before in &item.before {
                match before {
                    InventoryExtensionSqlPositioningRef::FullPath(path) => {
                        for (other, other_index) in &this.types {
                            if other.full_path == *path {
                                tracing::trace!(from = ?item.full_path, to = ?other.full_path, "Adding ExtensionSQL after Type edge.");
                                this.graph.add_edge(
                                    index,
                                    *other_index,
                                    SqlGraphRelationship::RequiredBy,
                                );
                                break;
                            }
                        }
                        for (other, other_index) in &this.enums {
                            if other.full_path == *path {
                                tracing::trace!(from = ?item.full_path, to = ?other.full_path, "Adding ExtensionSQL after Enum edge.");
                                this.graph.add_edge(
                                    index,
                                    *other_index,
                                    SqlGraphRelationship::RequiredBy,
                                );
                                break;
                            }
                        }
                        for (other, other_index) in &this.externs {
                            if other.full_path == *path {
                                tracing::trace!(from = ?item.full_path, to = ?other.full_path, "Adding ExtensionSQL after Extern edge.");
                                this.graph.add_edge(
                                    index,
                                    *other_index,
                                    SqlGraphRelationship::RequiredBy,
                                );
                                break;
                            }
                        }
                    }
                    InventoryExtensionSqlPositioningRef::Name(name) => {
                        for (other, other_index) in &this.extension_sqls {
                            if other.name == Some(name) {
                                tracing::trace!(from = ?item.full_path, to = ?other.full_path, "Adding ExtensionSQL after ExtensionSql edge.");
                                this.graph.add_edge(
                                    index,
                                    *other_index,
                                    SqlGraphRelationship::RequiredBy,
                                );
                                break;
                            }
                        }
                    }
                }
            }
        }
        for (item, &index) in &this.enums {
            for (schema_item, &schema_index) in &this.schemas {
                if item.module_path.starts_with(schema_item.module_path) {
                    tracing::trace!(from = ?item.full_path, to = schema_item.module_path, "Adding Enum after Schema edge.");
                    this.graph
                        .add_edge(schema_index, index, SqlGraphRelationship::RequiredBy);
                    break;
                }
            }
        }
        for (item, &index) in &this.types {
            for (schema_item, &schema_index) in &this.schemas {
                if item.module_path.starts_with(schema_item.module_path) {
                    tracing::trace!(from = ?item.full_path, to = schema_item.module_path, "Adding Type after Schema edge.");
                    this.graph
                        .add_edge(schema_index, index, SqlGraphRelationship::RequiredBy);
                    break;
                }
            }
        }
        for (item, &index) in &this.externs {
            for (schema_item, &schema_index) in &this.schemas {
                if item.module_path.starts_with(schema_item.module_path) {
                    tracing::trace!(from = ?item.full_path, to = schema_item.module_path, "Adding Extern after Schema edge.");
                    this.graph
                        .add_edge(schema_index, index, SqlGraphRelationship::RequiredBy);
                    break;
                }
            }
            for arg in &item.fn_args {
                let mut found = false;
                for (ty_item, &ty_index) in &this.types {
                    if ty_item.id_matches(&arg.ty_id) {
                        tracing::trace!(from = ?item.full_path, to = ty_item.full_path, "Adding Extern after Type (due to argument) edge.");
                        this.graph
                            .add_edge(ty_index, index, SqlGraphRelationship::RequiredByArg);
                        found = true;
                        break;
                    }
                }
                for (ty_item, &ty_index) in &this.enums {
                    if ty_item.id_matches(&arg.ty_id) {
                        tracing::trace!(from = ?item.full_path, to = ty_item.full_path, "Adding Extern after Enum (due to argument) edge.");
                        this.graph
                            .add_edge(ty_index, index, SqlGraphRelationship::RequiredByArg);
                        found = true;
                        break;
                    }
                }
                if !found {
                    let builtin_index = this
                        .builtin_types
                        .get(arg.full_path)
                        .expect(&format!("Could not fetch Builtin Type {}.", arg.full_path));
                    tracing::trace!(from = ?item.full_path, to = arg.full_path, "Adding Extern(arg) after BuiltIn Type (due to argument) edge.");
                    this.graph
                        .add_edge(*builtin_index, index, SqlGraphRelationship::RequiredByArg);
                }
            }
            match &item.fn_return {
                InventoryPgExternReturn::None | InventoryPgExternReturn::Trigger => (),
                InventoryPgExternReturn::Type { id, full_path, .. }
                | InventoryPgExternReturn::SetOf { id, full_path, .. } => {
                    let mut found = false;
                    for (ty_item, &ty_index) in &this.types {
                        if ty_item.id_matches(id) {
                            tracing::trace!(from = ?item.full_path, to = ty_item.full_path, "Adding Extern after Type (due to return) edge.");
                            this.graph.add_edge(
                                ty_index,
                                index,
                                SqlGraphRelationship::RequiredByReturn,
                            );
                            found = true;
                            break;
                        }
                    }
                    for (ty_item, &ty_index) in &this.enums {
                        if ty_item.id_matches(id) {
                            tracing::trace!(from = ?item.full_path, to = ty_item.full_path, "Adding Extern after Enum (due to return) edge.");
                            this.graph.add_edge(
                                ty_index,
                                index,
                                SqlGraphRelationship::RequiredByReturn,
                            );
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        let builtin_index = this
                            .builtin_types
                            .get(full_path)
                            .expect(&format!("Could not fetch Builtin Type {}.", full_path));
                        tracing::trace!(from = ?item.full_path, to = full_path, "Adding Extern(return) after BuiltIn Type (due to return) edge.");
                        this.graph.add_edge(
                            *builtin_index,
                            index,
                            SqlGraphRelationship::RequiredByArg,
                        );
                    }
                }
                InventoryPgExternReturn::Iterated(iterated_returns) => {
                    for iterated_return in iterated_returns {
                        let mut found = false;
                        for (ty_item, &ty_index) in &this.types {
                            if ty_item.id_matches(&iterated_return.0) {
                                tracing::trace!(from = ?item.full_path, to = ty_item.full_path, "Adding Extern after Type (due to return) edge.");
                                this.graph.add_edge(
                                    ty_index,
                                    index,
                                    SqlGraphRelationship::RequiredByReturn,
                                );
                                found = true;
                                break;
                            }
                        }
                        for (ty_item, &ty_index) in &this.enums {
                            if ty_item.id_matches(&iterated_return.0) {
                                tracing::trace!(from = ?item.full_path, to = ty_item.full_path, "Adding Extern after Enum (due to return) edge.");
                                this.graph.add_edge(
                                    ty_index,
                                    index,
                                    SqlGraphRelationship::RequiredByReturn,
                                );
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            let builtin_index = this.builtin_types.get(&iterated_return.1).expect(
                                &format!("Could not fetch Builtin Type {}.", iterated_return.1),
                            );
                            tracing::trace!(from = ?item.full_path, to = iterated_return.1, "Adding Extern after BuiltIn Type (due to return) edge.");
                            this.graph.add_edge(
                                *builtin_index,
                                index,
                                SqlGraphRelationship::RequiredByArg,
                            );
                        }
                    }
                }
            }
        }
        for (item, &index) in &this.ords {
            for (schema_item, &schema_index) in &this.schemas {
                if item.module_path.starts_with(schema_item.module_path) {
                    tracing::trace!(from = ?item.full_path, to = schema_item.module_path, "Adding Ord after Schema edge.");
                    this.graph
                        .add_edge(schema_index, index, SqlGraphRelationship::RequiredBy);
                    break;
                }
            }
        }
        for (item, &index) in &this.hashes {
            for (schema_item, &schema_index) in &this.schemas {
                if item.module_path.starts_with(schema_item.module_path) {
                    tracing::trace!(from = ?item.full_path, to = schema_item.module_path, "Adding Hash after Schema edge.");
                    this.graph
                        .add_edge(schema_index, index, SqlGraphRelationship::RequiredBy);
                    break;
                }
            }
        }

        this.register_types();
        Ok(this)
    }

    #[instrument(level = "info", err, skip(self))]
    pub fn to_file(&self, file: impl AsRef<str> + Debug) -> eyre::Result<()> {
        use std::{
            fs::{create_dir_all, File},
            io::Write,
            path::Path,
        };
        let generated = self.to_sql()?;
        let path = Path::new(file.as_ref());

        let parent = path.parent();
        if let Some(parent) = parent {
            create_dir_all(parent)?;
        }
        let mut out = File::create(path)?;
        write!(out, "{}", generated)?;
        Ok(())
    }

    #[instrument(level = "info", err, skip(self))]
    pub fn to_dot(&self, file: impl AsRef<str> + Debug) -> eyre::Result<()> {
        use std::{
            fs::{create_dir_all, File},
            io::Write,
            path::Path,
        };
        let generated = Dot::with_attr_getters(
            &self.graph,
            &[
                petgraph::dot::Config::EdgeNoLabel,
                petgraph::dot::Config::NodeNoLabel,
            ],
            &|_graph, edge| match edge.weight() {
                SqlGraphRelationship::RequiredBy => format!(r#"color = "gray""#),
                SqlGraphRelationship::RequiredByArg => format!(r#"color = "black""#),
                SqlGraphRelationship::RequiredByReturn => {
                    format!(r#"dir = "back", color = "black""#)
                }
            },
            &|_graph, (_index, node)| {
                match node {
                    // Colors derived from https://www.schemecolor.com/touch-of-creativity.php
                    SqlGraphEntity::Schema(_item) => format!(
                        "label = \"{}\", weight = 6, shape = \"tab\"",
                        node.dot_format()
                    ),
                    SqlGraphEntity::Function(_item) => format!(
                        "label = \"{}\", penwidth = 0, style = \"filled\", fillcolor = \"#ADC7C6\", weight = 4, shape = \"box\"",
                        node.dot_format()
                    ),
                    SqlGraphEntity::Type(_item) => format!(
                        "label = \"{}\", penwidth = 0, style = \"filled\", fillcolor = \"#AE9BBD\", weight = 5, shape = \"oval\"",
                        node.dot_format()
                    ),
                    SqlGraphEntity::BuiltinType(_item) => format!(
                        "label = \"{}\", shape = \"plain\"",
                        node.dot_format()
                    ),
                    SqlGraphEntity::Enum(_item) => format!(
                        "label = \"{}\", penwidth = 0, style = \"filled\", fillcolor = \"#C9A7C8\", weight = 5, shape = \"oval\"",
                        node.dot_format()
                    ),
                    SqlGraphEntity::Ord(_item) => format!(
                        "label = \"{}\", penwidth = 0, style = \"filled\", fillcolor = \"#FFCFD3\", weight = 5, shape = \"diamond\"",
                        node.dot_format()
                    ),
                    SqlGraphEntity::Hash(_item) => format!(
                        "label = \"{}\", penwidth = 0, style = \"filled\", fillcolor = \"#FFE4E0\", weight = 5, shape = \"diamond\"",
                        node.dot_format()
                    ),
                    SqlGraphEntity::CustomSql(_item) => format!(
                        "label = \"{}\", weight = 3, shape = \"signature\"",
                        node.dot_format()
                    ),
                    SqlGraphEntity::ExtensionRoot(_item) => format!(
                        "label = \"{}\", shape = \"cylinder\"",
                        node.dot_format()
                    ),
                }
            },
        );
        let path = Path::new(file.as_ref());

        let parent = path.parent();
        if let Some(parent) = parent {
            create_dir_all(parent)?;
        }
        let mut out = File::create(path)?;
        write!(out, "{:?}", generated)?;
        Ok(())
    }

    pub fn schema_alias_of(&self, item_index: &NodeIndex) -> Option<String> {
        self.graph
            .neighbors_undirected(*item_index)
            .flat_map(|neighbor_index| match &self.graph[neighbor_index] {
                SqlGraphEntity::Schema(s) => Some(String::from(s.name)),
                SqlGraphEntity::ExtensionRoot(control) => {
                    if !control.relocatable {
                        control.schema.clone()
                    } else {
                        Some(String::from("@extname@"))
                    }
                }
                _ => None,
            })
            .next()
    }

    pub fn schema_prefix_for(&self, target: &NodeIndex) -> String {
        self.schema_alias_of(target)
            .map(|v| (v + ".").to_string())
            .unwrap_or_else(|| "".to_string())
    }

    pub fn to_sql(&self) -> eyre::Result<String> {
        let mut full_sql = String::new();
        for step_id in
            toposort(&self.graph, None).map_err(|_e| eyre_err!("Depgraph was Cyclic."))?
        {
            let step = &self.graph[step_id];

            let sql = match step {
                Schema(item) => if item.name != "public" && item.name != "pg_catalog" {
                    self.inventory_schema_to_sql(&step_id)?
                } else { String::default() },
                CustomSql(_item) => self.inventory_extension_sql_to_sql(&step_id)?,
                Function(item) => if self.graph.neighbors_undirected(self.externs.get(item).unwrap().clone()).any(|neighbor| {
                    let neighbor_item = &self.graph[neighbor];
                    match neighbor_item {
                        SqlGraphEntity::Type(InventoryPostgresType { in_fn, in_fn_module_path, out_fn, out_fn_module_path, .. }) => {
                            let is_in_fn = item.full_path.starts_with(in_fn_module_path) && item.full_path.ends_with(in_fn);
                            if is_in_fn {
                                tracing::debug!(r#type = %neighbor_item.dot_format(), "Skipping, is an in_fn.");
                            }
                            let is_out_fn = item.full_path.starts_with(out_fn_module_path) && item.full_path.ends_with(out_fn);
                            if is_out_fn {
                                tracing::debug!(r#type = %neighbor_item.dot_format(), "Skipping, is an out_fn.");
                            }
                            is_in_fn || is_out_fn
                        },
                        _ => false,
                    }
                }) {
                    String::default()
                } else { self.inventory_extern_to_sql(&step_id)? },
                Type(_item) => self.inventory_type_to_sql(&step_id)?,
                BuiltinType(_) => String::default(),
                Enum(_item) => self.inventory_enums_to_sql(&step_id)?,
                Ord(_item) => self.inventory_ord_to_sql(&step_id)?,
                Hash(_item) => self.inventory_hash_to_sql(&step_id)?,
                ExtensionRoot(_item) => format!("\
                    /* \n\
                       This file is auto generated by pgx.\n\
                       \n\
                       The ordering of items is not stable, it is driven by a dependency graph.\n\
                    */\n\
                "),
            };

            full_sql.push_str(&sql)
        }
        Ok(full_sql)
    }

    #[instrument(level = "debug", skip(self, item_index))]
    fn inventory_extension_sql_to_sql(&self, item_index: &NodeIndex) -> eyre::Result<String> {
        let item_node = &self.graph[*item_index];
        let item = match item_node {
            SqlGraphEntity::CustomSql(item) => item,
            _ => {
                return Err(eyre_err!(
                    "Was not called on a ExtensionSql. Got: {:?}",
                    item_node
                ))
            }
        };

        let sql = format!(
            "\n\
                -- {file}:{line}\n\
                {sql}\
                ",
            file = item.file,
            line = item.line,
            sql = item.sql,
        );
        tracing::debug!(%sql);
        Ok(sql)
    }

    #[instrument(level = "debug", err, skip(self, item_index))]
    fn inventory_schema_to_sql(&self, item_index: &NodeIndex) -> eyre::Result<String> {
        let item_node = &self.graph[*item_index];
        let item = match item_node {
            SqlGraphEntity::Schema(item) => item,
            _ => {
                return Err(eyre_err!(
                    "Was not called on a Schema. Got: {:?}",
                    item_node
                ))
            }
        };

        let sql = format!(
            "\n\
                    -- {file}:{line}\n\
                    CREATE SCHEMA IF NOT EXISTS {name}; /* {module_path} */\n\
                ",
            name = item.name,
            file = item.file,
            line = item.line,
            module_path = item.module_path,
        );
        tracing::debug!(%sql);
        Ok(sql)
    }

    #[instrument(level = "debug", err, skip(self, item_index))]
    fn inventory_enums_to_sql(&self, item_index: &NodeIndex) -> eyre::Result<String> {
        let item_node = &self.graph[*item_index];
        let item = match item_node {
            SqlGraphEntity::Enum(item) => item,
            _ => return Err(eyre_err!("Was not called on an Enum. Got: {:?}", item_node)),
        };

        let sql = format!(
            "\n\
                    -- {file}:{line}\n\
                    -- {full_path}\n\
                    CREATE TYPE {schema}{name} AS ENUM (\n\
                        {variants}\
                    );\n\
                ",
            schema = self.schema_prefix_for(item_index),
            full_path = item.full_path,
            file = item.file,
            line = item.line,
            name = item.name,
            variants = item
                .variants
                .iter()
                .map(|variant| format!("\t'{}'", variant))
                .collect::<Vec<_>>()
                .join(",\n")
                + "\n",
        );
        tracing::debug!(%sql);
        Ok(sql)
    }

    #[instrument(level = "debug", err, skip(self, item_index))]
    fn inventory_extern_to_sql(&self, item_index: &NodeIndex) -> eyre::Result<String> {
        let item_node = &self.graph[*item_index];
        let item = match item_node {
            SqlGraphEntity::Function(item) => item,
            _ => {
                return Err(eyre_err!(
                    "Was not called on a function. Got: {:?}",
                    item_node
                ))
            }
        };

        let mut extern_attrs = item.extern_attrs.clone();
        let mut strict_upgrade = true;
        if !extern_attrs.iter().any(|i| i == &ExternArgs::Strict) {
            for arg in &item.fn_args {
                if arg.is_optional {
                    strict_upgrade = false;
                }
            }
        }
        tracing::trace!(?extern_attrs, strict_upgrade);

        if strict_upgrade {
            extern_attrs.push(ExternArgs::Strict);
        }

        let fn_sql = format!("\
                                CREATE OR REPLACE FUNCTION {schema}\"{name}\"({arguments}) {returns}\n\
                                {extern_attrs}\
                                {search_path}\
                                LANGUAGE c /* Rust */\n\
                                AS 'MODULE_PATHNAME', '{name}_wrapper';\n\
                            ",
                             schema = self.schema_prefix_for(item_index),
                             name = item.name,
                             arguments = if !item.fn_args.is_empty() {
                                 let mut args = Vec::new();
                                 for (idx, arg) in item.fn_args.iter().enumerate() {
                                     let graph_index = self.graph.neighbors_undirected(*item_index).find(|neighbor| match &self.graph[*neighbor] {
                                         SqlGraphEntity::Type(ty) => ty.id_matches(&arg.ty_id),
                                         SqlGraphEntity::Enum(en) => en.id_matches(&arg.ty_id),
                                         SqlGraphEntity::BuiltinType(defined) => defined == &arg.full_path,
                                         _ => false,
                                     }).ok_or_else(|| eyre_err!("Could not find arg type in graph. Got: {:?}", arg))?;
                                     let needs_comma = idx < (item.fn_args.len() - 1);
                                     let buf = format!("\
                                            \t\"{pattern}\" {schema_prefix}{sql_type}{default}{maybe_comma}/* {full_path} */\
                                        ",
                                                       pattern = arg.pattern,
                                                       schema_prefix = self.schema_prefix_for(&graph_index),
                                                       sql_type = self.type_id_to_sql_type(arg.ty_id).ok_or_else(|| eyre_err!("Failed to map argument `{}` type `{}` to SQL type while building function `{}`.", arg.pattern, arg.full_path, item.name))?,
                                                       default = if let Some(def) = arg.default { format!(" DEFAULT {}", def) } else { String::from("") },
                                                       maybe_comma = if needs_comma { ", " } else { " " },
                                                       full_path = arg.full_path,
                                     );
                                     args.push(buf);
                                 };
                                 String::from("\n") + &args.join("\n") + "\n"
                             } else { Default::default() },
                             returns = match &item.fn_return {
                                 InventoryPgExternReturn::None => String::from("RETURNS void"),
                                 InventoryPgExternReturn::Type { id, full_path, .. } => {
                                     let graph_index = self.graph.neighbors_undirected(*item_index).find(|neighbor| match &self.graph[*neighbor] {
                                         SqlGraphEntity::Type(ty) => ty.id_matches(&id),
                                         SqlGraphEntity::Enum(en) => en.id_matches(&id),
                                         SqlGraphEntity::BuiltinType(defined) => &*defined == full_path,
                                         _ => false,
                                     }).ok_or_else(|| eyre_err!("Could not find return type in graph."))?;
                                     format!("RETURNS {schema_prefix}{sql_type} /* {full_path} */",
                                             sql_type = self.type_id_to_sql_type(*id).ok_or_else(|| eyre_err!("Failed to map return type `{}` to SQL type while building function `{}`.", full_path, item.full_path))?,
                                             schema_prefix = self.schema_prefix_for(&graph_index),
                                             full_path = full_path
                                     )
                                 },
                                 InventoryPgExternReturn::SetOf { id, full_path, .. } => {
                                     let graph_index = self.graph.neighbors_undirected(*item_index).find(|neighbor| match &self.graph[*neighbor] {
                                         SqlGraphEntity::Type(ty) => ty.id_matches(&id),
                                         SqlGraphEntity::Enum(en) => en.id_matches(&id),
                                         SqlGraphEntity::BuiltinType(defined) => defined == full_path,
                                         _ => false,
                                     }).ok_or_else(|| eyre_err!("Could not find return type in graph."))?;
                                     format!("RETURNS SETOF {schema_prefix}{sql_type} /* {full_path} */",
                                             sql_type = self.type_id_to_sql_type(*id).ok_or_else(|| eyre_err!("Failed to map return type `{}` to SQL type while building function `{}`.", full_path, item.full_path))?,
                                             schema_prefix = self.schema_prefix_for(&graph_index),
                                             full_path = full_path
                                     )
                                 },
                                 InventoryPgExternReturn::Iterated(table_items) => {
                                     let mut items = String::new();
                                     for (idx, (id, ty_name, _module_path, col_name)) in table_items.iter().enumerate() {
                                         let graph_index = self.graph.neighbors_undirected(*item_index).find(|neighbor| match &self.graph[*neighbor] {
                                             SqlGraphEntity::Type(ty) => ty.id_matches(&id),
                                             SqlGraphEntity::Enum(en) => en.id_matches(&id),
                                             SqlGraphEntity::BuiltinType(defined) => defined == ty_name,
                                             _ => false,
                                         }).ok_or_else(|| eyre_err!("Could not find return type in graph."))?;
                                         let needs_comma = idx < (table_items.len() - 1);
                                         let item = format!("\n\t{col_name} {schema_prefix}{ty_resolved}{needs_comma} /* {ty_name} */",
                                                            col_name = col_name.unwrap(),
                                                            schema_prefix = self.schema_prefix_for(&graph_index),
                                                            ty_resolved = self.type_id_to_sql_type(*id).ok_or_else(|| eyre_err!("Failed to map return type `{}` to SQL type while building function `{}`.", ty_name, item.name))?,
                                                            needs_comma = if needs_comma { ", " } else { " " },
                                                            ty_name = ty_name
                                         );
                                         items.push_str(&item);
                                     }
                                     format!("RETURNS TABLE ({}\n)", items)
                                 },
                                 InventoryPgExternReturn::Trigger => String::from("RETURNS trigger"),
                             },
                             search_path = if let Some(search_path) = &item.search_path {
                                 let retval = format!("SET search_path TO {}", search_path.join(", "));
                                 retval + "\n"
                             } else { Default::default() },
                             extern_attrs = if extern_attrs.is_empty() {
                                 String::default()
                             } else {
                                 let mut retval = extern_attrs.iter().map(|attr| format!("{}", attr).to_uppercase()).collect::<Vec<_>>().join(" ");
                                 retval.push('\n');
                                 retval
                             },
        );

        let ext_sql = format!(
            "\n\
                                -- {file}:{line}\n\
                                -- {module_path}::{name}\n\
                                {fn_sql}\n\
                                {overridden}\
                            ",
            name = item.name,
            module_path = item.module_path,
            file = item.file,
            line = item.line,
            fn_sql = if item.overridden.is_some() {
                let mut inner = fn_sql
                    .lines()
                    .map(|f| format!("-- {}", f))
                    .collect::<Vec<_>>()
                    .join("\n");
                inner.push_str(
                    "\n--\n-- Overridden as (due to a `//` comment with a `pgxsql` code block):",
                );
                inner
            } else {
                fn_sql
            },
            overridden = item
                .overridden
                .map(|f| f.to_owned() + "\n")
                .unwrap_or_default(),
        );
        tracing::debug!(sql = %ext_sql);

        let rendered = match (item.overridden, &item.operator) {
            (None, Some(op)) => {
                let mut optionals = vec![];
                if let Some(it) = op.commutator {
                    optionals.push(format!("\tCOMMUTATOR = {}", it));
                };
                if let Some(it) = op.negator {
                    optionals.push(format!("\tNEGATOR = {}", it));
                };
                if let Some(it) = op.restrict {
                    optionals.push(format!("\tRESTRICT = {}", it));
                };
                if let Some(it) = op.join {
                    optionals.push(format!("\tJOIN = {}", it));
                };
                if op.hashes {
                    optionals.push(String::from("\tHASHES"));
                };
                if op.merges {
                    optionals.push(String::from("\tMERGES"));
                };

                let left_arg = item.fn_args.get(0).ok_or_else(|| {
                    eyre_err!("Did not find `left_arg` for operator `{}`.", item.name)
                })?;
                let left_arg_graph_index = self
                    .graph
                    .neighbors_undirected(*item_index)
                    .find(|neighbor| match &self.graph[*neighbor] {
                        SqlGraphEntity::Type(ty) => ty.id_matches(&left_arg.ty_id),
                        _ => false,
                    })
                    .ok_or_else(|| eyre_err!("Could not find left arg function in graph."))?;
                let right_arg = item.fn_args.get(1).ok_or_else(|| {
                    eyre_err!("Did not find `left_arg` for operator `{}`.", item.name)
                })?;
                let right_arg_graph_index = self
                    .graph
                    .neighbors_undirected(*item_index)
                    .find(|neighbor| match &self.graph[*neighbor] {
                        SqlGraphEntity::Type(ty) => ty.id_matches(&right_arg.ty_id),
                        _ => false,
                    })
                    .ok_or_else(|| eyre_err!("Could not find right arg function in graph."))?;

                let operator_sql = format!("\n\
                                        -- {file}:{line}\n\
                                        -- {module_path}::{name}\n\
                                        CREATE OPERATOR {opname} (\n\
                                            \tPROCEDURE=\"{name}\",\n\
                                            \tLEFTARG={schema_prefix_left}{left_arg}, /* {left_name} */\n\
                                            \tRIGHTARG={schema_prefix_right}{right_arg}{maybe_comma} /* {right_name} */\n\
                                            {optionals}\
                                        );
                                    ",
                                           opname = op.opname.unwrap(),
                                           file = item.file,
                                           line = item.line,
                                           name = item.name,
                                           module_path = item.module_path,
                                           left_name = left_arg.full_path,
                                           right_name = right_arg.full_path,
                                           schema_prefix_left = self.schema_prefix_for(&left_arg_graph_index),
                                           left_arg = self.type_id_to_sql_type(left_arg.ty_id).ok_or_else(|| eyre_err!("Failed to map argument `{}` type `{}` to SQL type while building operator `{}`.", left_arg.pattern, left_arg.full_path, item.name))?,
                                           schema_prefix_right = self.schema_prefix_for(&right_arg_graph_index),
                                           right_arg = self.type_id_to_sql_type(right_arg.ty_id).ok_or_else(|| eyre_err!("Failed to map argument `{}` type `{}` to SQL type while building operator `{}`.", right_arg.pattern, right_arg.full_path, item.name))?,
                                           maybe_comma = if optionals.len() >= 1 { "," } else { "" },
                                           optionals = optionals.join(",\n") + "\n"
                );
                tracing::debug!(sql = %operator_sql);
                ext_sql + &operator_sql
            }
            (None, None) | (Some(_), Some(_)) | (Some(_), None) => ext_sql,
        };
        Ok(rendered)
    }

    #[instrument(level = "debug", err, skip(self, item_index))]
    fn inventory_type_to_sql(&self, item_index: &NodeIndex) -> eyre::Result<String> {
        let item_node = &self.graph[*item_index];
        let item = match item_node {
            SqlGraphEntity::Type(item) => item,
            _ => return Err(eyre_err!("Was not called on a Type. Got: {:?}", item_node)),
        };

        // The `in_fn`/`out_fn` need to be present in a certain order:
        // - CREATE TYPE;
        // - CREATE FUNCTION _in;
        // - CREATE FUNCTION _out;
        // - CREATE TYPE (...);

        let in_fn_module_path = if !item.in_fn_module_path.is_empty() {
            item.in_fn_module_path.clone()
        } else {
            item.module_path.to_string() // Presume a local
        };
        let in_fn_path = format!(
            "{module_path}{maybe_colons}{in_fn}",
            module_path = in_fn_module_path,
            maybe_colons = if !in_fn_module_path.is_empty() {
                "::"
            } else {
                ""
            },
            in_fn = item.in_fn,
        );
        let (_, _index) = self
            .externs
            .iter()
            .find(|(k, _v)| (**k).full_path == in_fn_path.as_str())
            .ok_or_else(|| eyre::eyre!("Did not find `in_fn: {}`.", in_fn_path))?;
        let in_fn_graph_index = self
            .graph
            .neighbors_undirected(*item_index)
            .find(|neighbor| match &self.graph[*neighbor] {
                SqlGraphEntity::Function(func) => func.full_path == in_fn_path,
                _ => false,
            })
            .ok_or_else(|| eyre_err!("Could not find in_fn graph entity."))?;
        tracing::trace!(in_fn = ?in_fn_path, "Found matching `in_fn`");
        let in_fn_sql = self.inventory_extern_to_sql(&in_fn_graph_index)?;
        tracing::trace!(%in_fn_sql);

        let out_fn_module_path = if !item.out_fn_module_path.is_empty() {
            item.out_fn_module_path.clone()
        } else {
            item.module_path.to_string() // Presume a local
        };
        let out_fn_path = format!(
            "{module_path}{maybe_colons}{out_fn}",
            module_path = out_fn_module_path,
            maybe_colons = if !out_fn_module_path.is_empty() {
                "::"
            } else {
                ""
            },
            out_fn = item.out_fn,
        );
        let (_, _index) = self
            .externs
            .iter()
            .find(|(k, _v)| {
                tracing::trace!(%k.full_path, %out_fn_path, "Checked");
                (**k).full_path == out_fn_path.as_str()
            })
            .ok_or_else(|| eyre::eyre!("Did not find `out_fn: {}`.", out_fn_path))?;
        let out_fn_graph_index = self
            .graph
            .neighbors_undirected(*item_index)
            .find(|neighbor| match &self.graph[*neighbor] {
                SqlGraphEntity::Function(func) => func.full_path == out_fn_path,
                _ => false,
            })
            .ok_or_else(|| eyre_err!("Could not find out_fn graph entity."))?;
        tracing::trace!(out_fn = ?out_fn_path, "Found matching `out_fn`");
        let out_fn_sql = self.inventory_extern_to_sql(&out_fn_graph_index)?;
        tracing::trace!(%out_fn_sql);

        let shell_type = format!(
            "\n\
                                -- {file}:{line}\n\
                                -- {full_path}\n\
                                CREATE TYPE {schema}{name};\n\
                            ",
            schema = self.schema_prefix_for(item_index),
            full_path = item.full_path,
            file = item.file,
            line = item.line,
            name = item.name,
        );
        tracing::debug!(sql = %shell_type);

        let materialized_type = format!("\n\
                                -- {file}:{line}\n\
                                -- {full_path}\n\
                                CREATE TYPE {schema}{name} (\n\
                                    \tINTERNALLENGTH = variable,\n\
                                    \tINPUT = {schema_prefix_in_fn}{in_fn}, /* {in_fn_path} */\n\
                                    \tOUTPUT = {schema_prefix_out_fn}{out_fn}, /* {out_fn_path} */\n\
                                    \tSTORAGE = extended\n\
                                );
                            ",
                                        full_path = item.full_path,
                                        file = item.file,
                                        line = item.line,
                                        schema = self.schema_prefix_for(item_index),
                                        name = item.name,
                                        schema_prefix_in_fn = self.schema_prefix_for(&in_fn_graph_index),
                                        in_fn = item.in_fn,
                                        in_fn_path = in_fn_path,
                                        schema_prefix_out_fn = self.schema_prefix_for(&out_fn_graph_index),
                                        out_fn = item.out_fn,
                                        out_fn_path = out_fn_path,
        );
        tracing::debug!(sql = %materialized_type);

        Ok(shell_type + &in_fn_sql + &out_fn_sql + &materialized_type)
    }

    #[instrument(level = "debug", err, skip(self, item_index))]
    fn inventory_hash_to_sql(&self, item_index: &NodeIndex) -> eyre::Result<String> {
        let item_node = &self.graph[*item_index];
        let item = match item_node {
            SqlGraphEntity::Hash(item) => item,
            _ => return Err(eyre_err!("Was not called on a Hash. Got: {:?}", item_node)),
        };

        let sql = format!("\n\
                            -- {file}:{line}\n\
                            -- {full_path}\n\
                            -- {id:?}\n\
                            CREATE OPERATOR FAMILY {name}_hash_ops USING hash;\n\
                            CREATE OPERATOR CLASS {name}_hash_ops DEFAULT FOR TYPE {name} USING hash FAMILY {name}_hash_ops AS\n\
                                \tOPERATOR    1   =  ({name}, {name}),\n\
                                \tFUNCTION    1   {name}_hash({name});\
                            ",
                          name = item.name,
                          full_path = item.full_path,
                          file = item.file,
                          line = item.line,
                          id = item.id,
        );
        tracing::debug!(%sql);
        Ok(sql)
    }

    #[instrument(level = "debug", err, skip(self, item_index))]
    fn inventory_ord_to_sql(&self, item_index: &NodeIndex) -> eyre::Result<String> {
        let item_node = &self.graph[*item_index];
        let item = match item_node {
            SqlGraphEntity::Ord(item) => item,
            _ => return Err(eyre_err!("Was not called on an Ord. Got: {:?}", item_node)),
        };

        let sql = format!("\n\
                            -- {file}:{line}\n\
                            -- {full_path}\n\
                            -- {id:?}\n\
                            CREATE OPERATOR FAMILY {name}_btree_ops USING btree;\n\
                            CREATE OPERATOR CLASS {name}_btree_ops DEFAULT FOR TYPE {name} USING btree FAMILY {name}_btree_ops AS\n\
                                  \tOPERATOR 1 <,\n\
                                  \tOPERATOR 2 <=,\n\
                                  \tOPERATOR 3 =,\n\
                                  \tOPERATOR 4 >=,\n\
                                  \tOPERATOR 5 >,\n\
                                  \tFUNCTION 1 {name}_cmp({name}, {name});\n\
                            ",
                          name = item.name,
                          full_path = item.full_path,
                          file = item.file,
                          line = item.line,
                          id = item.id,
        );
        tracing::debug!(%sql);
        Ok(sql)
    }

    #[instrument(level = "debug", skip(self))]
    pub fn register_types(&mut self) {
        for (item, _index) in self.enums.clone() {
            self.map_type_id_to_sql_type(item.id, item.name);
            if let Some(val) = item.option_id {
                self.map_type_id_to_sql_type(val, item.name);
            };
            if let Some(val) = item.varlena_id {
                self.map_type_id_to_sql_type(val, format!("{}[]", item.name));
            }
            if let Some(val) = item.varlena_id {
                self.map_type_id_to_sql_type(val, item.name);
            }
            if let Some(val) = item.array_id {
                self.map_type_id_to_sql_type(val, format!("{}[]", item.name));
            }
            if let Some(val) = item.option_array_id {
                self.map_type_id_to_sql_type(val, format!("{}[]", item.name));
            }
        }
        for (item, _index) in self.types.clone() {
            self.map_type_id_to_sql_type(item.id, item.name);
            self.map_type_id_to_sql_type(item.option_id, item.name);
            self.map_type_id_to_sql_type(item.vec_id, format!("{}[]", item.name));
            self.map_type_id_to_sql_type(item.vec_option_id, format!("{}[]", item.name));
            if let Some(val) = item.varlena_id {
                self.map_type_id_to_sql_type(val, item.name);
            }
            if let Some(val) = item.array_id {
                self.map_type_id_to_sql_type(val, format!("{}[]", item.name));
            }
            if let Some(val) = item.option_array_id {
                self.map_type_id_to_sql_type(val, format!("{}[]", item.name));
            }
        }
    }

    #[instrument(level = "debug")]
    pub fn type_id_to_sql_type(&self, id: TypeId) -> Option<String> {
        self.type_mappings.get(&id).map(|f| f.clone())
    }

    #[instrument(level = "debug")]
    pub fn map_type_to_sql_type<T: 'static>(&mut self, sql: impl AsRef<str> + Debug) {
        let sql = sql.as_ref().to_string();
        self.type_mappings.insert(TypeId::of::<T>(), sql.clone());
    }

    #[instrument(level = "debug")]
    pub fn map_type_id_to_sql_type(&mut self, id: TypeId, sql: impl AsRef<str> + Debug) {
        let sql = sql.as_ref().to_string();
        self.type_mappings.insert(id, sql);
    }
}
