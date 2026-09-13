//! Narrow C ABI for the native Apple frontend.
//!
//! Unsafe code is intentionally confined to this boundary. The portable
//! crates remain safe Rust; Swift owns the session handle on its main actor
//! and must release every returned string with [`rune_string_free`].

#![allow(unsafe_code)]

use std::ffi::{c_void, CStr, CString};
use std::fmt::Write as _;
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use rune_core::{
    CommandEvent, CommandOutput, DisabledNetworkProvider, EventSink, NetworkError, NetworkProvider,
    NetworkRequest, NetworkResponse, Session, MAX_FILE_TRANSFER_BYTES, MAX_NETWORK_BODY_BYTES,
};
use rune_fs::{FsError, SandboxedFileSystem};

/// An owned result crossing the C ABI.
#[repr(C)]
pub struct RuneOutput {
    pub stdout: *mut c_char,
    pub stderr: *mut c_char,
    pub status: i32,
}

/// A bounded binary file result crossing the C ABI.
#[repr(C)]
pub struct RuneFile {
    pub data: *mut u8,
    pub length: usize,
    pub status: i32,
    pub message: *mut c_char,
}

/// A borrowed execution event delivered synchronously during a streamed call.
/// The pointed-to strings are valid only for the duration of the callback.
#[repr(C)]
pub struct RuneEvent {
    /// `1` is output and `2` is a completed status boundary.
    pub kind: i32,
    pub stdout: *const c_char,
    pub stderr: *const c_char,
    pub status: i32,
    pub current_directory: *const c_char,
}

/// Callback used by the event-aware execution entry points.
pub type RuneEventCallback =
    Option<unsafe extern "C" fn(event: *const RuneEvent, user_data: *mut c_void)>;

pub const RUNE_EVENT_OUTPUT: i32 = 1;
pub const RUNE_EVENT_STATUS: i32 = 2;

/// Bounded response storage exchanged with a native network callback.
#[repr(C)]
pub struct RuneNetworkResponse {
    pub status_code: i32,
    pub body_length: usize,
    /// Zero is success; non-zero means the host rejected or could not finish
    /// the request. The Rust side turns it into a bounded diagnostic.
    pub error: i32,
}

/// Native transport callback for the core's explicit HTTP capability.
pub type RuneNetworkRequestCallback = Option<
    unsafe extern "C" fn(
        user_data: *mut c_void,
        method: *const c_char,
        url: *const c_char,
        headers: *const c_char,
        body: *const u8,
        body_length: usize,
        response_buffer: *mut u8,
        response_capacity: usize,
        response: *mut RuneNetworkResponse,
    ) -> bool,
>;

struct CallbackNetworkProvider {
    callback: unsafe extern "C" fn(
        user_data: *mut c_void,
        method: *const c_char,
        url: *const c_char,
        headers: *const c_char,
        body: *const u8,
        body_length: usize,
        response_buffer: *mut u8,
        response_capacity: usize,
        response: *mut RuneNetworkResponse,
    ) -> bool,
    user_data: *mut c_void,
}

impl NetworkProvider for CallbackNetworkProvider {
    fn request(&self, request: &NetworkRequest) -> Result<NetworkResponse, NetworkError> {
        let method = CString::new(request.method.as_str()).map_err(|_| {
            NetworkError::Transport("network method contains an invalid byte".to_string())
        })?;
        let url = CString::new(request.url.as_str()).map_err(|_| {
            NetworkError::Transport("network URL contains an invalid byte".to_string())
        })?;
        let headers = request
            .headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}"))
            .collect::<Vec<_>>()
            .join("\n");
        let headers = CString::new(headers).map_err(|_| {
            NetworkError::Transport("network headers contain an invalid byte".to_string())
        })?;
        let mut response_buffer = vec![0_u8; MAX_NETWORK_BODY_BYTES];
        let mut response = RuneNetworkResponse {
            status_code: 0,
            body_length: 0,
            error: 0,
        };
        let callback_succeeded = catch_unwind(AssertUnwindSafe(|| unsafe {
            (self.callback)(
                self.user_data,
                method.as_ptr(),
                url.as_ptr(),
                headers.as_ptr(),
                request.body.as_ptr(),
                request.body.len(),
                response_buffer.as_mut_ptr(),
                response_buffer.len(),
                std::ptr::addr_of_mut!(response),
            )
        }))
        .unwrap_or(false);
        if !callback_succeeded || response.error != 0 {
            return Err(NetworkError::Transport(
                "native network provider rejected the request".to_string(),
            ));
        }
        if response.body_length > response_buffer.len() {
            return Err(NetworkError::BodyTooLarge {
                actual: response.body_length,
                maximum: response_buffer.len(),
            });
        }
        let status_code = u16::try_from(response.status_code).map_err(|_| {
            NetworkError::InvalidResponse(
                "native provider returned an invalid status code".to_string(),
            )
        })?;
        let response = NetworkResponse {
            status_code,
            body: response_buffer[..response.body_length].to_vec(),
        };
        response.validate()?;
        Ok(response)
    }
}

