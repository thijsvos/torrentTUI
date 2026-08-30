use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

const PLACEHOLDER: &str = "magnet:?xt=urn:btih:... or /path/to/file.torrent";

/// Cap on the add-torrent buffer. Magnet links with a long tracker list run
/// 1–2 KB, so this leaves generous headroom while keeping an accidental
/// paste of the wrong clipboard buffer from growing the string unbounded.
const MAX_INPUT_CHARS: usize = 4096;

pub struct InputWidget {
    buffer: String,
}

impl Default for InputWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl InputWidget {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    pub fn value(&self) -> String {
        self.buffer.clone()
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    pub fn push(&mut self, c: char) {
        // Reject control chars (e.g. \x1b from an OSC sequence, \x07 BEL,
        // raw newlines from a multi-line paste). Tabs are also dropped here
        // since the input is a single-line magnet/path.
        if c.is_control() || self.buffer.chars().count() >= MAX_INPUT_CHARS {
            return;
        }
        self.buffer.push(c);
    }

    /// Push a string (e.g. the payload of a bracketed-paste event), filtering
    /// control characters character-by-character. Tracks the length locally
    /// and breaks at the cap: routing a multi-megabyte paste through `push`
    /// would recount the whole buffer per character — O(paste × cap) on the
    /// event loop, the same freeze class the cap exists to prevent.
    pub fn push_str(&mut self, s: &str) {
        let mut len = self.buffer.chars().count();
        for c in s.chars() {
            if len >= MAX_INPUT_CHARS {
                break;
            }
            if c.is_control() {
                continue;
            }
            self.buffer.push(c);
            len += 1;
        }
    }

    pub fn pop(&mut self) {
        self.buffer.pop();
    }
}

pub fn render_input(f: &mut Frame, area: Rect, input: &InputWidget) {
    let title = " Add Torrent (magnet link or .torrent path) ";
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let line = if input.buffer.is_empty() {
        Line::from(vec![
            Span::styled(PLACEHOLDER, Style::default().fg(Color::DarkGray)),
            Span::styled("\u{2588}", Style::default().fg(Color::White)),
        ])
    } else {
        Line::from(vec![
            Span::raw(input.buffer.as_str()),
            Span::styled("\u{2588}", Style::default().fg(Color::White)),
        ])
    };

    let widget = Paragraph::new(line).block(block);
    f.render_widget(widget, area);
}

/// Pre-flight check on what the user typed, so an obvious mistake produces a
/// status-bar message instead of a round trip to the engine. Dispatches on the
/// literal `.torrent` suffix: anything ending in it is checked for existence
/// only — not readability, size or content — and everything else is validated
/// as a magnet link. `Ok` therefore means "worth handing to the engine", not
/// "will succeed"; the engine re-checks size and parses for real.
///
/// Expects an already tilde-expanded path; both callers run `expand_tilde`
/// first.
pub fn validate_torrent_source(input: &str) -> Result<(), String> {
    let input = input.trim();

    if input.ends_with(".torrent") {
        let path = std::path::Path::new(input);
        if path.exists() {
            return Ok(());
        } else {
            return Err(format!("File not found: {}", input));
        }
    }

    validate_magnet(input)
}

pub fn validate_magnet(link: &str) -> Result<(), String> {
    let link = link.trim();
    if !link.starts_with("magnet:?") {
        return Err("Must be a magnet link (magnet:?) or .torrent file path".to_string());
    }
    if !link.contains("xt=urn:btih:") {
        return Err("Must contain 'xt=urn:btih:'".to_string());
    }

    if let Some(pos) = link.find("xt=urn:btih:") {
        let after = &link[pos + 12..];
        let hash = after.split('&').next().unwrap_or("");
        if hash.len() == 40 {
            if !hash.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err("Invalid hex info hash".to_string());
            }
        } else if hash.len() == 32 {
            if !hash.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err("Invalid base32 info hash".to_string());
            }
        } else {
            return Err(format!(
                "Info hash must be 40 hex or 32 base32 chars, got {}",
                hash.len()
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_buffer_starts_empty() {
        let w = InputWidget::new();
        assert_eq!(w.value(), "");
    }

    #[test]
    fn push_caps_the_buffer() {
        let mut w = InputWidget::new();
        w.push_str(&"m".repeat(MAX_INPUT_CHARS + 100));
        assert_eq!(w.value().chars().count(), MAX_INPUT_CHARS);
        // Still capped one char at a time as well.
        w.push('x');
        assert_eq!(w.value().chars().count(), MAX_INPUT_CHARS);
    }

    #[test]
    fn input_buffer_push_pop() {
        let mut w = InputWidget::new();
        w.push('a');
        w.push('b');
        w.push('c');
        assert_eq!(w.value(), "abc");
        w.pop();
        assert_eq!(w.value(), "ab");
    }

    #[test]
    fn input_buffer_clear() {
        let mut w = InputWidget::new();
        w.push('x');
        w.clear();
        assert_eq!(w.value(), "");
    }

    #[test]
    fn input_drops_control_chars() {
        let mut w = InputWidget::new();
        w.push('a');
        w.push('\x1b'); // ESC
        w.push('\x07'); // BEL
        w.push('\n');
        w.push('\t');
        w.push('b');
        assert_eq!(w.value(), "ab");
    }

    #[test]
    fn input_push_str_filters_controls() {
        let mut w = InputWidget::new();
        w.push_str("magnet:?\x1b[31mxt=urn:btih:0123456789abcdef0123456789abcdef01234567\n");
        // ANSI escape, the literal '[31m' bytes (printable), and the trailing
        // newline are filtered: \x1b and \n are control chars; '[31m' is
        // printable so it stays. The hash payload itself is unchanged.
        assert!(w.value().starts_with("magnet:?[31mxt=urn:btih:"));
        assert!(!w.value().contains('\n'));
        assert!(!w.value().contains('\x1b'));
    }

    #[test]
    fn valid_hex_magnet() {
        let link = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
        assert!(validate_magnet(link).is_ok());
    }

    #[test]
    fn valid_hex_magnet_with_params() {
        let link = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=test";
        assert!(validate_magnet(link).is_ok());
    }

    #[test]
    fn valid_base32_magnet() {
        let link = "magnet:?xt=urn:btih:ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
        assert!(validate_magnet(link).is_ok());
    }

    #[test]
    fn missing_magnet_prefix() {
        assert!(validate_magnet("http://example.com").is_err());
    }

    #[test]
    fn missing_btih() {
        assert!(validate_magnet("magnet:?dn=test").is_err());
    }

    #[test]
    fn wrong_hash_length() {
        let link = "magnet:?xt=urn:btih:0123456789";
        let err = validate_magnet(link).unwrap_err();
        assert!(err.contains("40 hex or 32 base32"));
    }

    #[test]
    fn invalid_hex_chars() {
        let link = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef0123456g";
        assert!(validate_magnet(link).is_err());
    }

    #[test]
    fn whitespace_trimmed() {
        let link = "  magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567  ";
        assert!(validate_magnet(link).is_ok());
    }

    #[test]
    fn torrent_file_not_found() {
        assert!(validate_torrent_source("/nonexistent/path.torrent").is_err());
    }
}
