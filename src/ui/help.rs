use crate::ui::util::centered_rect;
use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Clear, Row, Table},
    Frame,
};

/// Draw the keybinding overlay. The table is hand-maintained and has no link
/// to the handlers in main.rs or the hint strings in `render_status_bar`, so a
/// new keybinding needs editing in all three places — nothing here fails if
/// they drift.
pub fn render_help(f: &mut Frame, area: Rect) {
    let popup = centered_rect(60, 80, area);
    f.render_widget(Clear, popup);

    let keybindings = vec![
        ("a", "Add magnet link or .torrent file"),
        ("p", "(Un)pause selected torrent"),
        ("P", "(Un)pause all torrents"),
        ("d", "Delete selected torrent"),
        ("Enter", "Open detail view"),
        ("j / \u{2193}", "Move selection down"),
        ("k / \u{2191}", "Move selection up"),
        ("Tab", "Cycle sort column / detail tab"),
        ("r", "Reverse sort order"),
        ("/", "Search/filter torrents"),
        ("t", "Set speed limits"),
        ("Space", "Mark/unmark torrent"),
        ("v", "Mark all visible"),
        ("V", "Clear all marks"),
        ("?", "Toggle this help"),
        ("q / Esc", "Quit / back"),
        ("", ""),
        ("", "-- Detail View --"),
        ("j / k", "Navigate files / peers"),
        ("Space", "Toggle file selection"),
        ("S", "Apply file selection"),
        ("s", "Stream selected file in default player"),
    ];

    let rows: Vec<Row> = keybindings
        .iter()
        .map(|(key, action)| {
            Row::new(vec![
                Cell::from(*key).style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Cell::from(*action),
            ])
        })
        .collect();

    let table = Table::new(rows, [Constraint::Length(15), Constraint::Min(30)])
        .block(
            Block::default()
                .title(" Help \u{2014} Keybindings ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .style(Style::default().fg(Color::White));

    f.render_widget(table, popup);
}
