//! Versioned package metadata and artifact integrity checks for Rune.
//!
//! This crate deliberately stops at validated metadata and byte-level
//! verification. Network transport, installation, removal, and publisher trust
//! policy belong to the package manager layer and are not part of this API.

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
const CURRENT_REGISTRY_SCHEMA_VERSION: u16 = 1;
const MAX_REGISTRY_BYTES: usize = 64 * 1024;
const MAX_REGISTRY_PACKAGES: usize = 4_096;
const MAX_REGISTRY_ARTIFACTS: usize = 10_000;
const MAX_REGISTRY_URL_BYTES: usize = 2 * 1024;

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
    /// Capabilities explicitly requested by commands in this package.
    #[serde(default)]
    pub permissions: PackagePermissions,
}

/// Least-privilege capabilities available to a package runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PackagePermissions {
    /// Allows installed WASM commands to receive the Rune sandbox as `/`.
    #[serde(default)]
    pub filesystem: bool,
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

/// A bounded, HTTPS-only package registry index.
///
/// The index is discovery metadata, not a trust anchor. The package manager
/// still validates the downloaded manifest and verifies every artifact against
/// the manifest's SHA-256 digest before installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRegistryIndex {
    pub schema_version: u16,
    pub packages: Vec<RegistryPackage>,
}

/// One versioned package advertised by a registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryPackage {
    pub name: String,
    pub version: String,
    pub description: String,
    pub manifest_url: String,
    pub artifacts: Vec<RegistryArtifact>,
}

/// A remote URL for one manifest-declared package file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryArtifact {
    pub path: String,
    pub url: String,
}

