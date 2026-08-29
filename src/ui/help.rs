//! The keybinding help overlay. Rendered entirely from the action registry
//! in `crate::actions`, so it can no longer drift from the real handlers or
//! the README the way the old hand-maintained table did (it had drifted:
//! Ctrl+C was missing, Esc's role was wrong, Detail's `o` was absent).
//!
//! The full registry outgrows a 24-line terminal, so the overlay scrolls
//! (`j`/`k`); `App::help_scroll` is reset when help opens and clamped here
//! against the actual visible height.

use crate::actions::{keys_display, tui_description, Section, ACTIONS};
use crate::app::App;
use crate::ui::util::{centered_rect, pad_to_width};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

const KEY_COL_WIDTH: usize = 18;

/// The rows the overlay shows, in registry order: the main section, then the
/// search-results and detail-view sections under their headers. Split from
/// the renderer so a test can assert the content without a terminal.
pub fn help_rows() -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    let section = |rows: &mut Vec<(String, String)>, s: Section, header: Option<&str>| {
        if let Some(h) = header {
            rows.push((String::new(), String::new()));
            rows.push((String::new(), h.to_string()));
        }
        for a in ACTIONS.iter().filter(|a| a.section == s) {
            rows.push((keys_display(a), tui_description(a).to_string()));
        }
    };
    section(&mut rows, Section::Main, None);
    section(&mut rows, Section::Search, Some("-- Search Results --"));
    section(&mut rows, Section::Detail, Some("-- Detail View --"));
    rows
}

pub fn render_help(f: &mut Frame, area: Rect, app: &mut App) {
    let popup = centered_rect(65, 90, area);
    f.render_widget(Clear, popup);

    let rows = help_rows();
    let lines: Vec<Line> = rows
        .iter()
        .map(|(key, action)| {
            Line::from(vec![
                Span::styled(
                    pad_to_width(key, KEY_COL_WIDTH),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::raw(action.clone()),
            ])
        })
        .collect();

    // Clamp the scroll so the last page stays full instead of scrolling into
    // blank space; also heals a stale offset after a resize.
    let visible = popup.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(visible) as u16;
    app.help_scroll = app.help_scroll.min(max_scroll);

    let title = if max_scroll > 0 {
        " Help \u{2014} Keybindings (j/k to scroll) "
    } else {
        " Help \u{2014} Keybindings "
    };
    let widget = Paragraph::new(lines)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .style(Style::default().fg(Color::White))
        .scroll((app.help_scroll, 0));

    f.render_widget(widget, popup);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three drift bugs the registry migration fixed must stay fixed.
    #[test]
    fn help_content_comes_from_the_registry() {
        let rows = help_rows();
        let has =
            |key: &str, needle: &str| rows.iter().any(|(k, a)| k == key && a.contains(needle));
        assert!(has(": / Ctrl+P", "command palette"));
        assert!(
            has("Ctrl+C", "double press to force"),
            "Ctrl+C was the missing drift row"
        );
        assert!(
            has("Esc", "Clear marks"),
            "Esc's clear-marks role was the wrong drift row"
        );
        // Detail section must list `o` — the third drift bug.
        let detail_start = rows
            .iter()
            .position(|(_, a)| a == "-- Detail View --")
            .expect("detail header");
        assert!(
            rows[detail_start..]
                .iter()
                .any(|(k, a)| k == "o" && a.contains("file manager")),
            "Detail `o` was the absent drift row"
        );
    }

    fn screen_text(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn render_help_draws_on_a_test_backend() {
        // Prove the overlay actually paints the registry content (and
        // survives a small terminal).
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = App::new();
        let mut terminal = Terminal::new(TestBackend::new(100, 48)).unwrap();
        terminal
            .draw(|f| render_help(f, f.area(), &mut app))
            .unwrap();
        let screen = screen_text(&terminal);
        assert!(screen.contains("Ctrl+C"));
        assert!(screen.contains("Detail View"));
        // Tiny terminal must not panic.
        let mut tiny = Terminal::new(TestBackend::new(10, 4)).unwrap();
        tiny.draw(|f| render_help(f, f.area(), &mut app)).unwrap();
    }

    #[test]
    fn help_scrolls_the_tail_into_view_on_short_terminals() {
        // On a 24-line terminal the Detail section starts off-screen; the
        // clamped scroll must bring it in (and not scroll past the end).
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = App::new();
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|f| render_help(f, f.area(), &mut app))
            .unwrap();
        assert!(
            !screen_text(&terminal).contains("Detail View"),
            "detail section should start off-screen at 24 lines"
        );
        app.help_scroll = u16::MAX; // renderer clamps to the real maximum
        terminal
            .draw(|f| render_help(f, f.area(), &mut app))
            .unwrap();
        let screen = screen_text(&terminal);
        assert!(
            screen.contains("Detail View"),
            "scroll must reveal the tail"
        );
        assert!(app.help_scroll < 60, "offset must be clamped, not u16::MAX");
    }
}
