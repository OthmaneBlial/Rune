//! Filesystem policy and path resolution for Rune.
//!
//! The core talks to [`VirtualFileSystem`] rather than to platform APIs. The
//! initial implementation is host-backed and confines every operation to a
//! canonical root, which models the app's Documents directory during local
//! development.

#![allow(clippy::missing_errors_doc)]

use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

/// A file or directory entry exposed to shell commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub is_directory: bool,
    pub is_symlink: bool,
    pub size: u64,
}

/// Metadata for one resolved path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    pub name: String,
    pub is_directory: bool,
    pub is_symlink: bool,
    pub size: u64,
}

/// Errors exposed by the filesystem boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsError {
    InvalidPath(String),
    OutsideSandbox(String),
    NotFound(String),
    NotDirectory(String),
    NotFile(String),
    AlreadyExists(String),
    RootOperation(String),
    Io {
        operation: String,
        path: String,
        message: String,
    },
}

impl Display for FsError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath(path) => write!(formatter, "invalid path: {path}"),
            Self::OutsideSandbox(path) => {
                write!(formatter, "path escapes the Rune sandbox: {path}")
            }
            Self::NotFound(path) => write!(formatter, "no such file or directory: {path}"),
            Self::NotDirectory(path) => write!(formatter, "not a directory: {path}"),
            Self::NotFile(path) => write!(formatter, "not a regular file: {path}"),
            Self::AlreadyExists(path) => write!(formatter, "file already exists: {path}"),
            Self::RootOperation(operation) => {
                write!(formatter, "cannot {operation} the sandbox root")
            }
            Self::Io {
                operation,
                path,
                message,
            } => write!(formatter, "{operation} {path}: {message}"),
        }
    }
}

impl std::error::Error for FsError {}

/// Filesystem operations required by the portable command engine.
pub trait VirtualFileSystem {
    /// Returns the primary approved host root when this VFS is backed by a
    /// directory.
    ///
    /// Runtime providers may use this only to install an explicit capability
    /// such as a WASI preopen. A VFS without a host representation returns
    /// `None`, which keeps runtime filesystem access disabled.
    fn host_root(&self) -> Option<&Path> {
        None
    }
    /// Returns approved host directories and their guest paths for runtimes
    /// that support more than one explicit capability. The default keeps
    /// legacy VFS implementations compatible with one `/` preopen.
    fn host_preopens(&self) -> Vec<(&Path, &str)> {
        self.host_root()
            .into_iter()
            .map(|root| (root, "/"))
            .collect()
    }
    fn current_dir_display(&self) -> String;
    fn change_dir(&mut self, input: &str) -> Result<(), FsError>;
    fn metadata(&self, input: &str) -> Result<FileInfo, FsError>;
    fn list(&self, input: Option<&str>) -> Result<Vec<FileEntry>, FsError>;
    /// Expands unquoted `*` and `?` patterns while preserving the sandbox.
    fn glob(&self, input: &str) -> Result<Vec<String>, FsError>;
    fn read(&self, input: &str) -> Result<Vec<u8>, FsError>;
    fn write(&self, input: &str, content: &[u8], append: bool) -> Result<(), FsError>;
    fn make_directory(&self, input: &str, parents: bool) -> Result<(), FsError>;
    fn touch(&self, input: &str) -> Result<(), FsError>;
    /// Creates a relative symbolic link whose resolved target stays in Rune's
    /// sandbox.
    fn make_symlink(&self, target: &str, link: &str) -> Result<(), FsError>;
    /// Reads a symbolic link without exposing a path outside the sandbox.
    fn read_link(&self, input: &str) -> Result<String, FsError>;
    fn remove(&self, input: &str, recursive: bool, force: bool) -> Result<(), FsError>;
    fn copy(&self, source: &str, destination: &str, recursive: bool) -> Result<(), FsError>;
    fn move_path(&self, source: &str, destination: &str) -> Result<(), FsError>;
}

const MAX_COPY_ENTRIES: usize = 10_000;
const MAX_DIRECTORY_ENTRIES: usize = 10_000;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
struct MountedRoot {
    name: &'static str,
    physical: PathBuf,
}

/// A host-backed filesystem with a strict virtual root.
#[derive(Debug, Clone)]
pub struct SandboxedFileSystem {
    root: PathBuf,
    mounts: Vec<MountedRoot>,
    current_dir: PathBuf,
    previous_dir: Option<PathBuf>,
}

