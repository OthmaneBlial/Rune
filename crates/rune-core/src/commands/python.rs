use crate::{fs_failure, usage, CommandContext, CommandOutput};
use rune_runtime::{RuntimeKind, RuntimeRequest, MAX_PYTHON_SOURCE_BYTES};

pub(super) fn python(context: &mut CommandContext<'_>) -> CommandOutput {
    let Some(first) = context.args.first() else {
        return usage("python3", "usage: python3 SCRIPT [ARG ...]");
    };
    if first == "--help" || first == "-h" {
        return CommandOutput::success(
            "usage: python3 SCRIPT [ARG ...]\nRuns the bounded Rune Python subset without host modules.\n",
        );
    }
    if first == "--version" {
        return CommandOutput::success("Python 3-compatible RustPython subset (Rune)\n");
    }
    if first.starts_with('-') {
        return usage("python3", "usage: python3 SCRIPT [ARG ...]");
    }
    match context.fs.metadata(first) {
        Ok(info) if info.size > MAX_PYTHON_SOURCE_BYTES as u64 => {
            return CommandOutput::failure(
                1,
                format!("python3: {first}: source exceeds {MAX_PYTHON_SOURCE_BYTES} bytes\n"),
            );
        }
        Ok(_) => {}
        Err(error) => return fs_failure("python3", &error),
    }
    let source = match context.fs.read(first) {
        Ok(source) => source,
        Err(error) => return fs_failure("python3", &error),
    };
    let request = RuntimeRequest::new(
        RuntimeKind::Python,
        first,
        &source,
        &context.args[1..],
        context.env,
        context.stdin,
    )
    .with_cancellation(Some(context.cancellation));
    let execution = match context.python_runtime.execute(&request) {
        Ok(execution) => execution,
        Err(error) => {
            return CommandOutput::failure(126, format!("python3: {first}: {error}\n"));
        }
    };
    CommandOutput {
        stdout: execution.stdout,
        stderr: execution.stderr,
        status: execution.status,
    }
}
