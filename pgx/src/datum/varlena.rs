//! Wrapper for Postgres 'varlena' type, over Rust types of a fixed size (ie, `impl Copy`)
use crate::pg_sys::{VARATT_SHORT_MAX, VARHDRSZ_SHORT};
use crate::{
    pg_sys, rust_regtypein, set_varsize, set_varsize_short, vardata_any, varsize_any,
    varsize_any_exhdr, void_mut_ptr, FromDatum, IntoDatum, PgMemoryContexts, PgPtr, PostgresType,
    StringInfo,
};
use pgx_pg_sys::varlena;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};

struct PallocdVarlena {
    ptr: PgPtr<pg_sys::varlena>,
    len: usize,
}

impl Clone for PallocdVarlena {
    fn clone(&self) -> Self {
        let len = self.len;
        let ptr = PgMemoryContexts::Of(self.ptr as void_mut_ptr).copy_ptr_into(self.ptr, len);

        PallocdVarlena { ptr, len }
    }
}

/// Wraps a Postgres `varlena *`, presenting it as if it's a Rust type of a fixed size.
///
/// The wrapped `varlena *` is behind a Rust `alloc::borrow:Cow` which ensures that in the
/// common-use case of creating a `PgVarlena` from a Postgres-provided `Datum`, it's not
/// possible to scribble on that Postgres-allocated memory.
///
/// Generally, `PgVarlena` is meant to be used in conjunction with pgx's `PostgresType` derive macro
/// to provide transparent mapping of fixed-size Rust types as Postgres datums.
///
/// ## Example
///
/// ```rust,no_run
/// use pgx::*;
/// use std::ffi::CStr;
/// use std::str::FromStr;
///
/// pg_module_magic!();
///
/// #[derive(Copy, Clone, PostgresType)]
/// #[pgvarlena_inoutfuncs]
/// struct MyType {
///    a: f32,
///    b: f32,
///    c: i64
/// }
///
/// impl PgVarlenaInOutFuncs for MyType {
///     fn input(input: &std::ffi::CStr) -> PgVarlena<Self> {
///         let mut iter = input.to_str().unwrap().split(',');
///         let (a, b, c) = (iter.next(), iter.next(), iter.next());
///
///         let mut result = PgVarlena::<MyType>::new();
///         result.a = f32::from_str(a.unwrap()).expect("a is not a valid f32");
///         result.b = f32::from_str(b.unwrap()).expect("b is not a valid f32");
///         result.c = i64::from_str(c.unwrap()).expect("c is not a valid i64");
///
///         result
///     }
///
///     fn output(&self, buffer: &mut StringInfo) {
///         buffer.push_str(&format!("{},{},{}", self.a, self.b, self.c));
///     }
/// }
///
/// #[pg_extern]
/// fn do_a_thing(mut input: PgVarlena<MyType>) -> PgVarlena<MyType> {
///     input.c += 99;  // performs a copy-on-write
///     input
/// }
/// ```
pub struct PgVarlena<T>
where
    T: Copy + Sized,
{
    leaked: Option<*mut PallocdVarlena>,
    varlena: Cow<'static, PallocdVarlena>,
    need_free: bool,
    __marker: PhantomData<T>,
}