struct RuneSession {
    core: Session,
    cancellation: Arc<std::sync::atomic::AtomicBool>,
}

struct CallbackEventSink {
    callback: unsafe extern "C" fn(event: *const RuneEvent, user_data: *mut c_void),
    user_data: *mut c_void,
    disabled: bool,
}

impl EventSink for CallbackEventSink {
    fn emit(&mut self, event: CommandEvent) {
        if self.disabled {
            return;
        }
        let (kind, stdout, stderr, status, current_directory) = match event {
            CommandEvent::Output { stdout, stderr } => {
                (RUNE_EVENT_OUTPUT, stdout, stderr, 0, String::new())
            }
            CommandEvent::Status {
                status,
                current_directory,
            } => (
                RUNE_EVENT_STATUS,
                String::new(),
                String::new(),
                status,
                current_directory,
            ),
        };
        let stdout = callback_string(&stdout);
        let stderr = callback_string(&stderr);
        let current_directory = callback_string(&current_directory);
        let raw = RuneEvent {
            kind,
            stdout: stdout.as_ptr(),
            stderr: stderr.as_ptr(),
            status,
            current_directory: current_directory.as_ptr(),
        };
        let result = catch_unwind(AssertUnwindSafe(|| unsafe {
            (self.callback)(&raw, self.user_data);
        }));
        if result.is_err() {
            self.disabled = true;
        }
    }
}