impl SandboxedFileSystem {
    /// Creates the root directory if needed and canonicalizes it.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, FsError> {
        let root = root.as_ref();
        Self::from_roots(root, Vec::new())
    }

    /// Creates a virtual app sandbox whose home is `home` and whose standard
    /// iOS writable directories are mounted at `~/Library` and `~/tmp`.
    ///
    /// The three physical paths are canonicalized independently and are the
    /// only roots accepted by path and symlink validation. This lets the
    /// portable shell model Apple's Documents/Library/tmp layout without
    /// exposing the containing app directory or an arbitrary host path.
    pub fn new_with_layout(
        home: impl AsRef<Path>,
        library: impl AsRef<Path>,
        temporary: impl AsRef<Path>,
    ) -> Result<Self, FsError> {
        let home = home.as_ref();
        let library = library.as_ref();
        let temporary = temporary.as_ref();
        let mounts = vec![("Library", library), ("tmp", temporary)];
        Self::from_roots(home, mounts)
    }

    fn from_roots(root: &Path, mount_paths: Vec<(&'static str, &Path)>) -> Result<Self, FsError> {
        fs::create_dir_all(root).map_err(|error| Self::io_error("create", root, &error))?;
        let root = root
            .canonicalize()
            .map_err(|error| Self::io_error("canonicalize", root, &error))?;
        let mut physical_roots = BTreeSet::new();
        physical_roots.insert(root.clone());
        let mut mounts = Vec::with_capacity(mount_paths.len());
        for (name, path) in mount_paths {
            fs::create_dir_all(path).map_err(|error| Self::io_error("create", path, &error))?;
            let physical = path
                .canonicalize()
                .map_err(|error| Self::io_error("canonicalize", path, &error))?;
            if !physical_roots.insert(physical.clone()) {
                return Err(FsError::InvalidPath(format!(
                    "sandbox roots must be distinct: {name}"
                )));
            }
            mounts.push(MountedRoot { name, physical });
        }
        Ok(Self {
            current_dir: root.clone(),
            previous_dir: None,
            root,
            mounts,
        })
    }

    /// Returns the physical root used by this instance.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves a virtual path without exposing paths outside the root.
    pub fn resolve_path(&self, input: &str) -> Result<PathBuf, FsError> {
        if input.is_empty() {
            return Err(FsError::InvalidPath("empty path".to_string()));
        }
        let (base, relative_input) = if input == "~" {
            (&self.root, "")
        } else if let Some(path) = input.strip_prefix("~/") {
            (&self.root, path)
        } else if input.starts_with('~') {
            return Err(FsError::InvalidPath(
                "only the current user's ~ home is supported".to_string(),
            ));
        } else if let Some(path) = input.strip_prefix('/') {
            (&self.root, path)
        } else {
            (&self.current_dir, input)
        };

        let mut candidate = base.clone();
        for component in Path::new(relative_input).components() {
            match component {
                Component::CurDir => {}
                Component::Normal(part) => {
                    candidate = self.join_component(&candidate, part.to_string_lossy().as_ref());
                }
                Component::ParentDir => {
                    candidate = self.parent_component(&candidate, input)?;
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(FsError::InvalidPath(input.to_string()));
                }
            }
        }

        self.ensure_inside(&candidate, input)?;
        Ok(candidate)
    }

    fn display_path(&self, path: &Path) -> String {
        for mount in &self.mounts {
            match path.strip_prefix(&mount.physical) {
                Ok(relative) if relative.as_os_str().is_empty() => {
                    return format!("~/{}", mount.name);
                }
                Ok(relative) => return format!("~/{}/{}", mount.name, relative.display()),
                Err(_) => {}
            }
        }
        match path.strip_prefix(&self.root) {
            Ok(relative) if relative.as_os_str().is_empty() => "~".to_string(),
            Ok(relative) => format!("~/{}", relative.display()),
            Err(_) => "~".to_string(),
        }
    }

    fn ensure_inside(&self, candidate: &Path, input: &str) -> Result<(), FsError> {
        let mut existing = candidate;
        while !existing.exists() {
            existing = existing
                .parent()
                .ok_or_else(|| FsError::OutsideSandbox(input.to_string()))?;
        }
        let canonical = existing
            .canonicalize()
            .map_err(|error| Self::io_error("canonicalize", existing, &error))?;
        if self
            .approved_roots()
            .any(|root| canonical.starts_with(root))
        {
            Ok(())
        } else {
            Err(FsError::OutsideSandbox(input.to_string()))
        }
    }

    fn info_for_path(path: &Path) -> Result<FileInfo, FsError> {
        let metadata =
            fs::symlink_metadata(path).map_err(|error| Self::map_metadata_error(path, &error))?;
        Ok(FileInfo {
            name: path.file_name().map_or_else(
                || "~".to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
            is_directory: metadata.is_dir(),
            is_symlink: metadata.file_type().is_symlink(),
            size: metadata.len(),
        })
    }

    fn map_metadata_error(path: &Path, error: &std::io::Error) -> FsError {
        if error.kind() == std::io::ErrorKind::NotFound {
            FsError::NotFound(path.display().to_string())
        } else {
            Self::io_error("inspect", path, error)
        }
    }

    fn reframe(error: FsError, input: &str) -> FsError {
        match error {
            FsError::InvalidPath(_) => FsError::InvalidPath(input.to_string()),
            FsError::OutsideSandbox(_) => FsError::OutsideSandbox(input.to_string()),
            FsError::NotFound(_) => FsError::NotFound(input.to_string()),
            FsError::NotDirectory(_) => FsError::NotDirectory(input.to_string()),
            FsError::NotFile(_) => FsError::NotFile(input.to_string()),
            FsError::AlreadyExists(_) => FsError::AlreadyExists(input.to_string()),
            FsError::RootOperation(operation) => FsError::RootOperation(operation),
            FsError::Io {
                operation, message, ..
            } => FsError::Io {
                operation,
                path: input.to_string(),
                message,
            },
        }
    }

    fn io_error(operation: &str, path: &Path, error: &std::io::Error) -> FsError {
        FsError::Io {
            operation: operation.to_string(),
            path: path.display().to_string(),
            message: error.to_string(),
        }
    }

    fn parent_is_directory(path: &Path, input: &str) -> Result<(), FsError> {
        let parent = path
            .parent()
            .ok_or_else(|| FsError::InvalidPath(input.to_string()))?;
        let metadata =
            fs::metadata(parent).map_err(|error| Self::map_metadata_error(parent, &error))?;
        if metadata.is_dir() {
            Ok(())
        } else {
            Err(FsError::NotDirectory(input.to_string()))
        }
    }

    fn no_root_operation(&self, path: &Path, operation: &str) -> Result<(), FsError> {
        if path == self.root || self.mounts.iter().any(|mount| path == mount.physical) {
            Err(FsError::RootOperation(operation.to_string()))
        } else {
            Ok(())
        }
    }

    fn approved_roots(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.root.as_path())
            .chain(self.mounts.iter().map(|mount| mount.physical.as_path()))
    }

    fn mount_for_name(&self, name: &str) -> Option<&MountedRoot> {
        self.mounts.iter().find(|mount| mount.name == name)
    }

    fn join_component(&self, base: &Path, component: &str) -> PathBuf {
        if base == self.root {
            if let Some(mount) = self.mount_for_name(component) {
                return mount.physical.clone();
            }
        }
        base.join(component)
    }

    fn parent_component(&self, path: &Path, input: &str) -> Result<PathBuf, FsError> {
        if path == self.root {
            return Err(FsError::OutsideSandbox(input.to_string()));
        }
        if self.mounts.iter().any(|mount| path == mount.physical) {
            return Ok(self.root.clone());
        }
        path.parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| FsError::OutsideSandbox(input.to_string()))
    }
}

