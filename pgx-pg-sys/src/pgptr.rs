use crate as pg_sys;
use pgx_macros::*;
use std::fmt::{Display, Formatter};
use std::ops::{Deref, DerefMut};

#[inline]
pub(crate) unsafe fn palloc<T>(size: crate::Size) -> PgPtr<T> {
    #[pg_guard]
    extern "C" {
        fn palloc(size: crate::Size) -> *mut std::os::raw::c_void;
    }

    PgPtr(palloc(size) as *mut T)
}

#[inline]
pub(crate) unsafe fn palloc0<T>(size: crate::Size) -> PgPtr<T> {
    #[pg_guard]
    extern "C" {
        fn palloc0(size: crate::Size) -> *mut std::os::raw::c_void;
    }

    PgPtr(palloc0(size) as *mut T)
}

unsafe fn pfree<T>(mut ptr: PgPtr<T>) {
    #[pg_guard]
    extern "C" {
        fn pfree(ptr: *mut std::os::raw::c_void);
    }

    pfree(ptr.as_mut_ptr() as *mut std::os::raw::c_void)
}

pub trait New {
    fn new() -> Self;
    fn new0() -> Self;
}

#[repr(transparent)]
pub struct PgPtr<T>(pub(crate) *const T);

impl<T> PgPtr<T> {
    pub fn null_mut() -> PgPtr<T> {
        PgPtr(std::ptr::null_mut())
    }

    pub fn from_raw(ptr: *const T) -> PgPtr<T> {
        PgPtr(ptr)
    }

    pub fn array(len: usize) -> &'static mut [T] {
        unsafe {
            std::slice::from_raw_parts_mut(
                palloc::<T>(std::mem::size_of::<T>() * len).as_mut_ptr(),
                len,
            )
        }
    }

    pub fn array0(len: usize) -> &'static mut [T] {
        unsafe {
            std::slice::from_raw_parts_mut(
                palloc0::<T>(std::mem::size_of::<T>() * len).as_mut_ptr(),
                len,
            )
        }
    }

    // pub fn new() -> PgPtr<T> {
    //     unsafe { palloc(std::mem::size_of::<T>()) }
    // }
    //
    // pub fn new0() -> PgPtr<T> {
    //     unsafe { palloc0(std::mem::size_of::<T>()) }
    // }

    pub fn with_extra_len(len: usize) -> PgPtr<T> {
        unsafe { palloc(std::mem::size_of::<T>() + len) }
    }

    pub fn with_extra_len0(len: usize) -> PgPtr<T> {
        unsafe { palloc0(std::mem::size_of::<T>() + len) }
    }

    pub fn cast<C>(&self) -> PgPtr<C> {
        PgPtr(self.0 as *const C)
    }

    pub fn is_null(&self) -> bool {
        self.0 == std::ptr::null_mut()
    }

    pub fn as_ptr(&self) -> *const T {
        self.0
    }

    pub fn as_mut_ptr(&mut self) -> *mut T {
        self.0 as *mut T
    }

    pub fn free(self) {
        unsafe { pfree(self) }
    }
}

impl<T> Copy for PgPtr<T> {}

impl<T> Clone for PgPtr<T> {
    fn clone(&self) -> Self {
        PgPtr(self.0)
    }
}

impl Display for PgPtr<::std::os::raw::c_char> {
    fn fmt(&self, fmt: &mut Formatter<'_>) -> std::fmt::Result {
        write!(fmt, "{}", &self)
    }
}

impl<T> From<pg_sys::Datum> for PgPtr<T> {
    fn from(datum: pg_sys::Datum) -> Self {
        PgPtr(datum as *const T)
    }
}

impl AsRef<str> for PgPtr<::std::os::raw::c_char> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl PgPtr<::std::os::raw::c_char> {
    pub fn as_str(&self) -> &'static str {
        unsafe {
            let cstr = std::ffi::CStr::from_ptr(self.as_ptr());
            cstr.to_str().expect("not a valid UTF string")
        }
    }
}

pub trait IntoPgPtr {
    type Target;
    fn into_pg(self) -> PgPtr<Self::Target>;
}

impl<T> IntoPgPtr for &'static mut [T] {
    type Target = T;

    fn into_pg(self) -> PgPtr<Self::Target> {
        PgPtr(self.as_ptr() as *const T)
    }
}

impl<T> Deref for PgPtr<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref().expect("attempt to Deref null PgPtr") }
    }
}

impl<T> DerefMut for PgPtr<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe {
            (self.0 as *mut T)
                .as_mut()
                .expect("attempt to DerefMut null PgPtr")
        }
    }
}