/// Errors returned while parsing or verifying package metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageError {
    ManifestTooLarge {
        actual: usize,
        maximum: usize,
    },
    RegistryTooLarge {
        actual: usize,
        maximum: usize,
    },
    InvalidJson(String),
    UnsupportedSchema(u16),
    UnsupportedRegistrySchema(u16),
    InvalidField {
        field: String,
        reason: String,
    },
    DuplicatePath(String),
    DuplicateCommand(String),
    DuplicateRegistryPackage(String),
    DuplicateRegistryArtifact(String),
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
            Self::RegistryTooLarge { actual, maximum } => write!(
                formatter,
                "registry index is {actual} bytes; Rune allows at most {maximum} bytes"
            ),
            Self::InvalidJson(message) => {
                write!(formatter, "invalid package metadata JSON: {message}")
            }
            Self::UnsupportedSchema(version) => {
                write!(formatter, "unsupported manifest schema version: {version}")
            }
            Self::UnsupportedRegistrySchema(version) => {
                write!(formatter, "unsupported registry schema version: {version}")
            }
            Self::InvalidField { field, reason } => write!(formatter, "invalid {field}: {reason}"),
            Self::DuplicatePath(path) => write!(formatter, "duplicate package path: {path}"),
            Self::DuplicateCommand(name) => {
                write!(formatter, "duplicate package command: {name}")
            }
            Self::DuplicateRegistryPackage(name) => {
                write!(formatter, "duplicate registry package: {name}")
            }
            Self::DuplicateRegistryArtifact(path) => {
                write!(formatter, "duplicate registry artifact path: {path}")
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

impl PackageRegistryIndex {
    /// Parses and validates a bounded registry index.
    ///
    /// # Errors
    ///
    /// Returns a structured error for oversized, malformed, unsupported, or
    /// semantically invalid registry metadata.
    pub fn parse(bytes: &[u8]) -> Result<Self, PackageError> {
        if bytes.len() > MAX_REGISTRY_BYTES {
            return Err(PackageError::RegistryTooLarge {
                actual: bytes.len(),
                maximum: MAX_REGISTRY_BYTES,
            });
        }
        let index: Self = serde_json::from_slice(bytes)
            .map_err(|error| PackageError::InvalidJson(error.to_string()))?;
        index.validate()?;
        Ok(index)
    }

    /// Validates a registry index constructed in Rust rather than parsed from
    /// JSON.
    ///
    /// # Errors
    ///
    /// Returns an error when the schema, package identity, paths, or URLs are
    /// outside the registry metadata policy.
    pub fn validate(&self) -> Result<(), PackageError> {
        if self.schema_version != CURRENT_REGISTRY_SCHEMA_VERSION {
            return Err(PackageError::UnsupportedRegistrySchema(self.schema_version));
        }
        if self.packages.len() > MAX_REGISTRY_PACKAGES {
            return Err(invalid_field(
                "packages",
                format!("must contain at most {MAX_REGISTRY_PACKAGES} entries"),
            ));
        }

        let mut package_ids = BTreeSet::new();
        for package in &self.packages {
            package.validate()?;
            let id = format!("{}@{}", package.name, package.version);
            if !package_ids.insert(id.clone()) {
                return Err(PackageError::DuplicateRegistryPackage(id));
            }
        }
        Ok(())
    }

    /// Finds one exact package version in the index.
    #[must_use]
    pub fn find(&self, name: &str, version: &str) -> Option<&RegistryPackage> {
        self.packages
            .iter()
            .find(|package| package.name == name && package.version == version)
    }
}

impl RegistryPackage {
    fn validate(&self) -> Result<(), PackageError> {
        validate_package_name(&self.name)?;
        validate_version(&self.version)?;
        if self.description.chars().count() > MAX_DESCRIPTION_CHARS {
            return Err(invalid_field(
                "packages.description",
                format!("must be at most {MAX_DESCRIPTION_CHARS} characters"),
            ));
        }
        validate_registry_url("packages.manifest_url", &self.manifest_url)?;
        if self.artifacts.is_empty() {
            return Err(invalid_field(
                "packages.artifacts",
                "must contain at least one entry",
            ));
        }
        if self.artifacts.len() > MAX_REGISTRY_ARTIFACTS {
            return Err(invalid_field(
                "packages.artifacts",
                format!("must contain at most {MAX_REGISTRY_ARTIFACTS} entries"),
            ));
        }

        let mut paths = BTreeSet::new();
        for artifact in &self.artifacts {
            validate_relative_path(&artifact.path)?;
            validate_registry_url("packages.artifacts.url", &artifact.url)?;
            if !paths.insert(&artifact.path) {
                return Err(PackageError::DuplicateRegistryArtifact(
                    artifact.path.clone(),
                ));
            }
        }
        Ok(())
    }

    /// Returns the remote artifact metadata for one relative package path.
    #[must_use]
    pub fn artifact(&self, path: &str) -> Option<&RegistryArtifact> {
        self.artifacts.iter().find(|artifact| artifact.path == path)
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

fn validate_registry_url(field: &str, url: &str) -> Result<(), PackageError> {
    if url.is_empty() || url.len() > MAX_REGISTRY_URL_BYTES {
        return Err(invalid_field(
            field,
            format!("must contain 1-{MAX_REGISTRY_URL_BYTES} bytes"),
        ));
    }
    if url
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(invalid_field(
            field,
            "must not contain whitespace or control characters",
        ));
    }
    if !url
        .get(.."https://".len())
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
    {
        return Err(invalid_field(field, "must use HTTPS"));
    }
    let authority = &url["https://".len()..]
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.is_empty() {
        return Err(invalid_field(field, "must contain a host"));
    }
    if authority.contains('@') {
        return Err(invalid_field(field, "must not contain URL userinfo"));
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
    use super::{sha256_hex, PackageError, PackageManifest, PackageRegistryIndex};

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
        assert!(!manifest.permissions.filesystem);
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
    fn accepts_an_explicit_filesystem_permission_and_rejects_unknown_permissions() {
        let permitted = manifest_json(HELLO_DIGEST).replace(
            "\"commands\":",
            "\"permissions\": {\"filesystem\": true},\n                \"commands\":",
        );
        let manifest =
            PackageManifest::parse(permitted.as_bytes()).expect("known permission should validate");
        assert!(manifest.permissions.filesystem);

        let unknown = manifest_json(HELLO_DIGEST).replace(
            "\"commands\":",
            "\"permissions\": {\"network\": true},\n                \"commands\":",
        );
        assert!(matches!(
            PackageManifest::parse(unknown.as_bytes()),
            Err(PackageError::InvalidJson(_))
        ));
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

    #[test]
    fn parses_and_finds_an_https_registry_package() {
        let index = PackageRegistryIndex::parse(
            br#"{
                "schema_version": 1,
                "packages": [{
                    "name": "hello-rune",
                    "version": "0.1.0",
                    "description": "A remote package",
                    "manifest_url": "https://registry.example.test/hello/manifest.json",
                    "artifacts": [{
                        "path": "bin/hello.wasm",
                        "url": "https://registry.example.test/hello/bin/hello.wasm"
                    }]
                }]
            }"#,
        )
        .expect("registry index should validate");
        let package = index
            .find("hello-rune", "0.1.0")
            .expect("package should be indexed");
        assert_eq!(
            package.artifact("bin/hello.wasm").unwrap().url,
            "https://registry.example.test/hello/bin/hello.wasm"
        );
    }

    #[test]
    fn rejects_insecure_registry_urls_and_duplicate_versions() {
        let insecure = br#"{
            "schema_version": 1,
            "packages": [{
                "name": "hello-rune",
                "version": "0.1.0",
                "description": "A package",
                "manifest_url": "http://registry.example.test/manifest.json",
                "artifacts": [{"path": "bin/hello.wasm", "url": "https://registry.example.test/hello.wasm"}]
            }]
        }"#;
        assert!(matches!(
            PackageRegistryIndex::parse(insecure),
            Err(PackageError::InvalidField { field, .. }) if field == "packages.manifest_url"
        ));

        let duplicate = br#"{
            "schema_version": 1,
            "packages": [
                {
                    "name": "hello-rune",
                    "version": "0.1.0",
                    "description": "A package",
                    "manifest_url": "https://registry.example.test/one.json",
                    "artifacts": [{"path": "bin/hello.wasm", "url": "https://registry.example.test/one.wasm"}]
                },
                {
                    "name": "hello-rune",
                    "version": "0.1.0",
                    "description": "A package",
                    "manifest_url": "https://registry.example.test/two.json",
                    "artifacts": [{"path": "bin/hello.wasm", "url": "https://registry.example.test/two.wasm"}]
                }
            ]
        }"#;
        assert!(matches!(
            PackageRegistryIndex::parse(duplicate),
            Err(PackageError::DuplicateRegistryPackage(id)) if id == "hello-rune@0.1.0"
        ));
    }
}
