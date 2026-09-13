use std::collections::BTreeSet;

use crate::{fs_failure, usage, CommandContext, CommandOutput};

const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const MAX_ARCHIVE_PATH_BYTES: usize = 1_024;

#[derive(Debug)]
struct ArchiveEntry {
    name: String,
    bytes: Vec<u8>,
    directory: bool,
}

pub(super) fn zip(context: &mut CommandContext<'_>) -> CommandOutput {
    let mut recursive = false;
    let mut positional = Vec::new();
    for argument in context.args {
        match argument.as_str() {
            "-r" | "--recurse-paths" => recursive = true,
            "--" => {}
            _ if argument.starts_with('-') => {
                return usage("zip", "usage: zip [-r] ARCHIVE FILE ...");
            }
            _ => positional.push(argument.as_str()),
        }
    }
    if positional.len() < 2 {
        return usage("zip", "usage: zip [-r] ARCHIVE FILE ...");
    }
    let archive_path = positional[0];
    let mut entries = Vec::new();
    for path in &positional[1..] {
        let name = archive_name(path);
        let name = if name == "." { String::new() } else { name };
        if let Err(output) = collect_entries(context, path, &name, recursive, &mut entries) {
            return output;
        }
    }
    let archive = match build_archive(&entries) {
        Ok(archive) => archive,
        Err(error) => return archive_failure(&error),
    };
    if let Err(error) = context.fs.write(archive_path, &archive, false) {
        return fs_failure("zip", &error);
    }
    CommandOutput::success(format!(
        "created {archive_path} ({} entries)\n",
        entries.len()
    ))
}

pub(super) fn unzip(context: &mut CommandContext<'_>) -> CommandOutput {
    if !(1..=2).contains(&context.args.len()) {
        return usage("unzip", "usage: unzip ARCHIVE [DESTINATION]");
    }
    let archive_path = &context.args[0];
    let destination = context.args.get(1).map_or(".", String::as_str);
    let archive = match context.fs.read(archive_path) {
        Ok(bytes) => bytes,
        Err(error) => return fs_failure("unzip", &error),
    };
    if archive.len() > MAX_ARCHIVE_BYTES {
        return archive_failure("archive exceeds the 64 MiB limit");
    }
    let entries = match read_central_directory(&archive) {
        Ok(entries) => entries,
        Err(error) => return archive_failure(&error),
    };
    if let Err(error) = context.fs.make_directory(destination, true) {
        return fs_failure("unzip", &error);
    }
    let entry_count = entries.len();
    for entry in entries {
        if let Some(output) = context.take_cancellation() {
            return output;
        }
        let output_path = append_path(destination, &entry.name);
        if entry.directory {
            if let Err(error) = context.fs.make_directory(&output_path, true) {
                return fs_failure("unzip", &error);
            }
            continue;
        }
        let parent =
            output_path.rsplit_once('/').map_or(
                ".",
                |(parent, _)| if parent.is_empty() { "/" } else { parent },
            );
        if let Err(error) = context.fs.make_directory(parent, true) {
            return fs_failure("unzip", &error);
        }
        if let Err(error) = context.fs.write(&output_path, &entry.bytes, false) {
            return fs_failure("unzip", &error);
        }
    }
    CommandOutput::success(format!(
        "extracted {entry_count} entries into {destination}\n"
    ))
}

fn collect_entries(
    context: &mut CommandContext<'_>,
    source_path: &str,
    archive_name: &str,
    recursive: bool,
    entries: &mut Vec<ArchiveEntry>,
) -> Result<(), CommandOutput> {
    if let Some(output) = context.take_cancellation() {
        return Err(output);
    }
    if entries.len() >= MAX_ARCHIVE_ENTRIES {
        return Err(archive_failure("entry limit exceeded"));
    }
    if !archive_name.is_empty() {
        validate_archive_name(archive_name).map_err(|error| archive_failure(&error))?;
    }
    let info = context
        .fs
        .metadata(source_path)
        .map_err(|error| fs_failure("zip", &error))?;
    if info.is_symlink {
        return Err(archive_failure("symbolic links are not archived"));
    }
    if info.is_directory {
        if !recursive {
            return Err(archive_failure(&format!(
                "{source_path} is a directory; use -r to recurse"
            )));
        }
        if !archive_name.is_empty() {
            entries.push(ArchiveEntry {
                name: format!("{}/", archive_name.trim_end_matches('/')),
                bytes: Vec::new(),
                directory: true,
            });
        }
        let children = context
            .fs
            .list(Some(source_path))
            .map_err(|error| fs_failure("zip", &error))?;
        for child in children {
            let child_source = append_path(source_path, &child.name);
            let child_name = if archive_name.is_empty() {
                child.name.clone()
            } else {
                append_path(archive_name.trim_end_matches('/'), &child.name)
            };
            collect_entries(context, &child_source, &child_name, recursive, entries)?;
        }
        return Ok(());
    }
    let bytes = context
        .fs
        .read(source_path)
        .map_err(|error| fs_failure("zip", &error))?;
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(archive_failure("archive payload exceeds the 64 MiB limit"));
    }
    entries.push(ArchiveEntry {
        name: archive_name.to_string(),
        bytes,
        directory: false,
    });
    let total = entries.iter().map(|entry| entry.bytes.len()).sum::<usize>();
    if total > MAX_ARCHIVE_BYTES {
        return Err(archive_failure("archive payload exceeds the 64 MiB limit"));
    }
    Ok(())
}

