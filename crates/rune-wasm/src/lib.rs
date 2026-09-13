//! Bounded WASI preview1 execution for Rune.
//!
//! The runtime exposes process-like channels and, only when the caller passes
//! an approved root, one capability-scoped WASI preopen. No ambient process
//! execution, network, or directory outside that root is inherited.

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rune_runtime::{Runtime, RuntimeError, RuntimeKind, RuntimeOutput, RuntimeRequest};
use wasi_common::pipe::{ReadPipe, WritePipe};
use wasi_common::WasiCtx;
use wasmi::{
    errors::{ErrorKind, FuelError},
    Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc,
    TypedResumableCall,
};
use wasmi_wasi::sync::{add_to_linker, ambient_authority, Dir, WasiCtxBuilder};

/// Limits applied to each WASM invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WasmLimits {
    /// Maximum accepted module bytes before validation starts.
    pub max_module_bytes: usize,
    /// Maximum interpreter fuel for one invocation.
    pub fuel: u64,
    /// Maximum linear memory per module.
    pub memory_bytes: usize,
    /// Maximum elements in one table.
    pub table_elements: u32,
    /// Maximum captured bytes per output channel, including the marker.
    pub max_output_bytes: usize,
}

impl Default for WasmLimits {
    fn default() -> Self {
        Self {
            max_module_bytes: 8 * 1024 * 1024,
            fuel: 10_000_000,
            memory_bytes: 64 * 1024 * 1024,
            table_elements: 10_000,
            max_output_bytes: 1024 * 1024,
        }
    }
}

/// Output and exit status from one WASM invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmExecution {
    pub stdout: String,
    pub stderr: String,
    pub status: i32,
}

/// Errors that prevent a WASM module from being started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasmError {
    ModuleTooLarge { actual: usize, maximum: usize },
    InvalidModule(String),
    WasiSetup(String),
    Linker(String),
    Instantiation(String),
    StartFunction(String),
}

impl Display for WasmError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ModuleTooLarge { actual, maximum } => write!(
                formatter,
                "module is {actual} bytes; Rune allows at most {maximum} bytes"
            ),
            Self::InvalidModule(message) => write!(formatter, "invalid module: {message}"),
            Self::WasiSetup(message) => write!(formatter, "WASI setup failed: {message}"),
            Self::Linker(message) => write!(formatter, "WASI linker setup failed: {message}"),
            Self::Instantiation(message) => {
                write!(formatter, "module instantiation failed: {message}")
            }
            Self::StartFunction(message) => write!(formatter, "module start failed: {message}"),
        }
    }
}

impl std::error::Error for WasmError {}

/// A reusable bounded WASI interpreter configuration.
#[derive(Debug, Clone, Copy, Default)]
pub struct WasmRunner {
    limits: WasmLimits,
}

impl WasmRunner {
    /// Creates a runner with explicit resource and output limits.
    #[must_use]
    pub const fn new(limits: WasmLimits) -> Self {
        Self { limits }
    }

    /// Returns the limits used by future invocations.
    #[must_use]
    pub const fn limits(&self) -> WasmLimits {
        self.limits
    }

    /// Executes a WASI preview1 module without exposing a host directory.
    ///
    /// `argv0` is the virtual command name presented to the guest. The
    /// remaining `args` and the session `environment` are copied into the WASI
    /// context. The synchronous call returns after `_start` exits, traps, or
    /// consumes its fuel budget.
    ///
    /// # Errors
    ///
    /// Returns an error when the module cannot be validated, linked, or
    /// instantiated. Guest runtime traps are returned as a failed execution so
    /// captured output is preserved.
    pub fn execute(
        &self,
        wasm: &[u8],
        argv0: &str,
        args: &[String],
        environment: &BTreeMap<String, String>,
        stdin: &str,
    ) -> Result<WasmExecution, WasmError> {
        self.execute_with_options(
            wasm,
            argv0,
            args,
            environment,
            stdin,
            WasmExecutionOptions::default(),
        )
    }

