use crate::PgPtr;
use std::marker::PhantomData;
use std::ops::Deref;

impl PgPtr<crate::List> {
    pub fn new<T>() -> PgPtr<crate::List> {
        PgPtr::null_mut()
    }

    #[inline]
    pub fn len(&self) -> i32 {
        if self.is_null() {
            0
        } else {
            self.length
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn get_ptr<'a, T: 'a>(&'a self, i: i32) -> Option<&'a T> {
        if i >= self.len() {
            None
        } else {
            unsafe { Some(crate::pgx_list_nth(*self, i).cast::<T>().deref()) }
        }
    }

    #[inline]
    pub fn get_i32(&self, i: i32) -> Option<i32> {
        if i >= self.len() {
            None
        } else {
            unsafe { Some(crate::pgx_list_nth_int(*self, i)) }
        }
    }

    #[inline]
    pub fn get_oid(&self, i: i32) -> Option<crate::Oid> {
        if i >= self.len() {
            None
        } else {
            unsafe { Some(crate::pgx_list_nth_oid(*self, i)) }
        }
    }

    #[inline]
    pub fn head_ptr<T>(&self) -> Option<&T> {
        self.get_ptr(0)
    }

    #[inline]
    pub fn tail_ptr<T>(&self) -> Option<&T> {
        self.get_ptr(self.len() - 1)
    }

    #[inline]
    pub fn head_i32(&self) -> Option<i32> {
        self.get_i32(0)
    }

    #[inline]
    pub fn tail_i32(&self) -> Option<i32> {
        self.get_i32(self.len() - 1)
    }

    #[inline]
    pub fn head_oid(&self) -> Option<crate::Oid> {
        self.get_oid(0)
    }

    #[inline]
    pub fn tail_oid(&self) -> Option<crate::Oid> {
        self.get_oid(self.len() - 1)
    }

    #[inline]
    pub fn push_ptr<T>(&mut self, ptr: PgPtr<T>) {
        unsafe { self.0 = crate::lappend(PgPtr(self.0), ptr.cast()).0 }
    }

    #[inline]
    pub fn push_i32(&mut self, i: i32) {
        unsafe { self.0 = crate::lappend_int(PgPtr(self.0), i).0 }
    }

    #[inline]
    pub fn push_oid(&mut self, oid: crate::Oid) {
        unsafe { self.0 = crate::lappend_oid(PgPtr(self.0), oid).0 }
    }

    #[inline]
    pub fn pop_ptr<T>(&mut self) -> Option<PgPtr<T>> {
        match self.tail_ptr() {
            Some(tail) => {
                unsafe { self.0 = crate::list_truncate(PgPtr(self.0), (self.len() - 1) as i32).0 }
                Some(tail)
            }
            None => None,
        }
    }

    #[inline]
    pub fn pop_i32(&mut self) -> Option<i32> {
        match self.tail_i32() {
            Some(tail) => {
                unsafe { self.0 = crate::list_truncate(PgPtr(self.0), (self.len() - 1) as i32).0 }
                Some(tail)
            }
            None => None,
        }
    }

    #[inline]
    pub fn pop_oid(&mut self) -> Option<crate::Oid> {
        match self.tail_oid() {
            Some(tail) => {
                unsafe { self.0 = crate::list_truncate(PgPtr(self.0), (self.len() - 1) as i32).0 }
                Some(tail)
            }
            None => None,
        }
    }

    #[inline]
    pub fn replace_ptr<T>(&mut self, i: i32, with: PgPtr<T>) -> Option<PgPtr<T>> {
        match self.get_ptr(i) {
            Some(ptr) => unsafe {
                let mut cell = crate::pgx_list_nth_cell(*self, i);

                *cell.data.ptr_value.as_mut() = with.cast();
                Some(*ptr)
            },
            None => None,
        }
    }

    #[inline]
    pub fn replace_i32(&mut self, i: i32, with: i32) -> Option<i32> {
        match self.get_i32(i) {
            Some(int) => unsafe {
                let mut cell = crate::pgx_list_nth_cell(*self, i);

                *cell.data.int_value.as_mut() = with;
                Some(int)
            },
            None => None,
        }
    }

    #[inline]
    pub fn replace_oid(&mut self, i: i32, with: crate::Oid) -> Option<crate::Oid> {
        match self.get_oid(i) {
            Some(oid) => unsafe {
                let mut cell = crate::pgx_list_nth_cell(*self, i);

                *cell.data.oid_value.as_mut() = with;
                Some(oid)
            },
            None => None,
        }
    }

    #[inline]
    pub fn iter_ptr<'a, T: 'a>(&'a self) -> impl Iterator<Item = &'a T> {
        ListIteratorPtr {
            list: &self,
            pos: 0,
            __marker: PhantomData,
        }
    }

    #[inline]
    pub fn iter_int(&self) -> impl Iterator<Item = i32> {
        ListIteratorInt {
            list: self.clone(),
            pos: 0,
        }
    }

    #[inline]
    pub fn iter_oid(&self) -> impl Iterator<Item = crate::Oid> {
        ListIteratorOid {
            list: self.clone(),
            pos: 0,
        }
    }
}

struct ListIteratorPtr<'a, T: 'a> {
    list: &'a PgPtr<crate::List>,
    pos: i32,
    __marker: PhantomData<&'a T>,
}

struct ListIteratorInt {
    list: PgPtr<crate::List>,
    pos: i32,
}

struct ListIteratorOid {
    list: PgPtr<crate::List>,
    pos: i32,
}

impl<'a, T: 'a> Iterator for ListIteratorPtr<'a, T> {
    type Item = &'a T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let result = self.list.get_ptr(self.pos);
        self.pos += 1;
        result
    }
}

impl Iterator for ListIteratorInt {
    type Item = i32;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let result = self.list.get_i32(self.pos);
        self.pos += 1;
        result
    }
}

impl Iterator for ListIteratorOid {
    type Item = crate::Oid;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let result = self.list.get_oid(self.pos);
        self.pos += 1;
        result
    }
}
