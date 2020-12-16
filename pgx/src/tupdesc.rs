// Copyright 2020 ZomboDB, LLC <zombodb@gmail.com>. All rights reserved. Use of this source code is
// governed by the MIT license that can be found in the LICENSE file.

//! Provides a safe wrapper around Postgres' `pg_sys::TupleDescData` struct
use crate::{pg_sys, void_mut_ptr, FromDatum, PgPtr, PgRelation};

use std::ops::Deref;

/// This struct is passed around within the backend to describe the structure
/// of tuples.  For tuples coming from on-disk relations, the information is
/// collected from the pg_attribute, pg_attrdef, and pg_constraint catalogs.
/// Transient row types (such as the result of a join query) have anonymous
/// TupleDesc structs that generally omit any constraint info; therefore the
/// structure is designed to let the constraints be omitted efficiently.
///
/// Note that only user attributes, not system attributes, are mentioned in
/// TupleDesc; with the exception that tdhasoid indicates if OID is present.
///
/// If the tupdesc is known to correspond to a named rowtype (such as a table's
/// rowtype) then tdtypeid identifies that type and tdtypmod is -1.  Otherwise
/// tdtypeid is RECORDOID, and tdtypmod can be either -1 for a fully anonymous
/// row type, or a value >= 0 to allow the rowtype to be looked up in the
/// typcache.c type cache.
///
/// Note that tdtypeid is never the OID of a domain over composite, even if
/// we are dealing with values that are known (at some higher level) to be of
/// a domain-over-composite type.  This is because tdtypeid/tdtypmod need to
/// match up with the type labeling of composite Datums, and those are never
/// explicitly marked as being of a domain type, either.
///
/// Tuple descriptors that live in caches (relcache or typcache, at present)
/// are reference-counted: they can be deleted when their reference count goes
/// to zero.  Tuple descriptors created by the executor need no reference
/// counting, however: they are simply created in the appropriate memory
/// context and go away when the context is freed.  We set the tdrefcount
/// field of such a descriptor to -1, while reference-counted descriptors
/// always have tdrefcount >= 0.
///
/// PGX's safe wrapper takes care of properly freeing or decrementing reference counts
pub struct PgTupleDesc<'a> {
    tupdesc: PgPtr<pg_sys::TupleDescData>,
    parent: Option<&'a PgRelation>,
    data: Option<PgPtr<pg_sys::HeapTupleData>>,
    need_release: bool,
    need_pfree: bool,
}

impl<'a> PgTupleDesc<'a> {
    /// Wrap a Postgres-provided `pg_sys::TupleDescData`.  It is assumed the provided TupleDesc
    /// is reference counted by Postgres.
    ///
    /// The wrapped TupleDesc will have its reference count decremented  when this `PgTupleDesc`
    /// instance is dropped.
    ///
    /// ## Safety
    ///
    /// This method is unsafe as we cannot validate that the provided `pg_sys::TupleDesc` is valid
    /// or requires reference counting.
    pub unsafe fn from_pg<'b>(ptr: pg_sys::TupleDesc) -> PgTupleDesc<'b> {
        PgTupleDesc {
            tupdesc: PgBox::from_pg(ptr),
            parent: None,
            data: None,
            need_release: true,
            need_pfree: false,
        }
    }

    /// Wrap a copy of a `pg_sys::TupleDesc`.  This form is not reference counted and the copy is
    /// allocated in the `CurrentMemoryContext`
    ///
    /// When this instance is dropped, the copied TupleDesc is `pfree()`'d
    pub fn from_pg_copy<'b>(ptr: pg_sys::TupleDesc) -> PgTupleDesc<'b> {
        PgTupleDesc {
            tupdesc: PgBox::from_pg(unsafe { pg_sys::CreateTupleDescCopyConstr(ptr) }),
            parent: None,
            data: None,
            need_release: false,
            need_pfree: true,
        }
    }

    /// Similar to `::from_pg_copy()`, but assumes the provided `TupleDesc` is already a copy.
    ///
    /// When this instance is dropped, the TupleDesc is `pfree()`'d
    ///
    /// ## Examples
    ///
    /// ```rust,no_run
    /// use pgx::{pg_sys, PgTupleDesc};
    /// let typid = 42 as pg_sys::Oid;  // a valid pg_type "oid" value
    /// let typmod = 0; // it's corresponding typemod value
    /// let tupdesc = unsafe { PgTupleDesc::from_pg_is_copy(pg_sys::lookup_rowtype_tupdesc_copy(typid, typmod)) };
    ///
    /// // assert the tuple descriptor has 12 attributes
    /// assert_eq!(tupdesc.len(), 12);
    ///
    /// // the wrapped tupdesc pointer is pfree'd
    /// drop(tupdesc)
    /// ```
    ///
    /// ## Safety
    ///
    /// This method is unsafe as we cannot validate that the provided `pg_sys::TupleDesc` is valid
    /// or is actually a copy that requires a `pfree()` on Drop.
    pub unsafe fn from_pg_is_copy<'b>(ptr: pg_sys::TupleDesc) -> PgTupleDesc<'b> {
        PgTupleDesc {
            tupdesc: PgBox::from_pg(ptr),
            parent: None,
            data: None,
            need_release: false,
            need_pfree: true,
        }
    }

    /// wrap the `pg_sys::TupleDesc` contained by the specified `PgRelation`
    pub fn from_relation(parent: &PgRelation) -> PgTupleDesc {
        PgTupleDesc {
            tupdesc: parent.rd_att,
            parent: Some(parent),
            data: None,
            need_release: false,
            need_pfree: false,
        }
    }

