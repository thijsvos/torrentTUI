use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders},
    Frame,
};
use tui_textarea::TextArea;

pub struct InputWidget<'a> {
    pub textarea: TextArea<'a>,
}

impl<'a> InputWidget<'a> {
    pub fn new() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_block(
            Block::default()
                .title(" Add Torrent (magnet link or .torrent path) ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        textarea.set_cursor_line_style(Style::default());
        textarea.set_placeholder_text("magnet:?xt=urn:btih:... or /path/to/file.torrent");
        Self { textarea }
    }

    pub fn value(&self) -> String {
        self.textarea.lines()[0].clone()
    }

    pub fn clear(&mut self) {
        self.textarea.select_all();
        self.textarea.cut();
    }
}

pub fn render_input(f: &mut Frame, area: Rect, input: &InputWidget) {
    f.render_widget(&input.textarea, area);
}

pub fn validate_torrent_source(input: &str) -> Result<(), String> {
    let input = input.trim();

    // Check if it's a .torrent file path
    if input.ends_with(".torrent") {
        let path = std::path::Path::new(input);
        if path.exists() {
            return Ok(());
        } else {
            return Err(format!("File not found: {}", input));
        }
    }

    // Otherwise validate as magnet link
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