#[derive(Debug)]
struct GlobCandidate {
    physical: PathBuf,
    virtual_path: String,
}

fn append_virtual_path(prefix: &str, component: &str) -> String {
    if prefix.is_empty() || prefix.ends_with('/') {
        format!("{prefix}{component}")
    } else {
        format!("{prefix}/{component}")
    }
}

#[cfg(unix)]
fn create_symlink(target: &str, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn create_symlink(_target: &str, _link: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "symbolic links are unavailable on this target",
    ))
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    let mut memo = vec![vec![None; value.len() + 1]; pattern.len() + 1];

    wildcard_match_at(&pattern, &value, 0, 0, &mut memo)
}

fn wildcard_match_at(
    pattern: &[char],
    value: &[char],
    pattern_index: usize,
    value_index: usize,
    memo: &mut [Vec<Option<bool>>],
) -> bool {
    if let Some(result) = memo[pattern_index][value_index] {
        return result;
    }

    let result = if pattern_index == pattern.len() {
        value_index == value.len()
    } else {
        match pattern[pattern_index] {
            '*' => {
                wildcard_match_at(pattern, value, pattern_index + 1, value_index, memo)
                    || (value_index < value.len()
                        && wildcard_match_at(pattern, value, pattern_index, value_index + 1, memo))
            }
            '?' => {
                value_index < value.len()
                    && wildcard_match_at(pattern, value, pattern_index + 1, value_index + 1, memo)
            }
            character => {
                value.get(value_index) == Some(&character)
                    && wildcard_match_at(pattern, value, pattern_index + 1, value_index + 1, memo)
            }
        }
    };
    memo[pattern_index][value_index] = Some(result);
    result
}

