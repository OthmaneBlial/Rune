use std::fmt::Write as _;
use std::io::{Read, Write};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::{fs_failure, usage, CommandContext, CommandOutput};

const MAX_GZIP_BYTES: usize = 64 * 1024 * 1024;

/// Compresses VFS files to sibling `.gz` files without deleting the source.
pub(super) fn gzip(context: &mut CommandContext<'_>) -> CommandOutput {
    run(context, false, "gzip")
}

/// Decompresses VFS `.gz` files to sibling files without deleting the source.
pub(super) fn gunzip(context: &mut CommandContext<'_>) -> CommandOutput {
    run(context, true, "gunzip")
}

fn run(context: &mut CommandContext<'_>, mut decompress: bool, command: &str) -> CommandOutput {
    let mut paths = Vec::new();
    let mut options_done = false;
    for argument in context.args {
        if !options_done && argument == "--" {
            options_done = true;
            continue;
        }
        if !options_done && command == "gzip" && matches!(argument.as_str(), "-d" | "--decompress")
        {
            decompress = true;
            continue;
        }
        if !options_done && argument.starts_with('-') {
            return usage(
                command,
                if decompress {
                    "usage: gunzip [--] FILE ..."
                } else {
                    "usage: gzip [-d|--decompress] [--] FILE ..."
                },
            );
        }
        paths.push(argument.as_str());
    }
    if paths.is_empty() {
        return usage(
            command,
            if decompress {
                "usage: gunzip [--] FILE ..."
            } else {
                "usage: gzip [-d|--decompress] [--] FILE ..."
            },
        );
    }

    let mut output = String::new();
    for path in paths {
        if let Some(cancelled) = context.take_cancellation() {
            return cancelled;
        }
        let destination = match destination_path(path, decompress) {
            Ok(destination) => destination,
            Err(error) => return compression_failure(command, path, error),
        };
        if context.fs.metadata(&destination).is_ok() {
            return compression_failure(command, &destination, "destination already exists");
        }
        let input = match context.fs.read(path) {
            Ok(input) => input,
            Err(error) => return fs_failure(command, &error),
        };
        if input.len() > MAX_GZIP_BYTES {
            return compression_failure(command, path, "input exceeds the 64 MiB limit");
        }
        let bytes = if decompress {
            match decompress_gzip(&input) {
                Ok(bytes) => bytes,
                Err(error) => return compression_failure(command, path, &error),
            }
        } else {
            match compress_gzip(&input) {
                Ok(bytes) => bytes,
                Err(error) => return compression_failure(command, path, &error),
            }
        };
        if let Err(error) = context.fs.write(&destination, &bytes, false) {
            return fs_failure(command, &error);
        }
        let verb = if decompress {
            "decompressed"
        } else {
            "compressed"
        };
        let _ = writeln!(output, "{verb} {path} -> {destination}");
    }
    CommandOutput::success(output)
}

fn destination_path(path: &str, decompress: bool) -> Result<String, &'static str> {
    if decompress {
        let Some(destination) = path.strip_suffix(".gz") else {
            return Err("input must end in .gz");
        };
        if destination.is_empty() {
            return Err("input must include a filename before .gz");
        }
        Ok(destination.to_string())
    } else {
        Ok(format!("{path}.gz"))
    }
}

fn compress_gzip(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(input)
        .map_err(|error| format!("compression failed: {error}"))?;
    let output = encoder
        .finish()
        .map_err(|error| format!("compression failed: {error}"))?;
    if output.len() > MAX_GZIP_BYTES {
        return Err("compressed output exceeds the 64 MiB limit".to_string());
    }
    Ok(output)
}

fn decompress_gzip(input: &[u8]) -> Result<Vec<u8>, String> {
    let decoder = GzDecoder::new(input);
    let mut bounded = decoder.take((MAX_GZIP_BYTES + 1) as u64);
    let mut output = Vec::new();
    bounded
        .read_to_end(&mut output)
        .map_err(|error| format!("decompression failed: {error}"))?;
    if output.len() > MAX_GZIP_BYTES {
        return Err("decompressed output exceeds the 64 MiB limit".to_string());
    }
    Ok(output)
}

fn compression_failure(command: &str, path: &str, message: &str) -> CommandOutput {
    CommandOutput::failure(1, format!("{command}: {path}: {message}\n"))
}
