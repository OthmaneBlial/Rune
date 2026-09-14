//! Stable, platform-neutral contracts for Rune language runtimes.
//!
//! This crate owns the runtime request/output boundary and the embedded
//! Python, Lua, and JavaScript providers. WASM remains in its dedicated crate.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mlua::{ChunkMode, Error as LuaError, HookTriggers, Lua, LuaOptions, MultiValue, StdLib};
use rquickjs::{prelude::Func, Array, CatchResultExt, Context, Object, Runtime as JsRuntime};
use rustpython_vm::builtins::{PyCode, PyStrRef};
use rustpython_vm::bytecode::{Instruction, OpArgState};
use rustpython_vm::function::FuncArgs;
use rustpython_vm::{
    AsObject, Interpreter, PyObjectRef, PyResult as PythonResult, Settings, VirtualMachine,
};

/// Maximum UTF-8 Lua source accepted by the embedded provider.
pub const MAX_LUA_SOURCE_BYTES: usize = 256 * 1024;
/// Maximum captured stdout or stderr returned by one Lua invocation.
pub const MAX_LUA_OUTPUT_BYTES: usize = 1024 * 1024;
/// Maximum allocator budget for one Lua state.
pub const MAX_LUA_MEMORY_BYTES: usize = 32 * 1024 * 1024;
/// Maximum VM instructions for one Lua invocation.
pub const MAX_LUA_INSTRUCTIONS: u64 = 2_000_000;
const LUA_HOOK_INTERVAL: u32 = 1_000;

/// Maximum UTF-8 JavaScript source accepted by the embedded provider.
pub const MAX_JAVASCRIPT_SOURCE_BYTES: usize = 256 * 1024;
/// Maximum captured stdout or stderr returned by one JavaScript invocation.
pub const MAX_JAVASCRIPT_OUTPUT_BYTES: usize = 1024 * 1024;
/// Maximum allocator budget for one JavaScript runtime.
pub const MAX_JAVASCRIPT_MEMORY_BYTES: usize = 32 * 1024 * 1024;
/// Maximum native stack budget requested from `QuickJS`.
pub const MAX_JAVASCRIPT_STACK_BYTES: usize = 1024 * 1024;
/// Maximum `QuickJS` instructions represented by the interrupt budget.
pub const MAX_JAVASCRIPT_INSTRUCTIONS: u64 = 2_000_000;
const JAVASCRIPT_INTERRUPT_INTERVAL: u64 = 10_000;
const JAVASCRIPT_MAX_ARGUMENTS: usize = 64;
const JAVASCRIPT_MAX_STDIN_BYTES: usize = 1024 * 1024;
const JAVASCRIPT_MAX_ENVIRONMENT_BYTES: usize = 1024 * 1024;

/// Maximum UTF-8 Python source accepted by the embedded provider.
pub const MAX_PYTHON_SOURCE_BYTES: usize = 256 * 1024;
/// Maximum captured stdout or stderr returned by one Python invocation.
pub const MAX_PYTHON_OUTPUT_BYTES: usize = 1024 * 1024;
/// Maximum number of top-level Python bytecode instructions accepted.
///
/// `RustPython` 0.4 does not expose a public per-instruction interrupt hook.
/// Rune therefore accepts a deliberately finite bytecode subset for this
/// provider: loops and dynamically-created code are rejected before running.
pub const MAX_PYTHON_INSTRUCTIONS: usize = 100_000;
const PYTHON_MAX_ARGUMENTS: usize = 64;
const PYTHON_MAX_STDIN_BYTES: usize = 1024 * 1024;
const PYTHON_MAX_ENVIRONMENT_BYTES: usize = 1024 * 1024;
const PYTHON_MAX_RECURSION_DEPTH: usize = 64;

/// Maximum source size accepted by a future C, C++, or TeX toolchain
/// provider. The limit applies before a provider receives the source bytes.
pub const MAX_TOOLCHAIN_SOURCE_BYTES: usize = 8 * 1024 * 1024;
/// Maximum UTF-8 stdin accepted by a toolchain provider.
pub const MAX_TOOLCHAIN_STDIN_BYTES: usize = 1024 * 1024;
/// Maximum number of compiler or TeX arguments in one request.
pub const MAX_TOOLCHAIN_ARGUMENTS: usize = 64;
/// Maximum total UTF-8 environment bytes copied into a toolchain provider.
pub const MAX_TOOLCHAIN_ENVIRONMENT_BYTES: usize = 1024 * 1024;
/// Maximum captured output stream returned by a toolchain provider.
pub const MAX_TOOLCHAIN_OUTPUT_BYTES: usize = 1024 * 1024;
/// Maximum number of generated files returned by one toolchain invocation.
pub const MAX_TOOLCHAIN_ARTIFACTS: usize = 128;
/// Maximum size of one generated toolchain artifact.
pub const MAX_TOOLCHAIN_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOOLCHAIN_PROGRAM_NAME_BYTES: usize = 256;
const MAX_TOOLCHAIN_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_TOOLCHAIN_MEDIA_TYPE_BYTES: usize = 128;

/// A runtime family Rune may eventually host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RuntimeKind {
    /// WebAssembly modules using the WASI preview1 boundary.
    Wasm,
    /// Python source or bytecode.
    Python,
    /// JavaScript source or bytecode.
    JavaScript,
    /// Lua source or bytecode.
    Lua,
}

impl RuntimeKind {
    /// Returns the stable lower-case name used in diagnostics and metadata.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Wasm => "wasm",
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::Lua => "lua",
        }
    }
}

impl Display for RuntimeKind {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// One explicitly approved host directory and its guest-visible WASI path.
#[derive(Debug, Clone, Copy)]
pub struct RuntimePreopen<'a> {
    /// Host directory opened by the provider through its capability API.
    pub host_path: &'a Path,
    /// Absolute guest path at which the directory is mounted.
    pub guest_path: &'a str,
}

impl<'a> RuntimePreopen<'a> {
    /// Creates one runtime preopen descriptor.
    #[must_use]
    pub const fn new(host_path: &'a Path, guest_path: &'a str) -> Self {
        Self {
            host_path,
            guest_path,
        }
    }
}

/// Immutable input passed to one runtime provider.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeRequest<'a> {
    kind: RuntimeKind,
    /// The virtual module/script name shown to the guest runtime.
    pub program_name: &'a str,
    /// Module, source, or bytecode bytes owned by the caller.
    pub source: &'a [u8],
    /// Arguments after the program name.
    pub args: &'a [String],
    /// Session environment copied into the runtime boundary.
    pub environment: &'a BTreeMap<String, String>,
    /// Input connected to the runtime's standard input.
    pub stdin: &'a str,
    /// Optional primary approved host directory exposed at guest `/`.
    /// `None` means that this primary filesystem capability is unavailable.
    pub preopened_root: Option<&'a Path>,
    /// Additional explicitly approved host directories and their guest paths.
    pub additional_preopens: &'a [RuntimePreopen<'a>],
    /// Optional cooperative cancellation flag checked at provider-defined
    /// execution boundaries.
    pub cancellation: Option<&'a AtomicBool>,
}

impl<'a> RuntimeRequest<'a> {
    /// Builds a request for a specific runtime family.
    #[must_use]
    pub const fn new(
        kind: RuntimeKind,
        program_name: &'a str,
        source: &'a [u8],
        args: &'a [String],
        environment: &'a BTreeMap<String, String>,
        stdin: &'a str,
    ) -> Self {
        Self {
            kind,
            program_name,
            source,
            args,
            environment,
            stdin,
            preopened_root: None,
            additional_preopens: &[],
            cancellation: None,
        }
    }

    /// Adds one explicitly approved host directory to the runtime request.
    #[must_use]
    pub const fn with_preopened_root(mut self, root: Option<&'a Path>) -> Self {
        self.preopened_root = root;
        self
    }

    /// Adds additional explicitly approved host directories to the request.
    #[must_use]
    pub const fn with_additional_preopens(mut self, preopens: &'a [RuntimePreopen<'a>]) -> Self {
        self.additional_preopens = preopens;
        self
    }

    /// Adds a cooperative cancellation flag to the runtime request.
    #[must_use]
    pub const fn with_cancellation(mut self, cancellation: Option<&'a AtomicBool>) -> Self {
        self.cancellation = cancellation;
        self
    }

    /// Returns the runtime family requested by this invocation.
    #[must_use]
    pub const fn kind(&self) -> RuntimeKind {
        self.kind
    }
}

/// Output returned by a runtime provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: i32,
}

