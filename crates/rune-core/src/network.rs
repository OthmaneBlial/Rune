//! Explicit network capability for commands that need HTTP transport.
//!
//! The core owns request parsing and bounds, while a host supplies the actual
//! transport. This keeps iOS networking on a native `URLSession` boundary and
//! prevents commands or WASM guests from inheriting ambient sockets.

use std::fmt::{Display, Formatter};

/// Maximum URL length accepted by a Rune HTTP request.
pub const MAX_NETWORK_URL_BYTES: usize = 2 * 1024;
/// Maximum number of request headers accepted by one command.
pub const MAX_NETWORK_HEADERS: usize = 32;
/// Maximum combined UTF-8 bytes used by request headers.
pub const MAX_NETWORK_HEADER_BYTES: usize = 16 * 1024;
/// Maximum request or response body size exposed by the network boundary.
pub const MAX_NETWORK_BODY_BYTES: usize = 8 * 1024 * 1024;

/// HTTP methods exposed by the bounded `curl` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMethod {
    Get,
    Head,
    Post,
    Put,
    Delete,
}

impl NetworkMethod {
    /// Returns the wire spelling used by native providers.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }

    /// Parses one method accepted by Rune.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is not one of Rune's bounded HTTP
    /// methods.
    pub fn parse(value: &str) -> Result<Self, NetworkError> {
        match value.to_ascii_uppercase().as_str() {
            "GET" => Ok(Self::Get),
            "HEAD" => Ok(Self::Head),
            "POST" => Ok(Self::Post),
            "PUT" => Ok(Self::Put),
            "DELETE" => Ok(Self::Delete),
            _ => Err(NetworkError::InvalidRequest(format!(
                "unsupported HTTP method: {value}"
            ))),
        }
    }
}

/// One bounded request sent through an explicitly configured host provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkRequest {
    pub method: NetworkMethod,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl NetworkRequest {
    /// Validates URL, header, and body policy before transport begins.
    ///
    /// # Errors
    ///
    /// Returns an error when the URL, headers, or body exceed Rune's explicit
    /// network boundary.
    pub fn validate(&self) -> Result<(), NetworkError> {
        validate_url(&self.url)?;
        if self.headers.len() > MAX_NETWORK_HEADERS {
            return Err(NetworkError::InvalidRequest(format!(
                "request contains more than {MAX_NETWORK_HEADERS} headers"
            )));
        }
        let mut header_bytes = 0_usize;
        for (name, value) in &self.headers {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            {
                return Err(NetworkError::InvalidRequest(
                    "header name is not a valid HTTP token".to_string(),
                ));
            }
            if value.chars().any(char::is_control) {
                return Err(NetworkError::InvalidRequest(
                    "header value contains a control character".to_string(),
                ));
            }
            header_bytes = header_bytes
                .saturating_add(name.len())
                .saturating_add(value.len());
        }
        if header_bytes > MAX_NETWORK_HEADER_BYTES {
            return Err(NetworkError::InvalidRequest(format!(
                "request headers exceed {MAX_NETWORK_HEADER_BYTES} bytes"
            )));
        }
        if self.body.len() > MAX_NETWORK_BODY_BYTES {
            return Err(NetworkError::BodyTooLarge {
                actual: self.body.len(),
                maximum: MAX_NETWORK_BODY_BYTES,
            });
        }
        Ok(())
    }
}

/// Response returned by a host network provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkResponse {
    pub status_code: u16,
    pub body: Vec<u8>,
}

impl NetworkResponse {
    /// Validates the response limit before it reaches command output or VFS.
    ///
    /// # Errors
    ///
    /// Returns an error when the status code or response body is outside the
    /// bounded provider contract.
    pub fn validate(&self) -> Result<(), NetworkError> {
        if !(100..=599).contains(&self.status_code) {
            return Err(NetworkError::InvalidResponse(format!(
                "invalid HTTP status code: {}",
                self.status_code
            )));
        }
        if self.body.len() > MAX_NETWORK_BODY_BYTES {
            return Err(NetworkError::BodyTooLarge {
                actual: self.body.len(),
                maximum: MAX_NETWORK_BODY_BYTES,
            });
        }
        Ok(())
    }
}

