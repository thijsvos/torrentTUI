//! The theme-selector overlay: a centered popup listing the built-in themes
//! from `crate::theme::THEMES`. The App applies the highlighted theme live, so
//! this overlay — and the view behind it — re-colour as the cursor moves; a
//! `\u{2713}` marks the theme currently saved, which is not the same as the one
//! being previewed until Enter commits.
//!
//! Follows `palette.rs`'s centered `Clear` + `Block` pattern, and reuses the
//! theme table's `TableState` from `App::theme_select` so a long list scrolls
//! with the highlight the way every other table in the app does.

use ratatui::{
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Row, Table},
    Frame,
};

use crate::app::App;
use crate::theme::THEMES;
use crate::ui::util::centered_rect;

/// A one-cell colour chip; six of them show a theme's whole palette at a glance.
const SWATCH: &str = "\u{2588}";

pub fn render_theme_select(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = app.theme;
    let popup = centered_rect(72, 70, area);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .title(" Theme \u{2014} \u{2191}/\u{2193} preview  Enter apply  Esc cancel ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.primary));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let rows: Vec<Row> = THEMES
        .iter()
        .map(|entry| {
            let t = entry.theme;
            let mut swatch_spans = Vec::new();
            for (i, color) in [t.primary, t.accent, t.success, t.warning, t.error, t.info]
                .into_iter()
                .enumerate()
            {
                if i > 0 {
                    swatch_spans.push(Span::raw(" "));
                }
                swatch_spans.push(Span::styled(SWATCH, Style::default().fg(color)));
            }
            // `\u{2713}` marks the persisted theme, which stays put while the
            // preview moves — so "what will I keep" is never ambiguous.
            let marker = if entry.id == app.theme_name {
                "\u{2713} "
            } else {
                "  "
            };
            Row::new(vec![
                Cell::from(format!("{marker}{}", entry.name)),
                Cell::from(entry.id).style(Style::default().fg(theme.muted)),
                Cell::from(Line::from(swatch_spans)),
            ])
            .style(Style::default().fg(theme.text))
        })
        .collect();

    // Anchor the cursor to the previewed row; ratatui scrolls it into view.
    app.theme_select
        .table_state
        .select(Some(app.theme_select.index));

    let table = Table::new(
        rows,
        [
            Constraint::Min(18),    // name (+ saved marker)
            Constraint::Length(15), // id
            Constraint::Length(13), // six swatches + separators
        ],
    )
    .row_highlight_style(
        Style::default()
            .bg(theme.selection_bg)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("\u{25b6} ");

    f.render_stateful_widget(table, inner, &mut app.theme_select.table_state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn screen_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn lists_the_themes_and_survives_a_tiny_terminal() {
        let mut app = App::new();
        app.open_theme_select();
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal
            .draw(|f| render_theme_select(f, f.area(), &mut app))
            .unwrap();
        let text = screen_text(&terminal);
        assert!(text.contains("Theme"), "{text}");
        assert!(text.contains(THEMES[0].name), "first theme missing: {text}");

        // A terminal too small for the popup must clamp, not panic.
        let mut tiny = Terminal::new(TestBackend::new(12, 5)).unwrap();
        tiny.draw(|f| render_theme_select(f, f.area(), &mut app))
            .unwrap();
    }

    #[test]
    fn marks_the_saved_theme_even_while_previewing_another() {
        let mut app = App::new();
        app.set_theme("gruvbox");
        app.open_theme_select();
        // Preview the next theme; the tick stays on the saved one.
        app.theme_select_move(1);
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal
            .draw(|f| render_theme_select(f, f.area(), &mut app))
            .unwrap();
        let text = screen_text(&terminal);
        assert!(
            text.contains('\u{2713}'),
            "saved-theme marker missing: {text}"
        );
    }
}
