use std::fmt::Write as _;

use crate::{fs_failure, usage, CommandContext, CommandOutput};

pub(super) fn config(context: &mut CommandContext<'_>) -> CommandOutput {
    let Some(operation) = context.args.first().map(String::as_str) else {
        return show(context);
    };
    match operation {
        "get" if context.args.len() == 2 => get(context, &context.args[1]),
        "set" if context.args.len() == 3 && context.args[1] == "history-limit" => {
            set_history_limit(context, &context.args[2])
        }
        "reset" if context.args.len() == 1 => reset(context),
        "get" => usage("config", "usage: config get history-limit"),
        "set" => usage("config", "usage: config set history-limit VALUE"),
        _ => usage(
            "config",
            "usage: config [get history-limit|set history-limit VALUE|reset]",
        ),
    }
}

fn show(context: &CommandContext<'_>) -> CommandOutput {
    let mut stdout = String::new();
    let _ = writeln!(stdout, "history-limit={}", context.config.history_limit());
    CommandOutput::success(stdout)
}

fn get(context: &CommandContext<'_>, key: &str) -> CommandOutput {
    if key == "history-limit" {
        return CommandOutput::success(format!(
            "history-limit={}\n",
            context.config.history_limit()
        ));
    }
    usage("config", "unknown key; available key: history-limit")
}

fn set_history_limit(context: &mut CommandContext<'_>, value: &str) -> CommandOutput {
    match crate::config::update_history_limit(context.fs, context.config, value) {
        Ok(()) => CommandOutput::success(""),
        Err(message) => CommandOutput::failure(2, format!("config: {message}\n")),
    }
}

fn reset(context: &mut CommandContext<'_>) -> CommandOutput {
    let previous = context.config.clone();
    *context.config = crate::config::TerminalConfig::default();
    if let Err(error) = context.config.save(context.fs) {
        *context.config = previous;
        return fs_failure("config", &error);
    }
    CommandOutput::success("")
}