/// Errors raised before a runtime can return command output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    /// The provider does not serve the requested runtime family.
    UnsupportedKind {
        requested: RuntimeKind,
        provider: RuntimeKind,
    },
    /// The request is invalid independently of guest execution.
    InvalidRequest(String),
    /// The provider could not validate, start, or execute the guest.
    Execution(String),
}

impl Display for RuntimeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedKind {
                requested,
                provider,
            } => write!(
                formatter,
                "runtime provider {provider} cannot execute {requested}"
            ),
            Self::InvalidRequest(message) => {
                write!(formatter, "invalid runtime request: {message}")
            }
            Self::Execution(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for RuntimeError {}

/// Provider boundary used by the command engine.
pub trait Runtime {
    /// Identifies the runtime family implemented by this provider.
    fn kind(&self) -> RuntimeKind;

    /// Executes one bounded request and returns captured output/status.
    ///
    /// Providers must not use an ambient host shell or inherit capabilities
    /// that are absent from the request's explicit boundary.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when the provider rejects the requested kind,
    /// the input boundary is invalid, or guest execution cannot start.
    fn execute(&self, request: &RuntimeRequest<'_>) -> Result<RuntimeOutput, RuntimeError>;
}

/// A source toolchain family that can be supplied by a bounded provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ToolchainKind {
    /// C source compiled to a Rune-approved artifact, normally WASM.
    C,
    /// C++ source compiled to a Rune-approved artifact, normally WASM.
    Cpp,
    /// TeX source rendered to a Rune-approved document artifact.
    Tex,
}

impl ToolchainKind {
    /// Returns the stable lower-case name used in diagnostics and metadata.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::C => "c",
            Self::Cpp => "c++",
            Self::Tex => "tex",
        }
    }
}

impl Display for ToolchainKind {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// Errors raised at the bounded source-toolchain boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolchainError {
    /// The provider serves a different toolchain family.
    UnsupportedKind {
        requested: ToolchainKind,
        provider: ToolchainKind,
    },
    /// The request or a provider-produced artifact violates a limit.
    InvalidRequest(String),
    /// The provider is unavailable or could not finish the request.
    Execution(String),
}

impl Display for ToolchainError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedKind {
                requested,
                provider,
            } => write!(
                formatter,
                "toolchain provider {provider} cannot execute {requested}"
            ),
            Self::InvalidRequest(message) => {
                write!(formatter, "invalid toolchain request: {message}")
            }
            Self::Execution(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ToolchainError {}

/// Immutable source and capability input for one toolchain invocation.
#[derive(Debug, Clone, Copy)]
pub struct ToolchainRequest<'a> {
    kind: ToolchainKind,
    /// Source or project entry name shown in diagnostics.
    pub program_name: &'a str,
    /// Source bytes supplied by the confined caller.
    pub source: &'a [u8],
    /// Explicit compiler or TeX arguments after the program name.
    pub args: &'a [String],
    /// Session environment copied into the provider boundary.
    pub environment: &'a BTreeMap<String, String>,
    /// Input connected to the toolchain's standard input.
    pub stdin: &'a str,
    /// Optional cooperative cancellation flag checked by the provider.
    pub cancellation: Option<&'a AtomicBool>,
}

impl<'a> ToolchainRequest<'a> {
    /// Builds a request for a specific C, C++, or TeX provider.
    #[must_use]
    pub const fn new(
        kind: ToolchainKind,
        program_name: &'a str,
        source: &'a [u8],
        args: &'a [String],
        environment: &'a BTreeMap<String, String>,
        stdin: &'a str,
    ) -> Self {
        Self {
            kind,
            program_name,
            source,
            args,
            environment,
            stdin,
            cancellation: None,
        }
    }

    /// Adds the cooperative cancellation flag for this invocation.
    #[must_use]
    pub const fn with_cancellation(mut self, cancellation: Option<&'a AtomicBool>) -> Self {
        self.cancellation = cancellation;
        self
    }

    /// Returns the requested toolchain family.
    #[must_use]
    pub const fn kind(&self) -> ToolchainKind {
        self.kind
    }

    /// Validates all input bounds before a provider is called.
    ///
    /// # Errors
    ///
    /// Returns an explicit error when source, arguments, environment, or stdin
    /// exceed the stable toolchain boundary.
    pub fn validate(&self) -> Result<(), ToolchainError> {
        if self.program_name.is_empty()
            || self.program_name.len() > MAX_TOOLCHAIN_PROGRAM_NAME_BYTES
        {
            return Err(ToolchainError::InvalidRequest(format!(
                "program name must contain 1-{MAX_TOOLCHAIN_PROGRAM_NAME_BYTES} bytes"
            )));
        }
        if self.source.len() > MAX_TOOLCHAIN_SOURCE_BYTES {
            return Err(ToolchainError::InvalidRequest(format!(
                "source exceeds {MAX_TOOLCHAIN_SOURCE_BYTES} bytes"
            )));
        }
        if self.args.len() > MAX_TOOLCHAIN_ARGUMENTS {
            return Err(ToolchainError::InvalidRequest(format!(
                "arguments exceed the {MAX_TOOLCHAIN_ARGUMENTS}-argument limit"
            )));
        }
        let mut argument_bytes = 0_usize;
        for argument in self.args {
            if argument.len() > MAX_TOOLCHAIN_ARGUMENT_BYTES {
                return Err(ToolchainError::InvalidRequest(format!(
                    "one argument exceeds {MAX_TOOLCHAIN_ARGUMENT_BYTES} bytes"
                )));
            }
            argument_bytes = argument_bytes
                .checked_add(argument.len())
                .ok_or_else(|| ToolchainError::InvalidRequest("argument size overflow".into()))?;
        }
        if argument_bytes > MAX_TOOLCHAIN_ENVIRONMENT_BYTES {
            return Err(ToolchainError::InvalidRequest(format!(
                "arguments exceed {MAX_TOOLCHAIN_ENVIRONMENT_BYTES} bytes"
            )));
        }
        let environment_bytes =
            self.environment
                .iter()
                .try_fold(0_usize, |total, (key, value)| {
                    total
                        .checked_add(key.len())
                        .and_then(|total| total.checked_add(value.len()))
                        .ok_or_else(|| {
                            ToolchainError::InvalidRequest("environment size overflow".into())
                        })
                })?;
        if environment_bytes > MAX_TOOLCHAIN_ENVIRONMENT_BYTES {
            return Err(ToolchainError::InvalidRequest(format!(
                "environment exceeds {MAX_TOOLCHAIN_ENVIRONMENT_BYTES} bytes"
            )));
        }
        if self.stdin.len() > MAX_TOOLCHAIN_STDIN_BYTES {
            return Err(ToolchainError::InvalidRequest(format!(
                "stdin exceeds {MAX_TOOLCHAIN_STDIN_BYTES} bytes"
            )));
        }
        Ok(())
    }
}

/// One generated file returned by a C/C++ or TeX provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainArtifact {
    /// Relative path at which the caller may materialize the artifact.
    pub path: String,
    /// Bounded media type used by the caller to select execution or display.
    pub media_type: String,
    /// Artifact bytes owned by the provider result.
    pub bytes: Vec<u8>,
}

/// Captured output and generated files returned by a toolchain provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: i32,
    pub artifacts: Vec<ToolchainArtifact>,
}

impl ToolchainOutput {
    /// Validates provider output before the caller writes any artifact.
    ///
    /// # Errors
    ///
    /// Returns an explicit error for oversized streams, duplicate or unsafe
    /// paths, unsupported media metadata, or oversized artifacts.
    pub fn validate(&self) -> Result<(), ToolchainError> {
        if self.stdout.len() > MAX_TOOLCHAIN_OUTPUT_BYTES {
            return Err(ToolchainError::InvalidRequest(format!(
                "stdout exceeds {MAX_TOOLCHAIN_OUTPUT_BYTES} bytes"
            )));
        }
        if self.stderr.len() > MAX_TOOLCHAIN_OUTPUT_BYTES {
            return Err(ToolchainError::InvalidRequest(format!(
                "stderr exceeds {MAX_TOOLCHAIN_OUTPUT_BYTES} bytes"
            )));
        }
        if self.artifacts.len() > MAX_TOOLCHAIN_ARTIFACTS {
            return Err(ToolchainError::InvalidRequest(format!(
                "artifacts exceed the {MAX_TOOLCHAIN_ARTIFACTS}-file limit"
            )));
        }
        let mut paths = BTreeSet::new();
        for artifact in &self.artifacts {
            if artifact.path.is_empty()
                || artifact.path.starts_with('/')
                || artifact.path.contains('\\')
                || artifact
                    .path
                    .split('/')
                    .any(|component| component.is_empty() || matches!(component, "." | ".."))
                || !paths.insert(&artifact.path)
            {
                return Err(ToolchainError::InvalidRequest(format!(
                    "artifact path is unsafe or duplicated: {}",
                    artifact.path
                )));
            }
            if artifact.media_type.is_empty()
                || artifact.media_type.len() > MAX_TOOLCHAIN_MEDIA_TYPE_BYTES
                || artifact.media_type.chars().any(char::is_control)
            {
                return Err(ToolchainError::InvalidRequest(format!(
                    "artifact media type is invalid: {}",
                    artifact.media_type
                )));
            }
            if artifact.bytes.len() > MAX_TOOLCHAIN_ARTIFACT_BYTES {
                return Err(ToolchainError::InvalidRequest(format!(
                    "artifact {} exceeds {MAX_TOOLCHAIN_ARTIFACT_BYTES} bytes",
                    artifact.path
                )));
            }
        }
        Ok(())
    }
}

