//! Versioned package metadata and artifact integrity checks for Rune.
//!
//! This crate deliberately stops at a validated manifest and byte-level
//! verification. Network transport, installation, removal, and trust policy
//! belong to the package manager layer and are not part of this metadata API.

use std::collections::BTreeSet;
use std::fmt::{Display, Formatter, Write as _};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CURRENT_SCHEMA_VERSION: u16 = 1;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_FILES: usize = 10_000;
const MAX_COMMANDS: usize = 1_000;
const MAX_NAME_CHARS: usize = 64;
const MAX_VERSION_CHARS: usize = 64;
const MAX_DESCRIPTION_CHARS: usize = 256;

/// A package manifest supported by Rune's initial metadata boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    pub schema_version: u16,
    pub name: String,
    pub version: String,
    pub description: String,
    pub files: Vec<PackageFile>,
    #[serde(default)]
    pub commands: Vec<PackageCommand>,
}

/// One package file and its expected SHA-256 digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageFile {
    pub path: String,
    pub sha256: String,
}

/// A command entry exposed by a package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageCommand {
    pub name: String,
    pub entry: String,
}

/// Errors returned while parsing or verifying package metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageError {
    ManifestTooLarge {
        actual: usize,
        maximum: usize,
    },
    InvalidJson(String),
    UnsupportedSchema(u16),
    InvalidField {
        field: String,
        reason: String,
    },
    DuplicatePath(String),
    DuplicateCommand(String),
    UndeclaredFile(String),
    IntegrityMismatch {
        path: String,
        expected: String,
        actual: String,
    },
}

impl Display for PackageError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ManifestTooLarge { actual, maximum } => write!(
                formatter,
                "manifest is {actual} bytes; Rune allows at most {maximum} bytes"
            ),
            Self::InvalidJson(message) => write!(formatter, "invalid manifest JSON: {message}"),
            Self::UnsupportedSchema(version) => {
                write!(formatter, "unsupported manifest schema version: {version}")
            }
            Self::InvalidField { field, reason } => write!(formatter, "invalid {field}: {reason}"),
            Self::DuplicatePath(path) => write!(formatter, "duplicate package path: {path}"),
            Self::DuplicateCommand(name) => {
                write!(formatter, "duplicate package command: {name}")
            }
            Self::UndeclaredFile(path) => write!(formatter, "file is not declared: {path}"),
            Self::IntegrityMismatch {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "integrity mismatch for {path}: expected {expected}, got {actual}"
            ),
        }
    }
}

impl std::error::Error for PackageError {}