fn build_archive(entries: &[ArchiveEntry]) -> Result<Vec<u8>, String> {
    if entries.len() > MAX_ARCHIVE_ENTRIES {
        return Err("entry limit exceeded".to_string());
    }
    let mut output = Vec::new();
    let mut central = Vec::new();
    for entry in entries {
        validate_archive_name(&entry.name)?;
        let name = entry.name.as_bytes();
        let name_length = u16::try_from(name.len()).map_err(|_| "file name is too long")?;
        let size = u32::try_from(entry.bytes.len()).map_err(|_| "entry is too large")?;
        let offset = u32::try_from(output.len()).map_err(|_| "archive is too large")?;
        let crc = crc32(&entry.bytes);
        push_u32(&mut output, 0x0403_4b50);
        push_u16(&mut output, 20);
        push_u16(&mut output, 0);
        push_u16(&mut output, 0);
        push_u16(&mut output, 0);
        push_u16(&mut output, 0);
        push_u32(&mut output, crc);
        push_u32(&mut output, size);
        push_u32(&mut output, size);
        push_u16(&mut output, name_length);
        push_u16(&mut output, 0);
        output.extend_from_slice(name);
        output.extend_from_slice(&entry.bytes);

        push_u32(&mut central, 0x0201_4b50);
        push_u16(&mut central, 20);
        push_u16(&mut central, 20);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u32(&mut central, crc);
        push_u32(&mut central, size);
        push_u32(&mut central, size);
        push_u16(&mut central, name_length);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u32(&mut central, if entry.directory { 0x10 } else { 0 });
        push_u32(&mut central, offset);
        central.extend_from_slice(name);
    }
    let central_offset = u32::try_from(output.len()).map_err(|_| "archive is too large")?;
    let central_size = u32::try_from(central.len()).map_err(|_| "archive is too large")?;
    output.extend_from_slice(&central);
    let count = u16::try_from(entries.len()).map_err(|_| "too many archive entries")?;
    push_u32(&mut output, 0x0605_4b50);
    push_u16(&mut output, 0);
    push_u16(&mut output, 0);
    push_u16(&mut output, count);
    push_u16(&mut output, count);
    push_u32(&mut output, central_size);
    push_u32(&mut output, central_offset);
    push_u16(&mut output, 0);
    if output.len() > MAX_ARCHIVE_BYTES {
        return Err("archive exceeds the 64 MiB limit".to_string());
    }
    Ok(output)
}

fn read_central_directory(archive: &[u8]) -> Result<Vec<ArchiveEntry>, String> {
    let eocd = find_end_of_central_directory(archive).ok_or("end record is missing")?;
    if eocd + 22 > archive.len() {
        return Err("end record is truncated".to_string());
    }
    let disk = read_u16(archive, eocd + 4)?;
    let central_disk = read_u16(archive, eocd + 6)?;
    let entries_on_disk = usize::from(read_u16(archive, eocd + 8)?);
    let entries_total = usize::from(read_u16(archive, eocd + 10)?);
    let central_size = usize_from_u32(read_u32(archive, eocd + 12)?)?;
    let central_offset = usize_from_u32(read_u32(archive, eocd + 16)?)?;
    if disk != 0 || central_disk != 0 || entries_on_disk != entries_total {
        return Err("multi-disk ZIP archives are not supported".to_string());
    }
    if entries_total > MAX_ARCHIVE_ENTRIES {
        return Err("entry limit exceeded".to_string());
    }
    let central_end = central_offset
        .checked_add(central_size)
        .ok_or("central directory size overflows")?;
    if central_end > archive.len() || central_end > eocd {
        return Err("central directory is outside the archive".to_string());
    }

    let mut cursor = central_offset;
    let mut entries = Vec::with_capacity(entries_total);
    let mut names = BTreeSet::new();
    for _ in 0..entries_total {
        if cursor + 46 > central_end || read_u32(archive, cursor)? != 0x0201_4b50 {
            return Err("central directory entry is malformed".to_string());
        }
        let flags = read_u16(archive, cursor + 8)?;
        let method = read_u16(archive, cursor + 10)?;
        let crc = read_u32(archive, cursor + 16)?;
        let compressed_size = usize_from_u32(read_u32(archive, cursor + 20)?)?;
        let uncompressed_size = usize_from_u32(read_u32(archive, cursor + 24)?)?;
        let name_length = usize::from(read_u16(archive, cursor + 28)?);
        let extra_length = usize::from(read_u16(archive, cursor + 30)?);
        let comment_length = usize::from(read_u16(archive, cursor + 32)?);
        let local_offset = usize_from_u32(read_u32(archive, cursor + 42)?)?;
        let name_start = cursor + 46;
        let name_end = name_start
            .checked_add(name_length)
            .ok_or("file name length overflows")?;
        let next = name_end
            .checked_add(extra_length)
            .and_then(|value| value.checked_add(comment_length))
            .ok_or("central entry length overflows")?;
        if next > central_end {
            return Err("central directory entry is truncated".to_string());
        }
        let name = std::str::from_utf8(&archive[name_start..name_end])
            .map_err(|_| "central entry name is not UTF-8".to_string())?
            .to_string();
        validate_archive_name(&name)?;
        if !names.insert(name.clone()) {
            return Err(format!("duplicate ZIP entry: {name}"));
        }
        if flags != 0 || method != 0 || compressed_size != uncompressed_size {
            return Err(format!("unsupported ZIP entry: {name}"));
        }
        let bytes = read_local_entry(archive, local_offset, &name, compressed_size, crc)?;
        entries.push(ArchiveEntry {
            directory: name.ends_with('/'),
            name,
            bytes,
        });
        cursor = next;
    }
    Ok(entries)
}