fn expand_glob_component(
    filesystem: &SandboxedFileSystem,
    candidates: Vec<GlobCandidate>,
    pattern: &str,
    input: &str,
) -> Result<Vec<GlobCandidate>, FsError> {
    let has_wildcard = pattern
        .chars()
        .any(|character| matches!(character, '*' | '?'));
    let mut next = Vec::new();
    for candidate in candidates {
        if has_wildcard {
            let metadata = match fs::symlink_metadata(&candidate.physical) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(SandboxedFileSystem::io_error(
                        "glob",
                        Path::new(&candidate.virtual_path),
                        &error,
                    ));
                }
            };
            if !metadata.is_dir() {
                continue;
            }
            let mut entries_seen = 0;
            let mut entry_names = BTreeSet::new();
            for entry in fs::read_dir(&candidate.physical).map_err(|error| {
                SandboxedFileSystem::io_error("glob", Path::new(&candidate.virtual_path), &error)
            })? {
                entries_seen += 1;
                if entries_seen > MAX_DIRECTORY_ENTRIES {
                    return Err(FsError::Io {
                        operation: "glob".to_string(),
                        path: input.to_string(),
                        message: format!("directory exceeds {MAX_DIRECTORY_ENTRIES} entries"),
                    });
                }
                let entry = entry.map_err(|error| {
                    SandboxedFileSystem::io_error(
                        "glob",
                        Path::new(&candidate.virtual_path),
                        &error,
                    )
                })?;
                let name = entry.file_name().to_string_lossy().into_owned();
                entry_names.insert(name.clone());
                if candidate.physical == filesystem.root
                    && filesystem.mount_for_name(&name).is_some()
                {
                    continue;
                }
                if name.starts_with('.') && !pattern.starts_with('.') {
                    continue;
                }
                if !wildcard_match(pattern, &name) {
                    continue;
                }
                let physical = entry.path();
                filesystem
                    .ensure_inside(&physical, input)
                    .map_err(|error| SandboxedFileSystem::reframe(error, input))?;
                next.push(GlobCandidate {
                    physical,
                    virtual_path: append_virtual_path(&candidate.virtual_path, &name),
                });
            }
            if candidate.physical == filesystem.root {
                for mount in &filesystem.mounts {
                    if entry_names.contains(mount.name) || !wildcard_match(pattern, mount.name) {
                        continue;
                    }
                    if entries_seen >= MAX_DIRECTORY_ENTRIES {
                        return Err(FsError::Io {
                            operation: "glob".to_string(),
                            path: input.to_string(),
                            message: format!("directory exceeds {MAX_DIRECTORY_ENTRIES} entries"),
                        });
                    }
                    entries_seen += 1;
                    next.push(GlobCandidate {
                        physical: mount.physical.clone(),
                        virtual_path: append_virtual_path(&candidate.virtual_path, mount.name),
                    });
                }
            }
        } else {
            let physical = filesystem.join_component(&candidate.physical, pattern);
            if !physical.exists() {
                continue;
            }
            filesystem
                .ensure_inside(&physical, input)
                .map_err(|error| SandboxedFileSystem::reframe(error, input))?;
            next.push(GlobCandidate {
                physical,
                virtual_path: append_virtual_path(&candidate.virtual_path, pattern),
            });
        }
    }
    Ok(next)
}

impl SandboxedFileSystem {
    fn copy_directory(
        source: &Path,
        destination: &Path,
        source_label: &str,
        destination_label: &str,
        copied: &mut usize,
        copied_bytes: &mut u64,
    ) -> Result<(), FsError> {
        fs::create_dir(destination)
            .map_err(|error| Self::io_error("copy", Path::new(destination_label), &error))?;
        for entry in fs::read_dir(source)
            .map_err(|error| Self::io_error("copy", Path::new(source_label), &error))?
        {
            let entry =
                entry.map_err(|error| Self::io_error("copy", Path::new(source_label), &error))?;
            *copied += 1;
            if *copied > MAX_COPY_ENTRIES {
                return Err(FsError::Io {
                    operation: "copy".to_string(),
                    path: source_label.to_string(),
                    message: format!("directory exceeds {MAX_COPY_ENTRIES} entries"),
                });
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let source_child = entry.path();
            let destination_child = destination.join(&name);
            let source_child_label = format!("{source_label}/{name}");
            let destination_child_label = format!("{destination_label}/{name}");
            let metadata = fs::symlink_metadata(&source_child)
                .map_err(|error| Self::io_error("copy", Path::new(&source_child_label), &error))?;
            if metadata.file_type().is_symlink() {
                return Err(FsError::InvalidPath(format!(
                    "copy does not follow symlinks: {source_child_label}"
                )));
            }
            if metadata.is_dir() {
                Self::copy_directory(
                    &source_child,
                    &destination_child,
                    &source_child_label,
                    &destination_child_label,
                    copied,
                    copied_bytes,
                )?;
            } else if metadata.is_file() {
                *copied_bytes =
                    copied_bytes
                        .checked_add(metadata.len())
                        .ok_or_else(|| FsError::Io {
                            operation: "copy".to_string(),
                            path: source_child_label.clone(),
                            message: format!("file tree exceeds {MAX_FILE_BYTES} bytes"),
                        })?;
                if *copied_bytes > MAX_FILE_BYTES {
                    return Err(FsError::Io {
                        operation: "copy".to_string(),
                        path: source_child_label.clone(),
                        message: format!("file tree exceeds {MAX_FILE_BYTES} bytes"),
                    });
                }
                fs::copy(&source_child, &destination_child).map_err(|error| {
                    Self::io_error("copy", Path::new(&destination_child_label), &error)
                })?;
            } else {
                return Err(FsError::InvalidPath(format!(
                    "copy supports regular files and directories only: {source_child_label}"
                )));
            }
        }
        Ok(())
    }
}

impl VirtualFileSystem for SandboxedFileSystem {
    fn host_root(&self) -> Option<&Path> {
        Some(&self.root)
    }

    fn host_preopens(&self) -> Vec<(&Path, &str)> {
        let mut preopens = vec![(self.root.as_path(), "/")];
        preopens.extend(self.mounts.iter().map(|mount| {
            let guest_path = match mount.name {
                "Library" => "/Library",
                "tmp" => "/tmp",
                _ => unreachable!("unknown Rune mount"),
            };
            (mount.physical.as_path(), guest_path)
        }));
        preopens
    }

