use rune_fs::{FsError, VirtualFileSystem};

pub(super) const CONFIG_PATH: &str = "~/.rune/config.state";
const CONFIG_DIRECTORY: &str = "~/.rune";
const CONFIG_HEADER: &str = "RUNE_CONFIG_V1";
const DEFAULT_HISTORY_LIMIT: usize = 1_000;
const DEFAULT_HISTORY_REDACTION: bool = true;
const DEFAULT_ENVIRONMENT_PERSISTENCE: bool = false;
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
const DEFAULT_CURSOR_COLOR: TerminalCursorColor = TerminalCursorColor::Cyan;
const DEFAULT_CURSOR_SHAPE: TerminalCursorShape = TerminalCursorShape::Bar;
const DEFAULT_FONT: TerminalFont = TerminalFont::Monospaced;
const DEFAULT_BACKGROUND: TerminalBackground = TerminalBackground::Auto;
const DEFAULT_FOREGROUND: TerminalForeground = TerminalForeground::Auto;

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

/// Cursor tint values understood by the portable terminal configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalCursorColor {
    Cyan,
    Ember,
    Foreground,
}

impl TerminalCursorColor {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cyan => "cyan",
            Self::Ember => "ember",
            Self::Foreground => "foreground",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "cyan" => Some(Self::Cyan),
            "ember" => Some(Self::Ember),
            "foreground" => Some(Self::Foreground),
            _ => None,
        }
    }
}

/// Cursor shapes exposed by the native terminal configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalCursorShape {
    Block,
    Underline,
    Bar,
}

impl TerminalCursorShape {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Underline => "underline",
            Self::Bar => "bar",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "block" => Some(Self::Block),
            "underline" => Some(Self::Underline),
            "bar" => Some(Self::Bar),
            _ => None,
        }
    }
}

/// Font designs exposed by the portable terminal configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalFont {
    Monospaced,
    System,
    Rounded,
}

impl TerminalFont {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Monospaced => "monospaced",
            Self::System => "system",
            Self::Rounded => "rounded",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "monospaced" => Some(Self::Monospaced),
            "system" => Some(Self::System),
            "rounded" => Some(Self::Rounded),
            _ => None,
        }
    }
}

/// Background color overrides understood by the portable configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalBackground {
    Auto,
    Black,
    White,
    Slate,
}

impl TerminalBackground {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Black => "black",
            Self::White => "white",
            Self::Slate => "slate",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "black" => Some(Self::Black),
            "white" => Some(Self::White),
            "slate" => Some(Self::Slate),
            _ => None,
        }
    }
}

/// Foreground color overrides understood by the portable configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalForeground {
    Auto,
    Black,
    White,
    Cyan,
    Ember,
}

impl TerminalForeground {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Black => "black",
            Self::White => "white",
            Self::Cyan => "cyan",
            Self::Ember => "ember",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "black" => Some(Self::Black),
            "white" => Some(Self::White),
            "cyan" => Some(Self::Cyan),
            "ember" => Some(Self::Ember),
            _ => None,
        }
    }
}

/// Portable session settings currently owned by the Rust core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalConfig {
    history_limit: usize,
    history_redaction: bool,
    environment_persistence: bool,
    font_size: u8,
    scrollback_limit: usize,
    toolbar_visible: bool,
    theme: TerminalTheme,
    cursor_color: TerminalCursorColor,
    cursor_shape: TerminalCursorShape,
    font: TerminalFont,
    background: TerminalBackground,
    foreground: TerminalForeground,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            history_limit: DEFAULT_HISTORY_LIMIT,
            history_redaction: DEFAULT_HISTORY_REDACTION,
            environment_persistence: DEFAULT_ENVIRONMENT_PERSISTENCE,
            font_size: DEFAULT_FONT_SIZE,
            scrollback_limit: DEFAULT_SCROLLBACK_LIMIT,
            toolbar_visible: DEFAULT_TOOLBAR_VISIBLE,
            theme: DEFAULT_THEME,
            cursor_color: DEFAULT_CURSOR_COLOR,
            cursor_shape: DEFAULT_CURSOR_SHAPE,
            font: DEFAULT_FONT,
            background: DEFAULT_BACKGROUND,
            foreground: DEFAULT_FOREGROUND,
        }
    }
}

impl TerminalConfig {
    /// Returns the maximum number of history records retained by a session.
    #[must_use]
    pub const fn history_limit(&self) -> usize {
        self.history_limit
    }

    /// Returns whether potentially sensitive command lines are replaced before
    /// they enter the persisted history.
    #[must_use]
    pub const fn history_redaction(&self) -> bool {
        self.history_redaction
    }

