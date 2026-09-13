use std::fmt::Write as _;

use crate::{fs_failure, usage, CommandContext, CommandOutput};

const MAX_HEXDUMP_INPUT: usize = 256 * 1024;
const MAX_DISK_USAGE_ENTRIES: usize = 10_000;

pub(super) fn basename(context: &mut CommandContext<'_>) -> CommandOutput {
    if !(1..=2).contains(&context.args.len()) || context.args[0].is_empty() {
        return usage("basename", "usage: basename PATH [SUFFIX]");
    }
    let mut name = basename_value(&context.args[0]).to_string();
    if let Some(suffix) = context.args.get(1) {
        if !suffix.is_empty() && name != *suffix && name.ends_with(suffix) {
            name.truncate(name.len() - suffix.len());
        }
    }
    CommandOutput::success(format!("{name}\n"))
}

pub(super) fn dirname(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.len() != 1 || context.args[0].is_empty() {
        return usage("dirname", "usage: dirname PATH");
    }
    CommandOutput::success(format!("{}\n", dirname_value(&context.args[0])))
}

pub(super) fn du(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.len() > 1 {
        return usage("du", "usage: du [PATH]");
    }
    let path = context.args.first().map_or("~", String::as_str);
    let mut visited = 0;
    match disk_usage(context, path, &mut visited) {
        Ok(bytes) => CommandOutput::success(format!("{bytes}\t{path}\n")),
        Err(output) => output,
    }
}

pub(super) fn rmdir(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.is_empty() {
        return usage("rmdir", "usage: rmdir DIRECTORY ...");
    }
    for path in context.args {
        let info = match context.fs.metadata(path) {
            Ok(info) => info,
            Err(error) => return fs_failure("rmdir", &error),
        };
        if !info.is_directory {
            return fs_failure("rmdir", &rune_fs::FsError::NotDirectory(path.clone()));
        }
        if let Err(error) = context.fs.remove(path, false, false) {
            return fs_failure("rmdir", &error);
        }
    }
    CommandOutput::success("")
}

pub(super) fn stat(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.len() != 1 {
        return usage("stat", "usage: stat PATH");
    }
    let path = &context.args[0];
    let info = match context.fs.metadata(path) {
        Ok(info) => info,
        Err(error) => return fs_failure("stat", &error),
    };
    let kind = if info.is_symlink {
        "symlink"
    } else if info.is_directory {
        "directory"
    } else {
        "file"
    };
    CommandOutput::success(format!(
        "  File: {path}\n  Name: {}\n  Size: {}\n  Type: {kind}\n",
        info.name, info.size
    ))
}

pub(super) fn tee(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut append = false;
    let mut paths = Vec::new();
    for argument in context.args {
        match argument.as_str() {
            "-a" => append = true,
            "--" => {}
            _ if argument.starts_with('-') => {
                return usage("tee", "usage: tee [-a] [FILE ...]");
            }
            _ => paths.push(argument),
        }
    }
    for path in paths {
        if let Err(error) = context.fs.write(path, context.stdin.as_bytes(), append) {
            return fs_failure("tee", &error);
        }
    }
    CommandOutput::success(context.stdin)
}

pub(super) fn tr(context: &mut CommandContext<'_>) -> CommandOutput {
    let (delete, set_one, set_two) = match context.args {
        [flag, set_one] if flag == "-d" => (true, set_one.as_str(), None),
        [set_one, set_two] => (false, set_one.as_str(), Some(set_two.as_str())),
        _ => return usage("tr", "usage: tr [-d] SET1 [SET2]"),
    };
    if set_one.is_empty() || (!delete && set_two.is_some_and(str::is_empty)) {
        return usage("tr", "SET1 and SET2 must not be empty");
    }
    let source = set_one.chars().collect::<Vec<_>>();
    let target = set_two.map(|value| value.chars().collect::<Vec<_>>());
    let mut stdout = String::with_capacity(context.stdin.len());
    for character in context.stdin.chars() {
        let Some(index) = source.iter().position(|candidate| *candidate == character) else {
            stdout.push(character);
            continue;
        };
        if delete {
            continue;
        }
        let target = target.as_ref().expect("translation has a target");
        stdout.push(target[index.min(target.len() - 1)]);
    }
    CommandOutput::success(stdout)
}