impl PackageManifest {
    /// Parses and validates a bounded JSON manifest.
    ///
    /// Unknown fields are rejected so a future schema cannot silently change
    /// the meaning of a v1 manifest.
    ///
    /// # Errors
    ///
    /// Returns a structured error for oversized, malformed, unsupported, or
    /// semantically invalid metadata.
    pub fn parse(bytes: &[u8]) -> Result<Self, PackageError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(PackageError::ManifestTooLarge {
                actual: bytes.len(),
                maximum: MAX_MANIFEST_BYTES,
            });
        }
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|error| PackageError::InvalidJson(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validates a manifest constructed in Rust rather than parsed from JSON.
    ///
    /// # Errors
    ///
    /// Returns a structured error when a schema, name, path, command, or
    /// digest invariant is violated.
    pub fn validate(&self) -> Result<(), PackageError> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(PackageError::UnsupportedSchema(self.schema_version));
        }
        validate_package_name(&self.name)?;
        validate_version(&self.version)?;
        if self.description.chars().count() > MAX_DESCRIPTION_CHARS {
            return Err(invalid_field(
                "description",
                format!("must be at most {MAX_DESCRIPTION_CHARS} characters"),
            ));
        }
        if self.files.is_empty() {
            return Err(invalid_field("files", "must contain at least one file"));
        }
        if self.files.len() > MAX_FILES {
            return Err(invalid_field(
                "files",
                format!("must contain at most {MAX_FILES} entries"),
            ));
        }
        if self.commands.len() > MAX_COMMANDS {
            return Err(invalid_field(
                "commands",
                format!("must contain at most {MAX_COMMANDS} entries"),
            ));
        }

        let mut paths = BTreeSet::new();
        for file in &self.files {
            validate_relative_path(&file.path)?;
            validate_sha256(&file.sha256)?;
            if !paths.insert(&file.path) {
                return Err(PackageError::DuplicatePath(file.path.clone()));
            }
        }

        let mut commands = BTreeSet::new();
        for command in &self.commands {
            validate_command_name(&command.name)?;
            validate_relative_path(&command.entry)?;
            if !paths.contains(&command.entry) {
                return Err(PackageError::UndeclaredFile(command.entry.clone()));
            }
            if !commands.insert(&command.name) {
                return Err(PackageError::DuplicateCommand(command.name.clone()));
            }
        }
        Ok(())
    }

    /// Returns the declared file entry for a path, if present.
    #[must_use]
    pub fn file(&self, path: &str) -> Option<&PackageFile> {
        self.files.iter().find(|file| file.path == path)
    }

    /// Verifies bytes against the digest declared by the manifest.
    ///
    /// # Errors
    ///
    /// Returns an undeclared-file error or a digest mismatch.
    pub fn verify_file(&self, path: &str, bytes: &[u8]) -> Result<(), PackageError> {
        let Some(file) = self.file(path) else {
            return Err(PackageError::UndeclaredFile(path.to_string()));
        };
        let actual = sha256_hex(bytes);
        if !actual.eq_ignore_ascii_case(&file.sha256) {
            return Err(PackageError::IntegrityMismatch {
                path: path.to_string(),
                expected: file.sha256.clone(),
                actual,
            });
        }
        Ok(())
    }
}

/// Computes a lowercase SHA-256 digest suitable for a manifest entry.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn validate_package_name(name: &str) -> Result<(), PackageError> {
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(invalid_field(
            "name",
            format!("must contain 1-{MAX_NAME_CHARS} characters"),
        ));
    }
    if !name.chars().all(|character| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || matches!(character, '-' | '_' | '.')
    }) || !name.as_bytes()[0].is_ascii_lowercase() && !name.as_bytes()[0].is_ascii_digit()
    {
        return Err(invalid_field(
            "name",
            "must start with lowercase ASCII and contain only lowercase ASCII, digits, '-', '_', or '.'",
        ));
    }
    Ok(())
}

fn validate_command_name(name: &str) -> Result<(), PackageError> {
    if name.is_empty()
        || !name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return Err(invalid_field(
            "commands.name",
            "must contain only ASCII letters, digits, '-', '_', or '.'",
        ));
    }
    Ok(())
}

fn validate_version(version: &str) -> Result<(), PackageError> {
    if version.is_empty()
        || version.chars().count() > MAX_VERSION_CHARS
        || version.chars().any(char::is_whitespace)
        || !version.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '+')
        })
    {
        return Err(invalid_field(
            "version",
            format!("must be non-empty, path-safe, and at most {MAX_VERSION_CHARS} characters"),
        ));
    }
    Ok(())
}

fn validate_relative_path(path: &str) -> Result<(), PackageError> {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        return Err(invalid_field(
            "files.path",
            "must be a non-empty relative path using '/'",
        ));
    }
    if path
        .split('/')
        .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(invalid_field(
            "files.path",
            "must not contain empty, '.', or '..' path components",
        ));
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), PackageError> {
    if value.len() != 64 || !value.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err(invalid_field(
            "files.sha256",
            "must be exactly 64 hexadecimal characters",
        ));
    }
    Ok(())
}

