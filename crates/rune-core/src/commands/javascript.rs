use crate::{fs_failure, usage, CommandContext, CommandOutput};
use rune_runtime::{RuntimeKind, RuntimeRequest, MAX_JAVASCRIPT_SOURCE_BYTES};

pub(super) fn jsc(context: &mut CommandContext<'_>) -> CommandOutput {
    let Some(first) = context.args.first() else {
        return usage("jsc", "usage: jsc [--silent] SCRIPT [ARG ...]");
    };
    if first == "--help" || first == "-h" {
        return CommandOutput::success(
            "usage: jsc [--silent] SCRIPT [ARG ...]\n       jsc --reset\nRuns a bounded JavaScript source file in an isolated QuickJS runtime.\n",
        );
    }
    if first == "--reset" {
        if context.args.len() != 1 {
            return usage("jsc", "usage: jsc --reset");
        }
        return CommandOutput::success(
            "jsc: each invocation already starts a fresh runtime; reset acknowledged\n",
        );
    }
    let mut script_index = 0;
    while let Some(argument) = context.args.get(script_index) {
        match argument.as_str() {
            "--silent" => script_index += 1,
            "--in-window" => {
                return CommandOutput::failure(
                    2,
                    "jsc: --in-window is unavailable; JavaScript has no UI bridge\n",
                );
            }
            option if option.starts_with('-') => {
                return usage("jsc", "usage: jsc [--silent] SCRIPT [ARG ...]");
            }
            _ => break,
        }
    }
    let Some(source_path) = context.args.get(script_index) else {
        return usage("jsc", "usage: jsc [--silent] SCRIPT [ARG ...]");
    };
    match context.fs.metadata(source_path) {
        Ok(info) if info.size > MAX_JAVASCRIPT_SOURCE_BYTES as u64 => {
            return CommandOutput::failure(
                1,
                format!("jsc: {source_path}: source exceeds {MAX_JAVASCRIPT_SOURCE_BYTES} bytes\n"),
            );
        }
        Ok(_) => {}
        Err(error) => return fs_failure("jsc", &error),
    }
    let source = match context.fs.read(source_path) {
        Ok(source) => source,
        Err(error) => return fs_failure("jsc", &error),
    };
    let request = RuntimeRequest::new(
        RuntimeKind::JavaScript,
        source_path,
        &source,
        &context.args[script_index + 1..],
        context.env,
        context.stdin,
    )
    .with_cancellation(Some(context.cancellation));
    let execution = match context.javascript_runtime.execute(&request) {
        Ok(execution) => execution,
        Err(error) => {
            return CommandOutput::failure(126, format!("jsc: {source_path}: {error}\n"));
        }
    };
    CommandOutput {
        stdout: execution.stdout,
        stderr: execution.stderr,
        status: execution.status,
    }
}