impl<T> PgVarlena<T>
where
    T: Copy + Sized,
{
    /// Create a new `PgVarlena` representing a Rust type.  The backing varlena is allocated
    /// by Postgres and initially zero'd (using `pg_sys::palloc0`).  Unless `.into_pg()` is called,
    /// the Postgres-allocated memory will follow Rust's drop semantics.
    ///
    /// ## Example
    ///
    /// ```rust,no_run
    /// use pgx::PgVarlena;
    /// #[derive(Copy, Clone)]
    /// struct MyType {
    ///    a: f32,
    ///    b: f32,
    ///    c: i64
    /// }
    ///
    /// let mut v = PgVarlena::<MyType>::new();
    /// v.a = 42.0;
    /// v.b = 0.424242;
    /// v.c = 42;
    /// ```
    pub fn new() -> Self {
        let size_of = std::mem::size_of::<T>();

        let ptr = PgPtr::with_extra_len0(pg_sys::VARHDRSZ + size_of);

        // safe: ptr will halready be allocated
        unsafe {
            if size_of + VARHDRSZ_SHORT() <= VARATT_SHORT_MAX as usize {
                // we can use the short header size
                set_varsize_short(ptr, (size_of + VARHDRSZ_SHORT()) as i32);
            } else {
                // gotta use the full 4-byte header
                set_varsize(ptr, (size_of + pg_sys::VARHDRSZ) as i32);
            }
        }

        PgVarlena {
            leaked: None,
            varlena: Cow::Owned(PallocdVarlena {
                ptr,
                len: unsafe { varsize_any(ptr) },
            }),
            need_free: true,
            __marker: PhantomData,
        }
    }

    /// Construct a `PgVarlena` from a known-to-be-non-null `pg_sys::Datum`.  As
    /// `FromDatum for PgVarlena<T> where T: Copy + Sized` is already implemented, it is unlikely
    /// that this function will need to be called directly.
    ///
    /// The provided datum is automatically detoasted and the returned `PgVarlena` will either
    /// be considered borrowed or owned based on if detoasting actually needed to allocate memory.
    /// If it didn't, then we're borrowed, otherwise we're owned.
    ///
    /// ## Safety
    ///
    /// This function is considered unsafe as it cannot guarantee the provided `pg_sys::Datum` is a
    /// valid `*mut pg_sys::varlena`.
    pub unsafe fn from_datum(datum: pg_sys::Datum) -> Self {
        let ptr = pg_sys::pg_detoast_datum(PgPtr::from(datum));
        let len = varsize_any(ptr);

        if ptr == datum as *mut pg_sys::varlena {
            // no detoasting happened so we're using borrowed memory
            let leaked = Box::leak(Box::new(PallocdVarlena { ptr, len }));
            PgVarlena {
                leaked: Some(leaked),
                varlena: Cow::Borrowed(leaked),
                need_free: false,
                __marker: PhantomData,
            }
        } else {
            // datum was detoasted so we own and it need to free it
            PgVarlena {
                leaked: None,
                varlena: Cow::Owned(PallocdVarlena { ptr, len }),
                need_free: true,
                __marker: PhantomData,
            }
        }
    }

    /// Use when you need to pass the backing `*mut pg_sys::varlena` to a Postgres function.
    ///
    /// This method is also used by the `IntoDatum for PgVarlena<T> where T: Copy + Sized`
    /// implementation.
    pub fn into_pg(mut self) -> PgPtr<pg_sys::varlena> {
        // we don't want our varlena to be pfree'd
        self.need_free = false;
        self.varlena.ptr
    }
}

/// `pg_sys::pfree` a `PgVarlena` if we allocated it, instead of Postgres
impl<T> Drop for PgVarlena<T>
where
    T: Copy + Sized,
{
    fn drop(&mut self) {
        if self.need_free {
            unsafe {
                // safe: self.varlena.ptr will never be null
                self.varlena.ptr.free();
            }
        }

        if let Some(leaked) = self.leaked {
            unsafe { drop(Box::from_raw(leaked)) }
        }
    }
}

impl<T> Deref for PgVarlena<T>
where
    T: Copy + Sized,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl<T> DerefMut for PgVarlena<T>
where
    T: Copy + Sized,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut()
    }
}

impl<T> AsRef<T> for PgVarlena<T>
where
    T: Copy + Sized,
{
    fn as_ref(&self) -> &T {
        unsafe {
            // safe: ptr will never be null
            let ptr = vardata_any(self.varlena.ptr) as *const T;
            ptr.as_ref().unwrap()
        }
    }
}

/// Does a copy-on-write if the backing varlena pointer is borrowed
impl<T> AsMut<T> for PgVarlena<T>
where
    T: Copy + Sized,
{
    fn as_mut(&mut self) -> &mut T {
        unsafe {
            // safe: ptr will never be null
            let ptr = vardata_any(self.varlena.to_mut().ptr) as *mut T;
            ptr.as_mut().unwrap()
        }
    }
}

impl<T> Into<Option<pg_sys::Datum>> for PgVarlena<T>
where
    T: Copy + Sized,
{
    fn into(self) -> Option<pg_sys::Datum> {
        Some(self.into_pg() as pg_sys::Datum)
    }
}

impl<T> IntoDatum for PgVarlena<T>
where
    T: Copy + Sized,
{
    fn into_datum(self) -> Option<pg_sys::Datum> {
        Some(self.into_pg() as pg_sys::Datum)
    }

    fn type_oid() -> pg_sys::Oid {
        rust_regtypein::<T>()
    }
}