    fn current_dir_display(&self) -> String {
        self.display_path(&self.current_dir)
    }

    fn change_dir(&mut self, input: &str) -> Result<(), FsError> {
        let target = if input == "-" {
            self.previous_dir
                .clone()
                .ok_or_else(|| FsError::InvalidPath("no previous directory".to_string()))?
        } else {
            self.resolve_path(input)?
        };
        let metadata = fs::metadata(&target)
            .map_err(|error| Self::reframe(Self::map_metadata_error(&target, &error), input))?;
        if !metadata.is_dir() {
            return Err(FsError::NotDirectory(input.to_string()));
        }
        let previous = std::mem::replace(&mut self.current_dir, target);
        self.previous_dir = Some(previous);
        Ok(())
    }

    fn metadata(&self, input: &str) -> Result<FileInfo, FsError> {
        let path = self.resolve_path(input)?;
        Self::info_for_path(&path).map_err(|error| Self::reframe(error, input))
    }

    fn list(&self, input: Option<&str>) -> Result<Vec<FileEntry>, FsError> {
        let path = match input {
            Some(input) => self.resolve_path(input)?,
            None => self.current_dir.clone(),
        };
        let display_input = input.unwrap_or("~");
        let metadata = fs::metadata(&path).map_err(|error| {
            Self::reframe(Self::map_metadata_error(&path, &error), display_input)
        })?;
        if !metadata.is_dir() {
            return Err(FsError::NotDirectory(display_input.to_string()));
        }
        let mut entries = Vec::new();
        let mut entry_names = BTreeSet::new();
        let mut entries_seen = 0;
        for entry in fs::read_dir(&path)
            .map_err(|error| Self::io_error("list", Path::new(display_input), &error))?
        {
            let entry =
                entry.map_err(|error| Self::io_error("list", Path::new(display_input), &error))?;
            let entry_path = entry.path();
            let info = Self::info_for_path(&entry_path)
                .map_err(|error| Self::reframe(error, display_input))?;
            if path == self.root && self.mount_for_name(&info.name).is_some() {
                continue;
            }
            entries_seen += 1;
            if entries_seen > MAX_DIRECTORY_ENTRIES {
                return Err(FsError::Io {
                    operation: "list".to_string(),
                    path: display_input.to_string(),
                    message: format!("directory exceeds {MAX_DIRECTORY_ENTRIES} entries"),
                });
            }
            entry_names.insert(info.name.clone());
            entries.push(FileEntry {
                name: info.name,
                is_directory: info.is_directory,
                is_symlink: info.is_symlink,
                size: info.size,
            });
        }
        if path == self.root {
            for mount in &self.mounts {
                if entry_names.insert(mount.name.to_string()) {
                    entries_seen += 1;
                    if entries_seen > MAX_DIRECTORY_ENTRIES {
                        return Err(FsError::Io {
                            operation: "list".to_string(),
                            path: display_input.to_string(),
                            message: format!("directory exceeds {MAX_DIRECTORY_ENTRIES} entries"),
                        });
                    }
                    entries.push(FileEntry {
                        name: mount.name.to_string(),
                        is_directory: true,
                        is_symlink: false,
                        size: 0,
                    });
                }
            }
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    fn glob(&self, input: &str) -> Result<Vec<String>, FsError> {
        if input.is_empty() {
            return Err(FsError::InvalidPath("empty path".to_string()));
        }
        let (base, prefix, relative_input) = if input == "~" {
            (&self.root, "", "")
        } else if let Some(path) = input.strip_prefix("~/") {
            (&self.root, "~/", path)
        } else if input.starts_with('~') {
            return Err(FsError::InvalidPath(
                "only the current user's ~ home is supported".to_string(),
            ));
        } else if let Some(path) = input.strip_prefix('/') {
            (&self.root, "/", path)
        } else {
            (&self.current_dir, "", input)
        };

        let mut candidates = vec![GlobCandidate {
            physical: base.clone(),
            virtual_path: prefix.to_string(),
        }];
        for component in Path::new(relative_input).components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    for candidate in &mut candidates {
                        candidate.physical = self.parent_component(&candidate.physical, input)?;
                        candidate.virtual_path = append_virtual_path(&candidate.virtual_path, "..");
                        self.ensure_inside(&candidate.physical, input)
                            .map_err(|error| Self::reframe(error, input))?;
                    }
                }
                Component::Normal(part) => {
                    let pattern = part.to_string_lossy().into_owned();
                    candidates = expand_glob_component(self, candidates, &pattern, input)?;
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(FsError::InvalidPath(input.to_string()));
                }
            }
        }

        let mut matches = candidates
            .into_iter()
            .map(|candidate| candidate.virtual_path)
            .collect::<Vec<_>>();
        matches.sort();
        matches.dedup();
        if matches.is_empty() {
            Ok(vec![input.to_string()])
        } else {
            Ok(matches)
        }
    }

