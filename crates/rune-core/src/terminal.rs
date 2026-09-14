//! Bounded terminal-screen state for Rust-owned output handling.
//!
//! This is intentionally a small terminal grid rather than a claim of xterm
//! compatibility. It provides the cursor and erase semantics needed by common
//! command-line progress output while keeping all memory and parser state
//! bounded and independent from Apple UI frameworks.

pub const DEFAULT_COLUMNS: usize = 120;
pub const DEFAULT_ROWS: usize = 4_096;
const MAX_COLUMNS: usize = 512;
const MAX_ROWS: usize = 8_192;
const MAX_CSI_BYTES: usize = 1_024;
const MAX_OSC_BYTES: usize = 4 * 1_024;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParserState {
    Ground,
    Escape,
    Csi(String),
    Osc { bytes: usize, escaped: bool },
}

/// A bounded character grid with a streaming ANSI control parser.
#[derive(Debug, Clone)]
pub struct TerminalScreen {
    columns: usize,
    rows: usize,
    cells: Vec<Vec<char>>,
    cursor_row: usize,
    cursor_column: usize,
    saved_cursor: (usize, usize),
    scroll_top: usize,
    scroll_bottom: usize,
    parser: ParserState,
}

impl Default for TerminalScreen {
    fn default() -> Self {
        Self::new(DEFAULT_COLUMNS, DEFAULT_ROWS)
    }
}

impl TerminalScreen {
    /// Creates a screen, clamping dimensions to the Rust-owned bounds.
    #[must_use]
    pub fn new(columns: usize, rows: usize) -> Self {
        let columns = columns.clamp(1, MAX_COLUMNS);
        let rows = rows.clamp(1, MAX_ROWS);
        Self {
            columns,
            rows,
            cells: vec![vec![' '; columns]; rows],
            cursor_row: 0,
            cursor_column: 0,
            saved_cursor: (0, 0),
            scroll_top: 0,
            scroll_bottom: rows - 1,
            parser: ParserState::Ground,
        }
    }

    /// Feeds one UTF-8 chunk. Parser state survives across calls, so a CSI
    /// or OSC split by Rust event chunking is not rendered as user text.
    pub fn feed(&mut self, input: &str) {
        for character in input.chars() {
            self.consume(character);
        }
    }

    /// Clears the grid and parser state while preserving its dimensions.
    pub fn reset(&mut self) {
        for row in &mut self.cells {
            row.fill(' ');
        }
        self.cursor_row = 0;
        self.cursor_column = 0;
        self.saved_cursor = (0, 0);
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
        self.parser = ParserState::Ground;
    }

    /// Returns the visible rows through the last non-blank row.
    #[must_use]
    pub fn snapshot(&self) -> String {
        let last_row = self
            .cells
            .iter()
            .rposition(|row| row.iter().any(|character| *character != ' '));
        let Some(last_row) = last_row else {
            return String::new();
        };

        let mut snapshot = String::new();
        for (row_index, row) in self.cells.iter().take(last_row + 1).enumerate() {
            let end = row
                .iter()
                .rposition(|character| *character != ' ')
                .map_or(0, |index| index + 1);
            if row_index > 0 {
                snapshot.push('\n');
            }
            row[..end]
                .iter()
                .for_each(|character| snapshot.push(*character));
        }
        snapshot
    }

