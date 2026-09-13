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

/// Executes a newline-delimited automation script through the same Rust
/// parser and command registry as interactive input.
#[no_mangle]
pub extern "C" fn rune_session_execute_script(
    handle: *mut std::ffi::c_void,
    script: *const c_char,
) -> RuneOutput {
    let Some(script) = read_string(script) else {
        let output = CommandOutput::failure(2, "rune: script is not valid UTF-8\n");
        return into_output(&output);
    };
    if handle.is_null() {
        let output = CommandOutput::failure(1, "rune: session is unavailable\n");
        return into_output(&output);
    }
    // SAFETY: Swift serializes access to the opaque session handle and does
    // not call this after rune_session_destroy.
    let session = unsafe { &mut *handle.cast::<RuneSession>() };
    let output = session.core.execute_script(&script);
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

/// Returns the restored and in-session history as one newline-separated owned
/// string. Rune command lines are single-line records at this stage.
#[no_mangle]
pub extern "C" fn rune_session_history(handle: *const std::ffi::c_void) -> *mut c_char {
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the pointer is read-only and owned by the Swift session.
    let session = unsafe { &*handle.cast::<RuneSession>() };
    let history = session.core.history().join("\n");
    into_owned_c_string(&history)
}

/// Returns the registered built-in command names as a newline-separated owned
/// string. The names are metadata for native completion and do not imply that
/// Rune can execute arbitrary host commands.
#[no_mangle]
pub extern "C" fn rune_session_commands(handle: *const std::ffi::c_void) -> *mut c_char {
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the pointer is read-only and owned by the Swift session.
    let session = unsafe { &*handle.cast::<RuneSession>() };
    let mut commands: Vec<&str> = session
        .core
        .commands()
        .iter()
        .map(|definition| definition.name)
        .collect();
    commands.sort_unstable();
    into_owned_c_string(&commands.join("\n"))
}

/// Takes output generated while the session's `~/.rune_profile` was loaded.
#[no_mangle]
pub extern "C" fn rune_session_startup_output(handle: *mut std::ffi::c_void) -> RuneOutput {
    if handle.is_null() {
        let output = CommandOutput::failure(1, "rune: session is unavailable\n");
        return into_output(&output);
    }
    // SAFETY: Swift serializes access to the opaque session handle and does
    // not call this after rune_session_destroy.
    let session = unsafe { &mut *handle.cast::<RuneSession>() };
    let output = session.core.take_startup_output();
    into_output(&output)
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

#[cfg(test)]
mod tests {
    use super::{
        rune_session_commands, rune_session_current_directory, rune_session_destroy,
        rune_session_execute, rune_session_execute_script, rune_session_new,
        rune_session_startup_output, rune_string_free,
    };
    use std::ffi::{CStr, CString};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn c_abi_executes_and_releases_an_owned_result() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rune-ffi-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("test root created");
        std::fs::write(root.join(".rune_profile"), b"echo profile-start\n")
            .expect("profile written");
        let root_string = CString::new(root.to_string_lossy().as_bytes()).expect("valid root");
        let handle = rune_session_new(root_string.as_ptr());
        assert!(!handle.is_null());

        let startup = rune_session_startup_output(handle);
        assert_eq!(startup.status, 0);
        assert_eq!(c_string(startup.stdout), "profile-start\n");
        assert!(c_string(startup.stderr).is_empty());
        // SAFETY: both pointers were returned by rune_session_startup_output
        // and are released exactly once.
        unsafe {
            rune_string_free(startup.stdout);
            rune_string_free(startup.stderr);
        }
        let command = CString::new("echo from-ffi").expect("valid command");
        let output = rune_session_execute(handle, command.as_ptr());
        assert_eq!(output.status, 0);
        assert_eq!(
            c_string(output.stdout),
            "from-ffi\n",
            "stdout must cross the ABI intact"
        );
        assert!(c_string(output.stderr).is_empty());
        // SAFETY: both pointers were returned by rune_session_execute and are
        // released exactly once before destroying the owning session.
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        let commands = rune_session_commands(handle);
        let command_names = c_string(commands);
        assert!(command_names.lines().any(|name| name == "echo"));
        assert!(command_names.lines().any(|name| name == "export"));
        // SAFETY: commands was returned by rune_session_commands.
        unsafe { rune_string_free(commands) };
        let change_directory = CString::new("mkdir sub && cd sub").expect("valid command");
        let output = rune_session_execute(handle, change_directory.as_ptr());
        assert_eq!(output.status, 0);
        // SAFETY: both pointers were returned by rune_session_execute and are
        // released exactly once before destroying the owning session.
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        rune_session_destroy(handle);

        let reopened = rune_session_new(root_string.as_ptr());
        assert!(!reopened.is_null());
        let directory = rune_session_current_directory(reopened);
        assert_eq!(c_string(directory), "~/sub");
        // SAFETY: directory was returned by rune_session_current_directory.
        unsafe { rune_string_free(directory) };
        rune_session_destroy(reopened);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn c_abi_executes_a_multiline_automation_script() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rune-ffi-script-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("test root created");
        let root_string = CString::new(root.to_string_lossy().as_bytes()).expect("valid root");
        let handle = rune_session_new(root_string.as_ptr());
        assert!(!handle.is_null());
        let script = CString::new("echo first\n\nfalse\necho last").expect("valid script");
        let output = rune_session_execute_script(handle, script.as_ptr());
        assert_eq!(output.status, 0);
        assert_eq!(c_string(output.stdout), "first\nlast\n");
        assert!(c_string(output.stderr).is_empty());
        // SAFETY: both pointers were returned by rune_session_execute_script
        // and are released exactly once.
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        rune_session_destroy(handle);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    fn c_string(pointer: *mut std::os::raw::c_char) -> String {
        assert!(!pointer.is_null());
        // SAFETY: the test only reads a NUL-terminated CString returned by the
        // ABI before its matching free call.
        unsafe { CStr::from_ptr(pointer).to_string_lossy().into_owned() }
    }
}