pub(super) fn unlink(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.is_empty() {
        return usage("unlink", "usage: unlink FILE");
    }
    if context.args.len() != 1 {
        return usage("unlink", "usage: unlink FILE");
    }
    let path = &context.args[0];
    match context.fs.metadata(path) {
        Ok(info) if info.is_directory => {
            fs_failure("unlink", &rune_fs::FsError::NotFile(path.clone()))
        }
        Ok(_) => context.fs.remove(path, false, false).map_or_else(
            |error| fs_failure("unlink", &error),
            |()| CommandOutput::success(""),
        ),
        Err(error) => fs_failure("unlink", &error),
    }
}

pub(super) fn xxd(context: &mut CommandContext<'_>) -> CommandOutput {
    let (plain, path) = match context.args {
        [] => (false, None),
        [flag] if flag == "-p" => (true, None),
        [path] => (false, Some(path.as_str())),
        [flag, path] if flag == "-p" => (true, Some(path.as_str())),
        _ => return usage("xxd", "usage: xxd [-p] [FILE]"),
    };
    let bytes = match path {
        Some(path) => match context.fs.read(path) {
            Ok(bytes) => bytes,
            Err(error) => return fs_failure("xxd", &error),
        },
        None => context.stdin.as_bytes().to_vec(),
    };
    if bytes.len() > MAX_HEXDUMP_INPUT {
        return CommandOutput::failure(
            1,
            format!("xxd: input exceeds {MAX_HEXDUMP_INPUT} bytes\n"),
        );
    }
    if plain {
        return CommandOutput::success(plain_hex(&bytes));
    }
    CommandOutput::success(classic_hex(&bytes))
}

fn basename_value(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/"
    } else {
        trimmed.rsplit('/').next().unwrap_or(trimmed)
    }
}

fn dirname_value(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/";
    }
    match trimmed.rfind('/') {
        None => ".",
        Some(0) => "/",
        Some(index) => &trimmed[..index],
    }
}

fn disk_usage(
    context: &mut CommandContext<'_>,
    path: &str,
    visited: &mut usize,
) -> Result<u64, CommandOutput> {
    if *visited >= MAX_DISK_USAGE_ENTRIES {
        return Err(CommandOutput::failure(
            1,
            format!("du: traversal exceeded {MAX_DISK_USAGE_ENTRIES} entries\n"),
        ));
    }
    let info = context
        .fs
        .metadata(path)
        .map_err(|error| fs_failure("du", &error))?;
    *visited += 1;
    if !info.is_directory || info.is_symlink {
        return Ok(info.size);
    }
    let entries = context
        .fs
        .list(Some(path))
        .map_err(|error| fs_failure("du", &error))?;
    let mut total = 0_u64;
    for entry in entries {
        let child = if path == "/" {
            format!("/{name}", name = entry.name)
        } else if path.ends_with('/') {
            format!("{path}{name}", name = entry.name)
        } else {
            format!("{path}/{name}", name = entry.name)
        };
        total = total.saturating_add(disk_usage(context, &child, visited)?);
    }
    Ok(total)
}

fn plain_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2 + 1);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output.push('\n');
    output
}

fn classic_hex(bytes: &[u8]) -> String {
    let mut output = String::new();
    for (line, chunk) in bytes.chunks(16).enumerate() {
        let _ = write!(output, "{line:08x}: ");
        for index in 0..16 {
            if let Some(byte) = chunk.get(index) {
                let _ = write!(output, "{byte:02x} ");
            } else {
                output.push_str("   ");
            }
            if index == 7 {
                output.push(' ');
            }
        }
        output.push(' ');
        for byte in chunk {
            output.push(if byte.is_ascii_graphic() || *byte == b' ' {
                char::from(*byte)
            } else {
                '.'
            });
        }
        output.push('\n');
    }
    output
}
