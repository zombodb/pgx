// Copyright 2020 ZomboDB, LLC <zombodb@gmail.com>. All rights reserved. Use of this source code is
// governed by the MIT license that can be found in the LICENSE file.

//!
//! Provides interfacing into Postgres' `MemoryContext` system.
//!
//! The `PgBox<T>` projects Postgres-allocated memory pointers as if they're first-class Rust types.
//!
//! An enum-based interface (`PgMemoryContexts`) around Postgres' various `MemoryContext`s provides
//! simple accessibility to working with MemoryContexts in a compiler-checked manner
//!
use crate::pg_sys::{AsPgCStr, PgPtr};
use crate::{guard, pg_sys, PgBox};
use std::fmt::Debug;

/// A shorter type name for a `*const std::os::raw::c_void`
#[allow(non_camel_case_types)]
pub type void_ptr = *const std::os::raw::c_void;

/// A shorter type name for a `*mut std::os::raw::c_void`
#[allow(non_camel_case_types)]
pub type void_mut_ptr = *mut std::os::raw::c_void;

/// An Enumeration of Postgres top-level MemoryContexts.  Each have their own use and "lifetimes"
/// as defined by Postgres' memory management model.
///
/// It's possible to deference any one of these (except `Transient`) via the `::value()` method if
/// it's necessary to pass the raw pointer to a Postgres function.
///
/// Additionally, the `::switch_to()` function, which takes a closure as its argument, executes the
/// closure within that MemoryContext
#[derive(Debug)]
pub enum PgMemoryContexts {
    /// Because it would be too much notational overhead to always pass an
    /// appropriate memory context to called routines, there always exists the
    /// notion of the current memory context CurrentMemoryContext.  Without it,
    /// for example, the copyObject routines would need to be passed a context, as
    /// would function execution routines that return a pass-by-reference
    /// datatype.  Similarly for routines that temporarily allocate space
    /// internally, but don't return it to their caller?  We certainly don't
    /// want to clutter every call in the system with "here is a context to
    /// use for any temporary memory allocation you might want to do".
    ///
    /// The upshot of that reasoning, though, is that CurrentMemoryContext should
    /// generally point at a short-lifespan context if at all possible.  During
    /// query execution it usually points to a context that gets reset after each
    /// tuple.  Only in *very* circumscribed code should it ever point at a
    /// context having greater than transaction lifespan, since doing so risks
    /// permanent memory leaks.
    CurrentMemoryContext,

    /// this is the actual top level of the context tree;
    /// every other context is a direct or indirect child of this one.  Allocating
    /// here is essentially the same as "malloc", because this context will never
    /// be reset or deleted.  This is for stuff that should live forever, or for
    /// stuff that the controlling module will take care of deleting at the
    /// appropriate time.  An example is fd.c's tables of open files.  Avoid
    /// allocating stuff here unless really necessary, and especially avoid
    /// running with CurrentMemoryContext pointing here.
    TopMemoryContext,

    /// this is not actually a separate context, but a
    /// global variable pointing to the per-portal context of the currently active
    /// execution portal.  This can be used if it's necessary to allocate storage
    /// that will live just as long as the execution of the current portal requires.
    PortalContext,

    /// this permanent context is switched into for error
    /// recovery processing, and then reset on completion of recovery.  We arrange
    /// to have a few KB of memory available in it at all times.  In this way, we
    /// can ensure that some memory is available for error recovery even if the
    /// backend has run out of memory otherwise.  This allows out-of-memory to be
    /// treated as a normal ERROR condition, not a FATAL error.
    ErrorContext,

    /// this is the postmaster's normal working context.
    /// After a backend is spawned, it can delete PostmasterContext to free its
    /// copy of memory the postmaster was using that it doesn't need.
    /// Note that in non-EXEC_BACKEND builds, the postmaster's copy of pg_hba.conf
    /// and pg_ident.conf data is used directly during authentication in backend
    /// processes; so backends can't delete PostmasterContext until that's done.
    /// (The postmaster has only TopMemoryContext, PostmasterContext, and
    /// ErrorContext --- the remaining top-level contexts are set up in each
    /// backend during startup.)
    PostmasterContext,

