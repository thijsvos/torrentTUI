//! The command-palette overlay: a centered popup with a filter line and the
//! scored action list from `App::palette_matches`. Drawn on top of whatever
//! view the palette opened over, using the same `centered_rect` + `Clear`
//! pattern as the help overlay and dialogs.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table},
    Frame,
};

use crate::actions::{keys_display, tui_description};
use crate::app::App;
use crate::ui::util::centered_rect;

pub fn render_palette(f: &mut Frame, area: Rect, app: &mut App) {
    let popup = centered_rect(55, 60, area);
    f.render_widget(Clear, popup);

    let block = Block::default()
        .title(" Command Palette ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);

    let input_line = Line::from(vec![
        Span::styled(" > ", Style::default().fg(Color::Cyan)),
        Span::raw(app.palette.input.as_str()),
        Span::styled("\u{2588}", Style::default().fg(Color::White)),
    ]);
    f.render_widget(Paragraph::new(input_line), chunks[0]);

    let matches = app.palette_matches();
    if matches.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "No matching actions",
            Style::default().fg(Color::DarkGray),
        )))
        .centered();
        f.render_widget(empty, chunks[1]);
        return;
    }

    // The cursor follows the anchored action wherever the (re-filtered)
    // list puts it this frame; a vanished anchor falls back to the top.
    let selected = app.palette_selected_index(&matches);
    app.palette.table_state.select(Some(selected));

    let rows: Vec<Row> = matches
        .iter()
        .map(|a| {
            Row::new(vec![
                Cell::from(tui_description(a)),
                Cell::from(keys_display(a)).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    let table = Table::new(rows, [Constraint::Min(24), Constraint::Length(18)])
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("\u{25b6} ");

    f.render_stateful_widget(table, chunks[1], &mut app.palette.table_state);
}