/// Provider boundary for C, C++, and TeX toolchains.
pub trait ToolchainProvider {
    /// Identifies the one toolchain family served by this provider.
    fn kind(&self) -> ToolchainKind;

    /// Processes one bounded request without inheriting ambient host access.
    ///
    /// A provider returns generated artifacts instead of writing arbitrary
    /// host paths. The caller remains responsible for validating and
    /// materializing those artifacts inside Rune's VFS.
    ///
    /// # Errors
    ///
    /// Returns [`ToolchainError`] when the kind, input boundary, provider, or
    /// generated artifact set is invalid.
    fn execute(&self, request: &ToolchainRequest<'_>) -> Result<ToolchainOutput, ToolchainError>;
}

/// Explicit unavailable-provider implementation used until a real compiler
/// or TeX engine is installed through a reviewed capability boundary.
#[derive(Debug, Clone, Copy)]
pub struct DisabledToolchainProvider {
    kind: ToolchainKind,
}

impl DisabledToolchainProvider {
    /// Creates an unavailable provider for one toolchain family.
    #[must_use]
    pub const fn new(kind: ToolchainKind) -> Self {
        Self { kind }
    }
}

impl ToolchainProvider for DisabledToolchainProvider {
    fn kind(&self) -> ToolchainKind {
        self.kind
    }

    fn execute(&self, request: &ToolchainRequest<'_>) -> Result<ToolchainOutput, ToolchainError> {
        if request.kind() != self.kind() {
            return Err(ToolchainError::UnsupportedKind {
                requested: request.kind(),
                provider: self.kind(),
            });
        }
        request.validate()?;
        Err(ToolchainError::Execution(format!(
            "{kind} toolchain provider is unavailable",
            kind = self.kind
        )))
    }
}

/// A bounded Python provider backed by `RustPython` without its host standard
/// library.
///
/// Rune injects only explicit argv/environment/stdin values and captured
/// streams. Imports and host-capability builtins are denied, and the compiled
/// top-level bytecode must not contain loops or dynamically-created code. This
/// gives Rune a deterministic, useful Python subset while the broader Python
/// package/stdlib surface remains intentionally unsupported.
#[derive(Debug, Clone, Copy)]
pub struct PythonRunner;

struct PythonCapture {
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    stdout_limited: Arc<AtomicBool>,
    stderr_limited: Arc<AtomicBool>,
    stdin: Arc<Mutex<String>>,
}

impl Runtime for PythonRunner {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Python
    }

    fn execute(&self, request: &RuntimeRequest<'_>) -> Result<RuntimeOutput, RuntimeError> {
        if request.kind() != RuntimeKind::Python {
            return Err(RuntimeError::UnsupportedKind {
                requested: request.kind(),
                provider: self.kind(),
            });
        }
        validate_python_request(request)?;
        if request
            .cancellation
            .is_some_and(|cancellation| cancellation.load(Ordering::Acquire))
        {
            return Ok(RuntimeOutput {
                stdout: String::new(),
                stderr: "python: command cancelled\n".to_string(),
                status: 130,
            });
        }
        let source = std::str::from_utf8(request.source).map_err(|_| {
            RuntimeError::InvalidRequest("Python source must be valid UTF-8 text".to_string())
        })?;
        let capture = PythonCapture {
            stdout: Arc::new(Mutex::new(String::new())),
            stderr: Arc::new(Mutex::new(String::new())),
            stdout_limited: Arc::new(AtomicBool::new(false)),
            stderr_limited: Arc::new(AtomicBool::new(false)),
            stdin: Arc::new(Mutex::new(request.stdin.to_string())),
        };
        let execution_error = run_python_request(request, source, &capture);

        let mut output = RuntimeOutput {
            stdout: read_python_output(&capture.stdout)?,
            stderr: read_python_output(&capture.stderr)?,
            status: 0,
        };
        if capture.stdout_limited.load(Ordering::Acquire)
            || capture.stderr_limited.load(Ordering::Acquire)
        {
            output.status = 1;
            append_python_error(&mut output.stderr, "captured output limit exceeded");
        } else if let Err(message) = execution_error {
            output.status = 1;
            append_python_error(&mut output.stderr, &message);
        }
        if request
            .cancellation
            .is_some_and(|cancellation| cancellation.load(Ordering::Acquire))
        {
            output.status = 130;
            append_python_error(&mut output.stderr, "command cancelled");
        }
        Ok(output)
    }
}

fn run_python_request(
    request: &RuntimeRequest<'_>,
    source: &str,
    capture: &PythonCapture,
) -> Result<(), String> {
    let mut settings = Settings::default();
    settings.argv = std::iter::once(request.program_name.to_string())
        .chain(request.args.iter().cloned())
        .collect();
    settings.isolated = true;
    settings.import_site = false;
    settings.user_site_directory = false;
    settings.ignore_environment = true;
    settings.allow_external_library = false;
    settings.path_list.clear();
    settings.write_bytecode = false;
    settings.install_signal_handlers = false;

    let interpreter = Interpreter::with_init(settings, |vm| {
        vm.import_func = vm
            .new_function("rune_denied_host_capability", deny_python_host_capability)
            .into();
    });
    let execution = interpreter.enter(|vm| {
        vm.recursion_limit.set(PYTHON_MAX_RECURSION_DEPTH);
        let denied = vm.new_function("rune_denied_host_capability", deny_python_host_capability);
        let builtins = vm.builtins.dict();
        for name in [
            "__import__",
            "open",
            "eval",
            "exec",
            "compile",
            "breakpoint",
        ] {
            builtins.set_item(name, denied.clone().into(), vm)?;
        }
        let scope = vm.new_scope_with_builtins();
        install_python_environment(vm, &scope, request, capture)?;
        let code = vm
            .compile(
                source,
                rustpython_vm::compiler::Mode::Exec,
                request.program_name.to_string(),
            )
            .map_err(|error| vm.new_syntax_error(&error, Some(source)))?;
        if let Err(message) = validate_python_code(&code) {
            return Err(vm.new_runtime_error(message));
        }
        vm.run_code_obj(code, scope)
    });

    match execution {
        Ok(_) => Ok(()),
        Err(exception) => {
            let diagnostic = interpreter.enter(|vm| {
                let message = exception.as_object().str(vm).map_or_else(
                    |_| "Python execution failed".to_string(),
                    |value| value.to_string(),
                );
                format!("{}: {message}", exception.class().name())
            });
            Err(diagnostic)
        }
    }
}

fn validate_python_request(request: &RuntimeRequest<'_>) -> Result<(), RuntimeError> {
    if request.source.len() > MAX_PYTHON_SOURCE_BYTES {
        return Err(RuntimeError::InvalidRequest(format!(
            "Python source exceeds {MAX_PYTHON_SOURCE_BYTES} bytes"
        )));
    }
    if request.args.len() > PYTHON_MAX_ARGUMENTS {
        return Err(RuntimeError::InvalidRequest(format!(
            "Python argument list exceeds {PYTHON_MAX_ARGUMENTS} entries"
        )));
    }
    if request
        .args
        .iter()
        .any(|argument| argument.len() > MAX_PYTHON_SOURCE_BYTES)
    {
        return Err(RuntimeError::InvalidRequest(
            "Python argument exceeds the source input bound".to_string(),
        ));
    }
    if request.stdin.len() > PYTHON_MAX_STDIN_BYTES {
        return Err(RuntimeError::InvalidRequest(format!(
            "Python stdin exceeds {PYTHON_MAX_STDIN_BYTES} bytes"
        )));
    }
    let environment_bytes = request
        .environment
        .iter()
        .try_fold(0usize, |total, (key, value)| {
            total
                .checked_add(key.len())
                .and_then(|total| total.checked_add(value.len()))
        });
    if environment_bytes.map_or(true, |bytes| bytes > PYTHON_MAX_ENVIRONMENT_BYTES) {
        return Err(RuntimeError::InvalidRequest(format!(
            "Python environment exceeds {PYTHON_MAX_ENVIRONMENT_BYTES} bytes"
        )));
    }
    Ok(())
}

