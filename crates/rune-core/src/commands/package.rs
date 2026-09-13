use std::fmt::Write as _;

use crate::{fs_failure, usage, CommandContext, CommandOutput, PACKAGE_INSTALL_ROOT};
use rune_fs::FsError;
use rune_package::{PackageError, PackageManifest};

const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;

pub(super) fn pkg(context: &mut CommandContext<'_>) -> CommandOutput {
    let Some(operation) = context.args.first().map(String::as_str) else {
        return usage(
            "pkg",
            "usage: pkg info|verify|install MANIFEST; pkg list; pkg remove NAME [VERSION]",
        );
    };
    match operation {
        "list" => {
            if context.args.len() != 1 {
                return usage("pkg", "usage: pkg list");
            }
            list(context)
        }
        "remove" => remove(context),
        "info" | "verify" | "install" => {
            let Some(manifest_path) = context.args.get(1) else {
                return usage("pkg", "usage: pkg info|verify|install MANIFEST");
            };
            if context.args.len() != 2 {
                return usage("pkg", "usage: pkg info|verify|install MANIFEST");
            }
            let (manifest_bytes, manifest) = match read_manifest(context, manifest_path) {
                Ok(manifest) => manifest,
                Err(output) => return output,
            };
            match operation {
                "info" => info(&manifest),
                "verify" => verify(context, manifest_path, &manifest),
                "install" => install(context, manifest_path, &manifest_bytes, &manifest),
                _ => unreachable!("package operation was checked above"),
            }
        }
        _ => CommandOutput::failure(
            2,
            format!(
                "pkg: unsupported operation: {operation}; available operations are info, verify, install, list, and remove\n"
            ),
        ),
    }
}

fn read_manifest(
    context: &mut CommandContext<'_>,
    manifest_path: &str,
) -> Result<(Vec<u8>, PackageManifest), CommandOutput> {
    let bytes = context
        .fs
        .read(manifest_path)
        .map_err(|error| fs_failure("pkg", &error))?;
    let manifest =
        PackageManifest::parse(&bytes).map_err(|error| package_failure(manifest_path, &error))?;
    Ok((bytes, manifest))
}

fn info(manifest: &PackageManifest) -> CommandOutput {
    let mut stdout = String::new();
    let _ = writeln!(stdout, "{} {}", manifest.name, manifest.version);
    let _ = writeln!(stdout, "{}", manifest.description);
    let _ = writeln!(stdout, "files: {}", manifest.files.len());
    for command in &manifest.commands {
        let _ = writeln!(stdout, "command: {} -> {}", command.name, command.entry);
    }
    CommandOutput::success(stdout)
}

fn verify(
    context: &mut CommandContext<'_>,
    manifest_path: &str,
    manifest: &PackageManifest,
) -> CommandOutput {
    match read_verified_files(context, "pkg verify", manifest_path, manifest) {
        Ok(_) => CommandOutput::success(format!(
            "{}@{}: verified {} files\n",
            manifest.name,
            manifest.version,
            manifest.files.len()
        )),
        Err(output) => output,
    }
}

fn install(
    context: &mut CommandContext<'_>,
    manifest_path: &str,
    manifest_bytes: &[u8],
    manifest: &PackageManifest,
) -> CommandOutput {
    let files = match read_verified_files(context, "pkg install", manifest_path, manifest) {
        Ok(files) => files,
        Err(output) => return output,
    };
    let install_root = package_root(&manifest.name, &manifest.version);
    match context.fs.metadata(&install_root) {
        Ok(_) => {
            return CommandOutput::failure(
                1,
                format!(
                    "pkg: {}@{} is already installed\n",
                    manifest.name, manifest.version
                ),
            )
        }
        Err(FsError::NotFound(_)) => {}
        Err(error) => return fs_failure("pkg install", &error),
    }
    if let Err(error) = context.fs.make_directory(&install_root, true) {
        return fs_failure("pkg install", &error);
    }
    for (path, bytes) in files {
        let destination = format!("{install_root}/{path}");
        let Some((parent, _)) = destination.rsplit_once('/') else {
            return failed_install(context, &install_root, &FsError::InvalidPath(destination));
        };
        if let Err(error) = context.fs.make_directory(parent, true) {
            return failed_install(context, &install_root, &error);
        }
        if let Err(error) = context.fs.write(&destination, &bytes, false) {
            return failed_install(context, &install_root, &error);
        }
    }
    let installed_manifest = format!("{install_root}/manifest.json");
    if let Err(error) = context.fs.write(&installed_manifest, manifest_bytes, false) {
        return failed_install(context, &install_root, &error);
    }
    CommandOutput::success(format!(
        "installed {}@{}\n",
        manifest.name, manifest.version
    ))
}

