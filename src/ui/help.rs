use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Clear, Row, Table},
    Frame,
};

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
        ("", "-- Detail View (Files tab) --"),
        ("j / k", "Navigate files"),
        ("Space", "Toggle file selection"),
        ("S", "Apply file selection"),
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

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_width = r.width * percent_x / 100;
    let popup_height = r.height * percent_y / 100;
    let x = r.x + (r.width.saturating_sub(popup_width)) / 2;
    let y = r.y + (r.height.saturating_sub(popup_height)) / 2;
    Rect::new(x, y, popup_width, popup_height)
}
