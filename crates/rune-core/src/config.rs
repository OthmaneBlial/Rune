use rune_fs::{FsError, VirtualFileSystem};

pub(super) const CONFIG_PATH: &str = "~/.rune/config.state";
const CONFIG_DIRECTORY: &str = "~/.rune";
const CONFIG_HEADER: &str = "RUNE_CONFIG_V1";
const DEFAULT_HISTORY_LIMIT: usize = 1_000;
const MIN_HISTORY_LIMIT: usize = 1;
const MAX_HISTORY_LIMIT: usize = 10_000;

/// Portable session settings currently owned by the Rust core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalConfig {
    history_limit: usize,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            history_limit: DEFAULT_HISTORY_LIMIT,
        }
    }
}

impl TerminalConfig {
    /// Returns the maximum number of history records retained by a session.
    #[must_use]
    pub const fn history_limit(&self) -> usize {
        self.history_limit
    }

    pub(super) fn set_history_limit(&mut self, value: usize) {
        self.history_limit = value;
    }

    pub(super) fn load(filesystem: &dyn VirtualFileSystem) -> Self {
        let Ok(bytes) = filesystem.read(CONFIG_PATH) else {
            return Self::default();
        };
        let Ok(content) = String::from_utf8(bytes) else {
            return Self::default();
        };
        parse(&content).unwrap_or_default()
    }

    pub(super) fn save(&self, filesystem: &mut dyn VirtualFileSystem) -> Result<(), FsError> {
        match filesystem.make_directory(CONFIG_DIRECTORY, false) {
            Ok(()) | Err(FsError::AlreadyExists(_)) => {}
            Err(error) => return Err(error),
        }
        let content = format!("{CONFIG_HEADER}\nhistory_limit={}\n", self.history_limit);
        filesystem.write(CONFIG_PATH, content.as_bytes(), false)
    }
}

pub(super) fn update_history_limit(
    filesystem: &mut dyn VirtualFileSystem,
    config: &mut TerminalConfig,
    value: &str,
) -> Result<(), String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| "history-limit must be a positive integer".to_string())?;
    if !(MIN_HISTORY_LIMIT..=MAX_HISTORY_LIMIT).contains(&parsed) {
        return Err(format!(
            "history-limit must be between {MIN_HISTORY_LIMIT} and {MAX_HISTORY_LIMIT}"
        ));
    }
    let previous = config.clone();
    config.set_history_limit(parsed);
    if let Err(error) = config.save(filesystem) {
        *config = previous;
        return Err(format!("could not persist configuration: {error}"));
    }
    Ok(())
}

fn parse(content: &str) -> Option<TerminalConfig> {
    let mut lines = content.lines();
    if lines.next()? != CONFIG_HEADER {
        return None;
    }
    let (key, value) = lines.next()?.split_once('=')?;
    if key != "history_limit" || lines.next().is_some() {
        return None;
    }
    let history_limit = value.parse::<usize>().ok()?;
    if !(MIN_HISTORY_LIMIT..=MAX_HISTORY_LIMIT).contains(&history_limit) {
        return None;
    }
    Some(TerminalConfig { history_limit })
}

#[cfg(test)]
mod tests {
    use super::{parse, TerminalConfig, CONFIG_HEADER};

    #[test]
    fn parses_bounded_history_configuration() {
        let content = format!("{CONFIG_HEADER}\nhistory_limit=25\n");
        let config = parse(&content).expect("configuration should parse");
        assert_eq!(config.history_limit(), 25);
    }

    #[test]
    fn rejects_unknown_or_out_of_range_configuration() {
        assert!(parse("RUNE_CONFIG_V1\nunknown=value\n").is_none());
        assert!(parse("RUNE_CONFIG_V1\nhistory_limit=0\n").is_none());
        assert!(parse("RUNE_CONFIG_V1\nhistory_limit=10001\n").is_none());
    }

    #[test]
    fn defaults_when_configuration_is_missing_or_malformed() {
        assert_eq!(TerminalConfig::default().history_limit(), 1_000);
        assert!(parse("not-rune-config\n").is_none());
    }
}
