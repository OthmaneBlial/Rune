use std::fmt::Write as _;

use crate::open::{validate_target_size, validate_url_target};
use crate::{usage, CommandContext, CommandOutput, OpenError, OpenRequest, OpenTargetKind};

pub(super) fn open(context: &mut CommandContext<'_>) -> CommandOutput {
    open_target(context, "open", true)
}

pub(super) fn openurl(context: &mut CommandContext<'_>) -> CommandOutput {
    open_target(context, "openurl", false)
}

/// Open the native phone handler for one explicitly supplied phone number.
/// Contact-name lookup intentionally remains outside the portable core.
pub(super) fn call(context: &mut CommandContext<'_>) -> CommandOutput {
    if context.args.is_empty() {
        return usage("call", "usage: call PHONE");
    }
    let raw_number = context.args.join(" ");
    let number = match parse_phone_number(&raw_number) {
        Ok(number) => number,
        Err(error) => return invalid_target("call", &error),
    };
    open_url(context, "call", format!("tel://{number}"))
}

/// Open the native messaging handler for one number and an optional message.
/// Contact-name lookup intentionally remains outside the portable core.
pub(super) fn text(context: &mut CommandContext<'_>) -> CommandOutput {
    let Some(raw_number) = context.args.first() else {
        return usage("text", "usage: text PHONE [MESSAGE ...]");
    };
    let number = match parse_phone_number(raw_number) {
        Ok(number) => number,
        Err(error) => return invalid_target("text", &error),
    };
    let target = if context.args.len() == 1 {
        format!("sms://{number}")
    } else {
        let message = context.args[1..].join(" ");
        format!("sms://{number}&body={}", percent_encode_query(&message))
    };
    open_url(context, "text", target)
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

fn open_url(context: &mut CommandContext<'_>, command: &str, target: String) -> CommandOutput {
    if let Err(error) = validate_url_target(&target) {
        return invalid_target(command, &error);
    }
    context
        .opener
        .open(&OpenRequest {
            kind: OpenTargetKind::Url,
            target,
        })
        .map_or_else(
            |error| failure(command, &error),
            |()| CommandOutput::success(""),
        )
}

fn parse_phone_number(raw: &str) -> Result<String, OpenError> {
    if raw.is_empty() || raw.len() > 256 {
        return Err(OpenError::InvalidTarget(
            "phone number must contain between 3 and 32 digits".to_string(),
        ));
    }

    let mut number = String::with_capacity(raw.len());
    let mut digit_count = 0;
    let mut saw_plus = false;
    for character in raw.chars() {
        match character {
            '+' if number.is_empty() => {
                saw_plus = true;
                number.push(character);
            }
            '+' => {
                return Err(OpenError::InvalidTarget(
                    "phone number has an invalid plus sign".to_string(),
                ));
            }
            '0'..='9' => {
                digit_count += 1;
                number.push(character);
            }
            '-' | '(' | ')' | ' ' | '\t' => {}
            _ => {
                return Err(OpenError::InvalidTarget(
                    "phone number contains unsupported characters".to_string(),
                ));
            }
        }
    }

    if !(3..=32).contains(&digit_count) || (saw_plus && number == "+") {
        return Err(OpenError::InvalidTarget(
            "phone number must contain between 3 and 32 digits".to_string(),
        ));
    }
    Ok(number)
}

fn percent_encode_query(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
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