fn read_verified_files(
    context: &mut CommandContext<'_>,
    operation: &str,
    manifest_path: &str,
    manifest: &PackageManifest,
) -> Result<Vec<(String, Vec<u8>)>, CommandOutput> {
    let base = manifest_base(manifest_path);
    let mut total_bytes = 0_u64;
    let mut files = Vec::with_capacity(manifest.files.len());
    for file in &manifest.files {
        let file_path = package_file_path(base, &file.path);
        let info = context
            .fs
            .metadata(&file_path)
            .map_err(|error| fs_failure(operation, &error))?;
        if !info.is_directory && info.size > MAX_PACKAGE_BYTES {
            return Err(package_size_failure(operation, &file.path, info.size));
        }
        total_bytes = total_bytes.saturating_add(info.size);
        if total_bytes > MAX_PACKAGE_BYTES {
            return Err(package_size_failure(operation, "package", total_bytes));
        }
        let bytes = context
            .fs
            .read(&file_path)
            .map_err(|error| fs_failure(operation, &error))?;
        if let Err(error) = manifest.verify_file(&file.path, &bytes) {
            return Err(package_failure(manifest_path, &error));
        }
        files.push((file.path.clone(), bytes));
    }
    Ok(files)
}

fn list(context: &mut CommandContext<'_>) -> CommandOutput {
    let entries = match context.fs.list(Some(PACKAGE_INSTALL_ROOT)) {
        Ok(entries) => entries,
        Err(FsError::NotFound(_)) => return CommandOutput::success(""),
        Err(error) => return fs_failure("pkg list", &error),
    };
    let mut output = CommandOutput::success("");
    for package in entries.into_iter().filter(|entry| entry.is_directory) {
        let package_path = format!("{PACKAGE_INSTALL_ROOT}/{}", package.name);
        let versions = match context.fs.list(Some(&package_path)) {
            Ok(versions) => versions,
            Err(error) => {
                output.status = 1;
                let _ = writeln!(output.stderr, "pkg list: {package_path}: {error}");
                continue;
            }
        };
        for version in versions.into_iter().filter(|entry| entry.is_directory) {
            let manifest_path = format!("{PACKAGE_INSTALL_ROOT}/{}/{}", package.name, version.name)
                + "/manifest.json";
            let bytes = match context.fs.read(&manifest_path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    output.status = 1;
                    let _ = writeln!(output.stderr, "pkg list: {manifest_path}: {error}");
                    continue;
                }
            };
            match PackageManifest::parse(&bytes) {
                Ok(manifest) => {
                    let _ = writeln!(output.stdout, "{}@{}", manifest.name, manifest.version);
                }
                Err(error) => {
                    output.status = 1;
                    let _ = writeln!(output.stderr, "pkg list: {manifest_path}: {error}");
                }
            }
        }
    }
    output
}

fn remove(context: &mut CommandContext<'_>) -> CommandOutput {
    if !(context.args.len() == 2 || context.args.len() == 3) {
        return usage("pkg", "usage: pkg remove NAME [VERSION]");
    }
    let name = &context.args[1];
    if !is_safe_component(name) {
        return usage("pkg", "package name must be one safe path component");
    }
    let version = context.args.get(2);
    if let Some(version) = version {
        if !is_safe_component(version) {
            return usage("pkg", "package version must be one safe path component");
        }
    }
    let root = version.map_or_else(
        || format!("{PACKAGE_INSTALL_ROOT}/{name}"),
        |version| format!("{PACKAGE_INSTALL_ROOT}/{name}/{version}"),
    );
    match context.fs.metadata(&root) {
        Ok(_) => {}
        Err(FsError::NotFound(_)) => {
            return CommandOutput::failure(1, format!("pkg: {root}: package not installed\n"));
        }
        Err(error) => return fs_failure("pkg remove", &error),
    }
    if let Err(error) = context.fs.remove(&root, true, false) {
        return fs_failure("pkg remove", &error);
    }
    let suffix = version.map_or_else(String::new, |version| format!("@{version}"));
    CommandOutput::success(format!("removed {name}{suffix}\n"))
}

fn failed_install(
    context: &mut CommandContext<'_>,
    install_root: &str,
    error: &FsError,
) -> CommandOutput {
    let _ = context.fs.remove(install_root, true, false);
    fs_failure("pkg install", error)
}

fn package_root(name: &str, version: &str) -> String {
    format!("{PACKAGE_INSTALL_ROOT}/{name}/{version}")
}

fn manifest_base(path: &str) -> &str {
    path.rsplit_once('/')
        .map_or(".", |(base, _)| if base.is_empty() { "/" } else { base })
}

fn package_file_path(base: &str, path: &str) -> String {
    if base == "/" {
        format!("/{path}")
    } else if base == "." {
        format!("./{path}")
    } else {
        format!("{base}/{path}")
    }
}

fn package_size_failure(operation: &str, path: &str, size: u64) -> CommandOutput {
    CommandOutput::failure(
        1,
        format!(
            "{operation}: {path}: file set is {size} bytes; Rune allows at most {MAX_PACKAGE_BYTES} bytes\n"
        ),
    )
}

fn package_failure(path: &str, error: &PackageError) -> CommandOutput {
    CommandOutput::failure(1, format!("pkg: {path}: {error}\n"))
}

fn is_safe_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '+')
        })
}