    /// Executes a WASI preview1 module with an optional capability-scoped root.
    ///
    /// When `preopened_root` is present, the directory is opened once with
    /// capability-based filesystem APIs and exposed to the guest as `/` (WASI
    /// file descriptor 3). The guest can access only that directory tree; a
    /// missing root keeps the no-filesystem behavior of [`Self::execute`].
    ///
    /// # Errors
    ///
    /// Returns an error when the module, WASI boundary, capability root, or
    /// guest execution cannot be started.
    pub fn execute_with_preopened_root(
        &self,
        wasm: &[u8],
        argv0: &str,
        args: &[String],
        environment: &BTreeMap<String, String>,
        stdin: &str,
        preopened_root: Option<&Path>,
    ) -> Result<WasmExecution, WasmError> {
        self.execute_with_options(
            wasm,
            argv0,
            args,
            environment,
            stdin,
            WasmExecutionOptions {
                preopened_root,
                cancellation: None,
            },
        )
    }

    fn execute_with_options(
        &self,
        wasm: &[u8],
        argv0: &str,
        args: &[String],
        environment: &BTreeMap<String, String>,
        stdin: &str,
        options: WasmExecutionOptions<'_>,
    ) -> Result<WasmExecution, WasmError> {
        if wasm.len() > self.limits.max_module_bytes {
            return Err(WasmError::ModuleTooLarge {
                actual: wasm.len(),
                maximum: self.limits.max_module_bytes,
            });
        }

        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, wasm)
            .map_err(|error| WasmError::InvalidModule(error.to_string()))?;

        let stdout_pipe = WritePipe::new(BoundedOutput::new(self.limits.max_output_bytes));
        let stderr_pipe = WritePipe::new(BoundedOutput::new(self.limits.max_output_bytes));
        let wasi_context = build_wasi_context(
            argv0,
            args,
            environment,
            stdin,
            stdout_pipe.clone(),
            stderr_pipe.clone(),
            options.preopened_root,
        )?;