    fn read(&self, input: &str) -> Result<Vec<u8>, FsError> {
        let path = self.resolve_path(input)?;
        let metadata = fs::metadata(&path)
            .map_err(|error| Self::reframe(Self::map_metadata_error(&path, &error), input))?;
        if !metadata.is_file() {
            return Err(FsError::NotFile(input.to_string()));
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(FsError::Io {
                operation: "read".to_string(),
                path: input.to_string(),
                message: format!("file exceeds {MAX_FILE_BYTES} bytes"),
            });
        }
        fs::read(&path).map_err(|error| Self::io_error("read", Path::new(input), &error))
    }

    fn write(&self, input: &str, content: &[u8], append: bool) -> Result<(), FsError> {
        let path = self.resolve_path(input)?;
        Self::parent_is_directory(&path, input)?;
        let existing_bytes = if append {
            fs::metadata(&path).map_or(0, |metadata| metadata.len())
        } else {
            0
        };
        let requested_bytes = existing_bytes.saturating_add(content.len() as u64);
        if requested_bytes > MAX_FILE_BYTES {
            return Err(FsError::Io {
                operation: "write".to_string(),
                path: input.to_string(),
                message: format!("file exceeds {MAX_FILE_BYTES} bytes"),
            });
        }
        let mut options = OpenOptions::new();
        options.create(true).write(true);
        if append {
            options.append(true);
        } else {
            options.truncate(true);
        }
        let mut file = options
            .open(&path)
            .map_err(|error| Self::io_error("write", Path::new(input), &error))?;
        file.write_all(content)
            .map_err(|error| Self::io_error("write", Path::new(input), &error))
    }

    fn make_directory(&self, input: &str, parents: bool) -> Result<(), FsError> {
        let path = self.resolve_path(input)?;
        if path.exists() {
            let metadata = fs::metadata(&path)
                .map_err(|error| Self::reframe(Self::map_metadata_error(&path, &error), input))?;
            if parents && metadata.is_dir() {
                return Ok(());
            }
            return Err(FsError::AlreadyExists(input.to_string()));
        }
        if !parents {
            Self::parent_is_directory(&path, input)?;
        }
        let result = if parents {
            fs::create_dir_all(&path)
        } else {
            fs::create_dir(&path)
        };
        result.map_err(|error| Self::io_error("create directory", Path::new(input), &error))
    }

    fn touch(&self, input: &str) -> Result<(), FsError> {
        let path = self.resolve_path(input)?;
        Self::parent_is_directory(&path, input)?;
        if path.exists() {
            let metadata = fs::metadata(&path)
                .map_err(|error| Self::reframe(Self::map_metadata_error(&path, &error), input))?;
            if !metadata.is_file() {
                return Err(FsError::NotFile(input.to_string()));
            }
            return OpenOptions::new()
                .write(true)
                .open(&path)
                .map(|_| ())
                .map_err(|error| Self::io_error("touch", Path::new(input), &error));
        }
        File::create(&path)
            .map(|_| ())
            .map_err(|error| Self::io_error("touch", Path::new(input), &error))
    }

    fn make_symlink(&self, target: &str, link: &str) -> Result<(), FsError> {
        if target.is_empty() || target.starts_with('/') || target.starts_with('~') {
            return Err(FsError::InvalidPath(format!(
                "symbolic link target must be a relative path: {target}"
            )));
        }
        let link_path = self.resolve_path(link)?;
        self.no_root_operation(&link_path, "create symbolic link")?;
        if fs::symlink_metadata(&link_path).is_ok() {
            return Err(FsError::AlreadyExists(link.to_string()));
        }
        Self::parent_is_directory(&link_path, link)?;
        let parent = link_path
            .parent()
            .ok_or_else(|| FsError::InvalidPath(link.to_string()))?;
        let target_path = parent.join(target);
        self.ensure_inside(&target_path, target)
            .map_err(|error| Self::reframe(error, target))?;
        fs::metadata(&target_path).map_err(|error| {
            Self::reframe(Self::map_metadata_error(&target_path, &error), target)
        })?;
        create_symlink(target, &link_path)
            .map_err(|error| Self::io_error("create symbolic link", Path::new(link), &error))
    }