    /// Returns the current zero-based cursor position.
    #[must_use]
    pub fn cursor_position(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_column.min(self.columns))
    }

    fn consume(&mut self, character: char) {
        let parser = std::mem::replace(&mut self.parser, ParserState::Ground);
        match parser {
            ParserState::Ground => self.consume_ground(character),
            ParserState::Escape => self.consume_escape(character),
            ParserState::Csi(mut parameters) => {
                if is_csi_final(character) {
                    self.apply_csi(&parameters, character);
                } else if parameters.len() < MAX_CSI_BYTES {
                    parameters.push(character);
                    self.parser = ParserState::Csi(parameters);
                }
            }
            ParserState::Osc { bytes, escaped } => {
                if escaped && character == '\\' {
                    return;
                }
                if character == '\x07' {
                    return;
                }
                if character == '\x1b' {
                    self.parser = ParserState::Osc {
                        bytes: bytes.min(MAX_OSC_BYTES),
                        escaped: true,
                    };
                } else if bytes < MAX_OSC_BYTES {
                    self.parser = ParserState::Osc {
                        bytes: bytes + character.len_utf8(),
                        escaped: false,
                    };
                }
            }
        }
    }

    fn consume_ground(&mut self, character: char) {
        match character {
            '\x1b' => self.parser = ParserState::Escape,
            '\0' | '\x07' | '\x0b' | '\x0c' => {}
            '\r' => self.cursor_column = 0,
            '\n' => self.line_feed(),
            '\x08' => self.cursor_column = self.cursor_column.saturating_sub(1),
            '\t' => {
                let next = ((self.cursor_column / 8) + 1) * 8;
                self.cursor_column = next.min(self.columns);
            }
            character if !character.is_control() => self.write(character),
            _ => {}
        }
    }

    fn consume_escape(&mut self, character: char) {
        match character {
            '[' => self.parser = ParserState::Csi(String::new()),
            ']' => {
                self.parser = ParserState::Osc {
                    bytes: 0,
                    escaped: false,
                }
            }
            '7' => self.saved_cursor = self.cursor_position(),
            '8' => self.restore_cursor(),
            'c' => self.reset(),
            _ => self.consume_ground(character),
        }
    }

    fn apply_csi(&mut self, parameters: &str, final_character: char) {
        let values = parse_parameters(parameters);
        let first = |default: usize| values.first().copied().unwrap_or(default);
        match final_character {
            'A' => self.cursor_row = self.cursor_row.saturating_sub(first(1)),
            'B' | 'e' => self.cursor_down(first(1)),
            'C' | 'a' => {
                self.cursor_column = self
                    .cursor_column
                    .saturating_add(first(1))
                    .min(self.columns);
            }
            'D' => self.cursor_column = self.cursor_column.saturating_sub(first(1)),
            'E' => {
                self.cursor_down(first(1));
                self.cursor_column = 0;
            }
            'F' => {
                self.cursor_row = self.cursor_row.saturating_sub(first(1));
                self.cursor_column = 0;
            }
            'G' | '`' => self.cursor_column = first(1).saturating_sub(1).min(self.columns),
            'd' => self.cursor_row = first(1).saturating_sub(1).min(self.rows - 1),
            'H' | 'f' => {
                self.cursor_row = values
                    .first()
                    .copied()
                    .unwrap_or(1)
                    .saturating_sub(1)
                    .min(self.rows - 1);
                self.cursor_column = values
                    .get(1)
                    .copied()
                    .unwrap_or(1)
                    .saturating_sub(1)
                    .min(self.columns);
            }
            'J' => self.erase_display(first(0)),
            'K' => self.erase_line(first(0)),
            'P' => self.delete_characters(first(1)),
            '@' => self.insert_characters(first(1)),
            'X' => self.erase_characters(first(1)),
            'r' => self.set_scroll_region(&values),
            's' => self.saved_cursor = self.cursor_position(),
            'u' => self.restore_cursor(),
            // SGR and mode changes are intentionally state-free here. The
            // native renderer can still apply styles to its raw event text.
            _ => {}
        }
    }

    fn write(&mut self, character: char) {
        if self.cursor_column >= self.columns {
            self.line_feed();
        }
        self.cells[self.cursor_row][self.cursor_column] = character;
        self.cursor_column += 1;
    }

    fn line_feed(&mut self) {
        self.cursor_column = 0;
        if self.cursor_row < self.scroll_top || self.cursor_row > self.scroll_bottom {
            self.cursor_row = self.cursor_row.saturating_add(1).min(self.rows - 1);
        } else if self.cursor_row < self.scroll_bottom {
            self.cursor_row += 1;
        } else {
            self.scroll_region_up();
        }
    }

    fn cursor_down(&mut self, amount: usize) {
        let limit = if (self.scroll_top..=self.scroll_bottom).contains(&self.cursor_row) {
            self.scroll_bottom
        } else {
            self.rows - 1
        };
        self.cursor_row = self.cursor_row.saturating_add(amount).min(limit);
    }

    fn scroll_region_up(&mut self) {
        let region = &mut self.cells[self.scroll_top..=self.scroll_bottom];
        region.rotate_left(1);
        if let Some(last_row) = region.last_mut() {
            last_row.fill(' ');
        }
    }

    fn set_scroll_region(&mut self, values: &[usize]) {
        let top = values.first().copied().unwrap_or(1);
        let bottom = values.get(1).copied().unwrap_or(self.rows);
        if top == 0 || bottom == 0 || top > bottom || bottom > self.rows {
            return;
        }
        self.scroll_top = top - 1;
        self.scroll_bottom = bottom - 1;
        self.cursor_row = 0;
        self.cursor_column = 0;
    }

    fn restore_cursor(&mut self) {
        self.cursor_row = self.saved_cursor.0.min(self.rows - 1);
        self.cursor_column = self.saved_cursor.1.min(self.columns);
    }

    fn erase_display(&mut self, mode: usize) {
        match mode {
            0 => {
                self.erase_line(0);
                for row in self.cells.iter_mut().skip(self.cursor_row + 1) {
                    row.fill(' ');
                }
            }
            1 => {
                for row in self.cells.iter_mut().take(self.cursor_row) {
                    row.fill(' ');
                }
                self.cells[self.cursor_row][..=self.cursor_column.min(self.columns - 1)].fill(' ');
            }
            2 | 3 => {
                for row in &mut self.cells {
                    row.fill(' ');
                }
                self.cursor_row = 0;
                self.cursor_column = 0;
            }
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: usize) {
        match mode {
            0 => self.cells[self.cursor_row][self.cursor_column.min(self.columns - 1)..].fill(' '),
            1 => self.cells[self.cursor_row][..=self.cursor_column.min(self.columns - 1)].fill(' '),
            2 => self.cells[self.cursor_row].fill(' '),
            _ => {}
        }
    }

    fn erase_characters(&mut self, amount: usize) {
        let start = self.cursor_column.min(self.columns);
        let end = start.saturating_add(amount).min(self.columns);
        if start < end {
            self.cells[self.cursor_row][start..end].fill(' ');
        }
    }

    fn delete_characters(&mut self, amount: usize) {
        let row = &mut self.cells[self.cursor_row];
        let start = self.cursor_column.min(self.columns);
        let amount = amount.min(self.columns.saturating_sub(start));
        if amount == 0 {
            return;
        }
        row[start..].rotate_left(amount);
        row[self.columns - amount..].fill(' ');
    }

    fn insert_characters(&mut self, amount: usize) {
        let row = &mut self.cells[self.cursor_row];
        let start = self.cursor_column.min(self.columns);
        let amount = amount.min(self.columns.saturating_sub(start));
        if amount == 0 {
            return;
        }
        row[start..].rotate_right(amount);
        row[start..start + amount].fill(' ');
    }
}

