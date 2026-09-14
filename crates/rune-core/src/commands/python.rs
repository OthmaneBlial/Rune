use crate::{fs_failure, usage, CommandContext, CommandOutput};
use rune_runtime::{RuntimeKind, RuntimeRequest, MAX_PYTHON_SOURCE_BYTES};

pub(super) fn python(context: &mut CommandContext<'_>) -> CommandOutput {
    let Some(first) = context.args.first() else {
        return usage(
            "python3",
            "usage: python3 SCRIPT [ARG ...] | python3 -c CODE [ARG ...] | python3 -",
        );
    };
    if first == "--help" || first == "-h" {
        return CommandOutput::success(
            "usage: python3 SCRIPT [ARG ...] | python3 -c CODE [ARG ...] | python3 -\nRuns the bounded Rune Python subset without host modules.\n",
        );
    }
    if first == "--version" {
        return CommandOutput::success("Python 3-compatible RustPython subset (Rune)\n");
    }
    if first == "-c" {
        let Some(source) = context.args.get(1) else {
            return usage(
                "python3",
                "usage: python3 SCRIPT [ARG ...] | python3 -c CODE [ARG ...] | python3 -",
            );
        };
        if source.len() > MAX_PYTHON_SOURCE_BYTES {
            return CommandOutput::failure(
                1,
                format!("python3: -c: source exceeds {MAX_PYTHON_SOURCE_BYTES} bytes\n"),
            );
        }
        return execute_python(
            context,
            "-c",
            source.as_bytes(),
            &context.args[2..],
            context.stdin,
        );
    }
    if first == "-" {
        if context.stdin.len() > MAX_PYTHON_SOURCE_BYTES {
            return CommandOutput::failure(
                1,
                format!("python3: -: source exceeds {MAX_PYTHON_SOURCE_BYTES} bytes\n"),
            );
        }
        return execute_python(
            context,
            "-",
            context.stdin.as_bytes(),
            &context.args[1..],
            "",
        );
    }
    if first.starts_with('-') {
        return usage(
            "python3",
            "usage: python3 SCRIPT [ARG ...] | python3 -c CODE [ARG ...] | python3 -",
        );
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
    execute_python(context, first, &source, &context.args[1..], context.stdin)
}

fn execute_python(
    context: &mut CommandContext<'_>,
    program_name: &str,
    source: &[u8],
    arguments: &[String],
    stdin: &str,
) -> CommandOutput {
    let request = RuntimeRequest::new(
        RuntimeKind::Python,
        program_name,
        source,
        arguments,
        context.env,
        stdin,
    )
    .with_cancellation(Some(context.cancellation));
    let execution = match context.python_runtime.execute(&request) {
        Ok(execution) => execution,
        Err(error) => {
            return CommandOutput::failure(126, format!("python3: {program_name}: {error}\n"));
        }
    };
    CommandOutput {
        stdout: execution.stdout,
        stderr: execution.stderr,
        status: execution.status,
    }
}
