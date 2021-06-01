// Copyright 2020 ZomboDB, LLC <zombodb@gmail.com>. All rights reserved. Use of this source code is
// governed by the MIT license that can be found in the LICENSE file.

#![allow(non_snake_case)]

use crate::FlushErrorState;
use std::any::Any;
use std::cell::Cell;
use std::panic::catch_unwind;

extern "C" {
    fn pg_re_throw();
    fn pgx_ereport(
        level: i32,
        code: i32,
        message: *const std::os::raw::c_char,
        file: *const std::os::raw::c_char,
        lineno: i32,
        colno: i32,
    );
}

#[derive(Clone, Debug)]
pub struct JumpContext {}

#[derive(Debug, Clone, Copy)]
pub struct PgxPanic {
    pub message: &'static str,
    pub filename: &'static str,
    pub lineno: u32,
    pub colno: u32,
}

impl PgxPanic {
    pub fn new(message: &'static str, filename: &'static str, lineno: u32, colno: u32) -> Self {
        PgxPanic {
            message,
            filename,
            lineno,
            colno,
        }
    }
}

struct PanicLocation {
    file: String,
    line: u32,
    col: u32,
}

thread_local! { static PANIC_LOCATION: Cell<Option<PanicLocation>> = Cell::new(None) }

fn take_panic_location() -> PanicLocation {
    PANIC_LOCATION.with(|p| match p.take() {
        Some(location) => location,

        // this case shouldn't happen
        None => PanicLocation {
            file: "<unknown>".to_string(),
            line: 0,
            col: 0,
        },
    })
}

// via pg_module_magic!() this gets set to Some(()) for the "main" thread, and remains at None
// for all other threads.
#[cfg(debug_assertions)]
thread_local! { pub(crate) static MAIN_THREAD: once_cell::sync::OnceCell<()> = once_cell::sync::OnceCell::new() }