fn deny_python_host_capability(_args: FuncArgs, vm: &VirtualMachine) -> PythonResult {
    Err(vm.new_exception_msg(
        vm.ctx.exceptions.permission_error.to_owned(),
        "host capability unavailable in Rune Python".to_string(),
    ))
}

fn install_python_environment(
    vm: &VirtualMachine,
    scope: &rustpython_vm::scope::Scope,
    request: &RuntimeRequest<'_>,
    capture: &PythonCapture,
) -> PythonResult<()> {
    let stdout_stream = vm.new_module("rune.stdout", vm.ctx.new_dict(), None);
    let stdout_output = Arc::clone(&capture.stdout);
    let stdout_limit = Arc::clone(&capture.stdout_limited);
    stdout_stream.set_attr(
        "write",
        vm.new_function("write", move |value: PyStrRef, vm: &VirtualMachine| {
            write_python_output(&stdout_output, &stdout_limit, value.as_str(), vm)
        }),
        vm,
    )?;
    stdout_stream.set_attr("flush", vm.new_function("flush", || ()), vm)?;
    stdout_stream.set_attr("fileno", vm.new_function("fileno", || -1_i32), vm)?;

    let stderr_stream = vm.new_module("rune.stderr", vm.ctx.new_dict(), None);
    let stderr_output = Arc::clone(&capture.stderr);
    let stderr_limit = Arc::clone(&capture.stderr_limited);
    stderr_stream.set_attr(
        "write",
        vm.new_function("write", move |value: PyStrRef, vm: &VirtualMachine| {
            write_python_output(&stderr_output, &stderr_limit, value.as_str(), vm)
        }),
        vm,
    )?;
    stderr_stream.set_attr("flush", vm.new_function("flush", || ()), vm)?;
    stderr_stream.set_attr("fileno", vm.new_function("fileno", || -1_i32), vm)?;

    let stdin_stream = vm.new_module("rune.stdin", vm.ctx.new_dict(), None);
    let line_input = Arc::clone(&capture.stdin);
    stdin_stream.set_attr(
        "readline",
        vm.new_function("readline", move |vm: &VirtualMachine| {
            take_python_line(&line_input, vm)
        }),
        vm,
    )?;
    let all_input = Arc::clone(&capture.stdin);
    stdin_stream.set_attr(
        "read",
        vm.new_function("read", move |vm: &VirtualMachine| {
            take_python_all(&all_input, vm)
        }),
        vm,
    )?;
    stdin_stream.set_attr("fileno", vm.new_function("fileno", || -1_i32), vm)?;

    vm.sys_module.set_attr("stdin", stdin_stream.clone(), vm)?;
    vm.sys_module
        .set_attr("stdout", stdout_stream.clone(), vm)?;
    vm.sys_module
        .set_attr("stderr", stderr_stream.clone(), vm)?;
    let argv = vm.ctx.new_list(
        std::iter::once(request.program_name)
            .chain(request.args.iter().map(String::as_str))
            .map(|argument| vm.ctx.new_str(argument).into())
            .collect(),
    );
    vm.sys_module.set_attr("argv", argv.clone(), vm)?;
    vm.sys_module
        .set_attr("path", vm.ctx.new_list(Vec::new()), vm)?;

    let environment = vm.ctx.new_dict();
    for (key, value) in request.environment {
        environment.set_item(key, vm.ctx.new_str(value.as_str()).into(), vm)?;
    }
    let script_args = vm.ctx.new_list(
        request
            .args
            .iter()
            .map(|argument| vm.ctx.new_str(argument.as_str()).into())
            .collect(),
    );
    let rune = vm.new_module("rune", vm.ctx.new_dict(), None);
    rune.set_attr("program", vm.ctx.new_str(request.program_name), vm)?;
    rune.set_attr("args", script_args, vm)?;
    rune.set_attr("env", environment, vm)?;
    rune.set_attr("stdin", vm.ctx.new_str(request.stdin), vm)?;
    let stderr_output = Arc::clone(&capture.stderr);
    let stderr_limit = Arc::clone(&capture.stderr_limited);
    rune.set_attr(
        "stderr",
        vm.new_function("stderr", move |value: PyObjectRef, vm: &VirtualMachine| {
            let value = value.str(vm)?;
            write_python_output(&stderr_output, &stderr_limit, value.as_str(), vm).map(|_| ())
        }),
        vm,
    )?;

    scope
        .globals
        .set_item("__name__", vm.ctx.new_str("__main__").into(), vm)?;
    scope
        .globals
        .set_item("__file__", vm.ctx.new_str(request.program_name).into(), vm)?;
    scope.globals.set_item("rune", rune.into(), vm)?;
    scope
        .globals
        .set_item("sys", vm.sys_module.clone().into(), vm)?;
    Ok(())
}

fn take_python_line(input: &Arc<Mutex<String>>, vm: &VirtualMachine) -> PythonResult<String> {
    let mut input = input
        .lock()
        .map_err(|_| vm.new_runtime_error("Python stdin lock poisoned".to_string()))?;
    if input.is_empty() {
        return Ok(String::new());
    }
    let end = input.find('\n').map_or(input.len(), |index| index + 1);
    Ok(input.drain(..end).collect())
}

fn take_python_all(input: &Arc<Mutex<String>>, vm: &VirtualMachine) -> PythonResult<String> {
    let mut input = input
        .lock()
        .map_err(|_| vm.new_runtime_error("Python stdin lock poisoned".to_string()))?;
    Ok(std::mem::take(&mut *input))
}

fn write_python_output(
    output: &Arc<Mutex<String>>,
    limited: &AtomicBool,
    value: &str,
    vm: &VirtualMachine,
) -> PythonResult<usize> {
    let mut output = output
        .lock()
        .map_err(|_| vm.new_runtime_error("Python output lock poisoned".to_string()))?;
    if output.len().saturating_add(value.len()) > MAX_PYTHON_OUTPUT_BYTES {
        limited.store(true, Ordering::Release);
        return Err(vm.new_runtime_error("captured output limit exceeded".to_string()));
    }
    output.push_str(value);
    Ok(value.len())
}

fn read_python_output(output: &Arc<Mutex<String>>) -> Result<String, RuntimeError> {
    output
        .lock()
        .map(|value| value.clone())
        .map_err(|_| RuntimeError::Execution("Python output lock poisoned".to_string()))
}

fn validate_python_code(code: &PyCode) -> Result<(), String> {
    if code.code.instructions.len() > MAX_PYTHON_INSTRUCTIONS {
        return Err(format!(
            "Python bytecode exceeds {MAX_PYTHON_INSTRUCTIONS} instructions"
        ));
    }
    let mut argument_state = OpArgState::default();
    for (offset, code_unit) in code.code.instructions.iter().copied().enumerate() {
        let offset = u32::try_from(offset)
            .map_err(|_| "Python bytecode offset exceeds the supported range".to_string())?;
        let (instruction, argument) = argument_state.get(code_unit);
        let branch_target = match instruction {
            Instruction::Jump { target }
            | Instruction::JumpIfTrue { target }
            | Instruction::JumpIfFalse { target }
            | Instruction::JumpIfTrueOrPop { target }
            | Instruction::JumpIfFalseOrPop { target }
            | Instruction::Continue { target }
            | Instruction::Break { target } => Some(target),
            _ => None,
        };
        if branch_target.is_some_and(|target| target.get(argument).0 <= offset) {
            return Err(
                "backward Python branches are unavailable in the bounded provider".to_string(),
            );
        }
        match instruction {
            Instruction::ForIter { .. } | Instruction::SetupLoop => {
                return Err("Python loops are unavailable in the bounded provider".to_string());
            }
            Instruction::MakeFunction(_) => {
                return Err(
                    "Python functions, lambdas, generators, and comprehensions are unavailable in the bounded provider"
                        .to_string(),
                );
            }
            Instruction::ImportName { .. }
            | Instruction::ImportNameless
            | Instruction::ImportStar
            | Instruction::ImportFrom { .. } => {
                return Err("Python imports are unavailable in the bounded provider".to_string());
            }
            _ => {}
        }
    }
    Ok(())
}

