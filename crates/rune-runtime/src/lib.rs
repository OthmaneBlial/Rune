//! Stable, platform-neutral contracts for Rune language runtimes.
//!
//! This crate owns the runtime request/output boundary and the embedded Lua
//! providers. WASM remains in its dedicated crate, while Python remains an
//! explicit future runtime provider.

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mlua::{ChunkMode, Error as LuaError, HookTriggers, Lua, LuaOptions, MultiValue, StdLib};
use rquickjs::{prelude::Func, Array, CatchResultExt, Context, Object, Runtime as JsRuntime};

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
        JavaScriptRunner, LuaRunner, Runtime, RuntimeError, RuntimeKind, RuntimeOutput,
        RuntimePreopen, RuntimeRequest,
    };
    use std::collections::BTreeMap;

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
