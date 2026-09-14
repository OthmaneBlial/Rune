use std::fmt::Write as FmtWrite;

use rune_fs::FsError;

use crate::{fs_failure, usage, CommandContext, CommandOutput};

const AR_MAGIC: &[u8] = b"!<arch>\n";
const AR_HEADER_BYTES: usize = 60;
const MAX_AR_BYTES: usize = 64 * 1024 * 1024;
const MAX_AR_MEMBERS: usize = 10_000;
const MAX_AR_NAME_BYTES: usize = 15;

#[derive(Debug, Clone, Copy)]
enum ArOperation {
    Create { append: bool },
    List,
    Extract,
}

#[derive(Debug)]
struct ArArguments {
    operation: ArOperation,
    archive: String,
    members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArMember {
    name: String,
    bytes: Vec<u8>,
}

pub(super) fn ar(context: &mut CommandContext<'_>) -> CommandOutput {
    let arguments = match parse_arguments(context.args) {
        Ok(arguments) => arguments,
        Err(error) => return usage("ar", &error),
    };
    match arguments.operation {
        ArOperation::Create { append } => create(context, &arguments, append),
        ArOperation::List => list(context, &arguments),
        ArOperation::Extract => extract(context, &arguments),
    }
}

fn parse_arguments(args: &[String]) -> Result<ArArguments, String> {
    let Some(mode_argument) = args.first() else {
        return Err(
            "usage: ar [-rcs] ARCHIVE FILE ... | ar t ARCHIVE [MEMBER ...] | ar x ARCHIVE [MEMBER ...]"
                .to_string(),
        );
    };
    let mode = mode_argument.trim_start_matches('-');
    if mode.is_empty() {
        return Err("archive mode is empty".to_string());
    }
    let mut operation = None;
    let mut append = false;
    for flag in mode.chars() {
        match flag {
            'r' => operation = Some(ArOperation::Create { append: false }),
            'q' => {
                operation = Some(ArOperation::Create { append: true });
                append = true;
            }
            't' => operation = Some(ArOperation::List),
            'x' => operation = Some(ArOperation::Extract),
            'c' | 's' | 'v' => {}
            other => return Err(format!("unsupported archive mode: {other}")),
        }
    }
    let Some(mut operation) = operation else {
        return Err("archive mode must include r, q, t, or x".to_string());
    };
    if append {
        operation = ArOperation::Create { append: true };
    }
    let Some(archive) = args.get(1) else {
        return Err("archive path is missing".to_string());
    };
    let members = args[2..].to_vec();
    match operation {
        ArOperation::Create { .. } if members.is_empty() => {
            Err("creation requires at least one member file".to_string())
        }
        _ => Ok(ArArguments {
            operation,
            archive: archive.clone(),
            members,
        }),
    }
}

fn create(
    context: &mut CommandContext<'_>,
    arguments: &ArArguments,
    append: bool,
) -> CommandOutput {
    let mut archive_members = match context.fs.read(&arguments.archive) {
        Ok(bytes) => match parse_archive(&bytes) {
            Ok(members) => members,
            Err(error) => return ar_failure(&arguments.archive, &error),
        },
        Err(FsError::NotFound(_)) => Vec::new(),
        Err(error) => return fs_failure("ar", &error),
    };
    for path in &arguments.members {
        if let Some(output) = context.take_cancellation() {
            return output;
        }
        let info = match context.fs.metadata(path) {
            Ok(info) => info,
            Err(error) => return fs_failure("ar", &error),
        };
        if info.is_directory || info.is_symlink {
            return ar_failure(path, "only regular files can be archive members");
        }
        let name = match member_name(path) {
            Ok(name) => name,
            Err(error) => return ar_failure(path, error),
        };
        let name = match validate_member_name(&name) {
            Ok(name) => name,
            Err(error) => return ar_failure(path, &error),
        };
        let bytes = match context.fs.read(path) {
            Ok(bytes) => bytes,
            Err(error) => return fs_failure("ar", &error),
        };
        if !append {
            archive_members.retain(|member| member.name != name);
        } else if archive_members.iter().any(|member| member.name == name) {
            return ar_failure(path, "quick append would create a duplicate member");
        }
        archive_members.push(ArMember { name, bytes });
        if archive_members.len() > MAX_AR_MEMBERS {
            return ar_failure("archive", "member count exceeds the 10,000-entry limit");
        }
    }
    let bytes = match build_archive(&archive_members) {
        Ok(bytes) => bytes,
        Err(error) => return ar_failure(&arguments.archive, &error),
    };
    if let Err(error) = context.fs.write(&arguments.archive, &bytes, false) {
        return fs_failure("ar", &error);
    }
    CommandOutput::success(format!(
        "created {} ({} members)\n",
        arguments.archive,
        archive_members.len()
    ))
}

fn list(context: &mut CommandContext<'_>, arguments: &ArArguments) -> CommandOutput {
    let bytes = match context.fs.read(&arguments.archive) {
        Ok(bytes) => bytes,
        Err(error) => return fs_failure("ar", &error),
    };
    let members = match parse_archive(&bytes) {
        Ok(members) => members,
        Err(error) => return ar_failure(&arguments.archive, &error),
    };
    let selected = if arguments.members.is_empty() {
        members.iter().collect::<Vec<_>>()
    } else {
        let mut selected = Vec::with_capacity(arguments.members.len());
        for name in &arguments.members {
            let Some(member) = members.iter().find(|member| member.name == *name) else {
                return ar_failure(name, "member was not found");
            };
            selected.push(member);
        }
        selected
    };
    let mut output = String::new();
    for member in selected {
        if let Some(output) = context.take_cancellation() {
            return output;
        }
        let _ = writeln!(output, "{}", member.name);
    }
    CommandOutput::success(output)
}

fn extract(context: &mut CommandContext<'_>, arguments: &ArArguments) -> CommandOutput {
    let bytes = match context.fs.read(&arguments.archive) {
        Ok(bytes) => bytes,
        Err(error) => return fs_failure("ar", &error),
    };
    let members = match parse_archive(&bytes) {
        Ok(members) => members,
        Err(error) => return ar_failure(&arguments.archive, &error),
    };
    let selected = if arguments.members.is_empty() {
        members.iter().collect::<Vec<_>>()
    } else {
        let mut selected = Vec::with_capacity(arguments.members.len());
        for name in &arguments.members {
            let Some(member) = members.iter().find(|member| member.name == *name) else {
                return ar_failure(name, "member was not found");
            };
            selected.push(member);
        }
        selected
    };
    for member in &selected {
        if let Some(output) = context.take_cancellation() {
            return output;
        }
        if context.fs.metadata(&member.name).is_ok() {
            return ar_failure(&member.name, "destination already exists");
        }
    }
    for member in &selected {
        if let Some(output) = context.take_cancellation() {
            return output;
        }
        if let Err(error) = context.fs.write(&member.name, &member.bytes, false) {
            return fs_failure("ar", &error);
        }
    }
    CommandOutput::success(format!("extracted {} members\n", selected.len()))
}

fn member_name(path: &str) -> Result<String, &'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.is_empty() || name == "." || name == ".." {
        return Err("member path has no safe filename");
    }
    Ok(name.to_string())
}

