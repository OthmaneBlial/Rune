use rune_fs::{FsError, VirtualFileSystem};

pub(super) const CONFIG_PATH: &str = "~/.rune/config.state";
const CONFIG_DIRECTORY: &str = "~/.rune";
const CONFIG_HEADER: &str = "RUNE_CONFIG_V1";
const DEFAULT_HISTORY_LIMIT: usize = 1_000;
const MIN_HISTORY_LIMIT: usize = 1;
const MAX_HISTORY_LIMIT: usize = 10_000;
const DEFAULT_FONT_SIZE: u8 = 15;
const MIN_FONT_SIZE: u8 = 8;
const MAX_FONT_SIZE: u8 = 32;
const DEFAULT_SCROLLBACK_LIMIT: usize = 4_096;
const MIN_SCROLLBACK_LIMIT: usize = 128;
const MAX_SCROLLBACK_LIMIT: usize = 8_192;
const DEFAULT_TOOLBAR_VISIBLE: bool = true;
const DEFAULT_THEME: TerminalTheme = TerminalTheme::Ink;

/// Themes understood by the portable configuration contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalTheme {
    Ink,
    Light,
    Ember,
}

impl TerminalTheme {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ink => "ink",
            Self::Light => "light",
            Self::Ember => "ember",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "ink" => Some(Self::Ink),
            "light" => Some(Self::Light),
            "ember" => Some(Self::Ember),
            _ => None,
        }
    }
}

/// Portable session settings currently owned by the Rust core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalConfig {
    history_limit: usize,
    font_size: u8,
    scrollback_limit: usize,
    toolbar_visible: bool,
    theme: TerminalTheme,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            history_limit: DEFAULT_HISTORY_LIMIT,
            font_size: DEFAULT_FONT_SIZE,
            scrollback_limit: DEFAULT_SCROLLBACK_LIMIT,
            toolbar_visible: DEFAULT_TOOLBAR_VISIBLE,
            theme: DEFAULT_THEME,
        }
    }
}

impl TerminalConfig {
    /// Returns the maximum number of history records retained by a session.
    #[must_use]
    pub const fn history_limit(&self) -> usize {
        self.history_limit
    }

    /// Returns the monospace terminal font size in points.
    #[must_use]
    pub const fn font_size(&self) -> u8 {
        self.font_size
    }

    /// Returns the maximum number of rendered transcript entries retained by
    /// the native presentation layer.
    #[must_use]
    pub const fn scrollback_limit(&self) -> usize {
        self.scrollback_limit
    }

    /// Returns whether the native terminal input toolbar is visible by
    /// default. This is a presentation setting persisted by Rust so native
    /// frontends share one configuration source.
    #[must_use]
    pub const fn toolbar_visible(&self) -> bool {
        self.toolbar_visible
    }

    /// Returns the configured terminal color theme.
    #[must_use]
    pub const fn theme(&self) -> TerminalTheme {
        self.theme
    }

    pub(super) fn set_history_limit(&mut self, value: usize) {
        self.history_limit = value;
    }

    pub(super) fn set_font_size(&mut self, value: u8) {
        self.font_size = value;
    }

    pub(super) fn set_scrollback_limit(&mut self, value: usize) {
        self.scrollback_limit = value;
    }

    pub(super) fn set_toolbar_visible(&mut self, value: bool) {
        self.toolbar_visible = value;
    }

