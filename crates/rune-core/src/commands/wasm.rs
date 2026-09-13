use crate::{fs_failure, usage, CommandContext, CommandOutput};
use rune_runtime::{RuntimeKind, RuntimePreopen, RuntimeRequest};

pub(super) fn wasm(context: &mut CommandContext<'_>) -> CommandOutput {
    let Some(module_path) = context.args.first() else {
        return usage("wasm", "usage: wasm MODULE [arg ...]");
    };
    let module = match context.fs.read(module_path) {
        Ok(module) => module,
        Err(error) => return fs_failure("wasm", &error),
    };
    let host_preopens = context.fs.host_preopens();
    let runtime_preopens = host_preopens
        .iter()
        .skip(1)
        .map(|(host_path, guest_path)| RuntimePreopen::new(host_path, guest_path))
        .collect::<Vec<_>>();
    let request = RuntimeRequest::new(
        RuntimeKind::Wasm,
        module_path,
        &module,
        &context.args[1..],
        context.env,
        context.stdin,
    )
    .with_preopened_root(host_preopens.first().map(|(host_path, _)| *host_path))
    .with_additional_preopens(&runtime_preopens)
    .with_cancellation(Some(context.cancellation));
    let execution = match context.runtime.execute(&request) {
        Ok(execution) => execution,
        Err(error) => {
            return CommandOutput::failure(126, format!("wasm: {module_path}: {error}\n"));
        }
    };
    CommandOutput {
        stdout: execution.stdout,
        stderr: execution.stderr,
        status: execution.status,
    }
}