fn validate_member_name(name: &str) -> Result<String, String> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.len() > MAX_AR_NAME_BYTES
    {
        return Err(format!(
            "member name must be a filename of at most {MAX_AR_NAME_BYTES} bytes"
        ));
    }
    Ok(name.to_string())
}

fn build_archive(members: &[ArMember]) -> Result<Vec<u8>, String> {
    let mut output = Vec::with_capacity(AR_MAGIC.len());
    output.extend_from_slice(AR_MAGIC);
    for member in members {
        validate_member_name(&member.name)?;
        let size = member.bytes.len();
        let padding = size % 2;
        let next_size = output
            .len()
            .saturating_add(AR_HEADER_BYTES)
            .saturating_add(size)
            .saturating_add(padding);
        if next_size > MAX_AR_BYTES {
            return Err("archive exceeds the 64 MiB limit".to_string());
        }
        let mut header = [b' '; AR_HEADER_BYTES];
        write_ar_field(&mut header[0..16], &member.name, false)?;
        write_ar_field(&mut header[16..28], "0", true)?;
        write_ar_field(&mut header[28..34], "0", true)?;
        write_ar_field(&mut header[34..40], "0", true)?;
        write_ar_field(&mut header[40..48], "100644", true)?;
        write_ar_field(&mut header[48..58], &size.to_string(), true)?;
        header[58..60].copy_from_slice(b"`\n");
        output.extend_from_slice(&header);
        output.extend_from_slice(&member.bytes);
        if padding != 0 {
            output.push(b'\n');
        }
    }
    Ok(output)
}