    pub(super) fn set_theme(&mut self, value: TerminalTheme) {
        self.theme = value;
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
        let content = format!(
            "{CONFIG_HEADER}\nhistory_limit={}\nfont_size={}\nscrollback_limit={}\ntoolbar_visible={}\ntheme={}\n",
            self.history_limit,
            self.font_size,
            self.scrollback_limit,
            self.toolbar_visible,
            self.theme.as_str()
        );
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

pub(super) fn update_font_size(
    filesystem: &mut dyn VirtualFileSystem,
    config: &mut TerminalConfig,
    value: &str,
) -> Result<(), String> {
    let parsed = value
        .parse::<u8>()
        .map_err(|_| "font-size must be a positive integer".to_string())?;
    if !(MIN_FONT_SIZE..=MAX_FONT_SIZE).contains(&parsed) {
        return Err(format!(
            "font-size must be between {MIN_FONT_SIZE} and {MAX_FONT_SIZE}"
        ));
    }
    let previous = config.clone();
    config.set_font_size(parsed);
    if let Err(error) = config.save(filesystem) {
        *config = previous;
        return Err(format!("could not persist configuration: {error}"));
    }
    Ok(())
}

pub(super) fn update_theme(
    filesystem: &mut dyn VirtualFileSystem,
    config: &mut TerminalConfig,
    value: &str,
) -> Result<(), String> {
    let theme = TerminalTheme::parse(value)
        .ok_or_else(|| "theme must be one of: ink, light, ember".to_string())?;
    let previous = config.clone();
    config.set_theme(theme);
    if let Err(error) = config.save(filesystem) {
        *config = previous;
        return Err(format!("could not persist configuration: {error}"));
    }
    Ok(())
}

pub(super) fn update_scrollback_limit(
    filesystem: &mut dyn VirtualFileSystem,
    config: &mut TerminalConfig,
    value: &str,
) -> Result<(), String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| "scrollback-limit must be a positive integer".to_string())?;
    if !(MIN_SCROLLBACK_LIMIT..=MAX_SCROLLBACK_LIMIT).contains(&parsed) {
        return Err(format!(
            "scrollback-limit must be between {MIN_SCROLLBACK_LIMIT} and {MAX_SCROLLBACK_LIMIT}"
        ));
    }
    let previous = config.clone();
    config.set_scrollback_limit(parsed);
    if let Err(error) = config.save(filesystem) {
        *config = previous;
        return Err(format!("could not persist configuration: {error}"));
    }
    Ok(())
}

pub(super) fn update_toolbar_visible(
    filesystem: &mut dyn VirtualFileSystem,
    config: &mut TerminalConfig,
    value: &str,
) -> Result<(), String> {
    let parsed = match value {
        "true" | "1" => true,
        "false" | "0" => false,
        _ => return Err("toolbar-visible must be true or false".to_string()),
    };
    let previous = config.clone();
    config.set_toolbar_visible(parsed);
    if let Err(error) = config.save(filesystem) {
        *config = previous;
        return Err(format!("could not persist configuration: {error}"));
    }
    Ok(())
}

pub(super) fn update(
    filesystem: &mut dyn VirtualFileSystem,
    config: &mut TerminalConfig,
    key: &str,
    value: &str,
) -> Result<(), String> {
    match key {
        "history-limit" => update_history_limit(filesystem, config, value),
        "font-size" => update_font_size(filesystem, config, value),
        "scrollback-limit" => update_scrollback_limit(filesystem, config, value),
        "toolbar-visible" => update_toolbar_visible(filesystem, config, value),
        "theme" => update_theme(filesystem, config, value),
        _ => Err(
            "unknown key; available keys: history-limit, font-size, scrollback-limit, toolbar-visible, theme"
                .to_string(),
        ),
    }
}

pub(super) fn reset(
    filesystem: &mut dyn VirtualFileSystem,
    config: &mut TerminalConfig,
) -> Result<(), FsError> {
    let previous = config.clone();
    *config = TerminalConfig::default();
    if let Err(error) = config.save(filesystem) {
        *config = previous;
        return Err(error);
    }
    Ok(())
}