        let limits = StoreLimitsBuilder::new()
            .memory_size(self.limits.memory_bytes)
            .table_elements(self.limits.table_elements)
            .instances(1)
            .memories(1)
            .tables(1)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(
            &engine,
            HostState {
                wasi: wasi_context,
                limits,
            },
        );
        store.limiter(|state| &mut state.limits);
        let mut linker = Linker::new(&engine);
        add_to_linker(&mut linker, |state: &mut HostState| &mut state.wasi)
            .map_err(|error| WasmError::Linker(error.to_string()))?;
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|error| WasmError::Instantiation(error.to_string()))?
            .start(&mut store)
            .map_err(|error| WasmError::StartFunction(error.to_string()))?;
        let start = instance
            .get_typed_func::<(), ()>(&store, "_start")
            .map_err(|error| WasmError::StartFunction(error.to_string()))?;

        let (status, runtime_error) =
            run_start(start, &mut store, self.limits.fuel, options.cancellation)?;

        drop(store);
        let mut stdout = stdout_pipe
            .try_into_inner()
            .map_err(|_| WasmError::WasiSetup("stdout capture remained in use".to_string()))?;
        let mut stderr = stderr_pipe
            .try_into_inner()
            .map_err(|_| WasmError::WasiSetup("stderr capture remained in use".to_string()))?;
        if let Some(message) = runtime_error {
            stderr.write_all(message.as_bytes()).map_err(|error| {
                WasmError::WasiSetup(format!("could not report runtime error: {error}"))
            })?;
        }
        Ok(WasmExecution {
            stdout: output_to_string(&mut stdout),
            stderr: output_to_string(&mut stderr),
            status,
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct WasmExecutionOptions<'a> {
    preopened_root: Option<&'a Path>,
    cancellation: Option<&'a AtomicBool>,
}

fn run_start(
    start: TypedFunc<(), ()>,
    store: &mut Store<HostState>,
    fuel_limit: u64,
    cancellation: Option<&AtomicBool>,
) -> Result<(i32, Option<String>), WasmError> {
    if cancellation.is_some_and(|flag| flag.swap(false, Ordering::AcqRel)) {
        return Ok((130, Some("\nrune: wasm: command cancelled\n".to_string())));
    }
    store
        .set_fuel(fuel_limit)
        .map_err(|error| WasmError::StartFunction(error.to_string()))?;

    let call = start.call_resumable(&mut *store, ());
    match call {
        Ok(TypedResumableCall::Finished(())) => {
            if let Some(cancellation) = cancellation {
                cancellation.store(false, Ordering::Release);
            }
            Ok((0, None))
        }
        Err(error) => {
            if is_out_of_fuel(&error)
                && cancellation.is_some_and(|flag| flag.swap(false, Ordering::AcqRel))
            {
                return Ok((130, Some("\nrune: wasm: command cancelled\n".to_string())));
            }
            Ok((1, Some(format!("\nrune: wasm: {error}\n"))))
        }
        Ok(TypedResumableCall::Resumable(invocation)) => {
            let error = invocation.host_error();
            if let Some(status) = error.i32_exit_status() {
                return Ok((status, None));
            }
            if cancellation.is_some_and(|flag| flag.swap(false, Ordering::AcqRel)) {
                return Ok((130, Some("\nrune: wasm: command cancelled\n".to_string())));
            }
            Ok((1, Some(format!("\nrune: wasm: {error}\n"))))
        }
    }
}

fn is_out_of_fuel(error: &wasmi::Error) -> bool {
    matches!(error.kind(), ErrorKind::Fuel(FuelError::OutOfFuel))
}

fn build_wasi_context(
    argv0: &str,
    args: &[String],
    environment: &BTreeMap<String, String>,
    stdin: &str,
    stdout: WritePipe<BoundedOutput>,
    stderr: WritePipe<BoundedOutput>,
    preopened_root: Option<&Path>,
) -> Result<WasiCtx, WasmError> {
    let mut wasi_builder = WasiCtxBuilder::new();
    wasi_builder = wasi_builder
        .arg(argv0)
        .map_err(|error| WasmError::WasiSetup(error.to_string()))?;
    wasi_builder = wasi_builder
        .args(args)
        .map_err(|error| WasmError::WasiSetup(error.to_string()))?;
    let environment_entries = environment
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<Vec<_>>();
    wasi_builder = wasi_builder
        .envs(&environment_entries)
        .map_err(|error| WasmError::WasiSetup(error.to_string()))?;
    if let Some(root) = preopened_root {
        let directory = Dir::open_ambient_dir(root, ambient_authority())
            .map_err(|error| WasmError::WasiSetup(format!("open preopened root: {error}")))?;
        wasi_builder = wasi_builder
            .preopened_dir(directory, "/")
            .map_err(|error| WasmError::WasiSetup(error.to_string()))?;
    }
    Ok(wasi_builder
        .stdin(Box::new(ReadPipe::from(stdin.to_owned())))
        .stdout(Box::new(stdout))
        .stderr(Box::new(stderr))
        .build())
}

impl Runtime for WasmRunner {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Wasm
    }

    fn execute(&self, request: &RuntimeRequest<'_>) -> Result<RuntimeOutput, RuntimeError> {
        if request.kind() != self.kind() {
            return Err(RuntimeError::UnsupportedKind {
                requested: request.kind(),
                provider: self.kind(),
            });
        }
        let execution = WasmRunner::execute_with_options(
            self,
            request.source,
            request.program_name,
            request.args,
            request.environment,
            request.stdin,
            WasmExecutionOptions {
                preopened_root: request.preopened_root,
                cancellation: request.cancellation,
            },
        )
        .map_err(|error| RuntimeError::Execution(error.to_string()))?;
        Ok(RuntimeOutput {
            stdout: execution.stdout,
            stderr: execution.stderr,
            status: execution.status,
        })
    }
}

struct HostState {
    wasi: WasiCtx,
    limits: StoreLimits,
}

#[derive(Debug)]
struct BoundedOutput {
    bytes: Vec<u8>,
    maximum: usize,
    marker: String,
    truncated: bool,
}

impl BoundedOutput {
    fn new(maximum: usize) -> Self {
        let marker = format!("\n[rune: wasm output truncated at {maximum} bytes]\n");
        Self {
            bytes: Vec::new(),
            maximum: maximum.saturating_sub(marker.len()),
            marker,
            truncated: false,
        }
    }
}

