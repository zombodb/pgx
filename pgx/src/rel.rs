// Copyright 2020 ZomboDB, LLC <zombodb@gmail.com>. All rights reserved. Use of this source code is
// governed by the MIT license that can be found in the LICENSE file.

//! Provides a safe wrapper around Postgres' `pg_sys::RelationData` struct
use crate::{
    direct_function_call, name_data_to_str, pg_sys, FromDatum, IntoDatum, PgPtr, PgTupleDesc,
};
use pgx_pg_sys::RelationData;
use std::ops::Deref;

pub struct PgRelation {
    boxed: PgPtr<pg_sys::RelationData>,
    lockmode: Option<pg_sys::LOCKMODE>,
}

impl From<PgPtr<pg_sys::RelationData>> for PgRelation {
    fn from(r: PgPtr<pg_sys::RelationData>) -> Self {
        PgRelation {
            boxed: r,
            lockmode: None,
        }
    }
}

impl PgRelation {
    /// Given a relation oid, use `pg_sys::RelationIdGetRelation()` to open the relation
    ///
    /// If the specified relation oid was recently deleted, this function will panic.
    ///
    /// Additionally, the relation is closed via `pg_sys::RelationClose()` when this instance is
    /// dropped.
    ///
    /// ## Safety
    ///
    /// The caller should already have at least AccessShareLock on the relation ID, else there are
    /// nasty race conditions.
    ///
    /// As such, this function is unsafe as we cannot guarantee that this requirement is true.
    pub unsafe fn open(oid: pg_sys::Oid) -> Self {
        let rel = pg_sys::RelationIdGetRelation(oid);
        if rel.is_null() {
            // relation was recently deleted
            panic!("Cannot open relation with oid={}", oid);
        }

        PgRelation {
            boxed: rel,
            lockmode: None,
        }
    }

    /// relation_open - open any relation by relation OID
    ///
    /// If lockmode is not "NoLock", the specified kind of lock is
    /// obtained on the relation.  (Generally, NoLock should only be
    /// used if the caller knows it has some appropriate lock on the
    /// relation already.)
    ///
    /// An error is raised if the relation does not exist.
    ///
    /// NB: a "relation" is anything with a pg_class entry.  The caller is
    /// expected to check whether the relkind is something it can handle.
    ///
    /// The opened relation is automatically closed via `pg_sys::relation_close()`
    /// when this instance is dropped
    pub fn with_lock(oid: pg_sys::Oid, lockmode: pg_sys::LOCKMODE) -> Self {
        unsafe {
            PgRelation {
                boxed: pg_sys::relation_open(oid, lockmode),
                lockmode: Some(lockmode),
            }
        }
    }

    /// Given a relation name, use `pg_sys::to_regclass` to look up its oid, and then
    /// `pg_sys::RelationIdGetRelation()` to open the relation.
    ///
    /// If the specified relation name is not found, we return an `Err(&str)`.
    ///
    /// If the specified relation was recently deleted, this function will panic.
    ///
    /// Additionally, the relation is closed via `pg_sys::RelationClose()` when this instance is
    /// dropped.
    ///
    /// ## Safety
    ///
    /// The caller should already have at least AccessShareLock on the relation ID, else there are
    /// nasty race conditions.
    ///
    /// As such, this function is unsafe as we cannot guarantee that this requirement is true.
    pub unsafe fn open_with_name(relname: &str) -> std::result::Result<Self, &'static str> {
        match direct_function_call::<pg_sys::Oid>(pg_sys::to_regclass, vec![relname.into_datum()]) {
            Some(oid) => Ok(PgRelation::open(oid)),
            None => Err("no such relation"),
        }
    }

    /// Given a relation name, use `pg_sys::to_regclass` to look up its oid, and then
    /// open it with an AccessShareLock
    ///
    /// If the specified relation name is not found, we return an `Err(&str)`.
    ///
    /// If the specified relation was recently deleted, this function will panic.
    ///
    /// Additionally, the relation is closed via `pg_sys::RelationClose()` when this instance is
    /// dropped.
    pub fn open_with_name_and_share_lock(relname: &str) -> std::result::Result<Self, &'static str> {
        unsafe {
            match direct_function_call::<pg_sys::Oid>(
                pg_sys::to_regclass,
                vec![relname.into_datum()],
            ) {
                Some(oid) => Ok(PgRelation::with_lock(
                    oid,
                    pg_sys::AccessShareLock as pg_sys::LOCKMODE,
                )),
                None => Err("no such relation"),
            }
        }
    }

    /// If this `PgRelation` represents an index, return the `PgRelation` for the heap
    /// relation to which it is attached
    pub fn heap_relation(&self) -> Option<PgPtr<RelationData>> {
        if self.rd_index.is_null() {
            None
        } else {
            unsafe { Some(PgPtr::<RelationData>::open(self.rd_index.indrelid)) }
        }
    }

    /// Return an iterator of indices, as `PgRelation`s, attached to this relation
    pub fn indices(
        &self,
        lockmode: crate::LOCKMODE,
    ) -> impl std::iter::Iterator<Item = PgPtr<RelationData>> {
        let list = unsafe { crate::RelationGetIndexList(self.clone()) };

        list.iter_oid()
            .filter(|oid| *oid != crate::InvalidOid)
            .map(move |oid| PgPtr::<RelationData>::with_lock(oid, lockmode))
    }
}

impl Clone for PgRelation {
    /// Same as calling `PgRelation::with_lock(AccessShareLock)` on the underlying relation id
    fn clone(&self) -> Self {
        PgRelation::with_lock(self.rd_id, pg_sys::AccessShareLock as pg_sys::LOCKMODE)
    }
}

impl FromDatum for PgRelation {
    unsafe fn from_datum(datum: pg_sys::Datum, is_null: bool, _typoid: u32) -> Option<PgRelation> {
        if is_null {
            None
        } else {
            Some(PgRelation::with_lock(
                datum as pg_sys::Oid,
                pg_sys::AccessShareLock as pg_sys::LOCKMODE,
            ))
        }
    }
}

impl IntoDatum for PgRelation {
    fn into_datum(self) -> Option<pg_sys::Datum> {
        Some(self.oid() as pg_sys::Datum)
    }

    fn type_oid() -> u32 {
        pg_sys::REGCLASSOID as pg_sys::Oid
    }
}

impl Deref for PgRelation {
    type Target = PgPtr<pg_sys::RelationData>;

    fn deref(&self) -> &Self::Target {
        &self.boxed
    }
}

impl Drop for PgRelation {
    fn drop(&mut self) {
        if !self.boxed.is_null() {
            match self.lockmode {
                None => unsafe { pg_sys::RelationClose(self.boxed) },
                Some(lockmode) => unsafe { pg_sys::relation_close(self.boxed, lockmode) },
            }
        }
    }
}