    /// create a `PgTupleDesc` from a composite `pg_sys::Datum`, also tracking the backing
    /// `HeapTupleData` so its attribute values can get retrieved via the `get_attr()` function.
    ///
    /// ## Safety
    ///
    /// This function is unsafe as it cannot guarantee that the provided `pg_sys::Datum` actually
    /// points to a composite type
    pub unsafe fn from_composite(composite: pg_sys::Datum) -> Self {
        let htup_header = pg_sys::pg_detoast_datum(PgPtr::from(composite)).cast();
        let tup_type = crate::heap_tuple_header_get_type_id(htup_header);
        let tup_typmod = crate::heap_tuple_header_get_typmod(htup_header);
        let tupdesc = pg_sys::lookup_rowtype_tupdesc(tup_type, tup_typmod);

        let mut data = PgBox::<pg_sys::HeapTupleData>::alloc();

        data.t_len = crate::heap_tuple_header_get_datum_length(htup_header) as u32;
        data.t_data = htup_header;

        PgTupleDesc {
            tupdesc: PgBox::from_pg(tupdesc),
            parent: None,
            data: Some(data),
            need_release: true,
            need_pfree: false,
        }
    }

    /// From which relation was this TupleDesc created, if any?
    pub fn parent(&self) -> Option<&PgRelation> {
        self.parent
    }

    /// What is the pg_type oid of this TupleDesc?
    pub fn oid(&self) -> pg_sys::Oid {
        self.tupdesc.tdtypeid
    }

    /// What is the typemod of this TupleDesc?
    pub fn typmod(&self) -> i32 {
        self.tupdesc.tdtypmod
    }

    /// How many attributes do we have?
    pub fn len(&self) -> usize {
        self.tupdesc.natts as usize
    }

    /// Do we have attributes?
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get a numbered attribute.  Attribute numbers are zero-based
    pub fn get(&self, i: usize) -> Option<&pg_sys::FormData_pg_attribute> {
        if i >= self.len() {
            None
        } else {
            Some(tupdesc_get_attr(&self.tupdesc, i))
        }
    }

    /// Get a typed attribute Datum from the backing composite data.
    ///
    /// This is only possible for `PgTupleDesc` created with `from_composite()`.
    ///
    /// The `attno` argument is zero-based
    pub fn get_attr<T: FromDatum>(&self, attno: usize) -> Option<T> {
        crate::heap_getattr(
            self.data
                .as_ref()
                .expect("no composite data associated with this PgTupleDesc"),
            attno + 1, // +1 b/c heap_getattr is 1-based but we're not
            &self,
        )
    }

    /// Iterate over our attributes
    pub fn iter(&self) -> TupleDescIterator {
        TupleDescIterator {
            tupdesc: self,
            curr: 0,
        }
    }
}

impl<'a> Deref for PgTupleDesc<'a> {
    type Target = PgBox<pg_sys::TupleDescData>;

    fn deref(&self) -> &Self::Target {
        &self.tupdesc
    }
}

impl<'a> Drop for PgTupleDesc<'a> {
    fn drop(&mut self) {
        if self.need_release {
            unsafe { release_tupdesc(self.tupdesc) }
        } else if self.need_pfree {
            unsafe { pg_sys::pfree(self.tupdesc.as_ptr() as void_mut_ptr) }
        }
    }
}

pub unsafe fn release_tupdesc(ptr: pg_sys::TupleDesc) {
    if (*ptr).tdrefcount >= 0 {
        pg_sys::DecrTupleDescRefCount(ptr)
    }
}

/// `attno` is 0-based
#[cfg(feature = "pg10")]
#[inline]
fn tupdesc_get_attr(
    tupdesc: &PgBox<pg_sys::TupleDescData>,
    attno: usize,
) -> &pg_sys::FormData_pg_attribute {
    let atts = unsafe { std::slice::from_raw_parts(tupdesc.attrs, tupdesc.natts as usize) };
    unsafe { atts[attno].as_ref().unwrap() }
}

/// `attno` is 0-based
#[cfg(any(feature = "pg11", feature = "pg12", feature = "pg13"))]
#[inline]
fn tupdesc_get_attr(
    tupdesc: &PgBox<pg_sys::TupleDescData>,
    attno: usize,
) -> &pg_sys::FormData_pg_attribute {
    let atts = unsafe { tupdesc.attrs.as_slice(tupdesc.natts as usize) };
    &atts[attno]
}

pub struct TupleDescIterator<'a> {
    tupdesc: &'a PgTupleDesc<'a>,
    curr: usize,
}

impl<'a> Iterator for TupleDescIterator<'a> {
    type Item = &'a pg_sys::FormData_pg_attribute;

    fn next(&mut self) -> Option<Self::Item> {
        let result = self.tupdesc.get(self.curr);
        self.curr += 1;
        result
    }
}

pub struct TupleDescDataIntoIterator<'a> {
    tupdesc: PgTupleDesc<'a>,
    curr: usize,
}

impl<'a> IntoIterator for PgTupleDesc<'a> {
    type Item = pg_sys::FormData_pg_attribute;
    type IntoIter = TupleDescDataIntoIterator<'a>;

    fn into_iter(self) -> Self::IntoIter {
        TupleDescDataIntoIterator {
            tupdesc: self,
            curr: 0,
        }
    }
}

impl<'a> Iterator for TupleDescDataIntoIterator<'a> {
    type Item = pg_sys::FormData_pg_attribute;

    fn next(&mut self) -> Option<Self::Item> {
        let result = match self.tupdesc.get(self.curr) {
            Some(result) => *result,
            None => {
                return None;
            }
        };
        self.curr += 1;
        Some(result)
    }
}