fn parse(content: &str) -> Option<TerminalConfig> {
    let mut lines = content.lines();
    if lines.next()? != CONFIG_HEADER {
        return None;
    }
    let mut config = TerminalConfig::default();
    let mut seen_history_limit = false;
    let mut seen_font_size = false;
    let mut seen_scrollback_limit = false;
    let mut seen_toolbar_visible = false;
    let mut seen_theme = false;
    for line in lines {
        let (key, value) = line.split_once('=')?;
        match key {
            "history_limit" if !seen_history_limit => {
                let history_limit = value.parse::<usize>().ok()?;
                if !(MIN_HISTORY_LIMIT..=MAX_HISTORY_LIMIT).contains(&history_limit) {
                    return None;
                }
                config.history_limit = history_limit;
                seen_history_limit = true;
            }
            "font_size" if !seen_font_size => {
                let font_size = value.parse::<u8>().ok()?;
                if !(MIN_FONT_SIZE..=MAX_FONT_SIZE).contains(&font_size) {
                    return None;
                }
                config.font_size = font_size;
                seen_font_size = true;
            }
            "scrollback_limit" if !seen_scrollback_limit => {
                let scrollback_limit = value.parse::<usize>().ok()?;
                if !(MIN_SCROLLBACK_LIMIT..=MAX_SCROLLBACK_LIMIT).contains(&scrollback_limit) {
                    return None;
                }
                config.scrollback_limit = scrollback_limit;
                seen_scrollback_limit = true;
            }
            "toolbar_visible" if !seen_toolbar_visible => {
                config.toolbar_visible = match value {
                    "true" => true,
                    "false" => false,
                    _ => return None,
                };
                seen_toolbar_visible = true;
            }
            "theme" if !seen_theme => {
                config.theme = TerminalTheme::parse(value)?;
                seen_theme = true;
            }
            _ => return None,
        }
    }
    Some(config)
}

#[cfg(test)]
mod tests {
    use super::{
        parse, TerminalConfig, TerminalTheme, CONFIG_HEADER, DEFAULT_FONT_SIZE,
        DEFAULT_SCROLLBACK_LIMIT, DEFAULT_THEME, DEFAULT_TOOLBAR_VISIBLE,
    };

    #[test]
    fn parses_bounded_history_configuration() {
        let content = format!("{CONFIG_HEADER}\nhistory_limit=25\n");
        let config = parse(&content).expect("configuration should parse");
        assert_eq!(config.history_limit(), 25);
        assert_eq!(config.font_size(), DEFAULT_FONT_SIZE);
        assert_eq!(config.scrollback_limit(), DEFAULT_SCROLLBACK_LIMIT);
        assert_eq!(config.toolbar_visible(), DEFAULT_TOOLBAR_VISIBLE);
        assert_eq!(config.theme(), DEFAULT_THEME);
    }

    #[test]
    fn parses_and_bounds_font_size_while_accepting_older_files() {
        let content = format!("{CONFIG_HEADER}\nhistory_limit=25\nfont_size=20\n");
        let config = parse(&content).expect("configuration should parse");
        assert_eq!(config.font_size(), 20);
        assert_eq!(config.history_limit(), 25);
        let themed = format!("{content}theme=ember\n");
        assert_eq!(
            parse(&themed).expect("theme should parse").theme(),
            TerminalTheme::Ember
        );
        let configured = format!("{content}scrollback_limit=2048\ntheme=ember\n");
        assert_eq!(
            parse(&configured)
                .expect("scrollback should parse")
                .scrollback_limit(),
            2048
        );
        let toolbar = format!("{content}toolbar_visible=false\ntheme=ember\n");
        assert!(!parse(&toolbar)
            .expect("toolbar visibility should parse")
            .toolbar_visible());
        assert!(parse("RUNE_CONFIG_V1\nfont_size=7\n").is_none());
        assert!(parse("RUNE_CONFIG_V1\nfont_size=33\n").is_none());
        assert!(parse("RUNE_CONFIG_V1\nscrollback_limit=127\n").is_none());
        assert!(parse("RUNE_CONFIG_V1\nscrollback_limit=8193\n").is_none());
        assert!(parse("RUNE_CONFIG_V1\ntoolbar_visible=maybe\n").is_none());
        assert!(parse("RUNE_CONFIG_V1\ntheme=unknown\n").is_none());
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
        assert_eq!(TerminalConfig::default().font_size(), DEFAULT_FONT_SIZE);
        assert_eq!(
            TerminalConfig::default().scrollback_limit(),
            DEFAULT_SCROLLBACK_LIMIT
        );
        assert_eq!(
            TerminalConfig::default().toolbar_visible(),
            DEFAULT_TOOLBAR_VISIBLE
        );
        assert_eq!(TerminalConfig::default().theme(), DEFAULT_THEME);
        assert!(parse("not-rune-config\n").is_none());
    }
}
