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

const HEADER_LABELS: [&str; 9] = [
    "#",
    "Name",
    "Size",
    "Progress",
    "\u{2193} Speed",
    "\u{2191} Speed",
    "Peers",
    "ETA",
    "Status",
];

/// Cells in the progress bar; the Progress column is this plus " 100.0%".
const BAR_CELLS: usize = 11;

/// Each fixed column is exactly as wide as the widest thing it renders —
/// "1023.9 KB/s", "999h 59m", "Fetching Metadata" — because ratatui honours
/// Name's `Min` before any `Length`: when the row is too wide it clips the
/// fixed columns, not the name. Sized so the whole row, borders and the
/// selection marker included, fits 120 columns with Name at its minimum.
const COLUMN_WIDTHS: [Constraint; 9] = [
    Constraint::Length(5),                    // #
    Constraint::Min(20),                      // Name
    Constraint::Length(10),                   // Size
    Constraint::Length(BAR_CELLS as u16 + 7), // Progress
    Constraint::Length(11),                   // ↓ Speed
    Constraint::Length(11),                   // ↑ Speed
    Constraint::Length(8),                    // Peers
    Constraint::Length(8),                    // ETA
    Constraint::Length(17),                   // Status
];

/// Height of the strip that lists magnets still waiting for metadata: two
/// lines per add (the diagnosis wraps) inside a bordered block, `None` when
/// there is nothing pending so the table keeps the whole area.
pub fn pending_strip_height(app: &App) -> Option<u16> {
    let n = app
        .network_health
        .as_ref()
        .map(|n| n.pending_adds.len())
        .unwrap_or(0);
    (n > 0).then(|| (n as u16).saturating_mul(2).saturating_add(2).min(10))
}