fn callback_string(value: &str) -> CString {
    CString::new(value.replace('\0', "�")).unwrap_or_default()
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

fn into_owned_bytes(value: Vec<u8>) -> (*mut u8, usize) {
    if value.is_empty() {
        return (std::ptr::null_mut(), 0);
    }
    let boxed = value.into_boxed_slice();
    let length = boxed.len();
    (Box::into_raw(boxed).cast(), length)
}

fn into_file_result(result: Result<Vec<u8>, FsError>) -> RuneFile {
    match result {
        Ok(data) => {
            let (data, length) = into_owned_bytes(data);
            RuneFile {
                data,
                length,
                status: 0,
                message: std::ptr::null_mut(),
            }
        }
        Err(error) => RuneFile {
            data: std::ptr::null_mut(),
            length: 0,
            status: 1,
            message: into_owned_c_string(&error.to_string()),
        },
    }
}

fn into_output(output: &CommandOutput) -> RuneOutput {
    RuneOutput {
        stdout: into_owned_c_string(&output.stdout),
        stderr: into_owned_c_string(&output.stderr),
        status: output.status,
    }
}

fn persist_after_execution(core: &mut Session, mut output: CommandOutput) -> CommandOutput {
    if let Err(error) = core.persist() {
        let _ = writeln!(output.stderr, "rune: could not persist session: {error}");
        if output.status == 0 {
            output.status = 1;
        }
    }
    output
}

fn create_session_from_filesystem(
    filesystem: Result<SandboxedFileSystem, FsError>,
    session_id: Option<&str>,
) -> *mut std::ffi::c_void {
    let Ok(filesystem) = filesystem else {
        return std::ptr::null_mut();
    };
    let core = match session_id {
        Some(session_id) => match Session::restore_with_id(filesystem, session_id) {
            Ok(core) => core,
            Err(_) => return std::ptr::null_mut(),
        },
        None => Session::restore(filesystem),
    };
    let cancellation = core.cancellation_handle();
    let session = Box::new(RuneSession { core, cancellation });
    Box::into_raw(session).cast()
}

fn create_session(root: &str, session_id: Option<&str>) -> *mut std::ffi::c_void {
    create_session_from_filesystem(SandboxedFileSystem::new(root), session_id)
}

fn create_session_with_layout(
    home: &str,
    library: &str,
    temporary: &str,
    session_id: Option<&str>,
) -> *mut std::ffi::c_void {
    create_session_from_filesystem(
        SandboxedFileSystem::new_with_layout(home, library, temporary),
        session_id,
    )
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
    create_session(&root, None)
}

/// Creates a session whose cwd, history, and bookmarks are persisted in a
/// bounded namespace below the supplied root. Session IDs are not shell paths.
#[no_mangle]
pub extern "C" fn rune_session_new_named(
    root: *const c_char,
    session_id: *const c_char,
) -> *mut std::ffi::c_void {
    let (Some(root), Some(session_id)) = (read_string(root), read_string(session_id)) else {
        return std::ptr::null_mut();
    };
    create_session(&root, Some(&session_id))
}

/// Creates a session using the app's Documents, Library, and tmp directories
/// as the virtual `~`, `~/Library`, and `~/tmp` roots.
#[no_mangle]
pub extern "C" fn rune_session_new_with_layout(
    home: *const c_char,
    library: *const c_char,
    temporary: *const c_char,
) -> *mut std::ffi::c_void {
    let (Some(home), Some(library), Some(temporary)) = (
        read_string(home),
        read_string(library),
        read_string(temporary),
    ) else {
        return std::ptr::null_mut();
    };
    create_session_with_layout(&home, &library, &temporary, None)
}

/// Creates a named session using the app's Documents, Library, and tmp roots.
#[no_mangle]
pub extern "C" fn rune_session_new_named_with_layout(
    home: *const c_char,
    library: *const c_char,
    temporary: *const c_char,
    session_id: *const c_char,
) -> *mut std::ffi::c_void {
    let (Some(home), Some(library), Some(temporary), Some(session_id)) = (
        read_string(home),
        read_string(library),
        read_string(temporary),
        read_string(session_id),
    ) else {
        return std::ptr::null_mut();
    };
    create_session_with_layout(&home, &library, &temporary, Some(&session_id))
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

/// Requests cooperative cancellation for the next execution boundary.
///
/// The handle must remain alive until any in-flight execution returns. The
/// request is atomic so a UI cancellation callback can signal a session while
/// the command call is running on another thread.
#[no_mangle]
pub extern "C" fn rune_session_cancel(handle: *const std::ffi::c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: callers keep the opaque session alive while requesting cancel;
    // this function only touches the separately-owned atomic flag.
    let session = unsafe { &*handle.cast::<RuneSession>() };
    session.cancellation.store(true, Ordering::Release);
}

/// Installs or clears the native HTTP transport capability for one session.
///
/// The callback must synchronously fill the supplied response buffer and may
/// only write up to `response_capacity` bytes. Passing `None` removes the
/// capability. Rune never calls this callback unless a `curl` command has
/// already passed its Rust-owned policy checks.
#[no_mangle]
pub extern "C" fn rune_session_set_network_callback(
    handle: *mut c_void,
    callback: RuneNetworkRequestCallback,
    user_data: *mut c_void,
) -> i32 {
    if handle.is_null() {
        return 1;
    }
    // SAFETY: Swift serializes access to the opaque session handle and keeps
    // it alive while configuring the callback.
    let core = unsafe { &mut (*handle.cast::<RuneSession>()).core };
    if let Some(callback) = callback {
        core.set_network_provider(Box::new(CallbackNetworkProvider {
            callback,
            user_data,
        }));
    } else {
        core.set_network_provider(Box::new(DisabledNetworkProvider));
    }
    0
}

/// Updates one validated Rust-owned configuration value without recording a
/// shell command in history. The result is persisted before returning.
#[no_mangle]
pub extern "C" fn rune_session_set_configuration(
    handle: *mut c_void,
    key: *const c_char,
    value: *const c_char,
) -> RuneOutput {
    let (Some(key), Some(value)) = (read_string(key), read_string(value)) else {
        let output =
            CommandOutput::failure(2, "rune: configuration key/value is not valid UTF-8\n");
        return into_output(&output);
    };
    if handle.is_null() {
        let output = CommandOutput::failure(1, "rune: session is unavailable\n");
        return into_output(&output);
    }
    // SAFETY: Swift serializes access to the opaque session handle and does
    // not call this after rune_session_destroy.
    let core = unsafe { &mut (*handle.cast::<RuneSession>()).core };
    let output = core.set_configuration(&key, &value);
    let output = persist_after_execution(core, output);
    into_output(&output)
}

/// Resets Rust-owned configuration without recording a shell command in
/// history. The result is persisted before returning.
#[no_mangle]
pub extern "C" fn rune_session_reset_configuration(handle: *mut c_void) -> RuneOutput {
    if handle.is_null() {
        let output = CommandOutput::failure(1, "rune: session is unavailable\n");
        return into_output(&output);
    }
    // SAFETY: Swift serializes access to the opaque session handle and does
    // not call this after rune_session_destroy.
    let core = unsafe { &mut (*handle.cast::<RuneSession>()).core };
    let output = core.reset_configuration();
    let output = persist_after_execution(core, output);
    into_output(&output)
}

/// Executes one Rune command line and persists the session state before
/// returning. A persistence failure is reported on stderr and changes a
/// successful command's status to 1.
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
    let core = unsafe { &mut (*handle.cast::<RuneSession>()).core };
    let output = core.execute_line(&input);
    let output = persist_after_execution(core, output);
    into_output(&output)
}

/// Executes a newline-delimited automation script through the same Rust
/// parser and command registry as interactive input, then persists the
/// session state before returning.
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
    let core = unsafe { &mut (*handle.cast::<RuneSession>()).core };
    let output = core.execute_script(&script);
    let output = persist_after_execution(core, output);
    into_output(&output)
}

