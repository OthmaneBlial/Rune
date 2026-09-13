//! Narrow C ABI for the native Apple frontend.
//!
//! Unsafe code is intentionally confined to this boundary. The portable
//! crates remain safe Rust; Swift owns the session handle on its main actor
//! and must release every returned string with [`rune_string_free`].

#![allow(unsafe_code)]

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use rune_core::{CommandOutput, Session};
use rune_fs::SandboxedFileSystem;

/// An owned result crossing the C ABI.
#[repr(C)]
pub struct RuneOutput {
    pub stdout: *mut c_char,
    pub stderr: *mut c_char,
    pub status: i32,
}

struct RuneSession {
    core: Session,
}

fn read_string(pointer: *const c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: callers pass a NUL-terminated C string; invalid UTF-8 is
    // rejected instead of being guessed or copied unsafely.
    unsafe { CStr::from_ptr(pointer).to_str().ok().map(str::to_owned) }
}

fn into_owned_c_string(value: &str) -> *mut c_char {
    let sanitized = value.replace('\0', "�");
    match CString::new(sanitized) {
        Ok(value) => value.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

fn into_output(output: &CommandOutput) -> RuneOutput {
    RuneOutput {
        stdout: into_owned_c_string(&output.stdout),
        stderr: into_owned_c_string(&output.stderr),
        status: output.status,
    }
}

/// Creates a session rooted at the supplied physical sandbox directory.
///
/// A null return means the pointer was null/invalid or the root could not be
/// initialized. The root is still policy-checked by `rune-fs`.
#[no_mangle]
pub extern "C" fn rune_session_new(root: *const c_char) -> *mut std::ffi::c_void {
    let Some(root) = read_string(root) else {
        return std::ptr::null_mut();
    };
    let Ok(filesystem) = SandboxedFileSystem::new(root) else {
        return std::ptr::null_mut();
    };
    let session = Box::new(RuneSession {
        core: Session::restore(filesystem),
    });
    Box::into_raw(session).cast()
}

/// Destroys a handle created by [`rune_session_new`].
#[no_mangle]
pub extern "C" fn rune_session_destroy(handle: *mut std::ffi::c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: the pointer came from Box::into_raw in rune_session_new and is
    // consumed at most once by the Swift owner.
    unsafe {
        let mut session = Box::from_raw(handle.cast::<RuneSession>());
        let _ = session.core.persist();
        drop(session);
    };
}

/// Executes one Rune command line.
#[no_mangle]
pub extern "C" fn rune_session_execute(
    handle: *mut std::ffi::c_void,
    input: *const c_char,
) -> RuneOutput {
    let Some(input) = read_string(input) else {
        let output = CommandOutput::failure(2, "rune: input is not valid UTF-8\n");
        return into_output(&output);
    };
    if handle.is_null() {
        let output = CommandOutput::failure(1, "rune: session is unavailable\n");
        return into_output(&output);
    }
    // SAFETY: Swift serializes access to the opaque session handle and does
    // not call this after rune_session_destroy.
    let session = unsafe { &mut *handle.cast::<RuneSession>() };
    let output = session.core.execute_line(&input);
    into_output(&output)
}

/// Returns the current virtual directory as an owned C string.
#[no_mangle]
pub extern "C" fn rune_session_current_directory(handle: *const std::ffi::c_void) -> *mut c_char {
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the pointer is read-only and owned by the Swift session.
    let session = unsafe { &*handle.cast::<RuneSession>() };
    let directory = session.core.current_directory();
    into_owned_c_string(&directory)
}

/// Releases a string returned by Rune's C ABI.
///
/// # Safety
///
/// `value` must be null or a pointer returned by one of Rune's string-returning
/// functions, and it must be released at most once.
#[no_mangle]
pub unsafe extern "C" fn rune_string_free(value: *mut c_char) {
    if value.is_null() {
        return;
    }
    // SAFETY: value was allocated by CString::into_raw in this crate.
    unsafe { drop(CString::from_raw(value)) };
}
