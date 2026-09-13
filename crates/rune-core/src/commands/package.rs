use std::fmt::Write as _;

use crate::{fs_failure, usage, CommandContext, CommandOutput, PACKAGE_INSTALL_ROOT};
use rune_fs::FsError;
use rune_package::{PackageError, PackageManifest};

const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SEARCH_QUERY_CHARS: usize = 64;
const MAX_SEARCH_MANIFESTS: usize = 4_096;

pub(super) fn pkg(context: &mut CommandContext<'_>) -> CommandOutput {
    let Some(operation) = context.args.first().map(String::as_str) else {
        return usage(
            "pkg",
            "usage: pkg info MANIFEST|NAME [VERSION]; pkg verify|install MANIFEST; pkg list|search QUERY; pkg remove NAME [VERSION]",
        );
    };
    match operation {
        "list" => {
            if context.args.len() != 1 {
                return usage("pkg", "usage: pkg list");
            }
            list(context)
        }
        "search" => {
            if context.args.len() != 2 {
                return usage("pkg", "usage: pkg search QUERY");
            }
            search(context, &context.args[1])
        }
        "remove" => remove(context),
        "info" => info_command(context),
        "verify" | "install" => {
            let Some(manifest_path) = context.args.get(1) else {
                return usage("pkg", "usage: pkg verify|install MANIFEST");
            };
            if context.args.len() != 2 {
                return usage("pkg", "usage: pkg verify|install MANIFEST");
            }
            let (manifest_bytes, manifest) = match read_manifest(context, manifest_path) {
                Ok(manifest) => manifest,
                Err(output) => return output,
            };
            match operation {
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

fn info_command(context: &mut CommandContext<'_>) -> CommandOutput {
    if !(context.args.len() == 2 || context.args.len() == 3) {
        return usage("pkg", "usage: pkg info MANIFEST|NAME [VERSION]");
    }
    let manifest_path = if context.args.len() == 3 {
        let name = &context.args[1];
        let version = &context.args[2];
        if !is_safe_component(name) {
            return usage("pkg", "package name must be one safe path component");
        }
        if !is_safe_component(version) {
            return usage("pkg", "package version must be one safe path component");
        }
        format!("{PACKAGE_INSTALL_ROOT}/{name}/{version}/manifest.json")
    } else {
        let candidate = &context.args[1];
        let looks_like_path = candidate.contains('/')
            || std::path::Path::new(candidate)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
        if looks_like_path || context.fs.metadata(candidate).is_ok() {
            candidate.clone()
        } else {
            match installed_manifest_for_name(context, candidate) {
                Ok(path) => path,
                Err(output) => return output,
            }
        }
    };
    let (_, manifest) = match read_manifest(context, &manifest_path) {
        Ok(manifest) => manifest,
        Err(output) => return output,
    };
    info(&manifest)
}

fn installed_manifest_for_name(
    context: &mut CommandContext<'_>,
    name: &str,
) -> Result<String, CommandOutput> {
    if !is_safe_component(name) {
        return Err(usage(
            "pkg",
            "package name must be one safe path component or a manifest path",
        ));
    }
    let package_path = format!("{PACKAGE_INSTALL_ROOT}/{name}");
    let entries = match context.fs.list(Some(&package_path)) {
        Ok(entries) => entries,
        Err(FsError::NotFound(_)) => {
            return Err(CommandOutput::failure(
                1,
                format!("pkg: {name}: package is not installed\n"),
            ))
        }
        Err(error) => return Err(fs_failure("pkg info", &error)),
    };
    let versions = entries
        .into_iter()
        .filter(|entry| entry.is_directory)
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    match versions.as_slice() {
        [] => Err(CommandOutput::failure(
            1,
            format!("pkg: {name}: package is not installed\n"),
        )),
        [version] => Ok(format!(
            "{PACKAGE_INSTALL_ROOT}/{name}/{version}/manifest.json"
        )),
        _ => Err(CommandOutput::failure(
            2,
            format!("pkg: {name}: multiple versions are installed; specify VERSION\n"),
        )),
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

fn search(context: &mut CommandContext<'_>, query: &str) -> CommandOutput {
    if query.is_empty() || query.chars().count() > MAX_SEARCH_QUERY_CHARS {
        return usage("pkg", "search query must contain 1-64 characters");
    }
    let needle = query.to_lowercase();
    let entries = match context.fs.list(Some(PACKAGE_INSTALL_ROOT)) {
        Ok(entries) => entries,
        Err(FsError::NotFound(_)) => return CommandOutput::success(""),
        Err(error) => return fs_failure("pkg search", &error),
    };
    let mut output = CommandOutput::success("");
    let mut results = Vec::new();
    let mut inspected = 0;
    'packages: for package in entries.into_iter().filter(|entry| entry.is_directory) {
        let package_path = format!("{PACKAGE_INSTALL_ROOT}/{}", package.name);
        let versions = match context.fs.list(Some(&package_path)) {
            Ok(versions) => versions,
            Err(error) => {
                output.status = 1;
                let _ = writeln!(output.stderr, "pkg search: {package_path}: {error}");
                continue;
            }
        };
        for version in versions.into_iter().filter(|entry| entry.is_directory) {
            inspected += 1;
            if inspected > MAX_SEARCH_MANIFESTS {
                output.status = 1;
                let _ = writeln!(
                    output.stderr,
                    "pkg search: stopped after {MAX_SEARCH_MANIFESTS} installed manifests"
                );
                break 'packages;
            }
            let manifest_path = format!(
                "{PACKAGE_INSTALL_ROOT}/{}/{}/manifest.json",
                package.name, version.name
            );
            let bytes = match context.fs.read(&manifest_path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    output.status = 1;
                    let _ = writeln!(output.stderr, "pkg search: {manifest_path}: {error}");
                    continue;
                }
            };
            let manifest = match PackageManifest::parse(&bytes) {
                Ok(manifest) => manifest,
                Err(error) => {
                    output.status = 1;
                    let _ = writeln!(output.stderr, "pkg search: {manifest_path}: {error}");
                    continue;
                }
            };
            let is_match = [
                manifest.name.to_lowercase(),
                manifest.version.to_lowercase(),
                manifest.description.to_lowercase(),
            ]
            .into_iter()
            .any(|field| field.contains(&needle))
                || manifest
                    .commands
                    .iter()
                    .any(|command| command.name.to_lowercase().contains(&needle));
            if is_match {
                results.push(format!(
                    "{}@{}\t{}",
                    manifest.name, manifest.version, manifest.description
                ));
            }
        }
    }
    results.sort();
    for result in results {
        let _ = writeln!(output.stdout, "{result}");
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
