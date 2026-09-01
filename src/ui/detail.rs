//! The Detail view: four tabs (Stats, Info, Files, Peers) over one torrent.
//!
//! Everything here depends on the engine having been told which torrent is
//! being viewed. `TorrentInfo::files`, `peers`, `trackers`, `info_hash` and
//! `piece_length` are populated only for the Detail target; entering and
//! leaving this view sends `SetDetailTorrent`, and without that every tab but
//! Stats renders empty.
//!
//! All four renderers return early and draw nothing when there is no selection,
//! which is safe only because the caller has already cleared the frame.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Tabs},
    Frame,
};

use crate::app::App;
use crate::types::{DetailTab, PeerInfo};
use crate::ui::layout::{format_eta, format_size, format_speed};
use crate::ui::progress::render_progress_bar;
use crate::ui::util::{is_streamable_media, truncate};

pub fn render_detail(f: &mut Frame, area: Rect, app: &mut App) {
    let (torrent_name, tab_index) = match app.selected_torrent() {
        Some(t) => (t.name.clone(), app.detail_tab.index()),
        None => return,
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header with name
            Constraint::Length(3), // Tabs
            Constraint::Min(5),    // Tab content
        ])
        .split(area);

    // Header
    let header = Paragraph::new(Line::from(vec![Span::styled(
        torrent_name,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]))
    .block(
        Block::default()
            .title(" Torrent Detail ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(header, chunks[0]);

    // Tabs
    let tab_titles = vec!["Stats", "Info", "Files", "Peers"];
    let tabs = Tabs::new(tab_titles)
        .select(tab_index)
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
    f.render_widget(tabs, chunks[1]);

    // Tab content
    match app.detail_tab {
        DetailTab::Stats => render_stats_tab(f, chunks[2], app),
        DetailTab::Info => render_info_tab(f, chunks[2], app),
        DetailTab::Files => render_files_tab(f, chunks[2], app),
        DetailTab::Peers => render_peers_tab(f, chunks[2], app),
    }
}

fn render_stats_tab(f: &mut Frame, area: Rect, app: &App) {
    let torrent = match app.selected_torrent() {
        Some(t) => t,
        None => return,
    };

    let percent = torrent.progress_percent();
    let progress = render_progress_bar(percent, 30);

    let stats_text = vec![
        Line::from(vec![
            Span::styled("  Status:    ", Style::default().fg(Color::DarkGray)),
            Span::raw(torrent.status.to_string()),
        ]),
        Line::from(vec![
            Span::styled("  Size:      ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(
                "{} / {}",
                format_size(torrent.downloaded_bytes),
                format_size(torrent.size_bytes)
            )),
        ]),
        Line::from(vec![
            Span::styled("  Progress:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(progress),
        ]),
        Line::from(vec![
            Span::styled("  Uploaded:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(format_size(torrent.uploaded_bytes)),
        ]),
        Line::from(vec![
            Span::styled("  Ratio:     ", Style::default().fg(Color::DarkGray)),
            Span::raw(if torrent.downloaded_bytes > 0 {
                format!(
                    "{:.2}",
                    torrent.uploaded_bytes as f64 / torrent.downloaded_bytes as f64
                )
            } else {
                "\u{2014}".to_string()
            }),
        ]),
        Line::from(vec![
            Span::styled("  Down:      ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format_speed(torrent.download_speed),
                Style::default().fg(Color::Green),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Up:        ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format_speed(torrent.upload_speed),
                Style::default().fg(Color::Magenta),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Peers:     ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(
                "{} connected / {} total",
                torrent.peers_connected, torrent.peers_total
            )),
        ]),
        Line::from(vec![
            Span::styled("  ETA:       ", Style::default().fg(Color::DarkGray)),
            Span::raw(format_eta(torrent.eta_seconds)),
        ]),
    ];

    let stats = Paragraph::new(stats_text).block(
        Block::default()
            .title(" Stats ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(stats, area);
}

fn render_info_tab(f: &mut Frame, area: Rect, app: &App) {
    let torrent = match app.selected_torrent() {
        Some(t) => t,
        None => return,
    };

    let ratio = if torrent.downloaded_bytes > 0 {
        format!(
            "{:.2}",
            torrent.uploaded_bytes as f64 / torrent.downloaded_bytes as f64
        )
    } else {
        "\u{2014}".to_string()
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled("  Info Hash:    ", Style::default().fg(Color::DarkGray)),
            Span::styled(&torrent.info_hash, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("  Uploaded:     ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(
                "{}  (ratio: {})",
                format_size(torrent.uploaded_bytes),
                ratio
            )),
        ]),
    ];

    if let Some(pl) = torrent.piece_length {
        lines.push(Line::from(vec![
            Span::styled("  Piece Size:   ", Style::default().fg(Color::DarkGray)),
            Span::raw(format_size(pl as u64)),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Trackers:",
        Style::default().fg(Color::DarkGray),
    )));
    if torrent.trackers.is_empty() {
        lines.push(Line::from(Span::styled(
            "    (DHT only)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for tracker in &torrent.trackers {
            lines.push(Line::from(format!("    {}", tracker)));
        }
    }

    let info_widget = Paragraph::new(lines).block(
        Block::default()
            .title(" Info ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(info_widget, area);
}

fn render_files_tab(f: &mut Frame, area: Rect, app: &App) {
    let torrent = match app.selected_torrent() {
        Some(t) => t,
        None => return,
    };

    if torrent.files.is_empty() {
        let placeholder =
            Paragraph::new("  No file information available yet (waiting for metadata).")
                .style(Style::default().fg(Color::DarkGray))
                .block(
                    Block::default()
                        .title(" Files ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::DarkGray)),
                );
        f.render_widget(placeholder, area);
        return;
    }

    let torrent_id = torrent.id;
    let mut lines: Vec<Line> = Vec::new();
    for (idx, file) in torrent.files.iter().enumerate() {
        let percent = if file.size_bytes > 0 {
            (file.progress_bytes as f64 / file.size_bytes as f64 * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        let bar = crate::ui::progress::render_progress_bar(percent, 10);

        let selected = app.is_file_selected(torrent_id, idx);
        let checkbox = if selected { "[\u{2713}]" } else { "[ ]" };

        let is_highlighted = idx == app.detail_file_index;

        let highlight_style = if is_highlighted {
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let file_style = if !selected {
            Style::default().fg(Color::Gray)
        } else {
            highlight_style
        };

        let checkbox_style = if selected {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let is_media = is_streamable_media(&file.name);
        // ▶ on streamable media so users know `s` will work; two spaces
        // otherwise to keep the column alignment identical.
        let media_glyph = if is_media { "\u{25B6} " } else { "  " };
        let media_glyph_style = if is_media {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };

        lines.push(Line::from(vec![
            Span::styled(if is_highlighted { "> " } else { "  " }, highlight_style),
            Span::styled(format!("{} ", checkbox), checkbox_style),
            Span::styled(media_glyph, media_glyph_style),
            Span::styled(crate::ui::util::pad_to_width(&file.name, 45), file_style),
            Span::styled(format!("{:>10}", format_size(file.size_bytes)), file_style),
            Span::raw("  "),
            Span::styled(
                bar,
                Style::default().fg(crate::ui::progress::progress_color(percent)),
            ),
        ]));
    }

    let files_widget = Paragraph::new(lines).block(
        Block::default()
            .title(format!(
                " Files ({}) - Space:toggle  S:apply  s:stream \u{25B6} ",
                torrent.files.len()
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(files_widget, area);
}

/// Render the Peers tab. Takes `&mut App` because scrolling is resolved here:
/// the row list is what defines the visible window, so the scroll offset is
/// recomputed each frame to keep `detail_peer_index` on screen (the key
/// handlers only move the index).
///
/// Rows are sorted by bytes downloaded, descending — so `detail_peer_index` is
/// a position in *this* ordering, not in `TorrentInfo::peers`. It stays valid
/// only because both have the same length; do not use it to index the
/// underlying vec.
fn render_peers_tab(f: &mut Frame, area: Rect, app: &mut App) {
    // Pull just what we need so the immutable borrow ends before we touch
    // `app` mutably to update scroll state.
    let (peer_count, peers_connected, peers_total) = match app.selected_torrent() {
        Some(t) => (t.peers.len(), t.peers_connected, t.peers_total),
        None => return,
    };

    if peer_count == 0 {
        let text = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("  Connected:  ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{}", peers_connected)),
            ]),
            Line::from(vec![
                Span::styled("  Total seen: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{}", peers_total)),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "  No peers connected",
                Style::default().fg(Color::DarkGray),
            )),
        ];

        let peers_widget = Paragraph::new(text).block(
            Block::default()
                .title(" Peers ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        f.render_widget(peers_widget, area);
        return;
    }

    // 2 border rows + 3 header lines = 5, plus one row of slack so the last
    // peer never lands flush against the bottom border.
    let visible_height = area.height.saturating_sub(6) as usize;
    let peer_index = app.detail_peer_index.min(peer_count.saturating_sub(1));
    app.detail_peer_index = peer_index;

    // Keep the cursor visible. Track scroll_offset separately on App so
    // pressing `j` past the bottom of the visible window actually scrolls;
    // the previous cap-by-min logic froze the cursor at row 0 until the
    // very end of the list.
    if peer_index < app.detail_peer_scroll_offset {
        app.detail_peer_scroll_offset = peer_index;
    }
    if visible_height > 0 && peer_index >= app.detail_peer_scroll_offset + visible_height {
        app.detail_peer_scroll_offset = peer_index + 1 - visible_height;
    }
    // Bound to [0, peer_count - visible_height] when the list shrinks.
    let max_offset = peer_count.saturating_sub(visible_height.max(1));
    if app.detail_peer_scroll_offset > max_offset {
        app.detail_peer_scroll_offset = max_offset;
    }
    let scroll_offset = app.detail_peer_scroll_offset;

    // Re-borrow immutably (no more `app` mutations below) and sort peer
    // *references* — avoids deep-cloning every PeerInfo (two owned Strings each)
    // on every render of this tab.
    let Some(t) = app.selected_torrent() else {
        return;
    };
    let mut sorted: Vec<&PeerInfo> = t.peers.iter().collect();
    sorted.sort_by_key(|p| std::cmp::Reverse(p.downloaded_bytes));

    let mut lines = vec![
        Line::from(vec![
            Span::styled("  Connected: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}", peers_connected)),
            Span::styled("  /  Total seen: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}", peers_total)),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            format!(
                "  {:<22} {:<12} {:>12} {:>8} {:>6}",
                "Address", "State", "Downloaded", "Pieces", "Errs"
            ),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]),
    ];

    for (i, peer) in sorted
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_height)
    {
        let is_selected = i == peer_index;
        let prefix = if is_selected { "> " } else { "  " };
        let style = if is_selected {
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        lines.push(Line::from(Span::styled(
            format!(
                "{}{:<22} {:<12} {:>12} {:>8} {:>6}",
                prefix,
                truncate(&peer.address, 22),
                truncate(&peer.state, 12),
                format_size(peer.downloaded_bytes),
                peer.pieces,
                peer.errors
            ),
            style,
        )));
    }

    let peers_widget = Paragraph::new(lines).block(
        Block::default()
            .title(format!(" Peers ({}) - j/k:scroll ", peer_count))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(peers_widget, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FileInfo, TorrentInfo, TorrentStatus};
    use ratatui::{backend::TestBackend, Terminal};

    fn screen(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn torrent() -> TorrentInfo {
        TorrentInfo {
            id: 0,
            name: "debian-13.6.0-amd64-netinst.iso".to_string(),
            size_bytes: 786_432_000,
            downloaded_bytes: 393_216_000,
            uploaded_bytes: 131_072_000,
            download_speed: 512_000,
            upload_speed: 64_000,
            peers_connected: 42,
            peers_total: 518,
            status: TorrentStatus::Downloading,
            eta_seconds: Some(754),
            files: Vec::new(),
            peers: Vec::new(),
            info_hash: "8337c196d4536e9af5d2c7e599f0f1b7d71eee54".to_string(),
            trackers: Vec::new(),
            piece_length: Some(262_144),
            content_path: None,
        }
    }

    /// Distinct `downloaded_bytes` on purpose: the peer table is sorted by
    /// `Reverse(downloaded_bytes)` and `detail_peer_index` indexes *that*
    /// order, not `TorrentInfo::peers`. Identical peers would hide a
    /// regression in the sort.
    fn peers(n: usize) -> Vec<PeerInfo> {
        (0..n)
            .map(|i| PeerInfo {
                address: format!("10.0.0.{}:6881", i + 1),
                state: "Live".to_string(),
                downloaded_bytes: ((n - i) * 1024) as u64,
                pieces: (n - i) as u32,
                errors: 0,
            })
            .collect()
    }

    fn app_showing(torrent: TorrentInfo, tab: DetailTab) -> App {
        let mut app = App::new();
        app.handle_state_push(vec![torrent]);
        app.detail_tab = tab;
        app
    }

    fn draw(app: &mut App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render_detail(f, f.area(), app)).unwrap();
        screen(&terminal)
    }

    // -- the tabs render what they claim ------------------------------------

    #[test]
    fn stats_tab_shows_transfer_figures() {
        let mut app = app_showing(torrent(), DetailTab::Stats);
        let text = draw(&mut app, 100, 30);
        assert!(text.contains("Downloading"), "{text}");
        assert!(
            text.contains("42 connected / 518 total"),
            "peer counts missing: {text}"
        );
        // 393_216_000 / 786_432_000 is exactly half.
        assert!(text.contains("50.0%"), "progress missing: {text}");
    }

    #[test]
    fn info_tab_shows_the_info_hash_and_piece_size() {
        let mut app = app_showing(torrent(), DetailTab::Info);
        let text = draw(&mut app, 100, 30);
        assert!(
            text.contains("8337c196d4536e9af5d2c7e599f0f1b7d71eee54"),
            "{text}"
        );
        assert!(text.contains("256 KB"), "piece size missing: {text}");
    }

    #[test]
    fn info_tab_says_dht_only_when_there_are_no_trackers() {
        let mut app = app_showing(torrent(), DetailTab::Info);
        assert!(draw(&mut app, 100, 30).contains("(DHT only)"));

        let mut t = torrent();
        t.trackers = vec!["https://tracker.example/announce".to_string()];
        let mut app = app_showing(t, DetailTab::Info);
        let text = draw(&mut app, 100, 30);
        assert!(text.contains("tracker.example"), "{text}");
        assert!(!text.contains("(DHT only)"), "{text}");
    }

    #[test]
    fn files_tab_falls_back_to_a_waiting_message() {
        let mut app = app_showing(torrent(), DetailTab::Files);
        assert!(draw(&mut app, 100, 30).contains("waiting for metadata"));
    }

    #[test]
    fn files_tab_lists_files_with_selection_state() {
        let mut t = torrent();
        t.files = vec![
            FileInfo {
                name: "disc.iso".to_string(),
                size_bytes: 1024,
                progress_bytes: 512,
            },
            FileInfo {
                name: "notes.txt".to_string(),
                size_bytes: 100,
                progress_bytes: 100,
            },
        ];
        let mut app = app_showing(t, DetailTab::Files);
        let text = draw(&mut app, 120, 30);
        assert!(text.contains("disc.iso"), "{text}");
        assert!(text.contains("notes.txt"), "{text}");
        // Both selected by default, so both checkboxes are ticked.
        assert!(text.contains("[\u{2713}]"), "checkbox missing: {text}");
    }

    #[test]
    fn peers_tab_says_so_when_there_are_none() {
        // Reachable with non-zero counters: librqbit reports counts before the
        // per-peer snapshot arrives.
        let mut app = app_showing(torrent(), DetailTab::Peers);
        let text = draw(&mut app, 100, 30);
        assert!(text.contains("42"), "counters should still show: {text}");
    }

    #[test]
    fn peers_tab_lists_peers_highest_downloaded_first() {
        let mut t = torrent();
        t.peers = peers(3);
        let mut app = app_showing(t, DetailTab::Peers);
        let text = draw(&mut app, 120, 30);
        // peers() gives 10.0.0.1 the most bytes, so it sorts to the top.
        let first = text.find("10.0.0.1:6881").expect("peer 1 rendered");
        let last = text.find("10.0.0.3:6881").expect("peer 3 rendered");
        assert!(first < last, "peers not sorted by downloaded desc: {text}");
    }

    // -- the peer scroll invariant ------------------------------------------

    /// After the three clamps, the highlighted row must lie inside the window
    /// that is actually drawn: `offset <= index < offset + visible_height`.
    /// This is the arithmetic most likely to be "simplified" later.
    #[test]
    fn peer_scroll_keeps_the_cursor_inside_the_window() {
        let mut t = torrent();
        t.peers = peers(50);
        let mut app = app_showing(t, DetailTab::Peers);

        let height: u16 = 20;
        let visible = (height - 6) as usize;
        for index in [0usize, 1, 13, 14, 30, 49] {
            app.detail_peer_index = index;
            draw(&mut app, 120, height);
            let offset = app.detail_peer_scroll_offset;
            assert!(
                offset <= index && index < offset + visible,
                "index {index} outside window [{offset}, {})",
                offset + visible
            );
        }
    }

    #[test]
    fn peer_scroll_follows_the_cursor_back_up() {
        let mut t = torrent();
        t.peers = peers(50);
        let mut app = app_showing(t, DetailTab::Peers);

        app.detail_peer_index = 49;
        draw(&mut app, 120, 20);
        assert!(
            app.detail_peer_scroll_offset > 0,
            "should have scrolled down"
        );

        app.detail_peer_index = 0;
        draw(&mut app, 120, 20);
        assert_eq!(
            app.detail_peer_scroll_offset, 0,
            "should scroll back to top"
        );
    }

    #[test]
    fn peer_scroll_offset_is_bounded_when_the_list_shrinks() {
        let mut t = torrent();
        t.peers = peers(50);
        let mut app = app_showing(t, DetailTab::Peers);
        app.detail_peer_index = 49;
        draw(&mut app, 120, 20);
        let scrolled = app.detail_peer_scroll_offset;
        assert!(scrolled > 0);

        // The swarm collapses to three peers between ticks.
        let mut t = torrent();
        t.peers = peers(3);
        app.handle_state_push(vec![t]);
        draw(&mut app, 120, 20);
        assert!(
            app.detail_peer_scroll_offset <= 3usize.saturating_sub(1),
            "offset {} points past a 3-peer list",
            app.detail_peer_scroll_offset
        );
    }

    /// A pane too short to show anything must still leave the offset pointing
    /// inside the list, so the next resize renders from a sane position.
    ///
    /// Note on what this does *not* cover: `visible_height.max(1)` in
    /// `max_offset` looks load-bearing but is unreachable. `scroll_offset` is
    /// only ever assigned `peer_index` or `peer_index + 1 - visible_height`,
    /// both bounded by `peer_count - 1`, so the `offset > max_offset` clamp it
    /// guards cannot fire even with the `.max(1)` removed — verified by
    /// mutation, which every test here survives. It is defensive, not
    /// load-bearing, and no test can honestly pin it.
    #[test]
    fn peer_scroll_survives_a_pane_with_no_room() {
        let mut t = torrent();
        t.peers = peers(10);
        let mut app = app_showing(t, DetailTab::Peers);
        app.detail_peer_index = 9;
        for height in [1u16, 4, 6, 7] {
            draw(&mut app, 120, height);
            assert!(
                app.detail_peer_scroll_offset < 10,
                "height {height}: offset {} is past the list",
                app.detail_peer_scroll_offset
            );
        }
    }

    // -- degenerate sizes ---------------------------------------------------

    #[test]
    fn every_tab_survives_a_tiny_terminal() {
        // Mirrors the help overlay's 10x4 guard. Detail does index arithmetic
        // against terminal height, so this is the file where it matters most.
        let mut t = torrent();
        t.files = vec![FileInfo {
            name: "disc.iso".to_string(),
            size_bytes: 1024,
            progress_bytes: 512,
        }];
        t.peers = peers(5);
        t.trackers = vec!["https://tracker.example/announce".to_string()];

        for tab in [
            DetailTab::Stats,
            DetailTab::Info,
            DetailTab::Files,
            DetailTab::Peers,
        ] {
            let mut app = app_showing(t.clone(), tab);
            let _ = draw(&mut app, 10, 4);
            let _ = draw(&mut app, 1, 1);
            let _ = draw(&mut app, 200, 60);
        }
    }

    #[test]
    fn renders_nothing_and_does_not_panic_with_no_selection() {
        let mut app = App::new();
        app.detail_tab = DetailTab::Peers;
        let _ = draw(&mut app, 100, 30);
    }
}