/// Return true when called from the same thread that executed [pg_module_magic!()]
///
/// This function is the "public" API that is useful for checking that
/// concurrency invariants have been satisfied, e.g. no concurrency.
#[cfg(debug_assertions)]
pub(crate) fn is_main_thread() -> bool {
    MAIN_THREAD.with(|v| v.get().is_some()
}

pub fn register_pg_guard_panic_handler() {
    // first, lets ensure we're not calling ourselves twice
    #[cfg(debug_assertions)]
    {
        if MAIN_THREAD.with(|v| v.get().is_some()) {
            panic!("MAIN_THREAD has already been set")
        }

        // it's expected that this function will only ever be called by `pg_module_magic!()` by the main thread
        MAIN_THREAD.with(|v| v.set(()).expect("failed to set main thread sentinel"));
    }

    std::panic::set_hook(Box::new(|info| {
        #[cfg(debug_assertions)]
        {
            if MAIN_THREAD.with(|v| v.get().is_none()) {
                // a thread that isn't the main thread panic!()d
                // we make a best effort to push a message to stderr, which hopefully
                // Postgres is logging somewhere
                eprintln!(
                    "thread={:?}, id={:?}, {}",
                    std::thread::current().name(),
                    std::thread::current().id(),
                    info
                );
            }
        }

        PANIC_LOCATION.with(|p| {
            let existing = p.take();

            p.replace(if existing.is_none() {
                match info.location() {
                    Some(location) => Some(PanicLocation {
                        file: location.file().to_string(),
                        line: location.line(),
                        col: location.column(),
                    }),
                    None => None,
                }
            } else {
                existing
            })
        });
    }))
}

/// A `std::result::Result`-type value returned from `pg_try()` that allows for performing cleanup
/// work after a closure raised an error and before it is possibly rethrown
#[must_use = "this `PgTryResult` may be be holding a Postgres ERROR.  It must be consumed or rethrown"]
pub struct PgTryResult<T>(std::thread::Result<T>);

impl<T> PgTryResult<T> {
    /// Retrieve the returned value or panic if the try block raised an error
    pub fn unwrap(self) -> T {
        self.unwrap_or_rethrow(|| {})
    }

    /// ## Safety
    ///
    /// This function is unsafe because you might be ignoring a caught Postgres ERROR (or Rust panic)
    /// and you better know what you're doing when you do that.  
    ///
    /// Doing so can potentially leave Postgres in an undefined state and ultimately cause it
    /// to crash.
    pub unsafe fn unwrap_or(self, value: T) -> T {
        match self.0 {
            Ok(result) => result,
            Err(_) => {
                FlushErrorState();
                value
            }
        }
    }

    /// Perform some operation cleanup operation after the try block if an error was thrown.
    ///
    /// ## Safety
    ///
    /// This function does not rethrow a caught ERROR.  You better know what you're doing when you
    /// call this function.
    ///
    /// Ignoring a caught error can leave Postgres in an undefined state and ultimately cause it
    /// to crash.
    pub unsafe fn unwrap_or_else<F>(self, cleanup: F) -> T
    where
        F: FnOnce() -> T,
    {
        match self.0 {
            Ok(result) => result,
            Err(_) => {
                FlushErrorState();
                cleanup()
            }
        }
    }

    /// Perform some operation cleanup operation after the try block if an error was thrown.
    ///
    /// In the event an error was caught, it is rethrown.
    pub fn unwrap_or_rethrow<F>(self, cleanup: F) -> T
    where
        F: FnOnce(),
    {
        match self.0 {
            Ok(result) => result,
            Err(e) => {
                catch_guard(e, cleanup);
                unreachable!("failed to rethrow ERROR during pg_try().unwrap_or_rethrow()")
            }
        }
    }

    /// Perform some operation after the try block completes, regardless of if an error was thrown.
    ///
    /// In the event an error was caught, it is rethrown.  Otherwise, the return value from the try
    /// block is returned
    pub fn finally_or_rethrow<F>(self, finally_block: F) -> T
    where
        F: FnOnce(),
    {
        match self.0 {
            Ok(result) => {
                finally_block();
                result
            }
            Err(e) => {
                catch_guard(e, finally_block);
                unreachable!("failed to rethrow ERROR during pg_try().finally_or_rethrow()")
            }
        }
    }
}

/// Guard a closure such that Rust Panics are properly converted into Postgres ERRORs
///
/// Generally, this function won't need to be used directly, as it's also the implementation
/// behind the `#[pg_guard]` and `#[pg_extern]` macros.  Which means the function you'd like to guard
/// is likely already guarded.
///
/// This function is re-entrant and will properly "bubble-up" panics or errors to the top-level
/// before they're converted into Postgres ERRORs
pub fn guard<Func, R>(f: Func) -> R
where
    Func: FnOnce() -> R + std::panic::UnwindSafe + std::panic::RefUnwindSafe,
{
    pg_try(f).unwrap()
}

/// Similar to `guard`, but allows the caller to unwrap the result in various ways, possibly
/// performing cleanup work before the caught error is rethrown
pub fn pg_try<Try, R>(try_func: Try) -> PgTryResult<R>
where
    Try: FnOnce() -> R + std::panic::UnwindSafe + std::panic::RefUnwindSafe,
{
    try_guard(try_func)
}

fn try_guard<Try, R>(try_func: Try) -> PgTryResult<R>
where
    Try: FnOnce() -> R + std::panic::UnwindSafe + std::panic::RefUnwindSafe,
{
    // run try_func() in a catch_unwind, as we never want a Rust panic! to leak
    // from this function.  It's imperative that we nevery try to panic! across
    // FFI (extern "C") function boundaries
    let result = catch_unwind(try_func);

    // return our result -- it could be Ok(), or it could be an Err()
    PgTryResult(result)
}

fn catch_guard<Catch>(error: Box<dyn Any + std::marker::Send>, catch_func: Catch)
where
    Catch: FnOnce(),
{
    // call our catch function to do any cleanup work that might be necessary
    // before we end up rethrowing the error
    catch_func();

    // determine how to rethrow the error
    match downcast_err(error) {
        // the error is a String, which means it was originally a Rust panic!(), so
        // translate it into an elog(ERROR), including the code location that caused
        // the panic!()
        Ok(message) => {
            let location = take_panic_location();
            let c_message = std::ffi::CString::new(message.clone()).unwrap();
            let c_file = std::ffi::CString::new(location.file).unwrap();

            unsafe {
                pgx_ereport(
                    crate::ERROR as i32,
                    2600, // ERRCODE_INTERNAL_ERROR
                    c_message.as_ptr(),
                    c_file.as_ptr(),
                    location.line as i32,
                    location.col as i32,
                );
            }
            unreachable!("ereport() failed at depth==0 with message: {}", message);
        }

        // the error is a JumpContext, so we need to longjmp back into Postgres
        Err(_) => unsafe {
            pg_re_throw();
            unreachable!("siglongjmp failed");
        },
    }
}

/// convert types of `e` that we understand/expect into either a
/// `Ok(String)` or a `Err<JumpContext>`
fn downcast_err(e: Box<dyn Any + Send>) -> Result<String, JumpContext> {
    if let Some(cxt) = e.downcast_ref::<JumpContext>() {
        Err(cxt.clone())
    } else if let Some(s) = e.downcast_ref::<&str>() {
        Ok((*s).to_string())
    } else if let Some(s) = e.downcast_ref::<String>() {
        Ok(s.to_string())
    } else if let Some(s) = e.downcast_ref::<PgxPanic>() {
        Ok(format!(
            "{}: {}:{}:{}",
            s.message, s.filename, s.lineno, s.colno
        ))
    } else {
        // not a type we understand, so use a generic string
        Ok("Box<Any>".to_string())
    }
}