impl Write for BoundedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let remaining = self.maximum.saturating_sub(self.bytes.len());
        let accepted = bytes.len().min(remaining);
        self.bytes.extend_from_slice(&bytes[..accepted]);
        if accepted < bytes.len() {
            self.truncated = true;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn output_to_string(output: &mut BoundedOutput) -> String {
    let mut bytes = std::mem::take(&mut output.bytes);
    if output.truncated {
        bytes.extend_from_slice(output.marker.as_bytes());
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{WasmLimits, WasmRunner};
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn wasi_module_can_write_to_captured_stdout() {
        let wasm = wat::parse_str(
            r#"
                (module
                  (import "wasi_snapshot_preview1" "fd_write"
                    (func $fd_write (param i32 i32 i32 i32) (result i32)))
                  (memory (export "memory") 1)
                  (data (i32.const 8) "hello from wasm\n")
                  (data (i32.const 0) "\08\00\00\00\10\00\00\00")
                  (func (export "_start")
                    (i32.const 1)
                    (i32.const 0)
                    (i32.const 1)
                    (i32.const 24)
                    (call $fd_write)
                    (drop)))
            "#,
        )
        .expect("valid WAT");
        let execution = WasmRunner::default()
            .execute(&wasm, "demo.wasm", &[], &BTreeMap::new(), "")
            .expect("module should execute");
        assert_eq!(execution.stdout, "hello from wasm\n");
        assert_eq!(execution.stderr, "");
        assert_eq!(execution.status, 0);
    }

    #[test]
    fn infinite_module_is_stopped_by_fuel() {
        let wasm =
            wat::parse_str(r#"(module (func (export "_start") (loop br 0)))"#).expect("valid WAT");
        let runner = WasmRunner::new(WasmLimits {
            fuel: 100,
            ..WasmLimits::default()
        });
        let execution = runner
            .execute(&wasm, "loop.wasm", &[], &BTreeMap::new(), "")
            .expect("fuel exhaustion is a guest failure");
        assert_eq!(execution.status, 1);
        assert!(execution.stderr.contains("all fuel consumed"));
    }

    #[test]
    fn cancellable_module_stops_before_wasm_execution() {
        let wasm =
            wat::parse_str(r#"(module (func (export "_start") (loop br 0)))"#).expect("valid WAT");
        let cancellation = AtomicBool::new(true);
        let execution = WasmRunner::default()
            .execute_with_options(
                &wasm,
                "cancel.wasm",
                &[],
                &BTreeMap::new(),
                "",
                super::WasmExecutionOptions {
                    preopened_root: None,
                    cancellation: Some(&cancellation),
                },
            )
            .expect("cancellation should be a guest result");
        assert_eq!(execution.status, 130);
        assert!(execution.stderr.contains("command cancelled"));
        assert!(!cancellation.load(Ordering::Acquire));
    }

    #[test]
    fn wasi_exit_status_is_preserved() {
        let wasm = wat::parse_str(
            r#"
                (module
                  (import "wasi_snapshot_preview1" "proc_exit"
                    (func $proc_exit (param i32)))
                  (memory (export "memory") 1)
                  (func (export "_start")
                    (i32.const 7)
                    (call $proc_exit)))
            "#,
        )
        .expect("valid WAT");
        let execution = WasmRunner::default()
            .execute(&wasm, "exit.wasm", &[], &BTreeMap::new(), "")
            .expect("explicit WASI exit is a guest result");
        assert_eq!(execution.status, 7);
        assert!(execution.stdout.is_empty());
        assert!(execution.stderr.is_empty());
    }

    #[test]
    fn wasi_stdin_is_connected_to_the_invocation_input() {
        let wasm = wat::parse_str(
            r#"
                (module
                  (import "wasi_snapshot_preview1" "fd_read"
                    (func $fd_read (param i32 i32 i32 i32) (result i32)))
                  (import "wasi_snapshot_preview1" "fd_write"
                    (func $fd_write (param i32 i32 i32 i32) (result i32)))
                  (memory (export "memory") 1)
                  (data (i32.const 0) "\08\00\00\00\04\00\00\00")
                  (func (export "_start")
                    (i32.const 0)
                    (i32.const 0)
                    (i32.const 1)
                    (i32.const 24)
                    (call $fd_read)
                    (drop)
                    (i32.const 1)
                    (i32.const 0)
                    (i32.const 1)
                    (i32.const 24)
                    (call $fd_write)
                    (drop)))
            "#,
        )
        .expect("valid WAT");
        let execution = WasmRunner::default()
            .execute(&wasm, "stdin.wasm", &[], &BTreeMap::new(), "ping")
            .expect("module should execute");
        assert_eq!(execution.stdout, "ping");
        assert_eq!(execution.status, 0);
    }

    #[test]
    fn wasi_arguments_and_environment_are_copied_into_the_guest() {
        let wasm = wat::parse_str(
            r#"
                (module
                  (import "wasi_snapshot_preview1" "args_sizes_get"
                    (func $args_sizes_get (param i32 i32) (result i32)))
                  (import "wasi_snapshot_preview1" "args_get"
                    (func $args_get (param i32 i32) (result i32)))
                  (import "wasi_snapshot_preview1" "environ_sizes_get"
                    (func $environ_sizes_get (param i32 i32) (result i32)))
                  (import "wasi_snapshot_preview1" "environ_get"
                    (func $environ_get (param i32 i32) (result i32)))
                  (import "wasi_snapshot_preview1" "fd_write"
                    (func $fd_write (param i32 i32 i32 i32) (result i32)))
                  (memory (export "memory") 1)
                  (data (i32.const 32) "\40\00\00\00\09\00\00\00")
                  (data (i32.const 40) "\40\00\00\00\0c\00\00\00")
                  (func (export "_start")
                    (i32.const 0)
                    (i32.const 4)
                    (call $args_sizes_get)
                    (drop)
                    (i32.const 0)
                    (i32.const 64)
                    (call $args_get)
                    (drop)
                    (i32.const 1)
                    (i32.const 32)
                    (i32.const 1)
                    (i32.const 24)
                    (call $fd_write)
                    (drop)
                    (i32.const 0)
                    (i32.const 4)
                    (call $environ_sizes_get)
                    (drop)
                    (i32.const 0)
                    (i32.const 64)
                    (call $environ_get)
                    (drop)
                    (i32.const 1)
                    (i32.const 40)
                    (i32.const 1)
                    (i32.const 24)
                    (call $fd_write)
                    (drop)))
            "#,
        )
        .expect("valid WAT");
        let mut environment = BTreeMap::new();
        environment.insert("RUNE_TEST".to_string(), "ok".to_string());
        let execution = WasmRunner::default()
            .execute(
                &wasm,
                "args.wasm",
                &["ignored".to_string()],
                &environment,
                "",
            )
            .expect("module should execute");
        assert_eq!(execution.stdout, "args.wasmRUNE_TEST=ok");
        assert_eq!(execution.status, 0);
    }

    #[test]
    fn wasi_guest_has_no_preopened_directory() {
        let wasm = wat::parse_str(
            r#"
                (module
                  (import "wasi_snapshot_preview1" "fd_prestat_get"
                    (func $fd_prestat_get (param i32 i32) (result i32)))
                  (import "wasi_snapshot_preview1" "fd_write"
                    (func $fd_write (param i32 i32 i32 i32) (result i32)))
                  (memory (export "memory") 1)
                  (data (i32.const 8) "no preopen\n")
                  (data (i32.const 0) "\08\00\00\00\0b\00\00\00")
                  (func (export "_start")
                    (i32.const 3)
                    (i32.const 32)
                    (call $fd_prestat_get)
                    (if
                      (then
                        (i32.const 1)
                        (i32.const 0)
                        (i32.const 1)
                        (i32.const 24)
                        (call $fd_write)
                        (drop)))))
            "#,
        )
        .expect("valid WAT");
        let execution = WasmRunner::default()
            .execute(&wasm, "sandbox.wasm", &[], &BTreeMap::new(), "")
            .expect("module should execute");
        assert_eq!(execution.stdout, "no preopen\n");
        assert_eq!(execution.status, 0);
    }

    #[test]
    fn wasi_guest_can_read_only_from_an_explicit_preopened_root() {
        let root = std::env::temp_dir().join(format!(
            "rune-wasm-preopen-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock is after epoch")
                .as_nanos()
        ));
        std::fs::create_dir(&root).expect("preopen root created");
        std::fs::write(root.join("hello.txt"), b"hello from preopen\n")
            .expect("preopen fixture written");
        let wasm = wat::parse_str(
            r#"
                (module
                  (import "wasi_snapshot_preview1" "path_open"
                    (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
                  (import "wasi_snapshot_preview1" "fd_read"
                    (func $fd_read (param i32 i32 i32 i32) (result i32)))
                  (import "wasi_snapshot_preview1" "fd_write"
                    (func $fd_write (param i32 i32 i32 i32) (result i32)))
                  (memory (export "memory") 1)
                  (data (i32.const 0) "\80\00\00\00\13\00\00\00")
                  (data (i32.const 32) "hello.txt")
                  (data (i32.const 44) "\c8\00\00\00\0c\00\00\00")
                  (data (i32.const 200) "open failed\n")
                  (data (i32.const 48) "\dc\00\00\00\0c\00\00\00")
                  (data (i32.const 220) "read failed\n")
                  (func (export "_start")
                    (i32.const 3)
                    (i32.const 0)
                    (i32.const 32)
                    (i32.const 9)
                    (i32.const 0)
                    (i64.const 2)
                    (i64.const 2)
                    (i32.const 0)
                    (i32.const 28)
                    (call $path_open)
                    (i32.eqz)
                    (if
                      (then
                        (i32.load (i32.const 28))
                        (i32.const 0)
                        (i32.const 1)
                        (i32.const 24)
                        (call $fd_read)
                        (i32.eqz)
                        (if
                          (then
                            (i32.const 1)
                            (i32.const 0)
                            (i32.const 1)
                            (i32.const 24)
                            (call $fd_write)
                            (drop))
                          (else
                            (i32.const 1)
                            (i32.const 48)
                            (i32.const 1)
                            (i32.const 24)
                            (call $fd_write)
                            (drop))))
                      (else
                        (i32.const 1)
                            (i32.const 44)
                        (i32.const 1)
                        (i32.const 24)
                        (call $fd_write)
                        (drop)))))
            "#,
        )
        .expect("valid WAT");
        let execution = WasmRunner::default()
            .execute_with_preopened_root(&wasm, "read.wasm", &[], &BTreeMap::new(), "", Some(&root))
            .expect("module should execute");
        assert_eq!(execution.stdout, "hello from preopen\n", "{execution:?}");
        assert_eq!(execution.status, 0);
        std::fs::remove_dir_all(root).expect("preopen root removed");
    }

    #[test]
    fn captured_wasm_output_is_bounded_before_returning_to_the_shell() {
        let wasm = wat::parse_str(
            r#"
                (module
                  (import "wasi_snapshot_preview1" "fd_write"
                    (func $fd_write (param i32 i32 i32 i32) (result i32)))
                  (memory (export "memory") 1)
                  (data (i32.const 8) "x\n")
                  (data (i32.const 0) "\08\00\00\00\02\00\00\00")
                  (func (export "_start")
                    (loop
                      (i32.const 1)
                      (i32.const 0)
                      (i32.const 1)
                      (i32.const 24)
                      (call $fd_write)
                      (drop)
                      (br 0))))
            "#,
        )
        .expect("valid WAT");
        let runner = WasmRunner::new(WasmLimits {
            fuel: 1_000,
            max_output_bytes: 128,
            ..WasmLimits::default()
        });
        let execution = runner
            .execute(&wasm, "output.wasm", &[], &BTreeMap::new(), "")
            .expect("fuel exhaustion is a guest failure");
        assert!(execution.stdout.len() <= 128);
        assert!(execution
            .stdout
            .ends_with("[rune: wasm output truncated at 128 bytes]\n"));
        assert_eq!(execution.status, 1);
    }

    #[test]
    fn oversized_module_is_rejected_before_validation() {
        let runner = WasmRunner::new(WasmLimits {
            max_module_bytes: 2,
            ..WasmLimits::default()
        });
        let error = runner
            .execute(&[0, 1, 2], "large.wasm", &[], &BTreeMap::new(), "")
            .expect_err("module size must be bounded");
        assert!(error.to_string().contains("at most 2 bytes"));
    }
}