fn read_local_entry(
    archive: &[u8],
    offset: usize,
    name: &str,
    size: usize,
    expected_crc: u32,
) -> Result<Vec<u8>, String> {
    if offset + 30 > archive.len() || read_u32(archive, offset)? != 0x0403_4b50 {
        return Err(format!("local entry is malformed: {name}"));
    }
    if read_u16(archive, offset + 6)? != 0 || read_u16(archive, offset + 8)? != 0 {
        return Err(format!("unsupported local entry: {name}"));
    }
    let name_length = usize::from(read_u16(archive, offset + 26)?);
    let extra_length = usize::from(read_u16(archive, offset + 28)?);
    let data_start = offset
        .checked_add(30)
        .and_then(|value| value.checked_add(name_length))
        .and_then(|value| value.checked_add(extra_length))
        .ok_or("local entry length overflows")?;
    let data_end = data_start
        .checked_add(size)
        .ok_or("local entry size overflows")?;
    let local_name = std::str::from_utf8(&archive[offset + 30..offset + 30 + name_length])
        .map_err(|_| "local entry name is not UTF-8".to_string())?;
    if data_end > archive.len() || local_name != name {
        return Err(format!(
            "local entry does not match central directory: {name}"
        ));
    }
    let bytes = archive[data_start..data_end].to_vec();
    if crc32(&bytes) != expected_crc {
        return Err(format!("CRC mismatch: {name}"));
    }
    Ok(bytes)
}

fn validate_archive_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_ARCHIVE_PATH_BYTES || name.starts_with('/') {
        return Err("archive path is empty, absolute, or too long".to_string());
    }
    if name.contains('\\') {
        return Err("archive paths cannot contain backslashes".to_string());
    }
    let trimmed = name.trim_end_matches('/');
    if trimmed.is_empty()
        || trimmed
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(format!("unsafe archive path: {name}"));
    }
    Ok(())
}

fn archive_name(path: &str) -> String {
    path.strip_prefix("~/")
        .or_else(|| path.strip_prefix("./"))
        .unwrap_or(path)
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string()
}

fn append_path(parent: &str, child: &str) -> String {
    if parent == "/" {
        format!("/{child}")
    } else if parent.ends_with('/') {
        format!("{parent}{child}")
    } else {
        format!("{parent}/{child}")
    }
}

fn archive_failure(message: &str) -> CommandOutput {
    CommandOutput::failure(1, format!("zip/unzip: {message}\n"))
}

fn find_end_of_central_directory(bytes: &[u8]) -> Option<usize> {
    let start = bytes.len().saturating_sub(65_557);
    (start..bytes.len().saturating_sub(21))
        .rev()
        .find(|&index| {
            read_u32(bytes, index).ok() == Some(0x0605_4b50)
                && read_u16(bytes, index + 20)
                    .ok()
                    .and_then(|comment_length| index.checked_add(22 + usize::from(comment_length)))
                    == Some(bytes.len())
        })
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or("archive record is truncated")?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or("archive record is truncated")?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn usize_from_u32(value: u32) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| "archive size does not fit this platform".to_string())
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::{crc32, validate_archive_name};

    #[test]
    fn rejects_archive_names_that_can_escape_on_extraction() {
        for name in [
            "/absolute",
            "../outside",
            "safe/../outside",
            "safe\\outside",
        ] {
            assert!(
                validate_archive_name(name).is_err(),
                "unsafe archive name accepted: {name}"
            );
        }
        assert!(validate_archive_name("safe/note.txt").is_ok());
        assert!(validate_archive_name("safe/").is_ok());
    }

    #[test]
    fn computes_the_standard_crc32_vector() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }
}