fn append_python_error(output: &mut String, error: &str) {
    let prefix = "python: ";
    let suffix = "\n";
    let remaining = MAX_PYTHON_OUTPUT_BYTES.saturating_sub(output.len());
    if remaining <= prefix.len() + suffix.len() {
        return;
    }
    let available = remaining - prefix.len() - suffix.len();
    output.push_str(prefix);
    let mut error_bytes = 0;
    for character in error.chars() {
        let character_bytes = character.len_utf8();
        if error_bytes + character_bytes > available {
            break;
        }
        error_bytes += character_bytes;
    }
    output.push_str(&error[..error_bytes]);
    output.push_str(suffix);
}

/// A bounded Lua 5.4 provider for Rune scripts.
///
/// The provider creates a fresh safe Lua state for every request. It exposes
/// only deterministic data bridges (`arg`, `rune.args`, `rune.env`, and
/// `rune.stdin`) plus captured `print`/`rune.stderr` functions. Lua's `io`,
/// `os`, `package`, and `debug` libraries are intentionally not loaded, so the
/// guest cannot acquire host filesystem, process, module, or debugger access.
#[derive(Debug, Clone, Copy)]
pub struct LuaRunner;

impl Runtime for LuaRunner {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Lua
    }

    fn execute(&self, request: &RuntimeRequest<'_>) -> Result<RuntimeOutput, RuntimeError> {
        if request.kind() != RuntimeKind::Lua {
            return Err(RuntimeError::UnsupportedKind {
                requested: request.kind(),
                provider: self.kind(),
            });
        }
        validate_lua_request(request)?;
        if request
            .cancellation
            .is_some_and(|cancellation| cancellation.load(Ordering::Acquire))
        {
            return Ok(RuntimeOutput {
                stdout: String::new(),
                stderr: "lua: command cancelled\n".to_string(),
                status: 130,
            });
        }
        let source = std::str::from_utf8(request.source).map_err(|_| {
            RuntimeError::InvalidRequest("Lua source must be valid UTF-8 text".to_string())
        })?;
        let lua = Lua::new_with(
            StdLib::TABLE | StdLib::STRING | StdLib::UTF8 | StdLib::MATH | StdLib::COROUTINE,
            LuaOptions::default(),
        )
        .map_err(|error| lua_execution_error(&error))?;
        lua.set_memory_limit(MAX_LUA_MEMORY_BYTES)
            .map_err(|error| lua_execution_error(&error))?;

        let instruction_count = Arc::new(AtomicU64::new(0));
        let instruction_count_for_hook = Arc::clone(&instruction_count);
        lua.set_hook(
            HookTriggers::new().every_nth_instruction(LUA_HOOK_INTERVAL),
            move |_lua, _debug| {
                let count = instruction_count_for_hook
                    .fetch_add(u64::from(LUA_HOOK_INTERVAL), Ordering::Relaxed)
                    .saturating_add(u64::from(LUA_HOOK_INTERVAL));
                if count > MAX_LUA_INSTRUCTIONS {
                    Err(LuaError::RuntimeError(
                        "instruction limit exceeded".to_string(),
                    ))
                } else {
                    Ok(())
                }
            },
        );

        let stdout = Arc::new(Mutex::new(String::new()));
        let stderr = Arc::new(Mutex::new(String::new()));
        install_lua_environment(&lua, request, &stdout, &stderr)
            .map_err(|error| lua_execution_error(&error))?;
        let execution = lua
            .load(source)
            .set_name(request.program_name)
            .set_mode(ChunkMode::Text)
            .exec();
        let mut output = RuntimeOutput {
            stdout: read_output(&stdout)?,
            stderr: read_output(&stderr)?,
            status: 0,
        };
        if let Some(cancellation) = request.cancellation {
            if cancellation.load(Ordering::Acquire) {
                output.status = 130;
                output.stderr.push_str("lua: command cancelled\n");
            }
        }
        if let Err(error) = execution {
            output.status = if error.to_string().contains("instruction limit exceeded") {
                124
            } else if matches!(error, LuaError::MemoryError(_)) {
                125
            } else {
                1
            };
            append_error(&mut output.stderr, &error.to_string());
        }
        Ok(output)
    }
}

/// A bounded JavaScript provider backed by a fresh `QuickJS` runtime per request.
///
/// The provider exposes only explicit data and output bridges. It does not
/// enable a module loader or provide host filesystem, process, network, or
/// native-library access. `process` is a small Rune-owned object, not Node.js.
#[derive(Debug, Clone, Copy)]
pub struct JavaScriptRunner;

impl Runtime for JavaScriptRunner {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::JavaScript
    }

    fn execute(&self, request: &RuntimeRequest<'_>) -> Result<RuntimeOutput, RuntimeError> {
        if request.kind() != RuntimeKind::JavaScript {
            return Err(RuntimeError::UnsupportedKind {
                requested: request.kind(),
                provider: self.kind(),
            });
        }
        validate_javascript_request(request)?;
        if request
            .cancellation
            .is_some_and(|cancellation| cancellation.load(Ordering::Acquire))
        {
            return Ok(RuntimeOutput {
                stdout: String::new(),
                stderr: "javascript: command cancelled\n".to_string(),
                status: 130,
            });
        }
        let source = std::str::from_utf8(request.source).map_err(|_| {
            RuntimeError::InvalidRequest("JavaScript source must be valid UTF-8 text".to_string())
        })?;
        let runtime = JsRuntime::new().map_err(|error| javascript_execution_error(&error))?;
        runtime.set_memory_limit(MAX_JAVASCRIPT_MEMORY_BYTES);
        runtime.set_max_stack_size(MAX_JAVASCRIPT_STACK_BYTES);

        let interrupt_count = Arc::new(AtomicU64::new(0));
        let interrupt_limit = Arc::new(AtomicBool::new(false));
        let interrupt_count_for_handler = Arc::clone(&interrupt_count);
        let interrupt_limit_for_handler = Arc::clone(&interrupt_limit);
        runtime.set_interrupt_handler(Some(Box::new(move || {
            let count = interrupt_count_for_handler
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1)
                .saturating_mul(JAVASCRIPT_INTERRUPT_INTERVAL);
            if count >= MAX_JAVASCRIPT_INSTRUCTIONS {
                interrupt_limit_for_handler.store(true, Ordering::Release);
                true
            } else {
                false
            }
        })));

        let stdout = Arc::new(Mutex::new(String::new()));
        let stderr = Arc::new(Mutex::new(String::new()));
        let stdout_limited = Arc::new(AtomicBool::new(false));
        let stderr_limited = Arc::new(AtomicBool::new(false));
        let context =
            Context::full(&runtime).map_err(|error| javascript_execution_error(&error))?;
        let execution = context.with(|ctx| {
            if let Err(error) = install_javascript_environment(
                &ctx,
                request,
                &stdout,
                &stderr,
                &stdout_limited,
                &stderr_limited,
            ) {
                return Err((true, error.to_string()));
            }
            ctx.eval::<(), _>(source)
                .catch(&ctx)
                .map_err(|error| (false, error.to_string()))
        });

        let mut output = RuntimeOutput {
            stdout: read_javascript_output(&stdout)?,
            stderr: read_javascript_output(&stderr)?,
            status: 0,
        };
        if interrupt_limit.load(Ordering::Acquire) {
            output.status = 124;
            append_error_with_prefix(
                &mut output.stderr,
                "javascript: ",
                "interrupt limit exceeded",
            );
        } else if stdout_limited.load(Ordering::Acquire) || stderr_limited.load(Ordering::Acquire) {
            output.status = 1;
            append_error_with_prefix(
                &mut output.stderr,
                "javascript: ",
                "captured output limit exceeded",
            );
        } else if let Err((setup, message)) = execution {
            if setup {
                return Err(RuntimeError::Execution(format!(
                    "JavaScript runtime setup failed: {message}"
                )));
            }
            output.status = 1;
            append_error_with_prefix(&mut output.stderr, "javascript: ", &message);
        }
        if request
            .cancellation
            .is_some_and(|cancellation| cancellation.load(Ordering::Acquire))
        {
            output.status = 130;
            append_error_with_prefix(&mut output.stderr, "javascript: ", "command cancelled");
        }
        Ok(output)
    }
}

