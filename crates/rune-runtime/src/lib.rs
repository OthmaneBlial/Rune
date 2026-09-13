//! Stable, platform-neutral contracts for Rune language runtimes.
//!
//! This crate owns the runtime request/output boundary and the embedded Lua
//! provider. WASM remains in its dedicated crate, while Python and JavaScript
//! remain explicit future runtime providers.

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mlua::{ChunkMode, Error as LuaError, HookTriggers, Lua, LuaOptions, MultiValue, StdLib};

/// Maximum UTF-8 Lua source accepted by the embedded provider.
pub const MAX_LUA_SOURCE_BYTES: usize = 256 * 1024;
/// Maximum captured stdout or stderr returned by one Lua invocation.
pub const MAX_LUA_OUTPUT_BYTES: usize = 1024 * 1024;
/// Maximum allocator budget for one Lua state.
pub const MAX_LUA_MEMORY_BYTES: usize = 32 * 1024 * 1024;
/// Maximum VM instructions for one Lua invocation.
pub const MAX_LUA_INSTRUCTIONS: u64 = 2_000_000;
const LUA_HOOK_INTERVAL: u32 = 1_000;

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
    let remaining = MAX_LUA_OUTPUT_BYTES.saturating_sub(output.len());
    let prefix = "lua: ";
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
        LuaRunner, Runtime, RuntimeError, RuntimeKind, RuntimeOutput, RuntimePreopen,
        RuntimeRequest,
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
}
