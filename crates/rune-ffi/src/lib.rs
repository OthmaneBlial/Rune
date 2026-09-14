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
    ClipboardError, ClipboardProvider, CommandEvent, CommandOutput, DisabledClipboardProvider,
    DisabledNetworkProvider, DisabledOpenProvider, DisabledToolchainProvider, EventSink,
    NetworkError, NetworkProvider, NetworkRequest, NetworkResponse, OpenError, OpenProvider,
    OpenRequest, Session, SessionAction, ToolchainArtifact, ToolchainError, ToolchainKind,
    ToolchainOutput, ToolchainProvider, ToolchainRequest, MAX_CLIPBOARD_BYTES,
    MAX_FILE_TRANSFER_BYTES, MAX_NETWORK_BODY_BYTES, MAX_TOOLCHAIN_ARTIFACTS,
    MAX_TOOLCHAIN_ARTIFACT_BYTES, MAX_TOOLCHAIN_ARTIFACT_PATH_BYTES,
    MAX_TOOLCHAIN_MEDIA_TYPE_BYTES, MAX_TOOLCHAIN_OUTPUT_BYTES,
};
use rune_fs::{FsError, SandboxedFileSystem};

/// An owned result crossing the C ABI.
#[repr(C)]
pub struct RuneOutput {
    pub stdout: *mut c_char,
    pub stderr: *mut c_char,
    pub status: i32,
}

/// Zero-based cursor position for the bounded Rust-owned terminal screen.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuneTerminalCursor {
    pub row: usize,
    pub column: usize,
    pub visible: bool,
    /// 0 means no terminal shape override; 1 block, 2 underline, 3 bar.
    pub shape: u8,
    /// 0 means no blink override; 1 blinking, 2 steady.
    pub blink: u8,
}

impl Default for RuneTerminalCursor {
    fn default() -> Self {
        Self {
            row: 0,
            column: 0,
            visible: true,
            shape: 0,
            blink: 0,
        }
    }
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
pub const RUNE_SESSION_ACTION_NONE: i32 = 0;
pub const RUNE_SESSION_ACTION_EXIT: i32 = 1;
pub const RUNE_SESSION_ACTION_NEW_WINDOW: i32 = 2;
pub const RUNE_SESSION_ACTION_PICK_FOLDER: i32 = 3;

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

/// A bounded byte slice borrowed by a synchronous toolchain callback.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RuneToolchainSlice {
    pub data: *const u8,
    pub length: usize,
}

/// One environment entry borrowed by a synchronous toolchain callback.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RuneToolchainEnvironmentEntry {
    pub key: RuneToolchainSlice,
    pub value: RuneToolchainSlice,
}

/// One generated artifact slot supplied to a synchronous toolchain callback.
///
/// The callback writes path/media bytes into the per-slot buffers passed to it
/// and writes artifact bytes into the shared data arena using `data_offset`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RuneToolchainArtifactBuffer {
    pub path_length: usize,
    pub media_type_length: usize,
    pub data_offset: usize,
    pub data_length: usize,
}

/// Response storage filled by a native C, C++, or TeX provider.
///
/// Output buffers and artifact slots are owned by Rune for the duration of the
/// callback, so the provider never returns pointers whose lifetime must outlive
/// the callback. Rune copies and validates every filled field before it
/// materializes an artifact in the confined VFS.
#[repr(C)]
pub struct RuneToolchainResponse {
    pub stdout_length: usize,
    pub stderr_length: usize,
    pub status: i32,
    pub artifact_count: usize,
    pub error: i32,
}

/// Native callback for one explicit C, C++, or TeX capability.
///
/// All request and output pointers are valid only during the callback. The
/// callback must not retain them, write host paths, or start an ambient shell.
/// `kind` uses [`RUNE_TOOLCHAIN_C`], [`RUNE_TOOLCHAIN_CPP`], or
/// [`RUNE_TOOLCHAIN_TEX`]. Artifact bytes share a bounded Rune-owned arena;
/// every slot's `data_offset + data_length` must stay inside that arena.
pub type RuneToolchainCallbackFn = unsafe extern "C" fn(
    user_data: *mut c_void,
    kind: i32,
    program_name: *const c_char,
    source: RuneToolchainSlice,
    args: *const RuneToolchainSlice,
    argument_count: usize,
    environment: *const RuneToolchainEnvironmentEntry,
    environment_count: usize,
    stdin: RuneToolchainSlice,
    stdout_buffer: *mut u8,
    stdout_capacity: usize,
    stderr_buffer: *mut u8,
    stderr_capacity: usize,
    artifact_buffers: *mut RuneToolchainArtifactBuffer,
    artifact_capacity: usize,
    artifact_path_buffers: *mut u8,
    artifact_path_capacity: usize,
    artifact_media_type_buffers: *mut u8,
    artifact_media_type_capacity: usize,
    artifact_data_buffer: *mut u8,
    artifact_data_capacity: usize,
    response: *mut RuneToolchainResponse,
) -> bool;

pub type RuneToolchainRequestCallback = Option<RuneToolchainCallbackFn>;

pub const RUNE_TOOLCHAIN_C: i32 = 1;
pub const RUNE_TOOLCHAIN_CPP: i32 = 2;
pub const RUNE_TOOLCHAIN_TEX: i32 = 3;

/// Result storage exchanged with a native clipboard read callback.
#[repr(C)]
pub struct RuneClipboardResponse {
    pub text_length: usize,
    /// Zero is success; non-zero means the host rejected the operation.
    pub error: i32,
}

/// Native callback that fills a bounded UTF-8 clipboard buffer.
pub type RuneClipboardReadCallback = Option<
    unsafe extern "C" fn(
        user_data: *mut c_void,
        buffer: *mut u8,
        capacity: usize,
        response: *mut RuneClipboardResponse,
    ) -> bool,
>;

/// Native callback that replaces the host clipboard with bounded UTF-8 text.
pub type RuneClipboardWriteCallback =
    Option<unsafe extern "C" fn(user_data: *mut c_void, text: *const u8, length: usize) -> bool>;

/// Native callback for opening a validated URL or handling a confined host
/// file. `target_kind` is one of the `RUNE_OPEN_*` constants and distinguishes
/// normal opening, playback, and preview.
pub type RuneOpenCallback = Option<
    unsafe extern "C" fn(user_data: *mut c_void, target: *const c_char, target_kind: i32) -> bool,
>;

pub const RUNE_OPEN_URL: i32 = 1;
pub const RUNE_OPEN_FILE: i32 = 2;
pub const RUNE_OPEN_PLAY: i32 = 3;
pub const RUNE_OPEN_VIEW: i32 = 4;

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

struct CallbackToolchainProvider {
    kind: ToolchainKind,
    callback: RuneToolchainCallbackFn,
    user_data: *mut c_void,
}