fn is_csi_final(character: char) -> bool {
    ('@'..='~').contains(&character)
}

fn parse_parameters(parameters: &str) -> Vec<usize> {
    parameters
        .trim_start_matches(['?', '>', '<', '!'])
        .split(';')
        .filter_map(|value| value.parse::<usize>().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::TerminalScreen;

    #[test]
    fn applies_cursor_addressing_and_erase_controls() {
        let mut screen = TerminalScreen::new(12, 4);
        screen.feed("hello\nworld");
        screen.feed("\x1b[1;2H");
        screen.feed("X");
        assert_eq!(screen.snapshot(), "hXllo\nworld");
        screen.feed("\x1b[2K");
        assert_eq!(screen.snapshot(), "\nworld");
        screen.feed("\x1b[2J\x1b[Hdone");
        assert_eq!(screen.snapshot(), "done");
    }

    #[test]
    fn preserves_parser_state_across_split_control_sequences() {
        let mut screen = TerminalScreen::new(12, 4);
        screen.feed("progress");
        screen.feed("\r\x1b[");
        screen.feed("2K");
        screen.feed("ready");
        assert_eq!(screen.snapshot(), "ready");
    }

    #[test]
    fn scrolls_bounded_rows_and_handles_insert_delete_characters() {
        let mut screen = TerminalScreen::new(8, 2);
        screen.feed("abcdef\n123");
        assert_eq!(screen.snapshot(), "abcdef\n123");
        screen.feed("\x1b[1;2H\x1b[2@");
        assert_eq!(screen.snapshot(), "a  bcdef\n123");
        screen.feed("\x1b[1;2H\x1b[2P");
        assert_eq!(screen.snapshot(), "abcdef\n123");
    }

    #[test]
    fn scrolls_only_inside_a_configured_region_and_resets_on_full_reset() {
        let mut screen = TerminalScreen::new(8, 4);
        screen.feed("a\nb\nc\nd");
        screen.feed("\x1b[2;3r");
        assert_eq!(screen.cursor_position(), (0, 0));
        screen.feed("\x1b[2;1HX\nY\nZ");
        assert_eq!(screen.snapshot(), "a\nY\nZ\nd");
        screen.feed("\x1bc");
        screen.feed("reset");
        assert_eq!(screen.snapshot(), "reset");
    }

    #[test]
    fn bounds_dimensions_and_ignores_osc_payloads() {
        let mut screen = TerminalScreen::new(12, 4);
        screen.feed("\x1b]0;Rune title\x07ok");
        assert_eq!(screen.snapshot(), "ok");
        assert_eq!(screen.cursor_position(), (0, 2));
    }
}
