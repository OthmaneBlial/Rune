//! Stable, platform-neutral contracts for Rune language runtimes.
//!
//! This crate deliberately contains no interpreter. It gives the command
//! engine one request/output boundary so WASM can be integrated today while
//! Python, JavaScript, and Lua remain explicit future runtime providers.

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::sync::atomic::AtomicBool;

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

#[cfg(test)]
mod tests {
    use super::{
        Runtime, RuntimeError, RuntimeKind, RuntimeOutput, RuntimePreopen, RuntimeRequest,
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
}