    fn read_link(&self, input: &str) -> Result<String, FsError> {
        let path = self.resolve_path(input)?;
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| Self::reframe(Self::map_metadata_error(&path, &error), input))?;
        if !metadata.file_type().is_symlink() {
            return Err(FsError::InvalidPath(format!(
                "not a symbolic link: {input}"
            )));
        }
        fs::read_link(&path)
            .map(|target| target.to_string_lossy().into_owned())
            .map_err(|error| Self::io_error("read symbolic link", Path::new(input), &error))
    }

    fn remove(&self, input: &str, recursive: bool, force: bool) -> Result<(), FsError> {
        let path = match self.resolve_path(input) {
            Ok(path) => path,
            Err(FsError::NotFound(_)) if force => return Ok(()),
            Err(error) => return Err(error),
        };
        self.no_root_operation(&path, "remove")?;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if force && error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(Self::reframe(
                    Self::map_metadata_error(&path, &error),
                    input,
                ));
            }
        };
        if metadata.is_dir() {
            if recursive {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_dir(&path)
            }
        } else {
            fs::remove_file(&path)
        }
        .map_err(|error| Self::io_error("remove", Path::new(input), &error))
    }

    fn copy(&self, source: &str, destination: &str, recursive: bool) -> Result<(), FsError> {
        let source_path = self.resolve_path(source)?;
        let destination_path = self.resolve_path(destination)?;
        let source_metadata = fs::symlink_metadata(&source_path).map_err(|error| {
            Self::reframe(Self::map_metadata_error(&source_path, &error), source)
        })?;
        if source_metadata.file_type().is_symlink() {
            return Err(FsError::InvalidPath(format!(
                "copy does not follow symlinks: {source}"
            )));
        }
        if source_metadata.is_dir() {
            if !recursive {
                return Err(FsError::NotFile(source.to_string()));
            }
            if destination_path.exists() {
                return Err(FsError::AlreadyExists(destination.to_string()));
            }
            Self::parent_is_directory(&destination_path, destination)?;
            let mut copied = 0;
            let mut copied_bytes = 0;
            let result = Self::copy_directory(
                &source_path,
                &destination_path,
                source,
                destination,
                &mut copied,
                &mut copied_bytes,
            );
            if result.is_err() {
                let _ = fs::remove_dir_all(&destination_path);
            }
            return result;
        }
        if !source_metadata.is_file() {
            return Err(FsError::NotFile(source.to_string()));
        }
        Self::parent_is_directory(&destination_path, destination)?;
        if source_metadata.len() > MAX_FILE_BYTES {
            return Err(FsError::Io {
                operation: "copy".to_string(),
                path: source.to_string(),
                message: format!("file exceeds {MAX_FILE_BYTES} bytes"),
            });
        }
        fs::copy(&source_path, &destination_path)
            .map(|_| ())
            .map_err(|error| Self::io_error("copy", Path::new(destination), &error))
    }

    fn move_path(&self, source: &str, destination: &str) -> Result<(), FsError> {
        let source_path = self.resolve_path(source)?;
        let destination_path = self.resolve_path(destination)?;
        let source_metadata = fs::symlink_metadata(&source_path).map_err(|error| {
            Self::reframe(Self::map_metadata_error(&source_path, &error), source)
        })?;
        if source_metadata.file_type().is_symlink() {
            return Err(FsError::InvalidPath(format!(
                "move does not follow symlinks: {source}"
            )));
        }
        if !source_metadata.is_file() && !source_metadata.is_dir() {
            return Err(FsError::NotFile(source.to_string()));
        }
        Self::parent_is_directory(&destination_path, destination)?;
        fs::rename(&source_path, &destination_path)
            .map_err(|error| Self::io_error("move", Path::new(destination), &error))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FsError, SandboxedFileSystem, VirtualFileSystem, MAX_DIRECTORY_ENTRIES, MAX_FILE_BYTES,
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root() -> std::path::PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        loop {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock is after epoch")
                .as_nanos();
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "rune-fs-test-{}-{timestamp}-{id}",
                std::process::id()
            ));
            match std::fs::create_dir(&root) {
                Ok(()) => return root,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("test root could not be created: {error}"),
            }
        }
    }

    #[test]
    fn resolves_virtual_paths_and_rejects_escape() {
        let root = test_root();
        let fs = SandboxedFileSystem::new(&root).expect("root created");
        assert_eq!(fs.current_dir_display(), "~");
        assert_eq!(
            fs.resolve_path("~/notes").expect("inside root"),
            fs.root().join("notes")
        );
        assert!(matches!(
            fs.resolve_path("../../outside"),
            Err(FsError::OutsideSandbox(_))
        ));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn supports_file_lifecycle_and_previous_directory() {
        let root = test_root();
        let mut fs = SandboxedFileSystem::new(&root).expect("root created");
        fs.make_directory("work", false).expect("directory created");
        fs.change_dir("work").expect("directory entered");
        fs.touch("note.txt").expect("file created");
        fs.write("note.txt", b"hello", false).expect("file written");
        assert_eq!(fs.read("note.txt").expect("file read"), b"hello");
        fs.change_dir("-").expect("previous directory restored");
        assert_eq!(fs.current_dir_display(), "~");
        fs.remove("work", true, false).expect("directory removed");
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn exposes_documents_library_and_tmp_as_confined_virtual_mounts() {
        let container = test_root();
        let home = container.join("Documents");
        let library = container.join("Library");
        let temporary = container.join("tmp");
        let mut fs = SandboxedFileSystem::new_with_layout(&home, &library, &temporary)
            .expect("layout created");

        let names = fs
            .list(None)
            .expect("home listed")
            .into_iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["Library", "tmp"]);
        assert_eq!(
            fs.resolve_path("~/Library").expect("library resolved"),
            library.canonicalize().expect("library canonicalized")
        );

        fs.change_dir("~/Library").expect("library entered");
        assert_eq!(fs.current_dir_display(), "~/Library");
        fs.touch("cache.txt").expect("library file created");
        assert!(library.join("cache.txt").is_file());
        fs.change_dir("../tmp").expect("tmp entered from library");
        assert_eq!(fs.current_dir_display(), "~/tmp");
        fs.change_dir("..").expect("home restored from tmp");
        assert_eq!(fs.current_dir_display(), "~");
        assert_eq!(fs.glob("L*").expect("mount glob expanded"), ["Library"]);
        assert!(matches!(
            fs.resolve_path("~/Library/../../outside"),
            Err(FsError::OutsideSandbox(_))
        ));

        std::fs::remove_dir_all(container).expect("test container removed");
    }

    #[cfg(unix)]
    #[test]
    fn creates_reads_and_confines_symbolic_links() {
        let root = test_root();
        let fs = SandboxedFileSystem::new(&root).expect("root created");
        fs.touch("target.txt").expect("target created");
        fs.write("target.txt", b"linked", false)
            .expect("target written");
        fs.make_symlink("target.txt", "link.txt")
            .expect("link created");
        assert_eq!(fs.read_link("link.txt").expect("link read"), "target.txt");
        assert!(fs.metadata("link.txt").expect("link metadata").is_symlink);
        assert_eq!(fs.read("link.txt").expect("link target read"), b"linked");
        assert!(matches!(
            fs.make_symlink("../outside.txt", "escape.txt"),
            Err(FsError::OutsideSandbox(_))
        ));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn creates_nested_directories_with_idempotent_parents() {
        let root = test_root();
        let fs = SandboxedFileSystem::new(&root).expect("root created");
        fs.make_directory("project/src", true)
            .expect("nested directory created");
        fs.make_directory("project/src", true)
            .expect("existing parent directory accepted");
        assert!(root.join("project/src").is_dir());
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn copies_bounded_directories_and_moves_them() {
        let root = test_root();
        let fs = SandboxedFileSystem::new(&root).expect("root created");
        fs.make_directory("source/nested", true)
            .expect("source tree created");
        fs.touch("source/nested/note.txt")
            .expect("source file created");
        fs.write("source/nested/note.txt", b"hello", false)
            .expect("source file written");
        assert!(matches!(
            fs.copy("source", "file", false),
            Err(FsError::NotFile(_))
        ));
        fs.copy("source", "copy", true).expect("directory copied");
        assert_eq!(
            fs.read("copy/nested/note.txt").expect("copied file read"),
            b"hello"
        );
        fs.move_path("copy", "moved").expect("directory moved");
        assert_eq!(
            fs.read("moved/nested/note.txt").expect("moved file read"),
            b"hello"
        );
        assert!(matches!(fs.metadata("copy"), Err(FsError::NotFound(_))));
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn rejects_file_reads_writes_and_copies_over_the_size_limit() {
        let root = test_root();
        let large_path = root.join("large.bin");
        let file = std::fs::File::create(&large_path).expect("large file created");
        file.set_len(MAX_FILE_BYTES + 1).expect("sparse file sized");
        let fs = SandboxedFileSystem::new(&root).expect("root created");

        let read_error = fs.read("large.bin").expect_err("large read rejected");
        assert!(read_error.to_string().contains("exceeds 67108864 bytes"));
        let write_error = fs
            .write("large.bin", b"x", true)
            .expect_err("large append rejected");
        assert!(write_error.to_string().contains("exceeds 67108864 bytes"));
        let copy_error = fs
            .copy("large.bin", "large-copy.bin", false)
            .expect_err("large copy rejected");
        assert!(copy_error.to_string().contains("exceeds 67108864 bytes"));
        assert!(!root.join("large-copy.bin").exists());
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn expands_bounded_wildcards_without_matching_hidden_entries_by_default() {
        let root = test_root();
        let fs = SandboxedFileSystem::new(&root).expect("root created");
        std::fs::write(root.join("alpha.txt"), b"alpha").expect("alpha written");
        std::fs::write(root.join("beta.txt"), b"beta").expect("beta written");
        std::fs::write(root.join(".hidden.txt"), b"hidden").expect("hidden written");

        assert_eq!(
            fs.glob("*.txt").expect("glob expanded"),
            ["alpha.txt", "beta.txt"]
        );
        assert_eq!(
            fs.glob(".hidden*").expect("hidden glob expanded"),
            [".hidden.txt"]
        );
        assert_eq!(
            fs.glob("missing*").expect("unmatched glob preserved"),
            ["missing*"]
        );
        std::fs::remove_dir_all(root).expect("test root removed");
    }

    #[test]
    fn bounds_directory_listing_and_glob_enumeration() {
        let root = test_root();
        for index in 0..=MAX_DIRECTORY_ENTRIES {
            std::fs::File::create(root.join(format!("entry-{index}")))
                .expect("directory entry created");
        }
        let fs = SandboxedFileSystem::new(&root).expect("root created");
        let listing = fs.list(Some("~")).expect_err("listing should be bounded");
        assert!(listing.to_string().contains("directory exceeds"));
        let glob = fs.glob("*").expect_err("glob should be bounded");
        assert!(glob.to_string().contains("directory exceeds"));
        std::fs::remove_dir_all(root).expect("test root removed");
    }
}
