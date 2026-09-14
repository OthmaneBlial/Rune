use std::fmt::Write as _;

use crate::{fs_failure, usage, CommandContext, CommandOutput};

pub(super) fn config(context: &mut CommandContext<'_>) -> CommandOutput {
    let Some(operation) = context.args.first().map(String::as_str) else {
        return show(context);
    };
    match operation {
        "get" if context.args.len() == 2 => get(context, &context.args[1]),
        "set" if context.args.len() == 3 => set(context, &context.args[1], &context.args[2]),
        "set" => usage(
            "config",
            "usage: config set history-limit|history-redaction|environment-persistence|font-size|scrollback-limit|toolbar-visible|theme|cursor-color|cursor-shape|font|background|foreground VALUE",
        ),
        "reset" if context.args.len() == 1 => reset(context),
        "get" => usage(
            "config",
            "usage: config get history-limit|history-redaction|environment-persistence|font-size|scrollback-limit|toolbar-visible|theme|cursor-color|cursor-shape|font|background|foreground",
        ),
        _ => usage("config", "usage: config [get KEY|set KEY VALUE|reset]"),
    }
}

/// Hide the native input toolbar through the Rust-owned configuration state.
pub(super) fn hide_toolbar(context: &mut CommandContext<'_>) -> CommandOutput {
    set_toolbar_visibility(context, false, "hideToolbar")
}

/// Show the native input toolbar through the Rust-owned configuration state.
pub(super) fn show_toolbar(context: &mut CommandContext<'_>) -> CommandOutput {
    set_toolbar_visibility(context, true, "showToolbar")
}

fn set_toolbar_visibility(
    context: &mut CommandContext<'_>,
    visible: bool,
    command: &str,
) -> CommandOutput {
    if !context.args.is_empty() {
        return usage(command, &format!("usage: {command}"));
    }
    let value = if visible { "true" } else { "false" };
    match crate::config::update(context.fs, context.config, "toolbar-visible", value) {
        Ok(()) => CommandOutput::success(""),
        Err(error) => CommandOutput::failure(2, format!("{command}: {error}\n")),
    }
}

fn show(context: &CommandContext<'_>) -> CommandOutput {
    let mut stdout = String::new();
    let _ = writeln!(stdout, "history-limit={}", context.config.history_limit());
    let _ = writeln!(
        stdout,
        "history-redaction={}",
        context.config.history_redaction()
    );
    let _ = writeln!(
        stdout,
        "environment-persistence={}",
        context.config.environment_persistence()
    );
    let _ = writeln!(stdout, "font-size={}", context.config.font_size());
    let _ = writeln!(
        stdout,
        "scrollback-limit={}",
        context.config.scrollback_limit()
    );
    let _ = writeln!(
        stdout,
        "toolbar-visible={}",
        context.config.toolbar_visible()
    );
    let _ = writeln!(stdout, "theme={}", context.config.theme().as_str());
    let _ = writeln!(
        stdout,
        "cursor-color={}",
        context.config.cursor_color().as_str()
    );
    let _ = writeln!(
        stdout,
        "cursor-shape={}",
        context.config.cursor_shape().as_str()
    );
    let _ = writeln!(stdout, "font={}", context.config.font().as_str());
    let _ = writeln!(
        stdout,
        "background={}",
        context.config.background().as_str()
    );
    let _ = writeln!(
        stdout,
        "foreground={}",
        context.config.foreground().as_str()
    );
    CommandOutput::success(stdout)
}

fn get(context: &CommandContext<'_>, key: &str) -> CommandOutput {
    match key {
        "history-limit" => CommandOutput::success(format!(
            "history-limit={}\n",
            context.config.history_limit()
        )),
        "history-redaction" => CommandOutput::success(format!(
            "history-redaction={}\n",
            context.config.history_redaction()
        )),
        "environment-persistence" => CommandOutput::success(format!(
            "environment-persistence={}\n",
            context.config.environment_persistence()
        )),
        "font-size" => {
            CommandOutput::success(format!("font-size={}\n", context.config.font_size()))
        }
        "scrollback-limit" => CommandOutput::success(format!(
            "scrollback-limit={}\n",
            context.config.scrollback_limit()
        )),
        "toolbar-visible" => CommandOutput::success(format!(
            "toolbar-visible={}\n",
            context.config.toolbar_visible()
        )),
        "theme" => CommandOutput::success(format!("theme={}\n", context.config.theme().as_str())),
        "cursor-color" => CommandOutput::success(format!(
            "cursor-color={}\n",
            context.config.cursor_color().as_str()
        )),
        "cursor-shape" => CommandOutput::success(format!(
            "cursor-shape={}\n",
            context.config.cursor_shape().as_str()
        )),
        "font" => CommandOutput::success(format!("font={}\n", context.config.font().as_str())),
        "background" => CommandOutput::success(format!(
            "background={}\n",
            context.config.background().as_str()
        )),
        "foreground" => CommandOutput::success(format!(
            "foreground={}\n",
            context.config.foreground().as_str()
        )),
        _ => usage(
            "config",
            "unknown key; available keys: history-limit, history-redaction, environment-persistence, font-size, scrollback-limit, toolbar-visible, theme, cursor-color, cursor-shape, font, background, foreground",
        ),
    }
}

fn set(context: &mut CommandContext<'_>, key: &str, value: &str) -> CommandOutput {
    let result = crate::config::update(context.fs, context.config, key, value);
    match result {
        Ok(()) => CommandOutput::success(""),
        Err(message) if message.starts_with("unknown key") => usage("config", &message),
        Err(message) => CommandOutput::failure(2, format!("config: {message}\n")),
    }
}

fn reset(context: &mut CommandContext<'_>) -> CommandOutput {
    if let Err(error) = crate::config::reset(context.fs, context.config) {
        return fs_failure("config", &error);
    }
    CommandOutput::success("")
}