    /// permanent storage for relcache, catcache, and
    /// related modules.  This will never be reset or deleted, either, so it's
    /// not truly necessary to distinguish it from TopMemoryContext.  But it
    /// seems worthwhile to maintain the distinction for debugging purposes.
    /// (Note: CacheMemoryContext has child contexts with shorter lifespans.
    /// For example, a child context is the best place to keep the subsidiary
    /// storage associated with a relcache entry; that way we can free rule
    /// parsetrees and so forth easily, without having to depend on constructing
    /// a reliable version of freeObject().)
    CacheMemoryContext,

    /// this context holds the current command message from the
    /// frontend, as well as any derived storage that need only live as long as
    /// the current message (for example, in simple-Query mode the parse and plan
    /// trees can live here).  This context will be reset, and any children
    /// deleted, at the top of each cycle of the outer loop of PostgresMain.  This
    /// is kept separate from per-transaction and per-portal contexts because a
    /// query string might need to live either a longer or shorter time than any
    /// single transaction or portal.
    MessageContext,

    /// this holds everything that lives until end of the
    /// top-level transaction.  This context will be reset, and all its children
    /// deleted, at conclusion of each top-level transaction cycle.  In most cases
    /// you don't want to allocate stuff directly here, but in CurTransactionContext;
    /// what does belong here is control information that exists explicitly to manage
    /// status across multiple subtransactions.  Note: this context is NOT cleared
    /// immediately upon error; its contents will survive until the transaction block
    /// is exited by COMMIT/ROLLBACK.
    TopTransactionContext,

    /// this holds data that has to survive until the end
    /// of the current transaction, and in particular will be needed at top-level
    /// transaction commit.  When we are in a top-level transaction this is the same
    /// as TopTransactionContext, but in subtransactions it points to a child context.
    /// It is important to understand that if a subtransaction aborts, its
    /// CurTransactionContext is thrown away after finishing the abort processing;
    /// but a committed subtransaction's CurTransactionContext is kept until top-level
    /// commit (unless of course one of the intermediate levels of subtransaction
    /// aborts).  This ensures that we do not keep data from a failed subtransaction
    /// longer than necessary.  Because of this behavior, you must be careful to clean
    /// up properly during subtransaction abort --- the subtransaction's state must be
    /// delinked from any pointers or lists kept in upper transactions, or you will
    /// have dangling pointers leading to a crash at top-level commit.  An example of
    /// data kept here is pending NOTIFY messages, which are sent at top-level commit,
    /// but only if the generating subtransaction did not abort.
    CurTransactionContext,

    /// This represents a MemoryContext that was likely created via
    /// [pg_sys::AllocSetContextCreateExtended].
    ///
    /// That could be a MemoryContext you created yourself, or it could be one given to you from
    /// Postgres.  For example, the `TupleTableSlot` struct has a field referencing the MemoryContext
    /// in which slots are allocated.
    For(pg_sys::MemoryContext),

    /// A MemoryContext owned by Rust that will be freed when when Dropped
    Owned(OwnedMemoryContext),

    /// Use the MemoryContext in which the specified pointer was allocated.
    ///
    /// It's incredibly important that the specified pointer be one actually allocated by
    /// Postgres' memory management system.  Otherwise, it's undefined behavior and will
    /// **absolutely** crash Postgres
    Of(void_ptr),

    /// Create a temporary MemoryContext for use with [::switch_to()].  It gets deleted as soon
    /// as [::switch_to()] exits.
    ///
    /// Trying to use this context through [::value{}] will result in a panic!().
    Transient {
        parent: pg_sys::MemoryContext,
        name: &'static str,
        min_context_size: u32,
        initial_block_size: u32,
        max_block_size: u32,
    },
}

/// A `pg_sys::MemoryContext` that is owned by `PgMemoryContexts::Owned`
#[derive(Debug)]
pub struct OwnedMemoryContext(pg_sys::MemoryContext);