    /// Returns whether user-defined session environment entries may be
    /// serialized during explicit session persistence.
    #[must_use]
    pub const fn environment_persistence(&self) -> bool {
        self.environment_persistence
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

    /// Returns the configured native terminal cursor tint.
    #[must_use]
    pub const fn cursor_color(&self) -> TerminalCursorColor {
        self.cursor_color
    }

    /// Returns the configured native terminal cursor shape.
    #[must_use]
    pub const fn cursor_shape(&self) -> TerminalCursorShape {
        self.cursor_shape
    }

    /// Returns the configured native terminal font design.
    #[must_use]
    pub const fn font(&self) -> TerminalFont {
        self.font
    }

    /// Returns the configured background override.
    #[must_use]
    pub const fn background(&self) -> TerminalBackground {
        self.background
    }

    /// Returns the configured foreground override.
    #[must_use]
    pub const fn foreground(&self) -> TerminalForeground {
        self.foreground
    }

    pub(super) fn set_history_limit(&mut self, value: usize) {
        self.history_limit = value;
    }

    pub(super) fn set_history_redaction(&mut self, value: bool) {
        self.history_redaction = value;
    }

    pub(super) fn set_environment_persistence(&mut self, value: bool) {
        self.environment_persistence = value;
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

    pub(super) fn set_cursor_color(&mut self, value: TerminalCursorColor) {
        self.cursor_color = value;
    }

    pub(super) fn set_cursor_shape(&mut self, value: TerminalCursorShape) {
        self.cursor_shape = value;
    }

    pub(super) fn set_font(&mut self, value: TerminalFont) {
        self.font = value;
    }

    pub(super) fn set_background(&mut self, value: TerminalBackground) {
        self.background = value;
    }

    pub(super) fn set_foreground(&mut self, value: TerminalForeground) {
        self.foreground = value;
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
            "{CONFIG_HEADER}\nhistory_limit={}\nhistory_redaction={}\nenvironment_persistence={}\nfont_size={}\nscrollback_limit={}\ntoolbar_visible={}\ntheme={}\ncursor_color={}\ncursor_shape={}\nfont={}\nbackground={}\nforeground={}\n",
            self.history_limit,
            self.history_redaction,
            self.environment_persistence,
            self.font_size,
            self.scrollback_limit,
            self.toolbar_visible,
            self.theme.as_str(),
            self.cursor_color.as_str(),
            self.cursor_shape.as_str(),
            self.font.as_str(),
            self.background.as_str(),
            self.foreground.as_str()
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

pub(super) fn update_history_redaction(
    filesystem: &mut dyn VirtualFileSystem,
    config: &mut TerminalConfig,
    value: &str,
) -> Result<(), String> {
    let parsed = match value {
        "true" | "1" | "on" => true,
        "false" | "0" | "off" => false,
        _ => return Err("history-redaction must be true or false".to_string()),
    };
    let previous = config.clone();
    config.set_history_redaction(parsed);
    if let Err(error) = config.save(filesystem) {
        *config = previous;
        return Err(format!("could not persist configuration: {error}"));
    }
    Ok(())
}

pub(super) fn update_environment_persistence(
    filesystem: &mut dyn VirtualFileSystem,
    config: &mut TerminalConfig,
    value: &str,
) -> Result<(), String> {
    let parsed = match value {
        "true" | "1" | "on" => true,
        "false" | "0" | "off" => false,
        _ => return Err("environment-persistence must be true or false".to_string()),
    };
    let previous = config.clone();
    config.set_environment_persistence(parsed);
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

pub(super) fn update_cursor_color(
    filesystem: &mut dyn VirtualFileSystem,
    config: &mut TerminalConfig,
    value: &str,
) -> Result<(), String> {
    let cursor_color = TerminalCursorColor::parse(value)
        .ok_or_else(|| "cursor-color must be one of: cyan, ember, foreground".to_string())?;
    let previous = config.clone();
    config.set_cursor_color(cursor_color);
    if let Err(error) = config.save(filesystem) {
        *config = previous;
        return Err(format!("could not persist configuration: {error}"));
    }
    Ok(())
}

pub(super) fn update_cursor_shape(
    filesystem: &mut dyn VirtualFileSystem,
    config: &mut TerminalConfig,
    value: &str,
) -> Result<(), String> {
    let cursor_shape = TerminalCursorShape::parse(value)
        .ok_or_else(|| "cursor-shape must be one of: block, underline, bar".to_string())?;
    let previous = config.clone();
    config.set_cursor_shape(cursor_shape);
    if let Err(error) = config.save(filesystem) {
        *config = previous;
        return Err(format!("could not persist configuration: {error}"));
    }
    Ok(())
}

pub(super) fn update_font(
    filesystem: &mut dyn VirtualFileSystem,
    config: &mut TerminalConfig,
    value: &str,
) -> Result<(), String> {
    let font = TerminalFont::parse(value)
        .ok_or_else(|| "font must be one of: monospaced, system, rounded".to_string())?;
    let previous = config.clone();
    config.set_font(font);
    if let Err(error) = config.save(filesystem) {
        *config = previous;
        return Err(format!("could not persist configuration: {error}"));
    }
    Ok(())
}

pub(super) fn update_background(
    filesystem: &mut dyn VirtualFileSystem,
    config: &mut TerminalConfig,
    value: &str,
) -> Result<(), String> {
    let background = TerminalBackground::parse(value)
        .ok_or_else(|| "background must be one of: auto, black, white, slate".to_string())?;
    let previous = config.clone();
    config.set_background(background);
    if let Err(error) = config.save(filesystem) {
        *config = previous;
        return Err(format!("could not persist configuration: {error}"));
    }
    Ok(())
}

pub(super) fn update_foreground(
    filesystem: &mut dyn VirtualFileSystem,
    config: &mut TerminalConfig,
    value: &str,
) -> Result<(), String> {
    let foreground = TerminalForeground::parse(value)
        .ok_or_else(|| "foreground must be one of: auto, black, white, cyan, ember".to_string())?;
    let previous = config.clone();
    config.set_foreground(foreground);
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
        "history-redaction" => update_history_redaction(filesystem, config, value),
        "environment-persistence" => update_environment_persistence(filesystem, config, value),
        "font-size" => update_font_size(filesystem, config, value),
        "scrollback-limit" => update_scrollback_limit(filesystem, config, value),
        "toolbar-visible" => update_toolbar_visible(filesystem, config, value),
        "theme" => update_theme(filesystem, config, value),
        "cursor-color" => update_cursor_color(filesystem, config, value),
        "cursor-shape" => update_cursor_shape(filesystem, config, value),
        "font" => update_font(filesystem, config, value),
        "background" => update_background(filesystem, config, value),
        "foreground" => update_foreground(filesystem, config, value),
        _ => Err(
            "unknown key; available keys: history-limit, history-redaction, environment-persistence, font-size, scrollback-limit, toolbar-visible, theme, cursor-color, cursor-shape, font, background, foreground"
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
    let mut seen_history_redaction = false;
    let mut seen_environment_persistence = false;
    let mut seen_font_size = false;
    let mut seen_scrollback_limit = false;
    let mut seen_toolbar_visible = false;
    let mut seen_theme = false;
    let mut seen_cursor_color = false;
    let mut seen_cursor_shape = false;
    let mut seen_font = false;
    let mut seen_background = false;
    let mut seen_foreground = false;
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
            "history_redaction" if !seen_history_redaction => {
                config.history_redaction = match value {
                    "true" => true,
                    "false" => false,
                    _ => return None,
                };
                seen_history_redaction = true;
            }
            "environment_persistence" if !seen_environment_persistence => {
                config.environment_persistence = match value {
                    "true" => true,
                    "false" => false,
                    _ => return None,
                };
                seen_environment_persistence = true;
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
            "cursor_color" if !seen_cursor_color => {
                config.cursor_color = TerminalCursorColor::parse(value)?;
                seen_cursor_color = true;
            }
            "cursor_shape" if !seen_cursor_shape => {
                config.cursor_shape = TerminalCursorShape::parse(value)?;
                seen_cursor_shape = true;
            }
            "font" if !seen_font => {
                config.font = TerminalFont::parse(value)?;
                seen_font = true;
            }
            "background" if !seen_background => {
                config.background = TerminalBackground::parse(value)?;
                seen_background = true;
            }
            "foreground" if !seen_foreground => {
                config.foreground = TerminalForeground::parse(value)?;
                seen_foreground = true;
            }
            _ => return None,
        }
    }
    Some(config)
}

#[cfg(test)]
mod tests {
    use super::{
        parse, TerminalBackground, TerminalConfig, TerminalCursorColor, TerminalCursorShape,
        TerminalFont, TerminalForeground, TerminalTheme, CONFIG_HEADER, DEFAULT_BACKGROUND,
        DEFAULT_CURSOR_COLOR, DEFAULT_CURSOR_SHAPE, DEFAULT_ENVIRONMENT_PERSISTENCE, DEFAULT_FONT,
        DEFAULT_FONT_SIZE, DEFAULT_FOREGROUND, DEFAULT_HISTORY_REDACTION, DEFAULT_SCROLLBACK_LIMIT,
        DEFAULT_THEME, DEFAULT_TOOLBAR_VISIBLE,
    };

    #[test]
    fn parses_bounded_history_configuration() {
        let content = format!("{CONFIG_HEADER}\nhistory_limit=25\n");
        let config = parse(&content).expect("configuration should parse");
        assert_eq!(config.history_limit(), 25);
        assert_eq!(config.history_redaction(), DEFAULT_HISTORY_REDACTION);
        assert_eq!(
            config.environment_persistence(),
            DEFAULT_ENVIRONMENT_PERSISTENCE
        );
        assert_eq!(config.font_size(), DEFAULT_FONT_SIZE);
        assert_eq!(config.scrollback_limit(), DEFAULT_SCROLLBACK_LIMIT);
        assert_eq!(config.toolbar_visible(), DEFAULT_TOOLBAR_VISIBLE);
        assert_eq!(config.theme(), DEFAULT_THEME);
        assert_eq!(config.cursor_color(), DEFAULT_CURSOR_COLOR);
        assert_eq!(config.cursor_shape(), DEFAULT_CURSOR_SHAPE);
        assert_eq!(config.font(), DEFAULT_FONT);
        assert_eq!(config.background(), DEFAULT_BACKGROUND);
        assert_eq!(config.foreground(), DEFAULT_FOREGROUND);
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
        let redaction = format!("{content}history_redaction=false\n");
        assert!(!parse(&redaction)
            .expect("history redaction should parse")
            .history_redaction());
        let environment = format!("{content}environment_persistence=true\n");
        assert!(parse(&environment)
            .expect("environment persistence should parse")
            .environment_persistence());
        assert!(parse("RUNE_CONFIG_V1\nfont_size=7\n").is_none());
        assert!(parse("RUNE_CONFIG_V1\nfont_size=33\n").is_none());
        assert!(parse("RUNE_CONFIG_V1\nscrollback_limit=127\n").is_none());
        assert!(parse("RUNE_CONFIG_V1\nscrollback_limit=8193\n").is_none());
        assert!(parse("RUNE_CONFIG_V1\ntoolbar_visible=maybe\n").is_none());
        assert!(parse("RUNE_CONFIG_V1\ntheme=unknown\n").is_none());
        let cursor = format!("{content}cursor_color=ember\n");
        assert_eq!(
            parse(&cursor)
                .expect("cursor color should parse")
                .cursor_color(),
            TerminalCursorColor::Ember
        );
        assert!(parse("RUNE_CONFIG_V1\ncursor_color=violet\n").is_none());
        let cursor_shape = format!("{content}cursor_shape=underline\n");
        assert_eq!(
            parse(&cursor_shape)
                .expect("cursor shape should parse")
                .cursor_shape(),
            TerminalCursorShape::Underline
        );
        assert!(parse("RUNE_CONFIG_V1\ncursor_shape=diamond\n").is_none());
        let font = format!("{content}font=rounded\n");
        assert_eq!(
            parse(&font).expect("font should parse").font(),
            TerminalFont::Rounded
        );
        assert!(parse("RUNE_CONFIG_V1\nfont=serif\n").is_none());
        let background = format!("{content}background=slate\n");
        assert_eq!(
            parse(&background)
                .expect("background should parse")
                .background(),
            TerminalBackground::Slate
        );
        let foreground = format!("{content}foreground=ember\n");
        assert_eq!(
            parse(&foreground)
                .expect("foreground should parse")
                .foreground(),
            TerminalForeground::Ember
        );
        assert!(parse("RUNE_CONFIG_V1\nbackground=purple\n").is_none());
        assert!(parse("RUNE_CONFIG_V1\nforeground=purple\n").is_none());
    }

    #[test]
    fn rejects_unknown_or_out_of_range_configuration() {
        assert!(parse("RUNE_CONFIG_V1\nunknown=value\n").is_none());
        assert!(parse("RUNE_CONFIG_V1\nhistory_limit=0\n").is_none());
        assert!(parse("RUNE_CONFIG_V1\nhistory_limit=10001\n").is_none());
        assert!(parse("RUNE_CONFIG_V1\nhistory_redaction=maybe\n").is_none());
        assert!(parse("RUNE_CONFIG_V1\nenvironment_persistence=maybe\n").is_none());
    }

    #[test]
    fn defaults_when_configuration_is_missing_or_malformed() {
        assert_eq!(TerminalConfig::default().history_limit(), 1_000);
        assert_eq!(
            TerminalConfig::default().history_redaction(),
            DEFAULT_HISTORY_REDACTION
        );
        assert_eq!(
            TerminalConfig::default().environment_persistence(),
            DEFAULT_ENVIRONMENT_PERSISTENCE
        );
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
        assert_eq!(
            TerminalConfig::default().cursor_color(),
            DEFAULT_CURSOR_COLOR
        );
        assert_eq!(
            TerminalConfig::default().cursor_shape(),
            DEFAULT_CURSOR_SHAPE
        );
        assert_eq!(TerminalConfig::default().font(), DEFAULT_FONT);
        assert_eq!(TerminalConfig::default().background(), DEFAULT_BACKGROUND);
        assert_eq!(TerminalConfig::default().foreground(), DEFAULT_FOREGROUND);
        assert!(parse("not-rune-config\n").is_none());
    }
}