impl<T> FromDatum for PgVarlena<T>
where
    T: Copy + Sized,
{
    unsafe fn from_datum(datum: pg_sys::Datum, is_null: bool, _typoid: u32) -> Option<Self> {
        if is_null {
            None
        } else {
            Some(PgVarlena::<T>::from_datum(datum))
        }
    }

    unsafe fn from_datum_in_memory_context(
        mut memory_context: PgMemoryContexts,
        datum: usize,
        is_null: bool,
        _typoid: u32,
    ) -> Option<Self> {
        if is_null {
            None
        } else if datum == 0 {
            panic!("a varlena Datum was flagged as non-null but the datum is zero");
        } else {
            memory_context.switch_to(|_| {
                // this gets the varlena Datum copied into this memory context
                let detoasted = pg_sys::pg_detoast_datum_copy(PgPtr::from(datum));

                // and we need to unpack it (if necessary), which will decompress it too
                let varlena = pg_sys::pg_detoast_datum_packed(detoasted);

                // and now we return it as a &str
                Some(PgVarlena::<T>::from_datum(varlena as pg_sys::Datum))
            })
        }
    }
}

impl<T> IntoDatum for T
where
    T: PostgresType + Serialize,
{
    fn into_datum(self) -> Option<pg_sys::Datum> {
        Some(cbor_encode(&self) as pg_sys::Datum)
    }

    fn type_oid() -> u32 {
        crate::rust_regtypein::<T>()
    }
}

impl<'de, T> FromDatum for T
where
    T: PostgresType + Deserialize<'de>,
{
    unsafe fn from_datum(datum: pg_sys::Datum, is_null: bool, _typoid: u32) -> Option<Self> {
        if is_null {
            None
        } else {
            cbor_decode(PgPtr::from(datum))
        }
    }

    unsafe fn from_datum_in_memory_context(
        memory_context: PgMemoryContexts,
        datum: pg_sys::Datum,
        is_null: bool,
        _typoid: u32,
    ) -> Option<Self> {
        if is_null {
            None
        } else {
            cbor_decode_into_context(memory_context, PgPtr::from(datum))
        }
    }
}

fn cbor_encode<T>(input: T) -> *const pg_sys::varlena
where
    T: Serialize,
{
    let mut serialized = StringInfo::new();

    serialized.push_bytes(&[0u8; pg_sys::VARHDRSZ]); // reserve space fo the header
    serde_cbor::to_writer(&mut serialized, &input).expect("failed to encode as CBOR");

    let size = serialized.len() as usize;
    let varlena = serialized.into_char_ptr();
    unsafe {
        set_varsize(PgPtr::from_raw(varlena).cast(), size as i32);
    }

    varlena as *const pg_sys::varlena
}

pub unsafe fn cbor_decode<'de, T>(input: PgPtr<pg_sys::varlena>) -> T
where
    T: Deserialize<'de>,
{
    let varlena = pg_sys::pg_detoast_datum_packed(input);
    let len = varsize_any_exhdr(varlena);
    let data = vardata_any(varlena);
    let slice = std::slice::from_raw_parts(data as *const u8, len);
    serde_cbor::from_slice(slice).expect("failed to decode CBOR")
}

pub unsafe fn cbor_decode_into_context<'de, T>(
    mut memory_context: PgMemoryContexts,
    input: PgPtr<pg_sys::varlena>,
) -> T
where
    T: Deserialize<'de>,
{
    memory_context.switch_to(|_| {
        // this gets the varlena Datum copied into this memory context
        let varlena = pg_sys::pg_detoast_datum_copy(input);
        cbor_decode(varlena)
    })
}

#[allow(dead_code)]
fn json_encode<T>(input: T) -> *const varlena
where
    T: Serialize,
{
    let mut serialized = StringInfo::new();

    serialized.push_bytes(&[0u8; pg_sys::VARHDRSZ]); // reserve space fo the header
    serde_json::to_writer(&mut serialized, &input).expect("failed to encode as JSON");

    let size = serialized.len() as usize;
    let varlena = serialized.into_char_ptr();
    unsafe {
        set_varsize(PgPtr::from_raw(varlena).cast(), size as i32);
    }

    varlena as *const pg_sys::varlena
}

#[allow(dead_code)]
unsafe fn json_decode<'de, T>(input: PgPtr<pg_sys::varlena>) -> T
where
    T: Deserialize<'de>,
{
    let varlena = pg_sys::pg_detoast_datum_packed(input);
    let len = varsize_any_exhdr(varlena);
    let data = vardata_any(varlena);
    let slice = std::slice::from_raw_parts(data as *const u8, len);
    serde_json::from_slice(slice).expect("failed to decode JSON")
}
