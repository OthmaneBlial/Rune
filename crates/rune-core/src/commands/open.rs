use crate::open::{validate_target_size, validate_url_target};
use crate::{usage, CommandContext, CommandOutput, OpenError, OpenRequest, OpenTargetKind};

pub(super) fn open(context: &mut CommandContext<'_>) -> CommandOutput {
    open_target(context, "open", true)
}

pub(super) fn openurl(context: &mut CommandContext<'_>) -> CommandOutput {
    open_target(context, "openurl", false)
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
        let host_path = match context.fs.host_path(&target) {
            Ok(Some(path)) => path,
            Ok(None) => return failure(command, &OpenError::FileUnavailable),
            Err(error) => return CommandOutput::failure(1, format!("{command}: {error}\n")),
        };
        let Some(host_path) = host_path.to_str() else {
            return failure(command, &OpenError::FileUnavailable);
        };
        if let Err(error) = validate_target_size(host_path) {
            return invalid_target(command, &error);
        }
        OpenRequest {
            kind: OpenTargetKind::File,
            target: host_path.to_string(),
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