fn execute_with_events(
    handle: *mut c_void,
    input: *const c_char,
    callback: RuneEventCallback,
    user_data: *mut c_void,
    script: bool,
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
    let core = unsafe { &mut (*handle.cast::<RuneSession>()).core };
    let output = match callback {
        Some(callback) => {
            let mut sink = CallbackEventSink {
                callback,
                user_data,
                disabled: false,
            };
            if script {
                core.execute_script_with_events(&input, &mut sink)
            } else {
                core.execute_line_with_events(&input, &mut sink)
            }
        }
        None => {
            if script {
                core.execute_script(&input)
            } else {
                core.execute_line(&input)
            }
        }
    };
    let output = persist_after_execution(core, output);
    into_output(&output)
}

/// Executes one command line and synchronously delivers bounded Rust events.
/// Event strings are borrowed and must be copied by the callback if retained.
#[no_mangle]
pub extern "C" fn rune_session_execute_with_events(
    handle: *mut c_void,
    input: *const c_char,
    callback: RuneEventCallback,
    user_data: *mut c_void,
) -> RuneOutput {
    execute_with_events(handle, input, callback, user_data, false)
}

/// Executes a newline-delimited script and synchronously delivers bounded Rust
/// events for each line and for the script boundary.
#[no_mangle]
pub extern "C" fn rune_session_execute_script_with_events(
    handle: *mut c_void,
    script: *const c_char,
    callback: RuneEventCallback,
    user_data: *mut c_void,
) -> RuneOutput {
    execute_with_events(handle, script, callback, user_data, true)
}

/// Writes a bounded binary file through the session's confined filesystem.
/// The content is copied immediately and replaces any existing regular file.
///
/// # Safety
///
/// When `length` is non-zero, `data` must point to a readable buffer of at
/// least `length` bytes for the duration of this call. The path must be a
/// valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn rune_session_put_file(
    handle: *mut std::ffi::c_void,
    path: *const c_char,
    data: *const u8,
    length: usize,
) -> RuneOutput {
    let Some(path) = read_string(path) else {
        return into_output(&CommandOutput::failure(
            2,
            "rune: file path is not valid UTF-8\n",
        ));
    };
    if handle.is_null() {
        return into_output(&CommandOutput::failure(1, "rune: session is unavailable\n"));
    }
    if length > MAX_FILE_TRANSFER_BYTES || (length > 0 && data.is_null()) {
        return into_output(&CommandOutput::failure(
            2,
            format!(
                "rune: file payload exceeds the {MAX_FILE_TRANSFER_BYTES}-byte transfer limit\n"
            ),
        ));
    }
    // SAFETY: callers provide a valid byte buffer for the declared length and
    // keep the session alive. The bytes are copied by `write_file` before the
    // function returns.
    let content = if length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(data, length) }
    };
    // SAFETY: Swift serializes access to the opaque session handle and does
    // not call this after rune_session_destroy.
    let session = unsafe { &mut *handle.cast::<RuneSession>() };
    match session.core.write_file(&path, content) {
        Ok(()) => into_output(&CommandOutput::success("")),
        Err(error) => into_output(&CommandOutput::failure(1, format!("rune: {error}\n"))),
    }
}

