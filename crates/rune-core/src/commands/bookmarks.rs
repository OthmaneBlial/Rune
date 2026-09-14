use std::fmt::Write as _;

use crate::{
    fs_failure, usage, CommandContext, CommandOutput, MAX_BOOKMARKS, MAX_BOOKMARK_NAME_CHARS,
    MAX_BOOKMARK_PATH_BYTES,
};

const MAX_Z_KEYWORDS: usize = 8;
const MAX_Z_KEYWORD_BYTES: usize = 64;
const MAX_Z_QUERY_BYTES: usize = 256;

pub(super) fn bookmark(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.len() != 1 {
        return usage("bookmark", "usage: bookmark NAME");
    }
    let name = &context.args[0];
    if !is_valid_bookmark_name(name) {
        return invalid_name("bookmark", name);
    }
    let path = context.fs.current_dir_display();
    if path.len() > MAX_BOOKMARK_PATH_BYTES {
        return CommandOutput::failure(
            1,
            format!(
                "bookmark: current directory exceeds the {MAX_BOOKMARK_PATH_BYTES}-byte limit\n"
            ),
        );
    }
    if !context.bookmarks.contains_key(name) && context.bookmarks.len() >= MAX_BOOKMARKS {
        return CommandOutput::failure(
            1,
            format!("bookmark: maximum of {MAX_BOOKMARKS} bookmarks reached\n"),
        );
    }
    context.bookmarks.insert(name.clone(), path);
    CommandOutput::success("")
}

pub(super) fn showmarks(context: &mut CommandContext<'_>) -> CommandOutput {
    if !context.args.is_empty() {
        return usage("showmarks", "usage: showmarks");
    }
    let mut stdout = String::new();
    for (name, path) in context.bookmarks.iter() {
        let _ = writeln!(stdout, "{name} -> {path}");
    }
    CommandOutput::success(stdout)
}

pub(super) fn jump(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.len() != 1 {
        return usage("jump", "usage: jump NAME");
    }
    let name = &context.args[0];
    let Some(path) = context.bookmarks.get(name).cloned() else {
        return CommandOutput::failure(1, format!("jump: {name}: bookmark not found\n"));
    };
    match context.fs.change_dir(&path) {
        Ok(()) => CommandOutput::success(""),
        Err(error) => fs_failure("jump", &error),
    }
}

/// Change to the most frequently visited confined directory matching the
/// supplied keywords. This is a bounded Rust-owned equivalent of a-Shell's
/// `z` command; it never searches or changes the host working directory.
pub(super) fn z(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.is_empty() {
        return usage("z", "usage: z KEYWORD ...");
    }
    if context.args.len() > MAX_Z_KEYWORDS
        || context.args.iter().any(|keyword| {
            keyword.is_empty()
                || keyword.len() > MAX_Z_KEYWORD_BYTES
                || keyword.chars().any(char::is_control)
        })
        || context.args.iter().map(String::len).sum::<usize>() > MAX_Z_QUERY_BYTES
    {
        return CommandOutput::failure(
            2,
            format!(
                "z: use 1-{MAX_Z_KEYWORDS} non-empty keywords, each at most {MAX_Z_KEYWORD_BYTES} bytes and {MAX_Z_QUERY_BYTES} bytes total\n"
            ),
        );
    }

    if context.args.len() == 1 {
        let direct = context.args[0].as_str();
        if context
            .fs
            .metadata(direct)
            .is_ok_and(|metadata| metadata.is_directory)
        {
            return change_directory(context, direct);
        }
    }

    let mut candidates = context
        .directory_usage
        .iter()
        .filter(|(path, count)| **count > 0 && keywords_match(path, context.args))
        .filter_map(|(path, count)| {
            context
                .fs
                .metadata(path)
                .ok()
                .filter(|metadata| metadata.is_directory)
                .map(|_| (path.clone(), *count))
        })
        .collect::<Vec<_>>();

    if candidates.is_empty() {
        let current_directory = context.fs.current_dir_display();
        if let Ok(entries) = context.fs.list(None) {
            candidates = entries
                .into_iter()
                .filter(|entry| entry.is_directory)
                .map(|entry| child_path(&current_directory, &entry.name))
                .filter(|path| keywords_match(path, context.args))
                .map(|path| (path, 0))
                .collect();
        }
    }

    candidates.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let Some((path, _)) = candidates.into_iter().next() else {
        return CommandOutput::failure(
            1,
            format!("z: no directory matches {}\n", context.args.join(" ")),
        );
    };
    change_directory(context, &path)
}

fn change_directory(context: &mut CommandContext<'_>, path: &str) -> CommandOutput {
    match context.fs.change_dir(path) {
        Ok(()) => CommandOutput::success(""),
        Err(error) => fs_failure("z", &error),
    }
}

fn keywords_match(path: &str, keywords: &[String]) -> bool {
    let mut remainder = path;
    for keyword in keywords {
        let Some(offset) = remainder.find(keyword) else {
            return false;
        };
        remainder = &remainder[offset + keyword.len()..];
    }
    true
}

fn child_path(parent: &str, name: &str) -> String {
    if parent == "~" {
        format!("~/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

pub(super) fn renamemark(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.len() != 2 {
        return usage("renamemark", "usage: renamemark OLD NEW");
    }
    let old = &context.args[0];
    let new = &context.args[1];
    if !is_valid_bookmark_name(old) {
        return invalid_name("renamemark", old);
    }
    if !is_valid_bookmark_name(new) {
        return invalid_name("renamemark", new);
    }
    if !context.bookmarks.contains_key(old) {
        return CommandOutput::failure(1, format!("renamemark: {old}: bookmark not found\n"));
    }
    if context.bookmarks.contains_key(new) {
        return CommandOutput::failure(1, format!("renamemark: {new}: bookmark already exists\n"));
    }
    let Some(path) = context.bookmarks.remove(old) else {
        return CommandOutput::failure(1, format!("renamemark: {old}: bookmark not found\n"));
    };
    context.bookmarks.insert(new.clone(), path);
    CommandOutput::success("")
}

pub(super) fn deletemark(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.is_empty() {
        return usage("deletemark", "usage: deletemark NAME ...");
    }
    for name in context.args {
        if !is_valid_bookmark_name(name) {
            return invalid_name("deletemark", name);
        }
        if !context.bookmarks.contains_key(name) {
            return CommandOutput::failure(1, format!("deletemark: {name}: bookmark not found\n"));
        }
    }
    for name in context.args {
        context.bookmarks.remove(name);
    }
    CommandOutput::success("")
}

pub(super) fn lookup_cd_bookmark(
    context: &CommandContext<'_>,
    input: &str,
) -> Result<Option<String>, CommandOutput> {
    let Some(name) = input.strip_prefix('~') else {
        return Ok(None);
    };
    if name.is_empty() || name.starts_with('/') || name.contains('/') {
        return Ok(None);
    }
    context.bookmarks.get(name).cloned().map_or_else(
        || {
            Err(CommandOutput::failure(
                1,
                format!("cd: {name}: bookmark not found\n"),
            ))
        },
        |path| Ok(Some(path)),
    )
}

fn invalid_name(command: &str, name: &str) -> CommandOutput {
    usage(
        command,
        &format!("invalid bookmark name: {name}; use letters, digits, '_', '-', or '.'"),
    )
}

pub(super) fn is_valid_bookmark_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_BOOKMARK_NAME_CHARS
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
}