fn validate_javascript_request(request: &RuntimeRequest<'_>) -> Result<(), RuntimeError> {
    if request.source.len() > MAX_JAVASCRIPT_SOURCE_BYTES {
        return Err(RuntimeError::InvalidRequest(format!(
            "JavaScript source exceeds {MAX_JAVASCRIPT_SOURCE_BYTES} bytes"
        )));
    }
    if request.args.len() > JAVASCRIPT_MAX_ARGUMENTS {
        return Err(RuntimeError::InvalidRequest(format!(
            "JavaScript argument list exceeds {JAVASCRIPT_MAX_ARGUMENTS} entries"
        )));
    }
    if request
        .args
        .iter()
        .any(|argument| argument.len() > MAX_JAVASCRIPT_SOURCE_BYTES)
    {
        return Err(RuntimeError::InvalidRequest(
            "JavaScript argument exceeds the source input bound".to_string(),
        ));
    }
    if request.stdin.len() > JAVASCRIPT_MAX_STDIN_BYTES {
        return Err(RuntimeError::InvalidRequest(format!(
            "JavaScript stdin exceeds {JAVASCRIPT_MAX_STDIN_BYTES} bytes"
        )));
    }
    let environment_bytes = request
        .environment
        .iter()
        .try_fold(0usize, |total, (key, value)| {
            total
                .checked_add(key.len())
                .and_then(|total| total.checked_add(value.len()))
        });
    if environment_bytes.map_or(true, |bytes| bytes > JAVASCRIPT_MAX_ENVIRONMENT_BYTES) {
        return Err(RuntimeError::InvalidRequest(format!(
            "JavaScript environment exceeds {JAVASCRIPT_MAX_ENVIRONMENT_BYTES} bytes"
        )));
    }
    Ok(())
}

fn install_javascript_environment(
    ctx: &rquickjs::Ctx<'_>,
    request: &RuntimeRequest<'_>,
    stdout: &Arc<Mutex<String>>,
    stderr: &Arc<Mutex<String>>,
    stdout_limited: &Arc<AtomicBool>,
    stderr_limited: &Arc<AtomicBool>,
) -> rquickjs::Result<()> {
    let print_output = Arc::clone(stdout);
    let print_limit = Arc::clone(stdout_limited);
    ctx.globals().set(
        "__rune_print",
        Func::from(move |value: String| {
            append_javascript_output(&print_output, &print_limit, &value, true)
        }),
    )?;
    let write_output = Arc::clone(stdout);
    let write_limit = Arc::clone(stdout_limited);
    ctx.globals().set(
        "__rune_write",
        Func::from(move |value: String| {
            append_javascript_output(&write_output, &write_limit, &value, false)
        }),
    )?;
    let error_output = Arc::clone(stderr);
    let error_limit = Arc::clone(stderr_limited);
    ctx.globals().set(
        "__rune_stderr",
        Func::from(move |value: String| {
            append_javascript_output(&error_output, &error_limit, &value, true)
        }),
    )?;
    let raw_error_output = Arc::clone(stderr);
    let raw_error_limit = Arc::clone(stderr_limited);
    ctx.globals().set(
        "__rune_stderr_raw",
        Func::from(move |value: String| {
            append_javascript_output(&raw_error_output, &raw_error_limit, &value, false)
        }),
    )?;

    let arguments = Array::new(ctx.clone())?;
    arguments.set(0, request.program_name)?;
    for (index, argument) in request.args.iter().enumerate() {
        arguments.set(index + 1, argument.as_str())?;
    }
    let environment = Object::new(ctx.clone())?;
    for (key, value) in request.environment {
        environment.set(key.as_str(), value.as_str())?;
    }
    let globals = ctx.globals();
    globals.set("__rune_args", arguments.clone())?;
    globals.set("__rune_env", environment.clone())?;
    globals.set("__rune_program", request.program_name)?;
    globals.set("__rune_stdin", request.stdin)?;
    globals.set("arg", arguments.clone())?;
    globals.set("process", {
        let process = Object::new(ctx.clone())?;
        process.set("argv", arguments.clone())?;
        process.set("env", environment.clone())?;
        process.set("stdin", request.stdin)?;
        let stdout_object = Object::new(ctx.clone())?;
        stdout_object.set(
            "write",
            globals.get::<_, rquickjs::Function>("__rune_write")?,
        )?;
        process.set("stdout", stdout_object)?;
        let stderr_object = Object::new(ctx.clone())?;
        stderr_object.set(
            "write",
            globals.get::<_, rquickjs::Function>("__rune_stderr_raw")?,
        )?;
        process.set("stderr", stderr_object)?;
        process
    })?;
    let rune = Object::new(ctx.clone())?;
    rune.set("program", request.program_name)?;
    rune.set("args", arguments.clone())?;
    rune.set("env", environment)?;
    rune.set("stdin", request.stdin)?;
    rune.set(
        "stderr",
        globals.get::<_, rquickjs::Function>("__rune_stderr")?,
    )?;
    globals.set("rune", rune)?;
    ctx.eval::<(), _>(JAVASCRIPT_BOOTSTRAP)
}

const JAVASCRIPT_BOOTSTRAP: &str = r#"
globalThis.print = function (...values) {
    __rune_print(values.map((value) => String(value)).join("\t"));
};
globalThis.console = {
    log: function (...values) {
        __rune_print(values.map((value) => String(value)).join("\t"));
    },
    error: function (...values) {
        __rune_stderr(values.map((value) => String(value)).join("\t"));
    }
};
rune.args = __rune_args.slice(1);
"#;

fn append_javascript_output(
    output: &Arc<Mutex<String>>,
    limited: &AtomicBool,
    value: &str,
    newline: bool,
) -> rquickjs::Result<()> {
    let suffix = if newline { "\n" } else { "" };
    let mut output = output.lock().map_err(|_| rquickjs::Error::Exception)?;
    if output
        .len()
        .saturating_add(value.len())
        .saturating_add(suffix.len())
        > MAX_JAVASCRIPT_OUTPUT_BYTES
    {
        limited.store(true, Ordering::Release);
        return Err(rquickjs::Error::Exception);
    }
    output.push_str(value);
    output.push_str(suffix);
    Ok(())
}

fn read_javascript_output(output: &Arc<Mutex<String>>) -> Result<String, RuntimeError> {
    output
        .lock()
        .map(|value| value.clone())
        .map_err(|_| RuntimeError::Execution("JavaScript output lock poisoned".to_string()))
}

fn javascript_execution_error(error: &rquickjs::Error) -> RuntimeError {
    RuntimeError::Execution(format!("JavaScript runtime initialization failed: {error}"))
}

fn validate_lua_request(request: &RuntimeRequest<'_>) -> Result<(), RuntimeError> {
    if request.source.len() > MAX_LUA_SOURCE_BYTES {
        return Err(RuntimeError::InvalidRequest(format!(
            "Lua source exceeds {MAX_LUA_SOURCE_BYTES} bytes"
        )));
    }
    if request.args.len() > 64 {
        return Err(RuntimeError::InvalidRequest(
            "Lua argument list exceeds 64 entries".to_string(),
        ));
    }
    if request
        .args
        .iter()
        .any(|argument| argument.len() > MAX_LUA_SOURCE_BYTES)
    {
        return Err(RuntimeError::InvalidRequest(
            "Lua argument exceeds the source input bound".to_string(),
        ));
    }
    Ok(())
}

fn install_lua_environment(
    lua: &Lua,
    request: &RuntimeRequest<'_>,
    stdout: &Arc<Mutex<String>>,
    stderr: &Arc<Mutex<String>>,
) -> mlua::Result<()> {
    let print_output = Arc::clone(stdout);
    let print = lua.create_function(move |_lua, values: MultiValue| {
        append_values(&print_output, values, true)
    })?;
    let error_output = Arc::clone(stderr);
    let write_stderr = lua.create_function(move |_lua, values: MultiValue| {
        append_values(&error_output, values, true)
    })?;

    let globals = lua.globals();
    globals.set("print", print)?;
    let args = lua.create_table()?;
    args.set(0_i64, request.program_name)?;
    for (index, argument) in request.args.iter().enumerate() {
        let index = i64::try_from(index + 1)
            .map_err(|_| LuaError::RuntimeError("Lua argument index overflow".to_string()))?;
        args.set(index, argument.as_str())?;
    }
    globals.set("arg", args.clone())?;

    let environment = lua.create_table()?;
    for (key, value) in request.environment {
        environment.set(key.as_str(), value.as_str())?;
    }
    let rune = lua.create_table()?;
    rune.set("program", request.program_name)?;
    rune.set("stdin", request.stdin)?;
    rune.set("args", args)?;
    rune.set("env", environment)?;
    rune.set("stderr", write_stderr)?;
    globals.set("rune", rune)?;
    Ok(())
}

fn append_values(
    output: &Arc<Mutex<String>>,
    values: MultiValue,
    newline: bool,
) -> mlua::Result<()> {
    let mut line = String::new();
    for (index, value) in values.into_iter().enumerate() {
        if index > 0 {
            line.push('\t');
        }
        line.push_str(&value.to_string()?);
    }
    if newline {
        line.push('\n');
    }
    let mut output = output
        .lock()
        .map_err(|_| LuaError::RuntimeError("output lock poisoned".to_string()))?;
    if output.len().saturating_add(line.len()) > MAX_LUA_OUTPUT_BYTES {
        return Err(LuaError::RuntimeError(
            "captured output limit exceeded".to_string(),
        ));
    }
    output.push_str(&line);
    Ok(())
}