struct CallbackClipboardProvider {
    read: unsafe extern "C" fn(
        user_data: *mut c_void,
        buffer: *mut u8,
        capacity: usize,
        response: *mut RuneClipboardResponse,
    ) -> bool,
    write: unsafe extern "C" fn(user_data: *mut c_void, text: *const u8, length: usize) -> bool,
    user_data: *mut c_void,
}

struct CallbackOpenProvider {
    callback: unsafe extern "C" fn(
        user_data: *mut c_void,
        target: *const c_char,
        target_kind: i32,
    ) -> bool,
    user_data: *mut c_void,
}

impl ClipboardProvider for CallbackClipboardProvider {
    fn read_text(&self) -> Result<String, ClipboardError> {
        let mut buffer = vec![0_u8; MAX_CLIPBOARD_BYTES];
        let mut response = RuneClipboardResponse {
            text_length: 0,
            error: 0,
        };
        let callback_succeeded = catch_unwind(AssertUnwindSafe(|| unsafe {
            (self.read)(
                self.user_data,
                buffer.as_mut_ptr(),
                buffer.len(),
                std::ptr::addr_of_mut!(response),
            )
        }))
        .unwrap_or(false);
        if !callback_succeeded || response.error != 0 {
            return Err(ClipboardError::HostFailure);
        }
        if response.text_length > buffer.len() {
            return Err(ClipboardError::TooLarge {
                actual: response.text_length,
                maximum: buffer.len(),
            });
        }
        String::from_utf8(buffer[..response.text_length].to_vec())
            .map_err(|_| ClipboardError::InvalidText)
    }

    fn write_text(&self, text: &str) -> Result<(), ClipboardError> {
        if text.len() > MAX_CLIPBOARD_BYTES {
            return Err(ClipboardError::TooLarge {
                actual: text.len(),
                maximum: MAX_CLIPBOARD_BYTES,
            });
        }
        let callback_succeeded = catch_unwind(AssertUnwindSafe(|| unsafe {
            (self.write)(self.user_data, text.as_ptr(), text.len())
        }))
        .unwrap_or(false);
        if callback_succeeded {
            Ok(())
        } else {
            Err(ClipboardError::HostFailure)
        }
    }
}

impl OpenProvider for CallbackOpenProvider {
    fn open(&self, request: &OpenRequest) -> Result<(), OpenError> {
        let target = CString::new(request.target.as_str()).map_err(|_| OpenError::HostFailure)?;
        let accepted = catch_unwind(AssertUnwindSafe(|| unsafe {
            (self.callback)(self.user_data, target.as_ptr(), request.kind as i32)
        }))
        .unwrap_or(false);
        if accepted {
            Ok(())
        } else {
            Err(OpenError::HostFailure)
        }
    }
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

struct ToolchainCallbackBuffers {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    artifacts: Vec<RuneToolchainArtifactBuffer>,
    artifact_paths: Vec<u8>,
    artifact_media_types: Vec<u8>,
    artifact_data: Vec<u8>,
    response: RuneToolchainResponse,
}

impl ToolchainCallbackBuffers {
    fn new() -> Self {
        Self {
            stdout: vec![0_u8; MAX_TOOLCHAIN_OUTPUT_BYTES],
            stderr: vec![0_u8; MAX_TOOLCHAIN_OUTPUT_BYTES],
            artifacts: vec![RuneToolchainArtifactBuffer::default(); MAX_TOOLCHAIN_ARTIFACTS],
            artifact_paths: vec![0_u8; MAX_TOOLCHAIN_ARTIFACTS * MAX_TOOLCHAIN_ARTIFACT_PATH_BYTES],
            artifact_media_types: vec![
                0_u8;
                MAX_TOOLCHAIN_ARTIFACTS * MAX_TOOLCHAIN_MEDIA_TYPE_BYTES
            ],
            artifact_data: vec![0_u8; MAX_TOOLCHAIN_ARTIFACT_BYTES],
            response: RuneToolchainResponse {
                stdout_length: 0,
                stderr_length: 0,
                status: 1,
                artifact_count: 0,
                error: 0,
            },
        }
    }

    fn invoke(
        &mut self,
        provider: &CallbackToolchainProvider,
        request: &ToolchainRequest<'_>,
        program_name: &CString,
        arguments: &[RuneToolchainSlice],
        environment: &[RuneToolchainEnvironmentEntry],
    ) -> bool {
        catch_unwind(AssertUnwindSafe(|| unsafe {
            (provider.callback)(
                provider.user_data,
                toolchain_kind_code(provider.kind),
                program_name.as_ptr(),
                RuneToolchainSlice {
                    data: request.source.as_ptr(),
                    length: request.source.len(),
                },
                arguments.as_ptr(),
                arguments.len(),
                environment.as_ptr(),
                environment.len(),
                RuneToolchainSlice {
                    data: request.stdin.as_ptr(),
                    length: request.stdin.len(),
                },
                self.stdout.as_mut_ptr(),
                self.stdout.len(),
                self.stderr.as_mut_ptr(),
                self.stderr.len(),
                self.artifacts.as_mut_ptr(),
                self.artifacts.len(),
                self.artifact_paths.as_mut_ptr(),
                MAX_TOOLCHAIN_ARTIFACT_PATH_BYTES,
                self.artifact_media_types.as_mut_ptr(),
                MAX_TOOLCHAIN_MEDIA_TYPE_BYTES,
                self.artifact_data.as_mut_ptr(),
                self.artifact_data.len(),
                std::ptr::addr_of_mut!(self.response),
            )
        }))
        .unwrap_or(false)
    }

    fn into_output(self) -> Result<ToolchainOutput, ToolchainError> {
        let Self {
            stdout,
            stderr,
            artifacts,
            artifact_paths,
            artifact_media_types,
            artifact_data,
            response,
        } = self;
        if response.stdout_length > stdout.len() {
            return Err(ToolchainError::InvalidRequest(format!(
                "provider stdout exceeds {} bytes",
                stdout.len()
            )));
        }
        if response.stderr_length > stderr.len() {
            return Err(ToolchainError::InvalidRequest(format!(
                "provider stderr exceeds {} bytes",
                stderr.len()
            )));
        }
        let stdout = utf8_buffer(&stdout, response.stdout_length, "stdout")?;
        let stderr = utf8_buffer(&stderr, response.stderr_length, "stderr")?;
        let artifacts = collect_toolchain_artifacts(
            &artifacts,
            response.artifact_count,
            &artifact_paths,
            &artifact_media_types,
            &artifact_data,
        )?;
        let output = ToolchainOutput {
            stdout,
            stderr,
            status: response.status,
            artifacts,
        };
        output.validate()?;
        Ok(output)
    }
}

impl ToolchainProvider for CallbackToolchainProvider {
    fn kind(&self) -> ToolchainKind {
        self.kind
    }

