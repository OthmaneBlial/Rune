use std::fmt::Write as _;

use crate::{fs_failure, usage, CommandContext, CommandOutput};
use rune_package::{PackageError, PackageManifest};

pub(super) fn pkg(context: &mut CommandContext<'_>) -> CommandOutput {
    let Some(operation) = context.args.first().map(String::as_str) else {
        return usage("pkg", "usage: pkg info|verify MANIFEST");
    };
    let Some(manifest_path) = context.args.get(1) else {
        return usage("pkg", "usage: pkg info|verify MANIFEST");
    };
    if context.args.len() != 2 {
        return usage("pkg", "usage: pkg info|verify MANIFEST");
    }
    if !matches!(operation, "info" | "verify") {
        return CommandOutput::failure(
            2,
            format!(
                "pkg: unsupported operation: {operation}; only info and verify are available\n"
            ),
        );
    }

    let manifest_bytes = match context.fs.read(manifest_path) {
        Ok(bytes) => bytes,
        Err(error) => return fs_failure("pkg", &error),
    };
    let manifest = match PackageManifest::parse(&manifest_bytes) {
        Ok(manifest) => manifest,
        Err(error) => return package_failure(manifest_path, &error),
    };

    match operation {
        "info" => info(&manifest),
        "verify" => verify(context, manifest_path, &manifest),
        _ => unreachable!("unsupported pkg operation was checked above"),
    }
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
    let base = manifest_base(manifest_path);
    for file in &manifest.files {
        let file_path = package_file_path(base, &file.path);
        let bytes = match context.fs.read(&file_path) {
            Ok(bytes) => bytes,
            Err(error) => return fs_failure("pkg verify", &error),
        };
        if let Err(error) = manifest.verify_file(&file.path, &bytes) {
            return package_failure(manifest_path, &error);
        }
    }
    CommandOutput::success(format!(
        "{}@{}: verified {} files\n",
        manifest.name,
        manifest.version,
        manifest.files.len()
    ))
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

fn package_failure(path: &str, error: &PackageError) -> CommandOutput {
    CommandOutput::failure(1, format!("pkg: {path}: {error}\n"))
}