impl Drop for OwnedMemoryContext {
    fn drop(&mut self) {
        unsafe {
            pg_sys::MemoryContextDelete(self.0);
        }
    }
}

impl PgMemoryContexts {
    /// Create a new `PgMemoryContext::Owned`
    pub fn new(name: &str) -> PgMemoryContexts {
        PgMemoryContexts::Owned(OwnedMemoryContext(unsafe {
            pg_sys::AllocSetContextCreateExtended(
                PgMemoryContexts::CurrentMemoryContext.value(),
                name.as_pg_cstr(),
                pg_sys::ALLOCSET_DEFAULT_MINSIZE as usize,
                pg_sys::ALLOCSET_DEFAULT_INITSIZE as usize,
                pg_sys::ALLOCSET_DEFAULT_MAXSIZE as usize,
            )
        }))
    }

    /// Retrieve the underlying Postgres `*mut MemoryContextData`
    ///
    /// This works for every type except the `::Transient` type.
    pub fn value(&self) -> pg_sys::MemoryContext {
        match self {
            PgMemoryContexts::CurrentMemoryContext => unsafe { pg_sys::CurrentMemoryContext },
            PgMemoryContexts::TopMemoryContext => unsafe { pg_sys::TopMemoryContext },
            PgMemoryContexts::PortalContext => unsafe { pg_sys::PortalContext },
            PgMemoryContexts::ErrorContext => unsafe { pg_sys::ErrorContext },
            PgMemoryContexts::PostmasterContext => unsafe { pg_sys::PostmasterContext },
            PgMemoryContexts::CacheMemoryContext => unsafe { pg_sys::CacheMemoryContext },
            PgMemoryContexts::MessageContext => unsafe { pg_sys::MessageContext },
            PgMemoryContexts::TopTransactionContext => unsafe { pg_sys::TopTransactionContext },
            PgMemoryContexts::CurTransactionContext => unsafe { pg_sys::CurTransactionContext },
            PgMemoryContexts::For(mc) => *mc,
            PgMemoryContexts::Owned(mc) => mc.0,
            PgMemoryContexts::Of(ptr) => PgMemoryContexts::get_context_for_pointer(*ptr),
            PgMemoryContexts::Transient { .. } => {
                panic!("cannot use value() to retrieve a Transient PgMemoryContext")
            }
        }
    }

    /// Set this MemoryContext as the `CurrentMemoryContext, returning whatever `CurrentMemoryContext` is
    pub fn set_as_current(&self) -> PgMemoryContexts {
        unsafe {
            let old_context = pg_sys::CurrentMemoryContext;

            pg_sys::CurrentMemoryContext = self.value();

            PgMemoryContexts::For(old_context)
        }
    }

    /// Release all space allocated within a context and delete all its descendant contexts (but not
    /// the context itself).
    pub fn reset(&mut self) {
        unsafe {
            pg_sys::MemoryContextReset(self.value());
        }
    }

    /// Run the specified function "within" the `MemoryContext` represented by this enum.
    ///
    /// The important implementation detail is that Postgres' `CurrentMemoryContext` is changed
    /// to be this context, the function is run so that all Postgres memory allocations happen
    /// within that context, and then `CurrentMemoryContext` is restored to what it was before
    /// we started.
    ///
    /// ## Examples
    ///
    /// ```rust,no_run
    /// use pgx::*;
    ///
    /// #[pg_guard]
    /// pub fn do_something() -> pg_sys::ItemPointer {
    ///     PgMemoryContexts::TopTransactionContext.switch_to(|context| {
    ///         // allocate a new ItemPointerData, but inside the TopTransactionContext
    ///         let tid = PgBox::<pg_sys::ItemPointerData>::alloc();
    ///         
    ///         // do something with the tid and then return it.
    ///         // Note that it stays allocated here in the TopTransactionContext
    ///         tid.into_pg()
    ///     })
    /// }
    /// ```
    pub fn switch_to<
        R,
        F: Fn(&mut PgMemoryContexts) -> R + std::panic::UnwindSafe + std::panic::RefUnwindSafe,
    >(
        self,
        f: F,
    ) -> R {
        match self {
            PgMemoryContexts::Transient {
                parent,
                name,
                min_context_size,
                initial_block_size,
                max_block_size,
            } => {
                let context = unsafe {
                    pg_sys::AllocSetContextCreateExtended(
                        parent,
                        name.as_pg_cstr(),
                        *min_context_size as usize,
                        *initial_block_size as usize,
                        *max_block_size as usize,
                    )
                };

                let result = PgMemoryContexts::exec_in_context(context, f);

                unsafe {
                    pg_sys::MemoryContextDelete(context);
                }

                result
            }
            _ => PgMemoryContexts::exec_in_context(self.value(), f),
        }
    }