fn read_output(output: &Arc<Mutex<String>>) -> Result<String, RuntimeError> {
    output
        .lock()
        .map(|value| value.clone())
        .map_err(|_| RuntimeError::Execution("Lua output lock poisoned".to_string()))
}

fn append_error(output: &mut String, error: &str) {
    append_error_with_prefix(output, "lua: ", error);
}

fn append_error_with_prefix(output: &mut String, prefix: &str, error: &str) {
    let remaining = MAX_LUA_OUTPUT_BYTES.saturating_sub(output.len());
    let suffix = "\n";
    if remaining <= prefix.len() + suffix.len() {
        return;
    }
    let available = remaining.saturating_sub(prefix.len() + suffix.len());
    output.push_str(prefix);
    let mut error_bytes = 0;
    for character in error.chars() {
        let character_bytes = character.len_utf8();
        if error_bytes + character_bytes > available {
            break;
        }
        error_bytes += character_bytes;
    }
    output.push_str(&error[..error_bytes]);
    output.push_str(suffix);
}

fn lua_execution_error(error: &mlua::Error) -> RuntimeError {
    RuntimeError::Execution(format!("Lua runtime initialization failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{
        DisabledToolchainProvider, JavaScriptRunner, LuaRunner, PythonRunner, Runtime,
        RuntimeError, RuntimeKind, RuntimeOutput, RuntimePreopen, RuntimeRequest,
        ToolchainArtifact, ToolchainError, ToolchainKind, ToolchainOutput, ToolchainProvider,
        ToolchainRequest, MAX_TOOLCHAIN_SOURCE_BYTES,
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicBool;

    struct EchoRuntime;

    impl Runtime for EchoRuntime {
        fn kind(&self) -> RuntimeKind {
            RuntimeKind::Python
        }

        fn execute(&self, request: &RuntimeRequest<'_>) -> Result<RuntimeOutput, RuntimeError> {
            if request.kind() != self.kind() {
                return Err(RuntimeError::UnsupportedKind {
                    requested: request.kind(),
                    provider: self.kind(),
                });
            }
            Ok(RuntimeOutput {
                stdout: request.program_name.to_string(),
                stderr: String::new(),
                status: 0,
            })
        }
    }

    #[test]
    fn runtime_request_preserves_explicit_inputs() {
        let args = vec!["--check".to_string()];
        let mut environment = BTreeMap::new();
        environment.insert("RUNE_TEST".to_string(), "ok".to_string());
        let request = RuntimeRequest::new(
            RuntimeKind::Python,
            "script.py",
            b"print('ok')",
            &args,
            &environment,
            "stdin",
        );
        assert_eq!(request.kind(), RuntimeKind::Python);
        assert_eq!(request.program_name, "script.py");
        assert_eq!(request.source, b"print('ok')");
        assert_eq!(request.args, ["--check"]);
        assert_eq!(request.environment["RUNE_TEST"], "ok");
        assert_eq!(request.stdin, "stdin");
    }

    #[test]
    fn runtime_kind_names_are_stable() {
        assert_eq!(RuntimeKind::Wasm.name(), "wasm");
        assert_eq!(RuntimeKind::JavaScript.to_string(), "javascript");
        assert_eq!(RuntimeKind::Lua.to_string(), "lua");
    }

    #[test]
    fn toolchain_kind_names_are_stable() {
        assert_eq!(ToolchainKind::C.name(), "c");
        assert_eq!(ToolchainKind::Cpp.to_string(), "c++");
        assert_eq!(ToolchainKind::Tex.to_string(), "tex");
    }

    #[test]
    fn toolchain_request_preserves_explicit_inputs_and_cancellation() {
        let args = vec!["--target=wasm32-wasi".to_string()];
        let mut environment = BTreeMap::new();
        environment.insert("RUNE_TOOLCHAIN".to_string(), "test".to_string());
        let cancellation = AtomicBool::new(false);
        let request = ToolchainRequest::new(
            ToolchainKind::C,
            "hello.c",
            b"int main(void) { return 0; }",
            &args,
            &environment,
            "",
        )
        .with_cancellation(Some(&cancellation));

        request.validate().expect("request should remain bounded");
        assert_eq!(request.kind(), ToolchainKind::C);
        assert_eq!(request.program_name, "hello.c");
        assert_eq!(request.source, b"int main(void) { return 0; }");
        assert_eq!(request.args, ["--target=wasm32-wasi"]);
        assert_eq!(request.environment["RUNE_TOOLCHAIN"], "test");
        assert!(request.cancellation.is_some());
    }

    #[test]
    fn disabled_toolchain_provider_is_explicit_and_kind_checked() {
        let provider = DisabledToolchainProvider::new(ToolchainKind::Tex);
        let environment = BTreeMap::new();
        let request = ToolchainRequest::new(
            ToolchainKind::C,
            "hello.c",
            b"int main(void) { return 0; }",
            &[],
            &environment,
            "",
        );
        assert!(matches!(
            provider.execute(&request),
            Err(ToolchainError::UnsupportedKind {
                requested: ToolchainKind::C,
                provider: ToolchainKind::Tex,
            })
        ));

        let tex_request = ToolchainRequest::new(
            ToolchainKind::Tex,
            "document.tex",
            br"\\documentclass{article}\\begin{document}Rune\\end{document}",
            &[],
            &environment,
            "",
        );
        assert!(matches!(
            provider.execute(&tex_request),
            Err(ToolchainError::Execution(message)) if message.contains("unavailable")
        ));
    }

    #[test]
    fn toolchain_request_rejects_oversized_source_before_provider_execution() {
        let source = vec![b'x'; MAX_TOOLCHAIN_SOURCE_BYTES + 1];
        let environment = BTreeMap::new();
        let request = ToolchainRequest::new(
            ToolchainKind::Cpp,
            "large.cpp",
            &source,
            &[],
            &environment,
            "",
        );
        assert!(matches!(
            request.validate(),
            Err(ToolchainError::InvalidRequest(message)) if message.contains("source exceeds")
        ));
    }

    #[test]
    fn toolchain_output_rejects_unsafe_or_oversized_artifacts() {
        let unsafe_output = ToolchainOutput {
            stdout: String::new(),
            stderr: String::new(),
            status: 1,
            artifacts: vec![ToolchainArtifact {
                path: "../outside.wasm".to_string(),
                media_type: "application/wasm".to_string(),
                bytes: Vec::new(),
            }],
        };
        assert!(matches!(
            unsafe_output.validate(),
            Err(ToolchainError::InvalidRequest(message)) if message.contains("unsafe")
        ));

        let duplicate_output = ToolchainOutput {
            stdout: String::new(),
            stderr: String::new(),
            status: 0,
            artifacts: vec![
                ToolchainArtifact {
                    path: "out.pdf".to_string(),
                    media_type: "application/pdf".to_string(),
                    bytes: Vec::new(),
                },
                ToolchainArtifact {
                    path: "out.pdf".to_string(),
                    media_type: "application/pdf".to_string(),
                    bytes: Vec::new(),
                },
            ],
        };
        assert!(matches!(
            duplicate_output.validate(),
            Err(ToolchainError::InvalidRequest(message)) if message.contains("unsafe or duplicated")
        ));
    }

    #[test]
    fn runtime_request_preserves_named_preopens() {
        let root = std::path::Path::new("/sandbox/Documents");
        let library = std::path::Path::new("/sandbox/Library");
        let preopens = [RuntimePreopen::new(library, "/Library")];
        let environment = BTreeMap::new();
        let request = RuntimeRequest::new(
            RuntimeKind::Wasm,
            "module.wasm",
            b"module",
            &[],
            &environment,
            "",
        )
        .with_preopened_root(Some(root))
        .with_additional_preopens(&preopens);

        assert_eq!(request.preopened_root, Some(root));
        assert_eq!(request.additional_preopens.len(), 1);
        assert_eq!(request.additional_preopens[0].host_path, library);
        assert_eq!(request.additional_preopens[0].guest_path, "/Library");
    }

    #[test]
    fn providers_reject_a_different_runtime_kind() {
        let provider = EchoRuntime;
        let environment = BTreeMap::new();
        let request = RuntimeRequest::new(
            RuntimeKind::Wasm,
            "module.wasm",
            b"module",
            &[],
            &environment,
            "",
        );
        let error = provider
            .execute(&request)
            .expect_err("kind must be checked");
        assert_eq!(
            error,
            RuntimeError::UnsupportedKind {
                requested: RuntimeKind::Wasm,
                provider: RuntimeKind::Python,
            }
        );
    }

    #[test]
    fn python_runner_captures_output_and_explicit_inputs() {
        let runner = PythonRunner;
        let args = vec!["first".to_string()];
        let mut environment = BTreeMap::new();
        environment.insert("RUNE_TEST".to_string(), "ok".to_string());
        let request = RuntimeRequest::new(
            RuntimeKind::Python,
            "script.py",
            br#"print(sys.argv[1]); print(rune.args[0]); print(rune.env["RUNE_TEST"]); print(rune.stdin); print(sys.stdin.read()); rune.stderr("warning")"#,
            &args,
            &environment,
            "input",
        );
        let output = runner
            .execute(&request)
            .expect("Python script should execute");
        assert_eq!(output.stdout, "first\nfirst\nok\ninput\ninput\n");
        assert_eq!(output.stderr, "warning");
        assert_eq!(output.status, 0);
    }

    #[test]
    fn python_runner_denies_host_capabilities_and_unbounded_constructs() {
        let runner = PythonRunner;
        let environment = BTreeMap::new();
        let unsafe_request = RuntimeRequest::new(
            RuntimeKind::Python,
            "unsafe.py",
            b"open('outside', 'w')",
            &[],
            &environment,
            "",
        );
        let unsafe_output = runner
            .execute(&unsafe_request)
            .expect("Python errors should become command output");
        assert_eq!(unsafe_output.status, 1);
        assert!(unsafe_output.stderr.contains("PermissionError"));

        let loop_request = RuntimeRequest::new(
            RuntimeKind::Python,
            "loop.py",
            b"while True: pass",
            &[],
            &environment,
            "",
        );
        let loop_output = runner
            .execute(&loop_request)
            .expect("bounded Python rejection should return output");
        assert_eq!(loop_output.status, 1);
        assert!(loop_output.stderr.contains("loops are unavailable"));

        let import_request = RuntimeRequest::new(
            RuntimeKind::Python,
            "import.py",
            b"import os",
            &[],
            &environment,
            "",
        );
        let import_output = runner
            .execute(&import_request)
            .expect("bounded Python rejection should return output");
        assert_eq!(import_output.status, 1);
        assert!(import_output.stderr.contains("imports are unavailable"));
    }

    #[test]
    fn python_runner_rejects_non_text_or_oversized_source_before_starting() {
        let runner = PythonRunner;
        let environment = BTreeMap::new();
        let binary_request = RuntimeRequest::new(
            RuntimeKind::Python,
            "binary.py",
            &[0xff],
            &[],
            &environment,
            "",
        );
        assert!(matches!(
            runner.execute(&binary_request),
            Err(RuntimeError::InvalidRequest(message)) if message.contains("UTF-8")
        ));
        let oversized_source = vec![b' '; super::MAX_PYTHON_SOURCE_BYTES + 1];
        let oversized_request = RuntimeRequest::new(
            RuntimeKind::Python,
            "large.py",
            &oversized_source,
            &[],
            &environment,
            "",
        );
        assert!(matches!(
            runner.execute(&oversized_request),
            Err(RuntimeError::InvalidRequest(message)) if message.contains("exceeds")
        ));
    }

    #[test]
    fn lua_runner_captures_output_and_explicit_inputs() {
        let runner = LuaRunner;
        let args = vec!["first".to_string()];
        let mut environment = BTreeMap::new();
        environment.insert("RUNE_TEST".to_string(), "ok".to_string());
        let request = RuntimeRequest::new(
            RuntimeKind::Lua,
            "script.lua",
            br#"print(arg[1]); print(rune.stdin); print(rune.env.RUNE_TEST); rune.stderr("warning")"#,
            &args,
            &environment,
            "input",
        );
        let output = runner.execute(&request).expect("Lua script should execute");
        assert_eq!(output.stdout, "first\ninput\nok\n");
        assert_eq!(output.stderr, "warning\n");
        assert_eq!(output.status, 0);
    }

    #[test]
    fn lua_runner_rejects_unsafe_libraries_and_bounds_instructions() {
        let runner = LuaRunner;
        let environment = BTreeMap::new();
        let unsafe_request = RuntimeRequest::new(
            RuntimeKind::Lua,
            "unsafe.lua",
            b"return io.open('outside', 'w')",
            &[],
            &environment,
            "",
        );
        let unsafe_output = runner
            .execute(&unsafe_request)
            .expect("Lua errors should become command output");
        assert_eq!(unsafe_output.status, 1);
        assert!(unsafe_output.stderr.contains("lua:"));

        let looping_request = RuntimeRequest::new(
            RuntimeKind::Lua,
            "loop.lua",
            b"while true do end",
            &[],
            &environment,
            "",
        );
        let looping_output = runner
            .execute(&looping_request)
            .expect("bounded Lua execution should return output");
        assert_eq!(looping_output.status, 124);
        assert!(looping_output.stderr.contains("instruction limit exceeded"));
    }

    #[test]
    fn lua_runner_rejects_non_text_or_oversized_source_before_starting() {
        let runner = LuaRunner;
        let environment = BTreeMap::new();
        let binary_request = RuntimeRequest::new(
            RuntimeKind::Lua,
            "binary.lua",
            &[0xff],
            &[],
            &environment,
            "",
        );
        assert!(matches!(
            runner.execute(&binary_request),
            Err(RuntimeError::InvalidRequest(message)) if message.contains("UTF-8")
        ));
        let oversized_source = vec![b' '; super::MAX_LUA_SOURCE_BYTES + 1];
        let oversized_request = RuntimeRequest::new(
            RuntimeKind::Lua,
            "large.lua",
            &oversized_source,
            &[],
            &environment,
            "",
        );
        assert!(matches!(
            runner.execute(&oversized_request),
            Err(RuntimeError::InvalidRequest(message)) if message.contains("exceeds")
        ));
    }

    #[test]
    fn javascript_runner_captures_output_and_explicit_inputs() {
        let runner = JavaScriptRunner;
        let args = vec!["first".to_string()];
        let mut environment = BTreeMap::new();
        environment.insert("RUNE_TEST".to_string(), "ok".to_string());
        let request = RuntimeRequest::new(
            RuntimeKind::JavaScript,
            "script.js",
            br#"print(process.argv[1]); console.log(process.env.RUNE_TEST); console.log(rune.stdin); process.stderr.write("warning")"#,
            &args,
            &environment,
            "input",
        );
        let output = runner
            .execute(&request)
            .expect("JavaScript script should execute");
        assert_eq!(output.stdout, "first\nok\ninput\n");
        assert_eq!(output.stderr, "warning");
        assert_eq!(output.status, 0);
    }

    #[test]
    fn javascript_runner_has_no_host_modules_and_bounds_execution() {
        let runner = JavaScriptRunner;
        let environment = BTreeMap::new();
        let safe_request = RuntimeRequest::new(
            RuntimeKind::JavaScript,
            "safe.js",
            br#"if (typeof os !== "undefined" || typeof std !== "undefined" || typeof require !== "undefined") { throw new Error("host module exposed"); }"#,
            &[],
            &environment,
            "",
        );
        let safe_output = runner
            .execute(&safe_request)
            .expect("safe JavaScript script should execute");
        assert_eq!(safe_output.status, 0);

        let looping_request = RuntimeRequest::new(
            RuntimeKind::JavaScript,
            "loop.js",
            b"while (true) {}",
            &[],
            &environment,
            "",
        );
        let looping_output = runner
            .execute(&looping_request)
            .expect("bounded JavaScript execution should return output");
        assert_eq!(looping_output.status, 124);
        assert!(looping_output.stderr.contains("interrupt limit exceeded"));
    }

    #[test]
    fn javascript_runner_rejects_binary_or_oversized_inputs() {
        let runner = JavaScriptRunner;
        let environment = BTreeMap::new();
        let binary_request = RuntimeRequest::new(
            RuntimeKind::JavaScript,
            "binary.js",
            &[0xff],
            &[],
            &environment,
            "",
        );
        assert!(matches!(
            runner.execute(&binary_request),
            Err(RuntimeError::InvalidRequest(message)) if message.contains("UTF-8")
        ));

        let oversized_stdin = "x".repeat(super::JAVASCRIPT_MAX_STDIN_BYTES + 1);
        let oversized_stdin_request = RuntimeRequest::new(
            RuntimeKind::JavaScript,
            "stdin.js",
            b"",
            &[],
            &environment,
            &oversized_stdin,
        );
        assert!(matches!(
            runner.execute(&oversized_stdin_request),
            Err(RuntimeError::InvalidRequest(message)) if message.contains("stdin")
        ));
    }
}