fn write_ar_field(field: &mut [u8], value: &str, right_aligned: bool) -> Result<(), String> {
    if value.len() > field.len() {
        return Err("archive header field is too long".to_string());
    }
    let start = if right_aligned {
        field.len() - value.len()
    } else {
        0
    };
    field[start..start + value.len()].copy_from_slice(value.as_bytes());
    Ok(())
}

fn parse_archive(bytes: &[u8]) -> Result<Vec<ArMember>, String> {
    if bytes.len() > MAX_AR_BYTES {
        return Err("archive exceeds the 64 MiB limit".to_string());
    }
    if !bytes.starts_with(AR_MAGIC) {
        return Err("archive does not use the ar format".to_string());
    }
    let mut offset = AR_MAGIC.len();
    let mut members = Vec::new();
    while offset < bytes.len() {
        if bytes.len() - offset < AR_HEADER_BYTES {
            return Err("archive contains a truncated member header".to_string());
        }
        let header = &bytes[offset..offset + AR_HEADER_BYTES];
        if &header[58..60] != b"`\n" {
            return Err("archive member has an invalid header marker".to_string());
        }
        let raw_name = trim_ascii(&header[0..16]);
        if raw_name == "/" || raw_name == "//" || raw_name.starts_with('/') {
            return Err("symbol tables and long-name tables are unsupported".to_string());
        }
        let name = raw_name.strip_suffix('/').unwrap_or(raw_name);
        let name = validate_member_name(name)?;
        let size = parse_decimal_field(&header[48..58])?;
        let data_start = offset + AR_HEADER_BYTES;
        let data_end = data_start
            .checked_add(size)
            .ok_or_else(|| "archive member size overflows".to_string())?;
        if data_end > bytes.len() {
            return Err("archive contains a truncated member payload".to_string());
        }
        if members.iter().any(|member: &ArMember| member.name == name) {
            return Err(format!("archive contains duplicate member: {name}"));
        }
        members.push(ArMember {
            name,
            bytes: bytes[data_start..data_end].to_vec(),
        });
        if members.len() > MAX_AR_MEMBERS {
            return Err("archive contains more than 10,000 members".to_string());
        }
        if size % 2 != 0 && bytes.get(data_end) != Some(&b'\n') {
            return Err("archive member has invalid padding".to_string());
        }
        offset = data_end + (size % 2);
        if offset > bytes.len() {
            return Err("archive is missing member padding".to_string());
        }
    }
    Ok(members)
}

fn trim_ascii(field: &[u8]) -> &str {
    let end = field
        .iter()
        .rposition(|byte| *byte != b' ')
        .map_or(0, |index| index + 1);
    let field = &field[..end];
    std::str::from_utf8(field).unwrap_or_default()
}

fn parse_decimal_field(field: &[u8]) -> Result<usize, String> {
    let value = trim_ascii(field).trim();
    if value.is_empty() {
        return Ok(0);
    }
    value
        .parse::<usize>()
        .map_err(|_| "archive member size is not decimal".to_string())
}

fn ar_failure(path: &str, message: &str) -> CommandOutput {
    CommandOutput::failure(1, format!("ar: {path}: {message}\n"))
}

#[cfg(test)]
mod tests {
    use super::{build_archive, parse_archive, ArMember};

    #[test]
    fn round_trips_bounded_ar_members() {
        let members = vec![
            ArMember {
                name: "one.o".to_string(),
                bytes: b"object-one".to_vec(),
            },
            ArMember {
                name: "two.o".to_string(),
                bytes: b"object-two!".to_vec(),
            },
        ];
        let archive = build_archive(&members).expect("archive built");
        assert_eq!(parse_archive(&archive).expect("archive parsed"), members);
    }
}
