//! Explicit text clipboard capability for native hosts.
//!
//! The portable core never reaches for an operating-system clipboard. A host
//! may install a bounded provider (for example, `UIKit`'s `UIPasteboard`) while
//! the CLI and default tests keep the capability disabled.

use std::fmt::{Display, Formatter};

/// Maximum UTF-8 payload accepted by `pbcopy` or returned by `pbpaste`.
pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;

/// Errors returned by the clipboard policy or a configured host provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardError {
    /// No host clipboard capability has been installed.
    Unavailable,
    /// The host payload exceeded Rune's explicit bound.
    TooLarge { actual: usize, maximum: usize },
    /// The host callback rejected the operation without exposing private
    /// platform details to shell output.
    HostFailure,
    /// The host returned bytes that are not valid UTF-8 text.
    InvalidText,
}

impl Display for ClipboardError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("clipboard provider is unavailable"),
            Self::TooLarge { actual, maximum } => write!(
                formatter,
                "clipboard text is {actual} bytes; Rune allows at most {maximum}"
            ),
            Self::HostFailure => formatter.write_str("clipboard provider rejected the operation"),
            Self::InvalidText => formatter.write_str("clipboard text is not valid UTF-8"),
        }
    }
}

impl std::error::Error for ClipboardError {}

/// Host capability used by the `pbcopy` and `pbpaste` built-ins.
pub trait ClipboardProvider {
    /// Reads bounded UTF-8 text from the host clipboard.
    ///
    /// # Errors
    ///
    /// Returns a policy or host error when clipboard access is unavailable,
    /// rejected, oversized, or not valid UTF-8.
    fn read_text(&self) -> Result<String, ClipboardError>;

    /// Replaces the host clipboard with bounded UTF-8 text.
    ///
    /// # Errors
    ///
    /// Returns a policy or host error when the payload is oversized or the
    /// host rejects clipboard access.
    fn write_text(&self, text: &str) -> Result<(), ClipboardError>;
}

/// Default provider used when the host has not explicitly granted clipboard
/// access.
#[derive(Debug, Default)]
pub struct DisabledClipboardProvider;

impl ClipboardProvider for DisabledClipboardProvider {
    fn read_text(&self) -> Result<String, ClipboardError> {
        Err(ClipboardError::Unavailable)
    }

    fn write_text(&self, _text: &str) -> Result<(), ClipboardError> {
        Err(ClipboardError::Unavailable)
    }
}
