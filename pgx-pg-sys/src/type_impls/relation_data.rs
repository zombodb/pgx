use crate::{PgPtr, RelationData, TupleDescData};

impl PgPtr<RelationData> {
    /// RelationGetRelationName
    ///            Returns the rel's name.
    ///
    /// Note that the name is only unique within the containing namespace.
    pub fn name(&self) -> &str {
        self.rd_rel.relname.as_str()
    }

    /// RelationGetRelid
    ///          Returns the OID of the relation
    #[inline]
    pub fn oid(&self) -> crate::Oid {
        self.rd_id
    }

    /// RelationGetNamespace
    ///            Returns the rel's namespace OID.
    pub fn namespace_oid(&self) -> crate::Oid {
        self.rd_rel.relnamespace
    }

    /// What is the name of the namespace in which this relation is located?
    pub fn namespace(&self) -> &str {
        unsafe { crate::get_namespace_name(self.namespace_oid()).as_str() }
    }

    pub fn tupdesc(&self) -> &PgPtr<TupleDescData> {
        &self.rd_att
    }

    /// Number of tuples in this relation (not always up-to-date)
    pub fn reltuples(&self) -> Option<f32> {
        let reltuples = self.rd_rel.reltuples;

        if reltuples == 0f32 {
            None
        } else {
            Some(reltuples)
        }
    }

    pub fn is_table(&self) -> bool {
        self.rd_rel.relkind == crate::RELKIND_RELATION as i8
    }

    pub fn is_matview(&self) -> bool {
        self.rd_rel.relkind == crate::RELKIND_MATVIEW as i8
    }

    pub fn is_index(&self) -> bool {
        self.rd_rel.relkind == crate::RELKIND_INDEX as i8
    }

    pub fn is_view(&self) -> bool {
        self.rd_rel.relkind == crate::RELKIND_VIEW as i8
    }

    pub fn is_sequence(&self) -> bool {
        self.rd_rel.relkind == crate::RELKIND_SEQUENCE as i8
    }

    pub fn is_composite_type(&self) -> bool {
        self.rd_rel.relkind == crate::RELKIND_COMPOSITE_TYPE as i8
    }

    pub fn is_foreign_table(&self) -> bool {
        self.rd_rel.relkind == crate::RELKIND_FOREIGN_TABLE as i8
    }

    pub fn is_partitioned_table(&self) -> bool {
        self.rd_rel.relkind == crate::RELKIND_PARTITIONED_TABLE as i8
    }

    pub fn is_toast_value(&self) -> bool {
        self.rd_rel.relkind == crate::RELKIND_TOASTVALUE as i8
    }
}