fn invalid_field(field: &str, reason: impl Into<String>) -> PackageError {
    PackageError::InvalidField {
        field: field.to_string(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::{sha256_hex, PackageError, PackageManifest};

    const HELLO_DIGEST: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    fn manifest_json(digest: &str) -> String {
        format!(
            r#"{{
                "schema_version": 1,
                "name": "hello-rune",
                "version": "0.1.0",
                "description": "A small Rune package",
                "files": [{{"path": "bin/hello.wasm", "sha256": "{digest}"}}],
                "commands": [{{"name": "hello", "entry": "bin/hello.wasm"}}]
            }}"#
        )
    }

    #[test]
    fn parses_versioned_manifest_and_verifies_file_bytes() {
        let manifest = PackageManifest::parse(manifest_json(HELLO_DIGEST).as_bytes())
            .expect("manifest should validate");
        assert_eq!(manifest.name, "hello-rune");
        assert_eq!(manifest.commands[0].entry, "bin/hello.wasm");
        assert_eq!(sha256_hex(b"hello"), HELLO_DIGEST);
        manifest
            .verify_file("bin/hello.wasm", b"hello")
            .expect("digest should match");
        let uppercase = manifest_json(&HELLO_DIGEST.to_ascii_uppercase());
        let uppercase_manifest = PackageManifest::parse(uppercase.as_bytes())
            .expect("uppercase hexadecimal should remain valid");
        uppercase_manifest
            .verify_file("bin/hello.wasm", b"hello")
            .expect("digest comparison should be case-insensitive");
    }

    #[test]
    fn rejects_path_escape_duplicate_and_undeclared_entry() {
        let escaped = manifest_json(HELLO_DIGEST).replace("bin/hello.wasm", "../hello.wasm");
        assert!(matches!(
            PackageManifest::parse(escaped.as_bytes()),
            Err(PackageError::InvalidField { .. })
        ));

        let duplicate = format!(
            r#"{{
                "schema_version": 1,
                "name": "hello-rune",
                "version": "0.1.0",
                "description": "A small Rune package",
                "files": [
                    {{"path": "bin/hello.wasm", "sha256": "{HELLO_DIGEST}"}},
                    {{"path": "bin/hello.wasm", "sha256": "{HELLO_DIGEST}"}}
                ]
            }}"#
        );
        assert!(matches!(
            PackageManifest::parse(duplicate.as_bytes()),
            Err(PackageError::DuplicatePath(_))
        ));

        let undeclared = manifest_json(HELLO_DIGEST).replace(
            "\"entry\": \"bin/hello.wasm\"",
            "\"entry\": \"bin/other.wasm\"",
        );
        assert!(matches!(
            PackageManifest::parse(undeclared.as_bytes()),
            Err(PackageError::UndeclaredFile(_))
        ));
    }

    #[test]
    fn rejects_integrity_mismatch_and_unknown_fields() {
        let manifest = PackageManifest::parse(manifest_json(HELLO_DIGEST).as_bytes())
            .expect("manifest should validate");
        assert!(matches!(
            manifest.verify_file("bin/hello.wasm", b"tampered"),
            Err(PackageError::IntegrityMismatch { .. })
        ));
        let unknown = manifest_json(HELLO_DIGEST).replace(
            "\"schema_version\": 1,",
            "\"schema_version\": 1, \"future_flag\": true,",
        );
        assert!(matches!(
            PackageManifest::parse(unknown.as_bytes()),
            Err(PackageError::InvalidJson(_))
        ));
    }

    #[test]
    fn rejects_oversized_manifest_and_unsupported_schema() {
        let oversized = vec![b'x'; 64 * 1024 + 1];
        assert!(matches!(
            PackageManifest::parse(&oversized),
            Err(PackageError::ManifestTooLarge { .. })
        ));
        let unsupported =
            manifest_json(HELLO_DIGEST).replace("\"schema_version\": 1", "\"schema_version\": 2");
        assert!(matches!(
            PackageManifest::parse(unsupported.as_bytes()),
            Err(PackageError::UnsupportedSchema(2))
        ));
        let unsafe_version = manifest_json(HELLO_DIGEST)
            .replace("\"version\": \"0.1.0\"", "\"version\": \"0.1/unsafe\"");
        assert!(matches!(
            PackageManifest::parse(unsafe_version.as_bytes()),
            Err(PackageError::InvalidField { field, .. }) if field == "version"
        ));
    }
}