    fn execute(&self, request: &ToolchainRequest<'_>) -> Result<ToolchainOutput, ToolchainError> {
        if request.kind() != self.kind {
            return Err(ToolchainError::UnsupportedKind {
                requested: request.kind(),
                provider: self.kind,
            });
        }
        request.validate()?;
        let program_name = CString::new(request.program_name).map_err(|_| {
            ToolchainError::InvalidRequest("program name contains an invalid byte".to_string())
        })?;
        let arguments = request
            .args
            .iter()
            .map(|argument| RuneToolchainSlice {
                data: argument.as_ptr(),
                length: argument.len(),
            })
            .collect::<Vec<_>>();
        let environment = request
            .environment
            .iter()
            .map(|(key, value)| RuneToolchainEnvironmentEntry {
                key: RuneToolchainSlice {
                    data: key.as_ptr(),
                    length: key.len(),
                },
                value: RuneToolchainSlice {
                    data: value.as_ptr(),
                    length: value.len(),
                },
            })
            .collect::<Vec<_>>();
        let mut buffers = ToolchainCallbackBuffers::new();
        if !buffers.invoke(self, request, &program_name, &arguments, &environment)
            || buffers.response.error != 0
        {
            return Err(ToolchainError::Execution(format!(
                "native {} toolchain provider rejected the request",
                self.kind
            )));
        }
        buffers.into_output()
    }
}

fn utf8_buffer(buffer: &[u8], length: usize, label: &str) -> Result<String, ToolchainError> {
    String::from_utf8(buffer[..length].to_vec())
        .map_err(|_| ToolchainError::InvalidRequest(format!("provider returned non-UTF-8 {label}")))
}

fn collect_toolchain_artifacts(
    buffers: &[RuneToolchainArtifactBuffer],
    count: usize,
    path_storage: &[u8],
    media_type_storage: &[u8],
    data_storage: &[u8],
) -> Result<Vec<ToolchainArtifact>, ToolchainError> {
    if count > buffers.len() {
        return Err(ToolchainError::InvalidRequest(format!(
            "provider returned more than {} artifacts",
            buffers.len()
        )));
    }
    buffers[..count]
        .iter()
        .enumerate()
        .map(|(index, artifact)| {
            if artifact.path_length > MAX_TOOLCHAIN_ARTIFACT_PATH_BYTES {
                return Err(ToolchainError::InvalidRequest(
                    "provider artifact path exceeds its buffer".to_string(),
                ));
            }
            if artifact.media_type_length > MAX_TOOLCHAIN_MEDIA_TYPE_BYTES {
                return Err(ToolchainError::InvalidRequest(
                    "provider artifact media type exceeds its buffer".to_string(),
                ));
            }
            let data_end = artifact
                .data_offset
                .checked_add(artifact.data_length)
                .ok_or_else(|| {
                    ToolchainError::InvalidRequest(
                        "provider artifact data range overflows".to_string(),
                    )
                })?;
            if data_end > data_storage.len() {
                return Err(ToolchainError::InvalidRequest(
                    "provider artifact data exceeds its arena".to_string(),
                ));
            }
            let path_start = index * MAX_TOOLCHAIN_ARTIFACT_PATH_BYTES;
            let media_type_start = index * MAX_TOOLCHAIN_MEDIA_TYPE_BYTES;
            let path = utf8_buffer(
                &path_storage[path_start..path_start + MAX_TOOLCHAIN_ARTIFACT_PATH_BYTES],
                artifact.path_length,
                "artifact path",
            )?;
            let media_type = utf8_buffer(
                &media_type_storage
                    [media_type_start..media_type_start + MAX_TOOLCHAIN_MEDIA_TYPE_BYTES],
                artifact.media_type_length,
                "artifact media type",
            )?;
            Ok(ToolchainArtifact {
                path,
                media_type,
                bytes: data_storage[artifact.data_offset..data_end].to_vec(),
            })
        })
        .collect()
}

fn toolchain_kind_code(kind: ToolchainKind) -> i32 {
    match kind {
        ToolchainKind::C => RUNE_TOOLCHAIN_C,
        ToolchainKind::Cpp => RUNE_TOOLCHAIN_CPP,
        ToolchainKind::Tex => RUNE_TOOLCHAIN_TEX,
    }
}

fn toolchain_kind_from_code(code: i32) -> Option<ToolchainKind> {
    match code {
        RUNE_TOOLCHAIN_C => Some(ToolchainKind::C),
        RUNE_TOOLCHAIN_CPP => Some(ToolchainKind::Cpp),
        RUNE_TOOLCHAIN_TEX => Some(ToolchainKind::Tex),
        _ => None,
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

/// Installs or clears the native text clipboard capability for one session.
/// Both callbacks must be supplied together; passing two null callbacks
/// removes the capability. Rune never invokes these callbacks for commands
/// that fail their own argument and size validation.
#[no_mangle]
pub extern "C" fn rune_session_set_clipboard_callbacks(
    handle: *mut c_void,
    read: RuneClipboardReadCallback,
    write: RuneClipboardWriteCallback,
    user_data: *mut c_void,
) -> i32 {
    if handle.is_null() {
        return 1;
    }
    let core = unsafe { &mut (*handle.cast::<RuneSession>()).core };
    match (read, write) {
        (Some(read), Some(write)) => {
            core.set_clipboard_provider(Box::new(CallbackClipboardProvider {
                read,
                write,
                user_data,
            }));
            0
        }
        (None, None) => {
            core.set_clipboard_provider(Box::new(DisabledClipboardProvider));
            0
        }
        _ => 2,
    }
}

/// Installs or clears the native external-open capability for one session.
///
/// The callback receives a validated URL or an existing confined host file
/// path and must return whether the host accepted the request. Passing None
/// removes the capability.
#[no_mangle]
pub extern "C" fn rune_session_set_open_callback(
    handle: *mut c_void,
    callback: RuneOpenCallback,
    user_data: *mut c_void,
) -> i32 {
    if handle.is_null() {
        return 1;
    }
    let core = unsafe { &mut (*handle.cast::<RuneSession>()).core };
    if let Some(callback) = callback {
        core.set_open_provider(Box::new(CallbackOpenProvider {
            callback,
            user_data,
        }));
    } else {
        core.set_open_provider(Box::new(DisabledOpenProvider));
    }
    0
}

/// Installs or clears one explicit C, C++, or TeX provider.
///
/// The callback is synchronous and must keep all request and response memory
/// borrowed. Passing `None` restores the unavailable provider for that kind.
/// The callback must not start an ambient host shell or write outside the
/// artifacts returned in its response.
#[no_mangle]
pub extern "C" fn rune_session_set_toolchain_callback(
    handle: *mut c_void,
    kind: i32,
    callback: RuneToolchainRequestCallback,
    user_data: *mut c_void,
) -> i32 {
    if handle.is_null() {
        return 1;
    }
    let Some(kind) = toolchain_kind_from_code(kind) else {
        return 2;
    };
    // SAFETY: Swift serializes access to the opaque session handle and keeps
    // it alive while configuring the callback.
    let core = unsafe { &mut (*handle.cast::<RuneSession>()).core };
    if let Some(callback) = callback {
        core.set_toolchain_provider(Box::new(CallbackToolchainProvider {
            kind,
            callback,
            user_data,
        }));
    } else {
        core.set_toolchain_provider(Box::new(DisabledToolchainProvider::new(kind)));
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

/// Clears and persists the Rust-owned terminal screen without recording a
/// shell command in session history.
#[no_mangle]
pub extern "C" fn rune_session_clear_terminal(handle: *mut c_void) -> RuneOutput {
    if handle.is_null() {
        let output = CommandOutput::failure(1, "rune: session is unavailable\n");
        return into_output(&output);
    }
    // SAFETY: Swift serializes access to the opaque session handle and does
    // not call this after rune_session_destroy.
    let core = unsafe { &mut (*handle.cast::<RuneSession>()).core };
    let output = match core.clear_terminal_screen() {
        Ok(()) => CommandOutput::success(""),
        Err(error) => {
            CommandOutput::failure(1, format!("rune: could not clear terminal: {error}\n"))
        }
    };
    into_output(&output)
}

/// Resizes the bounded Rust-owned terminal grid for the native viewport.
/// Dimensions are clamped by Rust; zero is accepted and becomes one.
#[no_mangle]
pub extern "C" fn rune_session_resize_terminal(
    handle: *mut c_void,
    columns: usize,
    rows: usize,
) -> i32 {
    if handle.is_null() {
        return 1;
    }
    // SAFETY: Swift serializes access to the opaque session handle and does
    // not call this after rune_session_destroy.
    let core = unsafe { &mut (*handle.cast::<RuneSession>()).core };
    core.resize_terminal(columns, rows);
    0
}

/// Consumes one host-facing action requested by a Rust command. Returns
/// [`RUNE_SESSION_ACTION_NONE`] when there is no pending action.
#[no_mangle]
pub extern "C" fn rune_session_take_action(handle: *mut c_void) -> i32 {
    if handle.is_null() {
        return RUNE_SESSION_ACTION_NONE;
    }
    // SAFETY: Swift serializes access to the opaque session handle and does
    // not call this after rune_session_destroy.
    let session = unsafe { &mut *handle.cast::<RuneSession>() };
    match session.core.take_action() {
        Some(SessionAction::Exit) => RUNE_SESSION_ACTION_EXIT,
        Some(SessionAction::NewWindow) => RUNE_SESSION_ACTION_NEW_WINDOW,
        Some(SessionAction::PickFolder) => RUNE_SESSION_ACTION_PICK_FOLDER,
        None => RUNE_SESSION_ACTION_NONE,
    }
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

/// Returns a versioned, bounded, non-secret Rust-owned session snapshot as
/// JSON. Environment values and terminal text are deliberately excluded;
/// callers receive counts and terminal geometry instead.
#[no_mangle]
pub extern "C" fn rune_session_snapshot(handle: *const std::ffi::c_void) -> *mut c_char {
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the pointer is read-only and owned by the Swift session.
    let session = unsafe { &*handle.cast::<RuneSession>() };
    into_owned_c_string(&session.core.snapshot().to_json())
}

/// Returns the bounded Rust-owned terminal screen as visible UTF-8 text.
/// Control sequences have already updated the cursor grid and are not returned
/// as raw escape bytes.
#[no_mangle]
pub extern "C" fn rune_session_terminal_snapshot(handle: *const std::ffi::c_void) -> *mut c_char {
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the pointer is read-only and owned by the Swift session.
    let session = unsafe { &*handle.cast::<RuneSession>() };
    let snapshot = session.core.terminal_snapshot();
    into_owned_c_string(&snapshot)
}

/// Returns the bounded, non-persistent diagnostic log as an owned UTF-8
/// string. Records contain safe execution metadata only and must be released
/// with [`rune_string_free`].
#[no_mangle]
pub extern "C" fn rune_session_diagnostics(handle: *const std::ffi::c_void) -> *mut c_char {
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the pointer is read-only and owned by the Swift session.
    let session = unsafe { &*handle.cast::<RuneSession>() };
    into_owned_c_string(&session.core.diagnostics())
}

/// Clears the in-memory diagnostic log without changing persisted session
/// state. Returns zero on success and one for a null handle.
#[no_mangle]
pub extern "C" fn rune_session_clear_diagnostics(handle: *mut std::ffi::c_void) -> i32 {
    if handle.is_null() {
        return 1;
    }
    // SAFETY: Swift serializes access to the opaque session handle and does
    // not call this after rune_session_destroy.
    let session = unsafe { &mut *handle.cast::<RuneSession>() };
    session.core.clear_diagnostics();
    0
}

/// Returns the zero-based cursor position for the bounded Rust-owned screen.
#[no_mangle]
pub extern "C" fn rune_session_terminal_cursor(
    handle: *const std::ffi::c_void,
) -> RuneTerminalCursor {
    if handle.is_null() {
        return RuneTerminalCursor::default();
    }
    // SAFETY: the pointer is read-only and owned by the Swift session.
    let session = unsafe { &*handle.cast::<RuneSession>() };
    let (row, column) = session.core.terminal_cursor_position();
    RuneTerminalCursor {
        row,
        column,
        visible: session.core.terminal_cursor_visible(),
        shape: session.core.terminal_cursor_shape(),
        blink: session.core.terminal_cursor_blink(),
    }
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
        "history-limit={}\nhistory-redaction={}\nenvironment-persistence={}\nfont-size={}\nscrollback-limit={}\ntoolbar-visible={}\ntheme={}\ncursor-color={}\ncursor-shape={}\nfont={}\nbackground={}\nforeground={}\n",
        session.core.configuration().history_limit(),
        session.core.configuration().history_redaction(),
        session.core.configuration().environment_persistence(),
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

/// Applies one Rust-owned completion candidate and returns the complete
/// replacement command as an owned string. Null means invalid input or a
/// candidate that is not currently available for the supplied command line.
#[no_mangle]
pub extern "C" fn rune_session_apply_completion(
    handle: *const std::ffi::c_void,
    input: *const c_char,
    candidate: *const c_char,
) -> *mut c_char {
    let Some(input) = read_string(input) else {
        return std::ptr::null_mut();
    };
    let Some(candidate) = read_string(candidate) else {
        return std::ptr::null_mut();
    };
    if handle.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the pointer is read-only and owned by the Swift session.
    let session = unsafe { &*handle.cast::<RuneSession>() };
    session
        .core
        .apply_completion(&input, &candidate)
        .map_or(std::ptr::null_mut(), |replacement| {
            into_owned_c_string(&replacement)
        })
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
        rune_file_bytes_free, rune_session_apply_completion, rune_session_cancel,
        rune_session_clear_diagnostics, rune_session_clear_terminal, rune_session_commands,
        rune_session_complete, rune_session_configuration, rune_session_current_directory,
        rune_session_destroy, rune_session_diagnostics, rune_session_execute,
        rune_session_execute_script, rune_session_execute_script_with_events,
        rune_session_execute_with_events, rune_session_get_file, rune_session_history,
        rune_session_history_search, rune_session_new, rune_session_new_named,
        rune_session_new_with_layout, rune_session_put_file, rune_session_reset_configuration,
        rune_session_resize_terminal, rune_session_set_clipboard_callbacks,
        rune_session_set_configuration, rune_session_set_network_callback,
        rune_session_set_open_callback, rune_session_set_toolchain_callback, rune_session_snapshot,
        rune_session_startup_output, rune_session_take_action, rune_session_terminal_cursor,
        rune_session_terminal_snapshot, rune_string_free, RuneClipboardResponse, RuneEvent,
        RuneNetworkResponse, RuneTerminalCursor, RuneToolchainArtifactBuffer,
        RuneToolchainEnvironmentEntry, RuneToolchainResponse, RuneToolchainSlice,
        RUNE_EVENT_OUTPUT, RUNE_EVENT_STATUS, RUNE_OPEN_FILE, RUNE_OPEN_PLAY, RUNE_OPEN_URL,
        RUNE_OPEN_VIEW, RUNE_SESSION_ACTION_EXIT, RUNE_SESSION_ACTION_NEW_WINDOW,
        RUNE_SESSION_ACTION_NONE, RUNE_SESSION_ACTION_PICK_FOLDER, RUNE_TOOLCHAIN_C,
    };
    use rune_core::MAX_EVENT_CHUNK_BYTES;
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

    unsafe extern "C" fn test_toolchain_callback(
        user_data: *mut c_void,
        kind: i32,
        program_name: *const c_char,
        source: RuneToolchainSlice,
        args: *const RuneToolchainSlice,
        argument_count: usize,
        environment: *const RuneToolchainEnvironmentEntry,
        environment_count: usize,
        stdin: RuneToolchainSlice,
        stdout_buffer: *mut u8,
        stdout_capacity: usize,
        stderr_buffer: *mut u8,
        stderr_capacity: usize,
        artifact_buffers: *mut RuneToolchainArtifactBuffer,
        artifact_capacity: usize,
        artifact_path_buffers: *mut u8,
        artifact_path_capacity: usize,
        artifact_media_type_buffers: *mut u8,
        artifact_media_type_capacity: usize,
        artifact_data_buffer: *mut u8,
        artifact_data_capacity: usize,
        response: *mut RuneToolchainResponse,
    ) -> bool {
        static STDOUT: &[u8] = b"compiled\n";
        static PATH: &[u8] = b"build/app.wasm";
        static MEDIA_TYPE: &[u8] = b"application/wasm";
        static ARTIFACT_DATA: &[u8] = b"wasm-artifact";
        if user_data.is_null()
            || program_name.is_null()
            || source.data.is_null()
            || response.is_null()
            || args.is_null()
            || environment.is_null()
            || stdout_buffer.is_null()
            || stderr_buffer.is_null()
            || artifact_buffers.is_null()
            || artifact_path_buffers.is_null()
            || artifact_media_type_buffers.is_null()
            || artifact_data_buffer.is_null()
        {
            return false;
        }
        // SAFETY: the callback is invoked synchronously with borrowed request
        // data prepared by CallbackToolchainProvider.
        let calls = unsafe { &mut *user_data.cast::<usize>() };
        *calls += 1;
        let program_name = unsafe { CStr::from_ptr(program_name) }
            .to_str()
            .unwrap_or_default();
        if kind != RUNE_TOOLCHAIN_C || program_name != "main.c" {
            return false;
        }
        let source = unsafe { std::slice::from_raw_parts(source.data, source.length) };
        if source != b"int main(void) { return 0; }\n" || argument_count != 2 {
            return false;
        }
        let args = unsafe { std::slice::from_raw_parts(args, argument_count) };
        let argument = |slice: RuneToolchainSlice| {
            if slice.data.is_null() {
                return None;
            }
            Some(unsafe { std::slice::from_raw_parts(slice.data, slice.length) })
        };
        if argument(args[0]) != Some(b"-o".as_slice())
            || argument(args[1]) != Some(b"app.wasm".as_slice())
            || stdin.length != 0
        {
            return false;
        }
        let environment = unsafe { std::slice::from_raw_parts(environment, environment_count) };
        if environment.is_empty() || environment.iter().any(|entry| entry.key.data.is_null()) {
            return false;
        }
        if stdout_capacity < STDOUT.len()
            || stderr_capacity == 0
            || artifact_capacity == 0
            || artifact_path_capacity < PATH.len()
            || artifact_media_type_capacity < MEDIA_TYPE.len()
            || artifact_data_capacity < ARTIFACT_DATA.len()
        {
            return false;
        }
        // SAFETY: the provider callback receives writable Rune-owned buffers
        // for the duration of this call.
        let artifact_buffers = unsafe { std::slice::from_raw_parts_mut(artifact_buffers, 1) };
        let path_buffer = unsafe {
            std::slice::from_raw_parts_mut(artifact_path_buffers, artifact_path_capacity)
        };
        let media_type_buffer = unsafe {
            std::slice::from_raw_parts_mut(
                artifact_media_type_buffers,
                artifact_media_type_capacity,
            )
        };
        let data_buffer =
            unsafe { std::slice::from_raw_parts_mut(artifact_data_buffer, artifact_data_capacity) };
        unsafe {
            std::ptr::copy_nonoverlapping(STDOUT.as_ptr(), stdout_buffer, STDOUT.len());
            std::ptr::copy_nonoverlapping(PATH.as_ptr(), path_buffer.as_mut_ptr(), PATH.len());
            std::ptr::copy_nonoverlapping(
                MEDIA_TYPE.as_ptr(),
                media_type_buffer.as_mut_ptr(),
                MEDIA_TYPE.len(),
            );
            std::ptr::copy_nonoverlapping(
                ARTIFACT_DATA.as_ptr(),
                data_buffer.as_mut_ptr(),
                ARTIFACT_DATA.len(),
            );
        }
        artifact_buffers[0] = RuneToolchainArtifactBuffer {
            path_length: PATH.len(),
            media_type_length: MEDIA_TYPE.len(),
            data_offset: 0,
            data_length: ARTIFACT_DATA.len(),
        };
        unsafe {
            (*response).stdout_length = STDOUT.len();
            (*response).stderr_length = 0;
            (*response).status = 0;
            (*response).artifact_count = 1;
            (*response).error = 0;
        }
        true
    }

    struct ClipboardState {
        value: String,
        writes: usize,
    }

    unsafe extern "C" fn test_clipboard_read(
        user_data: *mut c_void,
        buffer: *mut u8,
        capacity: usize,
        response: *mut RuneClipboardResponse,
    ) -> bool {
        if user_data.is_null() || response.is_null() {
            return false;
        }
        let state = unsafe { &mut *user_data.cast::<ClipboardState>() };
        let bytes = state.value.as_bytes();
        if bytes.len() > capacity || (!bytes.is_empty() && buffer.is_null()) {
            unsafe {
                (*response).error = 1;
            }
            return false;
        }
        if !bytes.is_empty() {
            unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, bytes.len()) };
        }
        unsafe {
            (*response).text_length = bytes.len();
            (*response).error = 0;
        }
        true
    }

    unsafe extern "C" fn test_clipboard_write(
        user_data: *mut c_void,
        text: *const u8,
        length: usize,
    ) -> bool {
        if user_data.is_null() || (length > 0 && text.is_null()) {
            return false;
        }
        let state = unsafe { &mut *user_data.cast::<ClipboardState>() };
        let bytes = if length == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(text, length) }
        };
        let Ok(value) = std::str::from_utf8(bytes) else {
            return false;
        };
        state.value = value.to_string();
        state.writes += 1;
        true
    }

    struct OpenState {
        targets: Vec<(i32, String)>,
    }

    unsafe extern "C" fn test_open_callback(
        user_data: *mut c_void,
        target: *const c_char,
        target_kind: i32,
    ) -> bool {
        if user_data.is_null() || target.is_null() {
            return false;
        }
        let state = unsafe { &mut *user_data.cast::<OpenState>() };
        let Ok(target) = unsafe { CStr::from_ptr(target) }.to_str() else {
            return false;
        };
        state.targets.push((target_kind, target.to_string()));
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
        let completion_candidate = CString::new("echo").expect("valid completion candidate");
        let replacement =
            rune_session_apply_completion(handle, prefix.as_ptr(), completion_candidate.as_ptr());
        assert_eq!(c_string(replacement), "echo ");
        // SAFETY: replacement was returned by rune_session_apply_completion.
        unsafe { rune_string_free(replacement) };
        let configuration = rune_session_configuration(handle);
        assert_eq!(
            c_string(configuration),
            "history-limit=1000\nhistory-redaction=true\nenvironment-persistence=false\nfont-size=15\nscrollback-limit=4096\ntoolbar-visible=true\ntheme=ink\ncursor-color=cyan\ncursor-shape=bar\nfont=monospaced\nbackground=auto\nforeground=auto\n"
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

        let function_script =
            CString::new("finish() {\necho ffi\nreturn 7\necho unreachable\n}\nfinish")
                .expect("valid function script");
        let function_output = rune_session_execute_script(handle, function_script.as_ptr());
        assert_eq!(function_output.status, 7);
        assert_eq!(c_string(function_output.stdout), "ffi\n");
        assert!(c_string(function_output.stderr).is_empty());
        // SAFETY: both pointers were returned by rune_session_execute_script
        // and are released exactly once.
        unsafe {
            rune_string_free(function_output.stdout);
            rune_string_free(function_output.stderr);
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
        let redaction_key = CString::new("history-redaction").expect("valid key");
        let redaction_value = CString::new("false").expect("valid value");
        let redaction_changed = rune_session_set_configuration(
            handle,
            redaction_key.as_ptr(),
            redaction_value.as_ptr(),
        );
        assert_eq!(redaction_changed.status, 0);
        // SAFETY: both pointers came from rune_session_set_configuration.
        unsafe {
            rune_string_free(redaction_changed.stdout);
            rune_string_free(redaction_changed.stderr);
        }
        let environment_key = CString::new("environment-persistence").expect("valid key");
        let environment_value = CString::new("true").expect("valid value");
        let environment_changed = rune_session_set_configuration(
            handle,
            environment_key.as_ptr(),
            environment_value.as_ptr(),
        );
        assert_eq!(environment_changed.status, 0);
        // SAFETY: both pointers came from rune_session_set_configuration.
        unsafe {
            rune_string_free(environment_changed.stdout);
            rune_string_free(environment_changed.stderr);
        }
        let configuration = rune_session_configuration(handle);
        assert!(c_string(configuration).contains("font-size=20"));
        assert!(c_string(configuration).contains("history-redaction=false"));
        assert!(c_string(configuration).contains("environment-persistence=true"));
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

    #[test]
    fn c_abi_bridges_bounded_text_clipboard_commands() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rune-ffi-clipboard-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("test root created");
        let root_string = CString::new(root.to_string_lossy().as_bytes()).expect("valid root");
        let handle = rune_session_new(root_string.as_ptr());
        assert!(!handle.is_null());
        assert_eq!(
            rune_session_set_clipboard_callbacks(
                handle,
                Some(test_clipboard_read),
                None,
                std::ptr::null_mut(),
            ),
            2
        );
        let mut state = ClipboardState {
            value: String::new(),
            writes: 0,
        };
        assert_eq!(
            rune_session_set_clipboard_callbacks(
                handle,
                Some(test_clipboard_read),
                Some(test_clipboard_write),
                std::ptr::addr_of_mut!(state).cast(),
            ),
            0
        );

        let copy = CString::new("printf copied | pbcopy").expect("valid command");
        let output = rune_session_execute(handle, copy.as_ptr());
        assert_eq!(output.status, 0);
        assert_eq!(state.value, "copied");
        assert_eq!(state.writes, 1);
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }

        let paste = CString::new("pbpaste").expect("valid command");
        let output = rune_session_execute(handle, paste.as_ptr());
        assert_eq!(output.status, 0);
        assert_eq!(c_string(output.stdout), "copied");
        assert!(c_string(output.stderr).is_empty());
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }

        assert_eq!(
            rune_session_set_clipboard_callbacks(handle, None, None, std::ptr::null_mut(),),
            0
        );
        let unavailable = rune_session_execute(handle, paste.as_ptr());
        assert_eq!(unavailable.status, 1);
        assert!(c_string(unavailable.stderr).contains("clipboard provider is unavailable"));
        unsafe {
            rune_string_free(unavailable.stdout);
            rune_string_free(unavailable.stderr);
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
    fn c_abi_forwards_utf8_safe_output_chunks() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rune-ffi-chunks-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("test root created");
        let expected = "🙂".repeat((MAX_EVENT_CHUNK_BYTES / "🙂".len()) * 2 + 5);
        std::fs::write(root.join("large.txt"), expected.as_bytes()).expect("large file written");
        let root_string = CString::new(root.to_string_lossy().as_bytes()).expect("valid root");
        let handle = rune_session_new(root_string.as_ptr());
        assert!(!handle.is_null());
        let command = CString::new("cat large.txt").expect("valid command");
        let mut records: Vec<EventRecord> = Vec::new();
        let output = rune_session_execute_with_events(
            handle,
            command.as_ptr(),
            Some(collect_event),
            std::ptr::addr_of_mut!(records).cast(),
        );
        assert_eq!(output.status, 0);
        let mut emitted = String::new();
        let mut chunks = 0;
        for record in &records {
            if record.kind != RUNE_EVENT_OUTPUT {
                continue;
            }
            assert!(record.stdout.len() <= MAX_EVENT_CHUNK_BYTES);
            if !record.stdout.is_empty() {
                assert_eq!(record.stdout.len() % "🙂".len(), 0);
            }
            emitted.push_str(&record.stdout);
            chunks += 1;
        }
        assert!(chunks > 1);
        assert_eq!(emitted, expected);
        // SAFETY: both pointers came from the event-aware FFI call.
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
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

    #[test]
    fn c_abi_routes_open_through_the_native_callback() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rune-ffi-open-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("test root created");
        std::fs::write(root.join("note.txt"), b"open me\n").expect("file written");
        let root_string = CString::new(root.to_string_lossy().as_bytes()).expect("valid root");
        let handle = rune_session_new(root_string.as_ptr());
        assert!(!handle.is_null());
        let mut state = OpenState {
            targets: Vec::new(),
        };
        assert_eq!(
            rune_session_set_open_callback(
                handle,
                Some(test_open_callback),
                std::ptr::addr_of_mut!(state).cast(),
            ),
            0
        );

        let file_command = CString::new("open note.txt").expect("valid file command");
        let output = rune_session_execute(handle, file_command.as_ptr());
        assert_eq!(output.status, 0);
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        let url_command =
            CString::new("openurl shortcuts://run-shortcut").expect("valid URL command");
        let output = rune_session_execute(handle, url_command.as_ptr());
        assert_eq!(output.status, 0);
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        let play_command = CString::new("play note.txt").expect("valid play command");
        let output = rune_session_execute(handle, play_command.as_ptr());
        assert_eq!(output.status, 0);
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        let view_command = CString::new("view note.txt").expect("valid view command");
        let output = rune_session_execute(handle, view_command.as_ptr());
        assert_eq!(output.status, 0);
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        assert_eq!(state.targets.len(), 4);
        assert_eq!(state.targets[0].0, RUNE_OPEN_FILE);
        assert!(state.targets[0].1.ends_with("/note.txt"));
        assert_eq!(
            state.targets[1],
            (RUNE_OPEN_URL, "shortcuts://run-shortcut".to_string())
        );
        assert_eq!(state.targets[2].0, RUNE_OPEN_PLAY);
        assert!(state.targets[2].1.ends_with("/note.txt"));
        assert_eq!(state.targets[3].0, RUNE_OPEN_VIEW);
        assert!(state.targets[3].1.ends_with("/note.txt"));

        assert_eq!(
            rune_session_set_open_callback(handle, None, std::ptr::null_mut()),
            0
        );
        let disabled_command =
            CString::new("openurl https://example.test").expect("valid disabled command");
        let disabled = rune_session_execute(handle, disabled_command.as_ptr());
        assert_eq!(disabled.status, 1);
        assert!(c_string(disabled.stderr).contains("provider is unavailable"));
        unsafe {
            rune_string_free(disabled.stdout);
            rune_string_free(disabled.stderr);
        }
        rune_session_destroy(handle);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn c_abi_routes_toolchain_through_bounded_callback_and_vfs() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rune-ffi-toolchain-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("test root created");
        let root_string = CString::new(root.to_string_lossy().as_bytes()).expect("valid root");
        let handle = rune_session_new(root_string.as_ptr());
        assert!(!handle.is_null());
        let source_path = CString::new("main.c").expect("valid source path");
        let source = b"int main(void) { return 0; }\n";
        let source_output = unsafe {
            rune_session_put_file(handle, source_path.as_ptr(), source.as_ptr(), source.len())
        };
        assert_eq!(source_output.status, 0);
        unsafe {
            rune_string_free(source_output.stdout);
            rune_string_free(source_output.stderr);
        }

        let mut calls = 0_usize;
        assert_eq!(
            rune_session_set_toolchain_callback(
                handle,
                RUNE_TOOLCHAIN_C,
                Some(test_toolchain_callback),
                std::ptr::addr_of_mut!(calls).cast(),
            ),
            0
        );
        let command = CString::new("cc main.c -o app.wasm").expect("valid command");
        let output = rune_session_execute(handle, command.as_ptr());
        assert_eq!(
            output.status,
            0,
            "stdout={:?} stderr={:?} calls={calls}",
            c_string(output.stdout),
            c_string(output.stderr)
        );
        assert_eq!(c_string(output.stdout), "compiled\n");
        assert!(c_string(output.stderr).is_empty());
        assert_eq!(calls, 1);
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }

        let artifact_path = CString::new("build/app.wasm").expect("valid artifact path");
        let artifact = rune_session_get_file(handle, artifact_path.as_ptr());
        assert_eq!(artifact.status, 0);
        assert_eq!(artifact.length, b"wasm-artifact".len());
        assert_eq!(
            unsafe { std::slice::from_raw_parts(artifact.data, artifact.length) },
            b"wasm-artifact"
        );
        unsafe {
            rune_file_bytes_free(artifact.data, artifact.length);
            rune_string_free(artifact.message);
        }

        assert_eq!(
            rune_session_set_toolchain_callback(
                handle,
                RUNE_TOOLCHAIN_C,
                None,
                std::ptr::null_mut(),
            ),
            0
        );
        let disabled = rune_session_execute(handle, command.as_ptr());
        assert_eq!(disabled.status, 126);
        assert!(c_string(disabled.stderr).contains("toolchain provider is unavailable"));
        unsafe {
            rune_string_free(disabled.stdout);
            rune_string_free(disabled.stderr);
        }
        assert_eq!(
            rune_session_set_toolchain_callback(handle, 99, None, std::ptr::null_mut()),
            2
        );
        rune_session_destroy(handle);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn c_abi_exposes_the_rust_owned_terminal_snapshot() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rune-ffi-terminal-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("test root created");
        let root_string = CString::new(root.to_string_lossy().as_bytes()).expect("valid root");
        let handle = rune_session_new(root_string.as_ptr());
        assert!(!handle.is_null());

        let command = CString::new("printf 'stale\r\u{1b}[2Kready'").expect("valid command");
        let output = rune_session_execute(handle, command.as_ptr());
        assert_eq!(output.status, 0);
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        let snapshot = rune_session_terminal_snapshot(handle);
        assert_eq!(c_string(snapshot), "ready");
        unsafe { rune_string_free(snapshot) };
        assert_eq!(
            rune_session_terminal_cursor(handle),
            RuneTerminalCursor {
                row: 0,
                column: 5,
                visible: true,
                shape: 0,
                blink: 0
            }
        );
        assert_eq!(rune_session_resize_terminal(handle, 4, 2), 0);
        let resized_snapshot = rune_session_terminal_snapshot(handle);
        assert_eq!(c_string(resized_snapshot), "read");
        unsafe { rune_string_free(resized_snapshot) };
        assert_eq!(
            rune_session_terminal_cursor(handle),
            RuneTerminalCursor {
                row: 0,
                column: 4,
                visible: true,
                shape: 0,
                blink: 0
            }
        );
        assert!(rune_session_terminal_snapshot(std::ptr::null()).is_null());
        assert_eq!(
            rune_session_terminal_cursor(std::ptr::null()),
            RuneTerminalCursor::default()
        );
        assert_eq!(rune_session_resize_terminal(std::ptr::null_mut(), 4, 2), 1);

        rune_session_destroy(handle);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn c_abi_exposes_safe_bounded_diagnostics_and_can_clear_them() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rune-ffi-diagnostics-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("test root created");
        let root_string = CString::new(root.to_string_lossy().as_bytes()).expect("valid root");
        let handle = rune_session_new(root_string.as_ptr());
        assert!(!handle.is_null());

        let command = CString::new("echo private-command-value").expect("valid command");
        let output = rune_session_execute(handle, command.as_ptr());
        assert_eq!(output.status, 0);
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        let diagnostics = rune_session_diagnostics(handle);
        let diagnostics_text = c_string(diagnostics);
        assert!(diagnostics_text.contains("execution: completed status=0"));
        assert!(!diagnostics_text.contains("private-command-value"));
        unsafe { rune_string_free(diagnostics) };

        assert_eq!(rune_session_clear_diagnostics(handle), 0);
        let empty = rune_session_diagnostics(handle);
        assert!(c_string(empty).is_empty());
        unsafe { rune_string_free(empty) };
        assert!(rune_session_diagnostics(std::ptr::null()).is_null());
        assert_eq!(rune_session_clear_diagnostics(std::ptr::null_mut()), 1);

        rune_session_destroy(handle);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn c_abi_transfers_host_facing_session_actions_once() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rune-ffi-actions-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("test root created");
        let root_string = CString::new(root.to_string_lossy().as_bytes()).expect("valid root");
        let handle = rune_session_new(root_string.as_ptr());
        assert!(!handle.is_null());

        let exit = CString::new("exit").expect("valid command");
        let output = rune_session_execute(handle, exit.as_ptr());
        assert_eq!(output.status, 0);
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        assert_eq!(rune_session_take_action(handle), RUNE_SESSION_ACTION_EXIT);
        assert_eq!(rune_session_take_action(handle), RUNE_SESSION_ACTION_NONE);

        let new_window = CString::new("newWindow").expect("valid command");
        let output = rune_session_execute(handle, new_window.as_ptr());
        assert_eq!(output.status, 0);
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        assert_eq!(
            rune_session_take_action(handle),
            RUNE_SESSION_ACTION_NEW_WINDOW
        );
        let pick_folder = CString::new("pickFolder").expect("valid command");
        let output = rune_session_execute(handle, pick_folder.as_ptr());
        assert_eq!(output.status, 0);
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        assert_eq!(
            rune_session_take_action(handle),
            RUNE_SESSION_ACTION_PICK_FOLDER
        );
        assert_eq!(
            rune_session_take_action(std::ptr::null_mut()),
            RUNE_SESSION_ACTION_NONE
        );

        rune_session_destroy(handle);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn c_abi_exposes_versioned_non_secret_session_snapshot() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rune-ffi-snapshot-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("test root created");
        let root_string = CString::new(root.to_string_lossy().as_bytes()).expect("valid root");
        let session_id = CString::new("window-a").expect("valid session id");
        let handle = rune_session_new_named(root_string.as_ptr(), session_id.as_ptr());
        assert!(!handle.is_null());

        let command = CString::new("mkdir work && cd work").expect("valid command");
        let output = rune_session_execute(handle, command.as_ptr());
        assert_eq!(output.status, 0);
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        let secret = CString::new("export PRIVATE=not-in-snapshot").expect("valid command");
        let output = rune_session_execute(handle, secret.as_ptr());
        assert_eq!(output.status, 0);
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }

        let snapshot = rune_session_snapshot(handle);
        let snapshot_text = c_string(snapshot);
        assert!(snapshot_text.contains("\"schema_version\":1"));
        assert!(snapshot_text.contains("\"id\":\"window-a\""));
        assert!(snapshot_text.contains("\"working_directory\":\"~/work\""));
        assert!(snapshot_text.contains("\"history_count\":2"));
        assert!(snapshot_text.contains("\"terminal_state\""));
        assert!(!snapshot_text.contains("not-in-snapshot"));
        unsafe { rune_string_free(snapshot) };
        assert!(rune_session_snapshot(std::ptr::null()).is_null());

        rune_session_destroy(handle);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn c_abi_clears_and_persists_the_rust_owned_terminal_screen() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rune-ffi-clear-terminal-test-{suffix}"));
        std::fs::create_dir_all(&root).expect("test root created");
        let root_string = CString::new(root.to_string_lossy().as_bytes()).expect("valid root");
        let handle = rune_session_new(root_string.as_ptr());
        assert!(!handle.is_null());

        let command = CString::new("printf 'stale'").expect("valid command");
        let output = rune_session_execute(handle, command.as_ptr());
        assert_eq!(output.status, 0);
        unsafe {
            rune_string_free(output.stdout);
            rune_string_free(output.stderr);
        }
        let snapshot = rune_session_terminal_snapshot(handle);
        assert_eq!(c_string(snapshot), "stale");
        unsafe { rune_string_free(snapshot) };
        let cleared = rune_session_clear_terminal(handle);
        assert_eq!(cleared.status, 0);
        unsafe {
            rune_string_free(cleared.stdout);
            rune_string_free(cleared.stderr);
        }
        let snapshot = rune_session_terminal_snapshot(handle);
        assert!(c_string(snapshot).is_empty());
        unsafe { rune_string_free(snapshot) };
        rune_session_destroy(handle);

        let restored = rune_session_new(root_string.as_ptr());
        assert!(!restored.is_null());
        let snapshot = rune_session_terminal_snapshot(restored);
        assert!(c_string(snapshot).is_empty());
        unsafe { rune_string_free(snapshot) };
        rune_session_destroy(restored);
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    fn c_string(pointer: *mut std::os::raw::c_char) -> String {
        assert!(!pointer.is_null());
        // SAFETY: the test only reads a NUL-terminated CString returned by the
        // ABI before its matching free call.
        unsafe { CStr::from_ptr(pointer).to_string_lossy().into_owned() }
    }
}
