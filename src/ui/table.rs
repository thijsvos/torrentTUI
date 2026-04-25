use ratatui::{
    layout::Constraint,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Row, Table},
    Frame,
};

use crate::app::App;
use crate::types::TorrentStatus;
use crate::ui::layout::{format_eta, format_size, format_speed};
use crate::ui::progress::{progress_color, render_progress_bar, SPINNER_FRAMES};

const HEADER_LABELS: [&str; 8] = [
    "#",
    "Name",
    "Size",
    "Progress",
    "\u{2193} Speed",
    "Peers",
    "ETA",
    "Status",
];

pub fn render_table(f: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    let sorted = app.sorted_torrents();

    if sorted.is_empty() {
        let msg = if app.filter_text.is_empty() {
            "No torrents. Press 'a' to add a magnet link or .torrent file."
        } else {
            "No torrents match the current filter."
        };
        let empty_msg = ratatui::widgets::Paragraph::new(Line::from(vec![Span::styled(
            msg,
            Style::default().fg(Color::DarkGray),
        )]))
        .block(
            Block::default()
                .title(" Downloads ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .centered();
        f.render_widget(empty_msg, area);
        return;
    }

    let sort_col = app.sort_column;
    let sort_rev = app.sort_reversed;
    let spinner_tick = app.spinner_tick;
    let marked_ids = &app.marked_ids;

    // Build header with sort indicator
    let header_cells = HEADER_LABELS.iter().enumerate().map(|(i, h)| {
        let label = if sort_col.column_index() == i {
            let arrow = if sort_rev { "\u{25bc}" } else { "\u{25b2}" };
            format!("{} {}", h, arrow)
        } else {
            h.to_string()
        };
        Cell::from(label).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    });
    let header = Row::new(header_cells).height(1);

    // Build rows from sorted view (all owned data)
    let rows: Vec<Row> = sorted
        .iter()
        .map(|torrent| {
            let is_marked = marked_ids.contains(&torrent.id);
            let percent = torrent.progress_percent();
            let progress_bar = match torrent.status {
                TorrentStatus::FetchingMetadata => {
                    let spinner = SPINNER_FRAMES[spinner_tick];
                    format!("{} Fetching...", spinner)
                }
                _ => render_progress_bar(percent, 15),
            };

            let (status_text, status_style) = status_cell_style(&torrent.status, torrent.throttle_paused);

            let progress_style = match torrent.status {
                TorrentStatus::FetchingMetadata => Style::default().fg(Color::Magenta),
                _ => Style::default().fg(progress_color(percent)),
            };

            let id_text = if is_marked {
                format!("\u{25cf} {}", torrent.id)
            } else {
                format!("  {}", torrent.id)
            };

            let row = Row::new(vec![
                Cell::from(id_text),
                Cell::from(torrent.name.clone()),
                Cell::from(format_size(torrent.size_bytes)),
                Cell::from(progress_bar).style(progress_style),
                Cell::from(format_speed(torrent.download_speed)),
                Cell::from(format!(
                    "{}/{}",
                    torrent.peers_connected, torrent.peers_total
                )),
                Cell::from(format_eta(torrent.eta_seconds)),
                Cell::from(status_text).style(status_style),
            ]);

            if is_marked {
                row.style(Style::default().bg(Color::Indexed(236)))
            } else {
                row
            }
        })
        .collect();

    // Drop sorted to release the immutable borrow on app
    drop(sorted);

    let table = Table::new(
        rows,
        [
            Constraint::Length(5),  // #
            Constraint::Min(20),    // Name
            Constraint::Length(10), // Size
            Constraint::Length(24), // Progress
            Constraint::Length(12), // Speed
            Constraint::Length(8),  // Peers
            Constraint::Length(10), // ETA
            Constraint::Length(18), // Status
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title(" Downloads ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("\u{25b6} ");

    f.render_stateful_widget(table, area, &mut app.table_state);
}

/// Map a torrent's status (and the throttle-paused override) to the cell text
/// and style. Throttle takes precedence over the underlying engine state so
/// the user sees the higher-signal label.
pub fn status_cell_style(status: &TorrentStatus, throttle_paused: bool) -> (String, Style) {
    if throttle_paused {
        return ("Throttled".to_string(), Style::default().fg(Color::Cyan));
    }
    let style = match status {
        TorrentStatus::Downloading => Style::default().fg(Color::Blue),
        TorrentStatus::Complete | TorrentStatus::Seeding => Style::default().fg(Color::Green),
        TorrentStatus::Paused => Style::default().fg(Color::Yellow),
        TorrentStatus::FetchingMetadata => Style::default().fg(Color::Magenta),
        TorrentStatus::Error(_) => Style::default().fg(Color::Red),
    };
    (status.to_string(), style)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_paused_overrides_status() {
        let (text, _) = status_cell_style(&TorrentStatus::Downloading, true);
        assert_eq!(text, "Throttled");
    }

    #[test]
    fn downloading_is_blue() {
        let (text, style) = status_cell_style(&TorrentStatus::Downloading, false);
        assert_eq!(text, "Downloading");
        assert_eq!(style, Style::default().fg(Color::Blue));
    }

    #[test]
    fn complete_and_seeding_share_color() {
        let (_, c) = status_cell_style(&TorrentStatus::Complete, false);
        let (_, s) = status_cell_style(&TorrentStatus::Seeding, false);
        assert_eq!(c, s);
    }

    #[test]
    fn error_text_includes_message() {
        let (text, _) =
            status_cell_style(&TorrentStatus::Error("disk full".to_string()), false);
        assert!(text.contains("disk full"));
    }
}
