//! Explicit external-application opening capability.
//!
//! The portable shell validates a URL or confined VFS path, then asks a host
//! provider to open it. The core never calls an operating-system launcher and
//! the default provider is disabled.

use std::fmt::{Display, Formatter};

/// Maximum UTF-8 target size accepted by `open` and `openurl`.
pub const MAX_OPEN_TARGET_BYTES: usize = 8 * 1024;

/// Kind of target passed to a host external-open provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum OpenTargetKind {
    /// A URL using one of Rune's explicitly allowed schemes.
    Url = 1,
    /// An existing confined regular file or directory represented by its
    /// approved host path.
    File = 2,
}

/// One validated target sent to an explicit host provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRequest {
    pub kind: OpenTargetKind,
    pub target: String,
}

/// Errors returned by the external-open policy or host provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// No host launcher has been configured for this session.
    Unavailable,
    /// The shell target does not satisfy Rune's URL or size policy.
    InvalidTarget(String),
    /// The host rejected the request without exposing platform details.
    HostFailure,
    /// A confined VFS path could not be represented by the host.
    FileUnavailable,
}

impl Display for OpenError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("external open provider is unavailable"),
            Self::InvalidTarget(message) => write!(formatter, "invalid target: {message}"),
            Self::HostFailure => formatter.write_str("external open provider rejected the target"),
            Self::FileUnavailable => formatter.write_str("the confined file has no host URL"),
        }
    }
}

impl std::error::Error for OpenError {}

/// Host capability used by `open` and `openurl`.
pub trait OpenProvider {
    /// Opens one already-validated URL or confined host file path.
    ///
    /// # Errors
    ///
    /// Returns a policy or host error when no launcher is available or the
    /// host rejects the request.
    fn open(&self, request: &OpenRequest) -> Result<(), OpenError>;
}

/// Default provider used when the host has not granted external launching.
#[derive(Debug, Default)]
pub struct DisabledOpenProvider;

impl OpenProvider for DisabledOpenProvider {
    fn open(&self, _request: &OpenRequest) -> Result<(), OpenError> {
        Err(OpenError::Unavailable)
    }
}

/// Validates a URL accepted by `openurl` or URL-shaped `open` input.
pub fn validate_url_target(target: &str) -> Result<(), OpenError> {
    validate_target_size(target)?;
    if target
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(OpenError::InvalidTarget(
            "URL cannot contain whitespace or control characters".to_string(),
        ));
    }
    let Some((scheme, remainder)) = target.split_once(':') else {
        return Err(OpenError::InvalidTarget(
            "URL must include a scheme".to_string(),
        ));
    };
    if scheme.is_empty()
        || !scheme.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && b"+.-".contains(&byte))
        })
    {
        return Err(OpenError::InvalidTarget(
            "URL has an invalid scheme".to_string(),
        ));
    }
    let scheme = scheme.to_ascii_lowercase();
    match scheme.as_str() {
        "http" | "https" => validate_http_authority(remainder),
        "mailto" | "tel" | "sms" | "shortcuts" => {
            if remainder.is_empty() {
                Err(OpenError::InvalidTarget(
                    "URL target cannot be empty".to_string(),
                ))
            } else {
                Ok(())
            }
        }
        _ => Err(OpenError::InvalidTarget(format!(
            "URL scheme {scheme} is not allowed"
        ))),
    }
}

fn validate_http_authority(remainder: &str) -> Result<(), OpenError> {
    let Some(authority_and_path) = remainder.strip_prefix("//") else {
        return Err(OpenError::InvalidTarget(
            "HTTP URL must use // before the host".to_string(),
        ));
    };
    let authority = authority_and_path
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err(OpenError::InvalidTarget(
            "HTTP URL must contain a host without user information".to_string(),
        ));
    }
    if authority.starts_with('[') {
        if !authority.contains(']') {
            return Err(OpenError::InvalidTarget(
                "HTTP URL has an invalid IPv6 host".to_string(),
            ));
        }
    } else if authority.matches(':').count() > 1 {
        return Err(OpenError::InvalidTarget(
            "HTTP URL has an invalid host".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_target_size(target: &str) -> Result<(), OpenError> {
    if target.is_empty() || target.len() > MAX_OPEN_TARGET_BYTES {
        return Err(OpenError::InvalidTarget(format!(
            "target must contain 1-{MAX_OPEN_TARGET_BYTES} bytes"
        )));
    }
    Ok(())
}