/// Reads a bounded binary file through the session's confined filesystem.
/// The returned bytes are released with [`rune_file_bytes_free`] and the
/// optional error message with [`rune_string_free`].
#[no_mangle]
pub extern "C" fn rune_session_get_file(
    handle: *const std::ffi::c_void,
    path: *const c_char,
) -> RuneFile {
    let Some(path) = read_string(path) else {
        return RuneFile {
            data: std::ptr::null_mut(),
            length: 0,
            status: 2,
            message: into_owned_c_string("rune: file path is not valid UTF-8\n"),
        };
    };
    if handle.is_null() {
        return RuneFile {
            data: std::ptr::null_mut(),
            length: 0,
            status: 1,
            message: into_owned_c_string("rune: session is unavailable\n"),
        };
    }
    // SAFETY: the pointer is read-only and owned by the Swift session.
    let session = unsafe { &*handle.cast::<RuneSession>() };
    into_file_result(session.core.read_file(&path))
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

/// Returns newest-first bounded history matches as one newline-separated
/// owned string. An oversized or invalid query returns null and never mutates
/// the session or records a synthetic command.
#[no_mangle]
pub extern "C" fn rune_session_history_search(
    handle: *const std::ffi::c_void,
    query: *const c_char,
) -> *mut c_char {
    let Some(query) = read_string(query) else {
        return std::ptr::null_mut();
    };
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the pointer is read-only and owned by the Swift session.
    let session = unsafe { &*handle.cast::<RuneSession>() };
    let Some(matches) = session.core.search_history(&query) else {
        return std::ptr::null_mut();
    };
    into_owned_c_string(&matches.join("\n"))
}

/// Returns the portable Rust-owned session configuration as newline-delimited
/// key/value text.
#[no_mangle]
pub extern "C" fn rune_session_configuration(handle: *const std::ffi::c_void) -> *mut c_char {
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the pointer is read-only and owned by the Swift session.
    let session = unsafe { &*handle.cast::<RuneSession>() };
    let configuration = format!(
        "history-limit={}\nfont-size={}\nscrollback-limit={}\ntoolbar-visible={}\ntheme={}\ncursor-color={}\ncursor-shape={}\nfont={}\nbackground={}\nforeground={}\n",
        session.core.configuration().history_limit(),
        session.core.configuration().font_size(),
        session.core.configuration().scrollback_limit(),
        session.core.configuration().toolbar_visible(),
        session.core.configuration().theme().as_str(),
        session.core.configuration().cursor_color().as_str(),
        session.core.configuration().cursor_shape().as_str(),
        session.core.configuration().font().as_str(),
        session.core.configuration().background().as_str(),
        session.core.configuration().foreground().as_str()
    );
    into_owned_c_string(&configuration)
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

/// Returns bounded Rust-owned command/path completion candidates as a
/// newline-separated string. An invalid input or null handle returns null.
/// Command names and supported sandbox paths are returned as replacement
/// tokens; ambiguous quoted or compound fragments return an empty string.
#[no_mangle]
pub extern "C" fn rune_session_complete(
    handle: *const std::ffi::c_void,
    input: *const c_char,
) -> *mut c_char {
    let Some(input) = read_string(input) else {
        return std::ptr::null_mut();
    };
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the pointer is read-only and owned by the Swift session.
    let session = unsafe { &*handle.cast::<RuneSession>() };
    let candidates = session.core.completion_candidates(&input);
    into_owned_c_string(&candidates.join("\n"))
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

/// Releases bytes returned by [`rune_session_get_file`].
///
/// # Safety
///
/// `data` must be null or the exact pointer/length pair returned by Rune, and
/// it must be released at most once.
#[no_mangle]
pub unsafe extern "C" fn rune_file_bytes_free(data: *mut u8, length: usize) {
    if data.is_null() {
        return;
    }
    // SAFETY: the caller supplies the allocation and length returned by Rune.
    unsafe {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            data, length,
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        rune_file_bytes_free, rune_session_cancel, rune_session_commands, rune_session_complete,
        rune_session_configuration, rune_session_current_directory, rune_session_destroy,
        rune_session_execute, rune_session_execute_script, rune_session_execute_script_with_events,
        rune_session_execute_with_events, rune_session_get_file, rune_session_history,
        rune_session_history_search, rune_session_new, rune_session_new_named,
        rune_session_new_with_layout, rune_session_put_file, rune_session_reset_configuration,
        rune_session_set_configuration, rune_session_set_network_callback,
        rune_session_startup_output, rune_string_free, RuneEvent, RuneNetworkResponse,
        RUNE_EVENT_OUTPUT, RUNE_EVENT_STATUS,
    };
    use std::ffi::{c_void, CStr, CString};
    use std::os::raw::c_char;
    use std::time::{SystemTime, UNIX_EPOCH};

    unsafe extern "C" fn test_network_callback(
        user_data: *mut c_void,
        method: *const c_char,
        url: *const c_char,
        headers: *const c_char,
        body: *const u8,
        body_length: usize,
        response_buffer: *mut u8,
        response_capacity: usize,
        response: *mut RuneNetworkResponse,
    ) -> bool {
        if user_data.is_null()
            || method.is_null()
            || url.is_null()
            || headers.is_null()
            || response.is_null()
            || response_buffer.is_null()
        {
            return false;
        }
        // SAFETY: the callback is invoked synchronously with the pointers
        // prepared by CallbackNetworkProvider for this test.
        let calls = unsafe { &mut *user_data.cast::<usize>() };
        *calls += 1;
        let method = unsafe { CStr::from_ptr(method) }
            .to_str()
            .unwrap_or_default();
        let url = unsafe { CStr::from_ptr(url) }.to_str().unwrap_or_default();
        let headers = unsafe { CStr::from_ptr(headers) }
            .to_str()
            .unwrap_or_default();
        if method != "POST" || url != "https://example.test/ffi" || headers != "X-Test: yes" {
            return false;
        }
        if body_length != 7 || body.is_null() {
            return false;
        }
        // SAFETY: body points to the request bytes and response_buffer has
        // the capacity advertised by Rust.
        let body = unsafe { std::slice::from_raw_parts(body, body_length) };
        if body != b"payload" || response_capacity < 7 {
            return false;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(b"ffi-body".as_ptr(), response_buffer, 8);
            (*response).status_code = 201;
            (*response).body_length = 8;
            (*response).error = 0;
        }
        true
    }

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
        std::fs::write(root.join("readme.txt"), b"readme\n").expect("completion file written");
        std::fs::create_dir(root.join("docs")).expect("completion directory created");
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
        rune_session_cancel(handle);
        let cancelled_command = CString::new("echo cancelled").expect("valid command");
        let cancelled = rune_session_execute(handle, cancelled_command.as_ptr());
        assert_eq!(cancelled.status, 130);
        assert!(c_string(cancelled.stdout).is_empty());
        assert_eq!(c_string(cancelled.stderr), "rune: command cancelled\n");
        // SAFETY: both pointers were returned by rune_session_execute and
        // are released exactly once.
        unsafe {
            rune_string_free(cancelled.stdout);
            rune_string_free(cancelled.stderr);
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
        let prefix = CString::new("ec").expect("valid completion prefix");
        let completions = rune_session_complete(handle, prefix.as_ptr());
        assert_eq!(c_string(completions), "echo");
        // SAFETY: completions was returned by rune_session_complete.
        unsafe { rune_string_free(completions) };
        let path_prefix = CString::new("cat re").expect("valid path completion prefix");
        let path_completions = rune_session_complete(handle, path_prefix.as_ptr());
        assert_eq!(c_string(path_completions), "readme.txt");
        // SAFETY: path_completions was returned by rune_session_complete.
        unsafe { rune_string_free(path_completions) };
        let configuration = rune_session_configuration(handle);
        assert_eq!(
            c_string(configuration),
            "history-limit=1000\nfont-size=15\nscrollback-limit=4096\ntoolbar-visible=true\ntheme=ink\ncursor-color=cyan\ncursor-shape=bar\nfont=monospaced\nbackground=auto\nforeground=auto\n"
        );
        // SAFETY: configuration was returned by rune_session_configuration.
        unsafe { rune_string_free(configuration) };
        let change_directory = CString::new("mkdir sub && cd sub").expect("valid command");
        let output = rune_session_execute(handle, change_directory.as_ptr());
        assert_eq!(output.status, 0);
        // SAFETY: both pointers were returned by rune_session_execute and are
        // released exactly once before destroying the owning session.
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        // The command path must be durable before the native owner is
        // destroyed; iOS may suspend or terminate an app without giving the
        // UI an opportunity to run deinit first.
        let reopened = rune_session_new(root_string.as_ptr());
        assert!(!reopened.is_null());
        let directory = rune_session_current_directory(reopened);
        assert_eq!(c_string(directory), "~/sub");
        // SAFETY: directory was returned by rune_session_current_directory.
        unsafe { rune_string_free(directory) };
        rune_session_destroy(reopened);
        rune_session_destroy(handle);
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

    #[test]
    fn c_abi_updates_configuration_without_recording_a_shell_command() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rune-ffi-config-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("test root created");
        let root_string = CString::new(root.to_string_lossy().as_bytes()).expect("valid root");
        let handle = rune_session_new(root_string.as_ptr());
        assert!(!handle.is_null());

        let command = CString::new("echo keep").expect("valid command");
        let output = rune_session_execute(handle, command.as_ptr());
        assert_eq!(output.status, 0);
        // SAFETY: both pointers came from rune_session_execute.
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }

        let key = CString::new("font-size").expect("valid key");
        let value = CString::new("20").expect("valid value");
        let changed = rune_session_set_configuration(handle, key.as_ptr(), value.as_ptr());
        assert_eq!(changed.status, 0);
        // SAFETY: both pointers came from rune_session_set_configuration.
        unsafe {
            rune_string_free(changed.stdout);
            rune_string_free(changed.stderr);
        }
        let configuration = rune_session_configuration(handle);
        assert!(c_string(configuration).contains("font-size=20"));
        // SAFETY: configuration came from rune_session_configuration.
        unsafe { rune_string_free(configuration) };

        let history = rune_session_history(handle);
        assert_eq!(c_string(history), "echo keep");
        // SAFETY: history came from rune_session_history.
        unsafe { rune_string_free(history) };
        let search_query = CString::new("keep").expect("valid history query");
        let matches = rune_session_history_search(handle, search_query.as_ptr());
        assert_eq!(c_string(matches), "echo keep");
        // SAFETY: matches came from rune_session_history_search.
        unsafe { rune_string_free(matches) };

        let bad_key = CString::new("theme").expect("valid key");
        let bad_value = CString::new("paper").expect("valid value");
        let rejected = rune_session_set_configuration(handle, bad_key.as_ptr(), bad_value.as_ptr());
        assert_eq!(rejected.status, 2);
        assert!(c_string(rejected.stderr).contains("theme must be one of"));
        // SAFETY: both pointers came from rune_session_set_configuration.
        unsafe {
            rune_string_free(rejected.stdout);
            rune_string_free(rejected.stderr);
        }

        let reset = rune_session_reset_configuration(handle);
        assert_eq!(reset.status, 0);
        // SAFETY: both pointers came from rune_session_reset_configuration.
        unsafe {
            rune_string_free(reset.stdout);
            rune_string_free(reset.stderr);
        }
        rune_session_destroy(handle);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[derive(Debug, PartialEq, Eq)]
    struct EventRecord {
        kind: i32,
        stdout: String,
        stderr: String,
        status: i32,
        current_directory: String,
    }

    unsafe extern "C" fn collect_event(event: *const RuneEvent, user_data: *mut c_void) {
        if event.is_null() || user_data.is_null() {
            return;
        }
        // SAFETY: Rune invokes this callback synchronously with a valid event
        // and the test keeps the Vec alive for the whole call.
        let event = unsafe { &*event };
        let text = |pointer: *const std::os::raw::c_char| {
            if pointer.is_null() {
                String::new()
            } else {
                // SAFETY: event strings are NUL-terminated for this callback.
                unsafe { CStr::from_ptr(pointer) }
                    .to_string_lossy()
                    .into_owned()
            }
        };
        // SAFETY: user_data is the pointer to the test-owned Vec supplied to
        // the event-aware FFI call.
        let records = unsafe { &mut *user_data.cast::<Vec<EventRecord>>() };
        records.push(EventRecord {
            kind: event.kind,
            stdout: text(event.stdout),
            stderr: text(event.stderr),
            status: event.status,
            current_directory: text(event.current_directory),
        });
    }

    #[test]
    fn c_abi_delivers_borrowed_output_and_status_events() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rune-ffi-events-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("test root created");
        let root_string = CString::new(root.to_string_lossy().as_bytes()).expect("valid root");
        let handle = rune_session_new(root_string.as_ptr());
        assert!(!handle.is_null());
        let command = CString::new("echo first; false; echo last").expect("valid command");
        let mut records: Vec<EventRecord> = Vec::new();
        let output = rune_session_execute_with_events(
            handle,
            command.as_ptr(),
            Some(collect_event),
            std::ptr::addr_of_mut!(records).cast(),
        );
        assert_eq!(output.status, 0);
        assert_eq!(records.len(), 4);
        assert_eq!(records[0].kind, RUNE_EVENT_OUTPUT);
        assert_eq!(records[0].stdout, "first\n");
        assert_eq!(records[1].kind, RUNE_EVENT_OUTPUT);
        assert_eq!(records[2].stdout, "last\n");
        assert_eq!(records[3].kind, RUNE_EVENT_STATUS);
        assert_eq!(records[3].status, 0);
        assert_eq!(records[3].current_directory, "~");
        // SAFETY: both pointers came from the event-aware FFI call.
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }

        records.clear();
        let script = CString::new("echo scripted\nfalse").expect("valid script");
        let script_output = rune_session_execute_script_with_events(
            handle,
            script.as_ptr(),
            Some(collect_event),
            std::ptr::addr_of_mut!(records).cast(),
        );
        assert_eq!(script_output.status, 1);
        assert!(records
            .iter()
            .any(|record| record.kind == RUNE_EVENT_STATUS && record.status == 1));
        // SAFETY: both pointers came from the event-aware FFI call.
        unsafe {
            rune_string_free(script_output.stdout);
            rune_string_free(script_output.stderr);
        }
        rune_session_destroy(handle);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn c_abi_transfers_bounded_binary_files_without_treating_nuls_as_strings() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rune-ffi-file-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("test root created");
        let root_string = CString::new(root.to_string_lossy().as_bytes()).expect("valid root");
        let handle = rune_session_new(root_string.as_ptr());
        assert!(!handle.is_null());
        let path = CString::new("payload.bin").expect("valid path");
        let payload = [0_u8, 1, 2, 0, 255];
        let output = unsafe {
            rune_session_put_file(handle, path.as_ptr(), payload.as_ptr(), payload.len())
        };
        assert_eq!(output.status, 0);
        assert!(c_string(output.stdout).is_empty());
        assert!(c_string(output.stderr).is_empty());
        // SAFETY: both pointers were returned by rune_session_put_file.
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }

        let file = rune_session_get_file(handle, path.as_ptr());
        assert_eq!(file.status, 0);
        assert_eq!(file.length, payload.len());
        // SAFETY: the pointer/length pair was returned by Rune and is released
        // exactly once after the bytes are checked.
        unsafe {
            assert_eq!(std::slice::from_raw_parts(file.data, file.length), payload);
            rune_file_bytes_free(file.data, file.length);
            rune_string_free(file.message);
        }

        let outside = CString::new("../outside.bin").expect("valid path");
        let rejected = unsafe {
            rune_session_put_file(handle, outside.as_ptr(), payload.as_ptr(), payload.len())
        };
        assert_eq!(rejected.status, 1);
        assert!(c_string(rejected.stderr).contains("sandbox"));
        // SAFETY: both pointers were returned by rune_session_put_file.
        unsafe {
            rune_string_free(rejected.stdout);
            rune_string_free(rejected.stderr);
        }
        rune_session_destroy(handle);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn c_abi_named_sessions_persist_independent_working_directories() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rune-ffi-named-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("test root created");
        let root_string = CString::new(root.to_string_lossy().as_bytes()).expect("valid root");
        let first_id = CString::new("first").expect("valid session id");
        let second_id = CString::new("second").expect("valid session id");

        let first = rune_session_new_named(root_string.as_ptr(), first_id.as_ptr());
        assert!(!first.is_null());
        let command = CString::new("mkdir first-dir && cd first-dir").expect("valid command");
        let output = rune_session_execute(first, command.as_ptr());
        assert_eq!(output.status, 0);
        // SAFETY: both pointers were returned by rune_session_execute.
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        rune_session_destroy(first);

        let second = rune_session_new_named(root_string.as_ptr(), second_id.as_ptr());
        assert!(!second.is_null());
        let directory = rune_session_current_directory(second);
        assert_eq!(c_string(directory), "~");
        // SAFETY: directory was returned by rune_session_current_directory.
        unsafe { rune_string_free(directory) };
        rune_session_destroy(second);

        let reopened = rune_session_new_named(root_string.as_ptr(), first_id.as_ptr());
        assert!(!reopened.is_null());
        let directory = rune_session_current_directory(reopened);
        assert_eq!(c_string(directory), "~/first-dir");
        // SAFETY: directory was returned by rune_session_current_directory.
        unsafe { rune_string_free(directory) };
        rune_session_destroy(reopened);

        let invalid_id = CString::new("../escape").expect("valid bytes");
        assert!(rune_session_new_named(root_string.as_ptr(), invalid_id.as_ptr()).is_null());
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn c_abi_layout_mounts_documents_library_and_tmp() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let container = std::env::temp_dir().join(format!("rune-ffi-layout-test-{suffix}"));
        let home = container.join("Documents");
        let library_path = container.join("Library");
        let temporary_path = container.join("tmp");
        std::fs::create_dir_all(&container).expect("test container created");
        let home = CString::new(home.to_string_lossy().as_bytes()).expect("valid home");
        let library =
            CString::new(library_path.to_string_lossy().as_bytes()).expect("valid library");
        let temporary =
            CString::new(temporary_path.to_string_lossy().as_bytes()).expect("valid temporary");
        let handle =
            rune_session_new_with_layout(home.as_ptr(), library.as_ptr(), temporary.as_ptr());
        assert!(!handle.is_null());

        let command = CString::new(
            "touch ~/Library/cache.txt && touch ~/tmp/session.txt && cd ~/Library && pwd",
        )
        .expect("valid command");
        let output = rune_session_execute(handle, command.as_ptr());
        assert_eq!(output.status, 0);
        assert_eq!(c_string(output.stdout), "~/Library\n");
        assert!(c_string(output.stderr).is_empty());
        // SAFETY: both pointers were returned by rune_session_execute.
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        assert!(library_path.join("cache.txt").is_file());
        assert!(temporary_path.join("session.txt").is_file());
        rune_session_destroy(handle);
        std::fs::remove_dir_all(container).expect("test container removed");
    }

    #[test]
    fn c_abi_routes_curl_through_the_native_network_callback() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rune-ffi-network-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("test root created");
        let root_string = CString::new(root.to_string_lossy().as_bytes()).expect("valid root");
        let handle = rune_session_new(root_string.as_ptr());
        assert!(!handle.is_null());
        let mut calls = 0_usize;
        assert_eq!(
            rune_session_set_network_callback(
                handle,
                Some(test_network_callback),
                std::ptr::addr_of_mut!(calls).cast(),
            ),
            0
        );
        let command =
            CString::new("curl -X POST -H 'X-Test: yes' -d payload https://example.test/ffi")
                .expect("valid command");
        let output = rune_session_execute(handle, command.as_ptr());
        assert_eq!(output.status, 0);
        assert_eq!(c_string(output.stdout), "ffi-body");
        assert!(c_string(output.stderr).is_empty());
        assert_eq!(calls, 1);
        // SAFETY: both pointers came from rune_session_execute.
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        assert_eq!(
            rune_session_set_network_callback(handle, None, std::ptr::null_mut()),
            0
        );
        let disabled = rune_session_execute(handle, command.as_ptr());
        assert_eq!(disabled.status, 1);
        assert!(c_string(disabled.stderr).contains("network provider is unavailable"));
        // SAFETY: both pointers came from the second rune_session_execute.
        unsafe {
            rune_string_free(disabled.stdout);
            rune_string_free(disabled.stderr);
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