/// Errors returned by network policy or a configured host provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkError {
    /// No native transport has been configured for this session.
    Unavailable,
    /// The core rejected the request before transport.
    InvalidRequest(String),
    /// The provider returned a response that violates the boundary.
    InvalidResponse(String),
    /// The request or response exceeded the configured body limit.
    BodyTooLarge { actual: usize, maximum: usize },
    /// The host transport failed without exposing private provider details.
    Transport(String),
}

impl Display for NetworkError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => formatter.write_str("network provider is unavailable"),
            Self::InvalidRequest(message) => {
                write!(formatter, "invalid network request: {message}")
            }
            Self::InvalidResponse(message) => {
                write!(formatter, "invalid network response: {message}")
            }
            Self::BodyTooLarge { actual, maximum } => write!(
                formatter,
                "network body is {actual} bytes; Rune allows at most {maximum}"
            ),
            Self::Transport(message) => write!(formatter, "network request failed: {message}"),
        }
    }
}

impl std::error::Error for NetworkError {}

/// Host capability used by `curl`; the core never opens sockets itself.
pub trait NetworkProvider {
    /// Performs one already-bounded request.
    ///
    /// # Errors
    ///
    /// Returns a provider or transport error when the host cannot complete
    /// the request.
    fn request(&self, request: &NetworkRequest) -> Result<NetworkResponse, NetworkError>;
}

/// Default provider used by CLI and tests that have no host network grant.
#[derive(Debug, Default)]
pub struct DisabledNetworkProvider;

impl NetworkProvider for DisabledNetworkProvider {
    fn request(&self, _request: &NetworkRequest) -> Result<NetworkResponse, NetworkError> {
        Err(NetworkError::Unavailable)
    }
}

fn validate_url(url: &str) -> Result<(), NetworkError> {
    if url.is_empty() || url.len() > MAX_NETWORK_URL_BYTES {
        return Err(NetworkError::InvalidRequest(format!(
            "URL must contain 1-{MAX_NETWORK_URL_BYTES} bytes"
        )));
    }
    if url
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(NetworkError::InvalidRequest(
            "URL cannot contain whitespace or control characters".to_string(),
        ));
    }
    let scheme_end = url.find("://").ok_or_else(|| {
        NetworkError::InvalidRequest("URL must use http:// or https://".to_string())
    })?;
    let scheme = &url[..scheme_end];
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(NetworkError::InvalidRequest(
            "only http:// and https:// URLs are allowed".to_string(),
        ));
    }
    let authority = &url[scheme_end + 3..]
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.is_empty() {
        return Err(NetworkError::InvalidRequest(
            "URL must contain a host".to_string(),
        ));
    }
    if authority.contains('@') {
        return Err(NetworkError::InvalidRequest(
            "URL userinfo is not accepted; credentials must not cross the shell boundary"
                .to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{NetworkError, NetworkMethod, NetworkRequest, NetworkResponse};

    fn request(url: &str) -> NetworkRequest {
        NetworkRequest {
            method: NetworkMethod::Get,
            url: url.to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    #[test]
    fn accepts_bounded_http_requests_and_responses() {
        assert!(request("https://example.test/path").validate().is_ok());
        assert!(NetworkResponse {
            status_code: 204,
            body: Vec::new(),
        }
        .validate()
        .is_ok());
        assert_eq!(
            NetworkMethod::parse("post").expect("method"),
            NetworkMethod::Post
        );
    }

    #[test]
    fn rejects_unsafe_urls_and_headers() {
        assert!(matches!(
            request("file:///private/data").validate(),
            Err(NetworkError::InvalidRequest(_))
        ));
        assert!(matches!(
            request("https://user:secret@example.test").validate(),
            Err(NetworkError::InvalidRequest(_))
        ));
        let mut invalid = request("https://example.test");
        invalid
            .headers
            .push(("Bad Header".to_string(), "ok".to_string()));
        assert!(matches!(
            invalid.validate(),
            Err(NetworkError::InvalidRequest(_))
        ));
    }
}
