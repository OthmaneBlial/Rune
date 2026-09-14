use crate::open::{validate_target_size, validate_url_target};
use crate::{usage, CommandContext, CommandOutput, OpenError, OpenRequest, OpenTargetKind};

pub(super) fn open(context: &mut CommandContext<'_>) -> CommandOutput {
    open_target(context, "open", true)
}

pub(super) fn openurl(context: &mut CommandContext<'_>) -> CommandOutput {
    open_target(context, "openurl", false)
}

pub(super) fn play(context: &mut CommandContext<'_>) -> CommandOutput {
    open_file_target(context, "play", OpenTargetKind::Play)
}

pub(super) fn view(context: &mut CommandContext<'_>) -> CommandOutput {
    open_file_target(context, "view", OpenTargetKind::View)
}

fn open_target(context: &mut CommandContext<'_>, command: &str, allow_file: bool) -> CommandOutput {
    if context.args.is_empty() {
        return usage(command, &format!("usage: {command} TARGET"));
    }
    let target = context.args.join(" ");
    if let Err(error) = validate_target_size(&target) {
        return invalid_target(command, &error);
    }

    let request = if allow_file && !has_url_scheme(&target) {
        let host_path = match validated_host_file(context, command, &target, false) {
            Ok(path) => path,
            Err(output) => return output,
        };
        OpenRequest {
            kind: OpenTargetKind::File,
            target: host_path,
        }
    } else {
        if let Err(error) = validate_url_target(&target) {
            return invalid_target(command, &error);
        }
        OpenRequest {
            kind: OpenTargetKind::Url,
            target,
        }
    };

    context.opener.open(&request).map_or_else(
        |error| failure(command, &error),
        |()| CommandOutput::success(""),
    )
}

fn open_file_target(
    context: &mut CommandContext<'_>,
    command: &str,
    kind: OpenTargetKind,
) -> CommandOutput {
    if context.args.len() != 1 {
        return usage(command, &format!("usage: {command} FILE"));
    }
    let target = context.args[0].clone();
    let host_path = match validated_host_file(context, command, &target, true) {
        Ok(path) => path,
        Err(output) => return output,
    };
    context
        .opener
        .open(&OpenRequest {
            kind,
            target: host_path,
        })
        .map_or_else(
            |error| failure(command, &error),
            |()| CommandOutput::success(""),
        )
}

fn validated_host_file(
    context: &mut CommandContext<'_>,
    command: &str,
    target: &str,
    regular_file_only: bool,
) -> Result<String, CommandOutput> {
    let host_path = match context.fs.host_path(target) {
        Ok(Some(path)) => path,
        Ok(None) => return Err(failure(command, &OpenError::FileUnavailable)),
        Err(error) => return Err(CommandOutput::failure(1, format!("{command}: {error}\n"))),
    };
    if regular_file_only {
        let canonical_target = match context.fs.canonical_path(target) {
            Ok(path) => path,
            Err(error) => return Err(CommandOutput::failure(1, format!("{command}: {error}\n"))),
        };
        let info = match context.fs.metadata(&canonical_target) {
            Ok(info) => info,
            Err(error) => return Err(CommandOutput::failure(1, format!("{command}: {error}\n"))),
        };
        if info.is_directory {
            return Err(CommandOutput::failure(
                1,
                format!("{command}: target is a directory, not a regular file\n"),
            ));
        }
    }
    let Some(host_path) = host_path.to_str() else {
        return Err(failure(command, &OpenError::FileUnavailable));
    };
    validate_target_size(host_path).map_err(|error| invalid_target(command, &error))?;
    Ok(host_path.to_string())
}

fn has_url_scheme(target: &str) -> bool {
    let Some((scheme, _)) = target.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && scheme.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && b"+.-".contains(&byte))
        })
}

fn invalid_target(command: &str, error: &OpenError) -> CommandOutput {
    CommandOutput::failure(2, format!("{command}: {error}\n"))
}

fn failure(command: &str, error: &OpenError) -> CommandOutput {
    CommandOutput::failure(1, format!("{command}: {error}\n"))
}