/// The magnets librqbit has not turned into torrents yet, each with how long
/// it has been waiting and — past the warm-up — why nobody has answered.
///
/// A magnet only becomes a torrent once a peer hands over its metadata, so
/// until then it has no row in the table above. Before this strip existed
/// the UI said "Added" and then showed nothing at all: a magnet nobody was
/// seeding simply vanished, and the user was left to wonder.
pub fn render_pending_adds(f: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let Some(network) = app.network_health.as_ref() else {
        return;
    };
    let spinner = SPINNER_FRAMES[app.spinner_tick];
    let lines: Vec<Line> = network
        .pending_adds
        .iter()
        .map(|add| {
            let text = crate::health::pending_add_line(add, Some(network));
            let style = if add.secs >= crate::health::STALL_AFTER.as_secs() {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Magenta)
            };
            Line::from(vec![
                Span::styled(format!(" {} ", spinner), style),
                Span::styled(text, style),
            ])
        })
        .collect();
    // Wrapped, not clipped: the diagnosis is the point of the line.
    let widget = ratatui::widgets::Paragraph::new(lines)
        .wrap(ratatui::widgets::Wrap { trim: true })
        .block(
            Block::default()
                .title(format!(
                    " Resolving ({}) - appears above once a peer answers ",
                    network.pending_adds.len()
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
    f.render_widget(widget, area);
}

pub fn render_table(f: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    let sorted = app.sorted_torrents();

    if sorted.is_empty() {
        let msg = if !app.filter_text.is_empty() {
            "No torrents match the current filter."
        } else if pending_strip_height(app).is_some() {
            "No torrents yet — the magnet below appears here once a peer sends its metadata."
        } else {
            "No torrents. Press 'a' to add a magnet link or .torrent file."
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
    let stalled_ids = &app.stalled_ids;

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
                _ => render_progress_bar(percent, BAR_CELLS),
            };

            let (status_text, status_style) = if stalled_ids.contains(&torrent.id) {
                stalled_cell_style()
            } else {
                status_cell_style(&torrent.status)
            };

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
                Cell::from(format_speed(torrent.upload_speed)),
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

    let table = Table::new(rows, COLUMN_WIDTHS)
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

/// Map a torrent's status to the cell text and style. Speed limits are
/// enforced inside librqbit's rate limiter, so there is no "Throttled"
/// pseudo-status any more — a limited torrent simply shows Downloading (or
/// Seeding) at a capped speed.
pub fn status_cell_style(status: &TorrentStatus) -> (String, Style) {
    let style = match status {
        TorrentStatus::Downloading => Style::default().fg(Color::Blue),
        TorrentStatus::Seeding => Style::default().fg(Color::Green),
        TorrentStatus::Paused => Style::default().fg(Color::Yellow),
        TorrentStatus::FetchingMetadata => Style::default().fg(Color::Magenta),
        TorrentStatus::Error(_) => Style::default().fg(Color::Red),
    };
    (status.to_string(), style)
}

/// The Status cell for a downloading torrent that has not received a byte in
/// `health::STALL_AFTER`. The underlying `TorrentStatus` stays `Downloading`
/// (sorting and filtering are unchanged); only the cell says otherwise. The
/// glyph carries the state without colour, and red is shared with `Error`
/// deliberately — both mean "not going to finish by itself".
pub fn stalled_cell_style() -> (String, Style) {
    (
        "\u{26a0} Stalled".to_string(),
        Style::default().fg(Color::Red),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TorrentHealth, TorrentInfo};
    use ratatui::{backend::TestBackend, Terminal};

    fn torrent(id: usize) -> TorrentInfo {
        TorrentInfo {
            id,
            name: format!("t{id}"),
            size_bytes: 1000,
            downloaded_bytes: 10,
            uploaded_bytes: 0,
            download_speed: 0,
            upload_speed: 0,
            peers_connected: 0,
            peers_total: 0,
            status: TorrentStatus::Downloading,
            eta_seconds: None,
            files: Vec::new(),
            peers: Vec::new(),
            info_hash: String::new(),
            trackers: Vec::new(),
            piece_length: None,
            content_path: None,
            health: TorrentHealth::default(),
        }
    }

    fn screen(app: &mut App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render_table(f, f.area(), app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn screen_lines(app: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render_table(f, f.area(), app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(w))
            .map(|row| row.iter().map(|c| c.symbol()).collect())
            .collect()
    }

    #[test]
    fn a_stalled_row_says_so_in_words() {
        let mut app = App::new();
        let t0 = std::time::Instant::now();
        app.handle_state_push_at(vec![torrent(1), torrent(2)], t0);
        let text = screen(&mut app, 120, 10);
        assert!(text.contains("Downloading"), "{text}");
        assert!(!text.contains("Stalled"), "{text}");

        // Only torrent 1 crosses the threshold (2 got a byte).
        let mut t2 = torrent(2);
        t2.downloaded_bytes = 11;
        app.handle_state_push_at(
            vec![torrent(1), t2],
            t0 + std::time::Duration::from_secs(31),
        );
        let text = screen(&mut app, 120, 10);
        assert!(text.contains("\u{26a0} Stalled"), "{text}");
        assert!(
            text.contains("Downloading"),
            "torrent 2 must still read Downloading: {text}"
        );
    }

    #[test]
    fn downloading_is_blue() {
        let (text, style) = status_cell_style(&TorrentStatus::Downloading);
        assert_eq!(text, "Downloading");
        assert_eq!(style, Style::default().fg(Color::Blue));
    }

    #[test]
    fn seeding_is_green() {
        let (text, style) = status_cell_style(&TorrentStatus::Seeding);
        assert_eq!(text, "Seeding");
        assert_eq!(style, Style::default().fg(Color::Green));
    }

    #[test]
    fn each_row_shows_its_own_upload_speed() {
        // #86: upload speed was only in the header total and Detail → Stats,
        // so telling *which* torrent was uploading meant opening each one.
        let mut app = App::new();
        let mut seed = torrent(1);
        seed.status = TorrentStatus::Seeding;
        seed.downloaded_bytes = seed.size_bytes;
        seed.upload_speed = 300 * 1024;
        let mut leech = torrent(2);
        leech.download_speed = 2 * 1024 * 1024;
        app.handle_state_push(vec![seed, leech]);
        let lines = screen_lines(&mut app, 140, 10);
        assert!(lines[1].contains("\u{2191} Speed"), "{lines:#?}");
        let row = |name: &str| {
            lines
                .iter()
                .find(|l| l.contains(name))
                .unwrap_or_else(|| panic!("no row for {name}: {lines:#?}"))
                .clone()
        };
        let seed_row = row("t1");
        assert!(seed_row.contains("0 B/s"), "{seed_row}");
        assert!(seed_row.contains("300.0 KB/s"), "{seed_row}");
        assert!(seed_row.contains("Seeding"), "{seed_row}");
        let leech_row = row("t2");
        assert!(leech_row.contains("2.0 MB/s"), "{leech_row}");
        assert!(leech_row.contains("0 B/s"), "{leech_row}");
    }

    #[test]
    fn nothing_is_clipped_at_the_widest_values_on_a_120_column_terminal() {
        // Name's `Min` outranks every `Length`, so a row that doesn't fit
        // clips the fixed columns. At 120 columns none of them may.
        let mut app = App::new();
        let mut t = torrent(1);
        t.name = "debian-13.1.0-amd64-netinst.iso".to_string();
        t.download_speed = 1023 * 1024 + 900;
        t.upload_speed = 1023 * 1024 + 900;
        t.peers_connected = 999;
        t.peers_total = 9999;
        t.eta_seconds = Some(999 * 3600 + 59 * 60);
        let mut fetching = torrent(2);
        fetching.status = TorrentStatus::FetchingMetadata;
        app.handle_state_push(vec![t, fetching]);
        let text = screen(&mut app, 120, 10);
        assert_eq!(
            text.matches("1023.9 KB/s").count(),
            2,
            "both speed cells fit: {text}"
        );
        for cell in [
            "debian-13.1.0-amd64",
            "1.0%",
            "999/9999",
            "999h 59m",
            "Downloading",
            "Fetching Metadata",
        ] {
            assert!(text.contains(cell), "{cell:?} clipped: {text}");
        }
    }

    #[test]
    fn error_text_includes_message() {
        let (text, _) = status_cell_style(&TorrentStatus::Error("disk full".to_string()));
        assert!(text.contains("disk full"));
    }

    // -- the pending strip ----------------------------------------------------

    fn pending(label: &str, secs: u64) -> crate::types::PendingAdd {
        crate::types::PendingAdd {
            label: label.to_string(),
            secs,
            trackers: crate::types::TrackerCounts {
                total: 6,
                failing: 2,
                pending: 4,
                ..Default::default()
            },
            tracker_note: Some("opentrackr: timed out".to_string()),
        }
    }

    fn draw_main(app: &mut App, w: u16, h: u16) -> String {
        // Mirrors run_app: table on top, strip underneath when there is one.
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                let (table, strip) =
                    crate::ui::layout::split_pending(f.area(), pending_strip_height(app));
                render_table(f, table, app);
                if let Some(strip) = strip {
                    render_pending_adds(f, strip, app);
                }
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn a_resolving_magnet_is_visible_even_with_an_empty_list() {
        // The bug: "Added" was said, the list stayed empty, and nothing on
        // screen ever mentioned the magnet again.
        let mut app = App::new();
        app.network_health = Some(crate::types::NetworkHealth {
            pending_adds: vec![pending("Arch Linux 2026.09.01", 45)],
            dht: Some(crate::types::DhtHealth::default()),
            ..Default::default()
        });
        let text = draw_main(&mut app, 140, 20);
        assert!(text.contains("Resolving (1)"), "{text}");
        assert!(
            text.contains("Resolving \"Arch Linux 2026.09.01\" for 45 s"),
            "{text}"
        );
        assert!(text.contains("no peer has sent the metadata yet"), "{text}");
        assert!(text.contains("DHT has no nodes"), "{text}");
        assert!(text.contains("2 failing"), "{text}");
        assert!(text.contains("opentrackr: timed out"), "{text}");
        assert!(
            text.contains("appears here once a peer sends its metadata"),
            "{text}"
        );
        assert!(!text.contains("Press 'a' to add"), "{text}");
    }

    #[test]
    fn the_strip_sits_under_real_rows_and_disappears_when_nothing_is_pending() {
        let mut app = App::new();
        app.handle_state_push(vec![torrent(1)]);
        app.network_health = Some(crate::types::NetworkHealth {
            pending_adds: vec![pending("x", 3)],
            ..Default::default()
        });
        let text = draw_main(&mut app, 120, 20);
        let row = text.find("t1").expect("real row drawn");
        let strip = text.find("Resolving (1)").expect("strip drawn");
        assert!(row < strip, "strip must sit below the table: {text}");
        // Inside the warm-up: no diagnosis yet, just the wait.
        assert!(text.contains("Resolving \"x\" for 3 s"), "{text}");
        assert!(!text.contains("no peer has sent"), "{text}");

        app.network_health = Some(crate::types::NetworkHealth::default());
        let text = draw_main(&mut app, 120, 20);
        assert!(!text.contains("Resolving"), "{text}");
        assert_eq!(pending_strip_height(&app), None);
    }

    #[test]
    fn the_strip_never_squeezes_the_table_out_on_a_short_terminal() {
        let mut app = App::new();
        app.handle_state_push(vec![torrent(1)]);
        app.network_health = Some(crate::types::NetworkHealth {
            pending_adds: (0..20).map(|i| pending(&format!("m{i}"), 1)).collect(),
            ..Default::default()
        });
        assert_eq!(pending_strip_height(&app), Some(10), "capped");
        for h in [1u16, 4, 6, 8, 12] {
            let text = draw_main(&mut app, 100, h);
            // Whatever the height, drawing must not panic; with room for
            // both, the table header still shows.
            if h >= 12 {
                assert!(text.contains("Name"), "{text}");
            }
        }
    }
}