    /// Duplicate a Rust `&str` into a Postgres-allocated "char *"
    ///
    /// ## Examples
    ///
    /// ```rust,no_run
    /// use pgx::PgMemoryContexts;
    /// let copy = PgMemoryContexts::CurrentMemoryContext.pstrdup("make a copy of this");
    /// ```
    pub fn pstrdup(&self, s: &str) -> PgPtr<std::os::raw::c_char> {
        unsafe { pg_sys::MemoryContextStrdup(self.value(), s.as_pg_cstr()) }
    }

    /// Copies `len` bytes, starting at `src` into this memory context and
    /// returns a raw `*mut T` pointer to the newly allocated location
    pub fn copy_ptr_into<T>(&mut self, src: PgPtr<T>, len: usize) -> PgPtr<T> {
        if src.is_null() {
            panic!("attempt to copy a null pointer");
        }
        unsafe {
            let dest = pg_sys::MemoryContextAlloc(self.value(), len);
            pg_sys::memcpy(dest, src.cast(), len as u64);
            dest
        }
    }

    pub fn leak_and_drop_on_delete<T>(&mut self, v: T) -> PgPtr<T> {
        unsafe extern "C" fn drop_on_delete<T>(ptr: PgPtr<std::os::raw::c_void>) {
            let boxed = Box::from_raw(ptr as *mut T);
            drop(boxed);
        }

        let leaked_ptr = Box::leak(Box::new(v));
        let mut memcxt_callback = PgPtr::<pg_sys::MemoryContextCallback>::new();

        memcxt_callback.func = Some(drop_on_delete::<T>);
        memcxt_callback.arg = PgPtr::from_raw(leaked_ptr).cast();
        unsafe {
            pg_sys::MemoryContextRegisterResetCallback(self.value(), memcxt_callback.into_pg());
        }
        memcxt_callback.arg
    }

    /// helper function
    fn exec_in_context<
        R,
        F: Fn(&mut PgMemoryContexts) -> R + std::panic::UnwindSafe + std::panic::RefUnwindSafe,
    >(
        context: pg_sys::MemoryContext,
        f: F,
    ) -> R {
        let prev_context;

        // mimic what palloc.h does for switching memory contexts
        unsafe {
            prev_context = pg_sys::CurrentMemoryContext;
            pg_sys::CurrentMemoryContext = context;
        }

        let result = guard::guard(|| f(&mut PgMemoryContexts::For(context)));

        // restore our understanding of the current memory context
        unsafe {
            pg_sys::CurrentMemoryContext = prev_context;
        }

        result
    }

    ///
    /// GetMemoryChunkContext
    ///    Given a currently-allocated chunk, determine the context
    ///         it belongs to.
    ///
    /// All chunks allocated by any memory context manager are required to be
    /// preceded by the corresponding MemoryContext stored, without padding, in the
    /// preceding sizeof(void*) bytes.  A currently-allocated chunk must contain a
    /// backpointer to its owning context.  The backpointer is used by pfree() and
    /// repalloc() to find the context to call.
    ///
    fn get_context_for_pointer(ptr: void_ptr) -> pg_sys::MemoryContext {
        extern "C" {
            pub fn pgx_GetMemoryContextChunk(pointer: void_ptr) -> pg_sys::MemoryContext;
        }
        unsafe { pgx_GetMemoryContextChunk(ptr) }
    }
}
