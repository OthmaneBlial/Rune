//! Bounded WASI preview1 execution for Rune.
//!
//! The first runtime slice intentionally exposes only process-like channels:
//! arguments, environment variables, stdin, stdout, and stderr. It does not
//! preopen a host directory, so a module cannot use WASI filesystem calls to
//! bypass Rune's virtual filesystem boundary.

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::io::{self, Write};

use wasi_common::pipe::{ReadPipe, WritePipe};
use wasi_common::WasiCtx;
use wasmi::{
    Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, TypedResumableCall,
};
use wasmi_wasi::sync::{add_to_linker, WasiCtxBuilder};

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

        let stdin_pipe = ReadPipe::from(stdin.to_owned());
        let stdout_pipe = WritePipe::new(BoundedOutput::new(self.limits.max_output_bytes));
        let stderr_pipe = WritePipe::new(BoundedOutput::new(self.limits.max_output_bytes));
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
        let wasi_context = wasi_builder
            .stdin(Box::new(stdin_pipe))
            .stdout(Box::new(stdout_pipe.clone()))
            .stderr(Box::new(stderr_pipe.clone()))
            .build();

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
        store
            .set_fuel(self.limits.fuel)
            .map_err(|error| WasmError::StartFunction(error.to_string()))?;

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

        let (status, runtime_error) = match start.call_resumable(&mut store, ()) {
            Ok(TypedResumableCall::Finished(())) => (0, None),
            Ok(TypedResumableCall::Resumable(invocation)) => {
                let error = invocation.host_error();
                let status = error.i32_exit_status().unwrap_or(1);
                let runtime_error = error
                    .i32_exit_status()
                    .is_none()
                    .then(|| format!("\nrune: wasm: {error}\n"));
                (status, runtime_error)
            }
            Err(error) => {
                let runtime_error = format!("\nrune: wasm: {error}\n");
                (1, Some(runtime_error))
            }
        };

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
