use std::fmt::Write as _;

use crate::{fs_failure, usage, CommandContext, CommandOutput};

pub(super) fn bookmark(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.len() != 1 {
        return usage("bookmark", "usage: bookmark NAME");
    }
    let name = &context.args[0];
    if !is_valid_bookmark_name(name) {
        return invalid_name("bookmark", name);
    }
    let path = context.fs.current_dir_display();
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
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
}
