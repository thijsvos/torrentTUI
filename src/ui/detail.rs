//! The Detail view: five tabs (Stats, Info, Files, Peers, Health) over one
//! torrent.
//!
//! Everything here depends on the engine having been told which torrent is
//! being viewed. `TorrentInfo::files`, `peers`, `trackers`, `info_hash` and
//! `piece_length` are populated only for the Detail target; entering and
//! leaving this view sends `SetDetailTorrent`, and without that every tab but
//! Stats and Health renders empty (Health keeps its verdict — the numbers it
//! needs travel with every torrent — but loses the per-tracker table).
//!
//! All renderers return early and draw nothing when there is no selection,
//! which is safe only because the caller has already cleared the frame.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Tabs},
    Frame,
};

use crate::app::App;
use crate::health::{self, Severity};
use crate::types::{DetailTab, PeerInfo, TrackerStatus, UpnpState};
use crate::ui::layout::{format_eta, format_size, format_speed};
use crate::ui::progress::render_progress_bar;
use crate::ui::util::{is_streamable_media, truncate};

/// Label column width shared by the Stats and Health tabs.
const LABEL: Style = Style::new().fg(Color::DarkGray);

fn labelled(label: &'static str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![Span::styled(label, LABEL), Span::raw(value.into())])
}

/// Colour for a verdict's glyph. Never the only signal: every severity also
/// has a distinct glyph and word (#77).
fn severity_style(severity: Severity) -> Style {
    match severity {
        Severity::Healthy => Style::default().fg(Color::Green),
        Severity::Note | Severity::Capped => Style::default().fg(Color::Yellow),
        Severity::Stalled | Severity::Blocked => Style::default().fg(Color::Red),
    }
}

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
    let tab_titles = vec!["Stats", "Info", "Files", "Peers", "Health"];
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
        DetailTab::Health => render_health_tab(f, chunks[2], app),
    }
}

fn render_stats_tab(f: &mut Frame, area: Rect, app: &App) {
    let torrent = match app.selected_torrent() {
        Some(t) => t,
        None => return,
    };

    let percent = torrent.progress_percent();
    let progress = render_progress_bar(percent, 30);

    // The verdict headline is the answer to "why is it slow?" — it belongs on
    // the first tab the user lands on, with the Health tab holding the why.
    let verdict = app.diagnose(torrent.id, std::time::Instant::now());

    let mut stats_text = vec![
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
    if let Some(v) = verdict {
        stats_text.push(Line::from(vec![
            Span::styled("  Health:    ", LABEL),
            Span::styled(
                format!("{} {}", v.severity.glyph(), v.severity.label()),
                severity_style(v.severity),
            ),
            Span::raw(format!(" \u{2014} {}", v.headline)),
        ]));
        stats_text.push(Line::from(Span::styled(
            "             (Health tab for the details)",
            LABEL,
        )));
    }

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
            if torrent.health.trackers.stripped_udp > 0 {
                "    (none left — udp:// trackers stripped under the proxy)"
            } else {
                "    (DHT only)"
            },
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for tracker in &torrent.trackers {
            lines.push(Line::from(vec![
                Span::raw(format!("    {}  ", tracker.url)),
                Span::styled(
                    format!("[{}]", tracker.status.label()),
                    tracker_status_style(&tracker.status),
                ),
            ]));
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

fn render_files_tab(f: &mut Frame, area: Rect, app: &mut App) {
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
    let file_count = torrent.files.len();

    // Keep the highlighted row on screen. Without this the Files tab drew every
    // row into a Paragraph with no scroll at all: a torrent with more files
    // than the pane is tall simply ran off the bottom, and `j` moved a cursor
    // nobody could see. Same three clamps as the Peers tab — lower the offset
    // to follow the cursor up, raise it to follow the cursor down, then bound
    // it so a shrinking list cannot leave the window past the end.
    //
    // Only the block borders are subtracted here; unlike Peers there are no
    // header lines in this pane.
    let visible_height = area.height.saturating_sub(2) as usize;
    let file_index = app.detail_file_index.min(file_count.saturating_sub(1));
    app.detail_file_index = file_index;
    if file_index < app.detail_file_scroll_offset {
        app.detail_file_scroll_offset = file_index;
    }
    if visible_height > 0 && file_index >= app.detail_file_scroll_offset + visible_height {
        app.detail_file_scroll_offset = file_index + 1 - visible_height;
    }
    let max_offset = file_count.saturating_sub(visible_height.max(1));
    if app.detail_file_scroll_offset > max_offset {
        app.detail_file_scroll_offset = max_offset;
    }
    let scroll_offset = app.detail_file_scroll_offset;

    // Re-borrow immutably; no more `app` mutation below.
    let torrent = match app.selected_torrent() {
        Some(t) => t,
        None => return,
    };

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

    let files_widget = Paragraph::new(lines)
        .scroll((scroll_offset as u16, 0))
        .block(
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
    // Live peers first, then by bytes: the list now includes dead and
    // connecting peers (the doctor needs them), and the ones actually sending
    // are what the tab is for.
    sorted.sort_by_key(|p| {
        (
            !p.state.eq_ignore_ascii_case("live"),
            std::cmp::Reverse(p.downloaded_bytes),
        )
    });

    // The client column only fits on a wide pane; 66 columns without it.
    let show_client = area.width >= 100;
    let header = if show_client {
        format!(
            "  {:<22} {:<12} {:>12} {:>8} {:>6}  {}",
            "Address", "State", "Downloaded", "Pieces", "Errs", "Client"
        )
    } else {
        format!(
            "  {:<22} {:<12} {:>12} {:>8} {:>6}",
            "Address", "State", "Downloaded", "Pieces", "Errs"
        )
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("  Connected: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}", peers_connected)),
            Span::styled("  /  Total seen: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}", peers_total)),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            header,
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

        let mut row = format!(
            "{}{:<22} {:<12} {:>12} {:>8} {:>6}",
            prefix,
            truncate(&peer.address, 22),
            truncate(&peer.state, 12),
            format_size(peer.downloaded_bytes),
            peer.pieces,
            peer.errors
        );
        if show_client {
            row.push_str("  ");
            row.push_str(&truncate(peer.client_name.as_deref().unwrap_or(""), 24));
        }
        lines.push(Line::from(Span::styled(row, style)));
    }

    let peers_widget = Paragraph::new(lines).block(
        Block::default()
            .title(format!(" Peers ({}) - j/k:scroll ", peer_count))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(peers_widget, area);
}

fn tracker_status_style(status: &TrackerStatus) -> Style {
    match status {
        TrackerStatus::Ok { .. } => Style::default().fg(Color::Green),
        TrackerStatus::Pending => Style::default().fg(Color::DarkGray),
        TrackerStatus::Failing { .. } | TrackerStatus::BypassesProxy => {
            Style::default().fg(Color::Red)
        }
        TrackerStatus::Unsupported => Style::default().fg(Color::Yellow),
    }
}

/// The Health tab: the doctor's verdict on top, then every number it looked
/// at, grouped by question — can we find peers, can we reach them, is the
/// data any good, and how is the session as a whole. Scrolls with `j`/`k`
/// because a torrent with a dozen trackers outgrows a 24-line terminal.
fn render_health_tab(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(torrent) = app.selected_torrent() else {
        return;
    };
    let now = std::time::Instant::now();
    let verdict = app.diagnose(torrent.id, now);
    let proxied = app.privacy.as_ref().is_some_and(|p| p.proxy);
    let h = &torrent.health;
    let mut lines: Vec<Line> = Vec::new();

    if let Some(v) = &verdict {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {} {}", v.severity.glyph(), v.severity.label()),
                severity_style(v.severity).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" \u{2014} {}", v.headline)),
        ]));
        for cause in &v.causes {
            lines.push(Line::from(format!("    \u{2022} {}", cause)));
        }
        if let Some(step) = &v.next_step {
            lines.push(Line::from(vec![
                Span::styled("  \u{2192} ", Style::default().fg(Color::Cyan)),
                Span::styled(step.clone(), Style::default().fg(Color::Cyan)),
            ]));
        }
        lines.push(Line::from(""));
    }

    // Peers.
    let p = &h.peers;
    lines.push(Line::from(Span::styled("  Peers", LABEL)));
    lines.push(Line::from(format!(
        "    live {} / seen {} \u{b7} dead {} \u{b7} connecting {} \u{b7} queued {} \u{b7} not needed {}",
        p.live, p.seen, p.dead, p.connecting, p.queued, p.not_needed
    )));
    lines.push(Line::from(format!(
        "    live by transport: tcp {} \u{b7} socks {} \u{b7} utp {}",
        p.live_tcp, p.live_socks, p.live_utp
    )));
    lines.push(Line::from(""));

    // Discovery.
    lines.push(Line::from(Span::styled("  Discovery", LABEL)));
    match &app.network_health {
        None => lines.push(Line::from(Span::styled(
            "    (session facts arrive a second after start)",
            LABEL,
        ))),
        Some(n) => {
            let dht = match &n.dht {
                None if proxied => "off (proxy lockdown)".to_string(),
                None => "disabled (network.enable_dht = false)".to_string(),
                Some(d) if d.routing_table_size + d.routing_table_size_v6 == 0 => {
                    "no nodes yet (bootstrapping, or UDP is blocked)".to_string()
                }
                Some(d) => format!(
                    "{} nodes (v6: {}), {} requests in flight",
                    d.routing_table_size, d.routing_table_size_v6, d.outstanding_requests
                ),
            };
            lines.push(labelled("    DHT:       ", dht));
            let listener = match n.listen_port {
                Some(port) => format!("port {}", port),
                None if proxied => "none (proxy lockdown)".to_string(),
                None => "none".to_string(),
            };
            lines.push(labelled("    Listener:  ", listener));
            let upnp = match &n.upnp {
                UpnpState::Off => "off".to_string(),
                UpnpState::Pending => "pending".to_string(),
                UpnpState::Forwarded => "forwarded".to_string(),
                UpnpState::Failed(e) => format!("failed: {}", e),
            };
            lines.push(labelled("    UPnP:      ", upnp));
            lines.push(labelled(
                "    uTP:       ",
                if n.utp_enabled {
                    "on"
                } else {
                    "off (TCP only)"
                },
            ));
        }
    }
    let tc = &h.trackers;
    let mut summary = format!("{}", tc.total);
    let mut parts = Vec::new();
    if tc.ok > 0 {
        parts.push(format!("{} ok", tc.ok));
    }
    if tc.failing > 0 {
        parts.push(format!("{} failing", tc.failing));
    }
    if tc.pending > 0 {
        parts.push(format!("{} pending", tc.pending));
    }
    if tc.unsupported > 0 {
        parts.push(format!("{} unsupported", tc.unsupported));
    }
    if tc.bypassing_proxy > 0 {
        parts.push(format!("{} bypassing the proxy", tc.bypassing_proxy));
    }
    if !parts.is_empty() {
        summary.push_str(" \u{2014} ");
        summary.push_str(&parts.join(", "));
    }
    if tc.stripped_udp > 0 {
        summary.push_str(&format!("; {} udp:// stripped", tc.stripped_udp));
    }
    lines.push(labelled("    Trackers:  ", summary));
    for tr in &torrent.trackers {
        let (detail, style) = match &tr.status {
            TrackerStatus::Ok {
                last_announce_secs_ago,
                next_in_secs,
            } => (
                match next_in_secs {
                    Some(n) => format!(
                        "{} ago, next in {}",
                        health::fmt_secs(*last_announce_secs_ago),
                        health::fmt_secs(*n)
                    ),
                    None => format!("{} ago", health::fmt_secs(*last_announce_secs_ago)),
                },
                tracker_status_style(&tr.status),
            ),
            TrackerStatus::Failing {
                last_error,
                secs_ago,
            } => (
                format!("{} ago: {}", health::fmt_secs(*secs_ago), last_error),
                tracker_status_style(&tr.status),
            ),
            TrackerStatus::Pending => (
                "no announce yet".to_string(),
                tracker_status_style(&tr.status),
            ),
            TrackerStatus::Unsupported => (
                "scheme librqbit does not speak".to_string(),
                tracker_status_style(&tr.status),
            ),
            TrackerStatus::BypassesProxy => (
                "udp:// announces go around the proxy".to_string(),
                tracker_status_style(&tr.status),
            ),
        };
        lines.push(Line::from(vec![
            Span::raw(format!("      {:<44} ", truncate(&tr.url, 44))),
            Span::styled(format!("{:<15}", tr.status.label()), style),
            Span::raw(detail),
        ]));
    }
    lines.push(Line::from(""));

    // Transfer.
    lines.push(Line::from(Span::styled("  Transfer", LABEL)));
    let avg = match h.avg_piece_ms {
        Some(ms) => format!("{:.1} s", ms as f64 / 1000.0),
        None => "\u{2014}".to_string(),
    };
    lines.push(Line::from(format!(
        "    avg piece {} \u{b7} fetched {} \u{b7} verified {} \u{b7} unverified {}",
        avg,
        format_size(h.fetched_bytes),
        format_size(h.checked_bytes),
        format_size(h.fetched_bytes.saturating_sub(h.checked_bytes)),
    )));
    lines.push(Line::from(""));

    // Session.
    lines.push(Line::from(Span::styled("  Session", LABEL)));
    if let Some(n) = &app.network_health {
        let c = &n.connect;
        let fmt = |t: &crate::types::TransportStats| {
            if t.attempts == 0 {
                "\u{2014}".to_string()
            } else {
                format!("{}/{} ok", t.successes, t.attempts)
            }
        };
        lines.push(Line::from(format!(
            "    uptime {} \u{b7} outgoing tcp {} \u{b7} socks {} \u{b7} utp {}",
            health::fmt_secs(n.uptime_secs),
            fmt(&c.tcp),
            fmt(&c.socks),
            fmt(&c.utp)
        )));
        lines.push(Line::from(format!(
            "    all torrents: live {} / seen {} \u{b7} blocklist rejected {} in / {} out",
            n.peers.live, n.peers.seen, n.blocked_incoming, n.blocked_outgoing
        )));
        for add in &n.pending_adds {
            lines.push(Line::from(Span::styled(
                format!(
                    "    {}",
                    health::pending_add_line(&add.label, add.secs, Some(n))
                ),
                Style::default().fg(Color::Yellow),
            )));
        }
    }

    // Scroll: keep the offset inside the content, like the Peers tab.
    let visible = area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(visible) as u16;
    if app.detail_health_scroll > max_scroll {
        app.detail_health_scroll = max_scroll;
    }
    let scroll = app.detail_health_scroll;

    let widget = Paragraph::new(lines).scroll((scroll, 0)).block(
        Block::default()
            .title(" Health - j/k:scroll ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(widget, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        FileInfo, PeerBreakdown, TorrentHealth, TorrentInfo, TorrentStatus, TrackerInfo,
        TrackerStatus,
    };
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
            health: TorrentHealth {
                peers: PeerBreakdown {
                    live: 42,
                    seen: 518,
                    dead: 400,
                    connecting: 6,
                    ..Default::default()
                },
                ..Default::default()
            },
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
                client_name: Some("qBittorrent 5.0".to_string()),
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

    fn tracker(url: &str, status: TrackerStatus) -> TrackerInfo {
        TrackerInfo {
            url: url.to_string(),
            status,
        }
    }

    #[test]
    fn info_tab_says_dht_only_when_there_are_no_trackers() {
        let mut app = app_showing(torrent(), DetailTab::Info);
        assert!(draw(&mut app, 100, 30).contains("(DHT only)"));

        let mut t = torrent();
        t.trackers = vec![tracker(
            "https://tracker.example/announce",
            TrackerStatus::Ok {
                last_announce_secs_ago: 5,
                next_in_secs: Some(100),
            },
        )];
        let mut app = app_showing(t, DetailTab::Info);
        let text = draw(&mut app, 100, 30);
        assert!(text.contains("tracker.example"), "{text}");
        assert!(text.contains("[ok]"), "status word missing: {text}");
        assert!(!text.contains("(DHT only)"), "{text}");
    }

    #[test]
    fn info_tab_explains_a_tracker_list_emptied_by_the_proxy_strip() {
        let mut t = torrent();
        t.health.trackers.stripped_udp = 3;
        let mut app = app_showing(t, DetailTab::Info);
        let text = draw(&mut app, 100, 30);
        assert!(text.contains("udp:// trackers stripped"), "{text}");
        assert!(!text.contains("(DHT only)"), "{text}");
    }

    // -- the Health tab -------------------------------------------------------

    #[test]
    fn stats_tab_carries_the_verdict_headline() {
        let mut app = app_showing(torrent(), DetailTab::Stats);
        let text = draw(&mut app, 120, 30);
        assert!(text.contains("Health:"), "{text}");
        // 42 live peers, flowing: healthy.
        assert!(text.contains("\u{2713} Healthy"), "{text}");
        assert!(text.contains("42 peers"), "{text}");
    }

    #[test]
    fn health_tab_shows_verdict_sections_and_trackers() {
        let mut t = torrent();
        t.trackers = vec![
            tracker(
                "https://tracker.opentrackr.org:443/announce",
                TrackerStatus::Ok {
                    last_announce_secs_ago: 180,
                    next_in_secs: Some(1620),
                },
            ),
            tracker(
                "udp://dead.example:1337/announce",
                TrackerStatus::Failing {
                    last_error: "connection refused".to_string(),
                    secs_ago: 12,
                },
            ),
        ];
        t.health.trackers.total = 2;
        t.health.trackers.ok = 1;
        t.health.trackers.failing = 1;
        t.health.avg_piece_ms = Some(800);
        let mut app = app_showing(t, DetailTab::Health);
        app.network_health = Some(crate::types::NetworkHealth {
            listen_port: Some(6881),
            dht: Some(crate::types::DhtHealth {
                routing_table_size: 312,
                routing_table_size_v6: 4,
                outstanding_requests: 2,
            }),
            uptime_secs: 3_720,
            ..Default::default()
        });
        let text = draw(&mut app, 140, 40);
        for needle in [
            "Healthy",
            "Peers",
            "live 42 / seen 518",
            "Discovery",
            "312 nodes",
            "port 6881",
            "off (TCP only)",
            "Trackers:  2 \u{2014} 1 ok, 1 failing",
            "tracker.opentrackr.org",
            "3 min ago, next in 27 min",
            "failing",
            "connection refused",
            "Transfer",
            "avg piece 0.8 s",
            "Session",
            "uptime 1 h 2 min",
        ] {
            assert!(text.contains(needle), "missing {needle:?}: {text}");
        }
    }

    #[test]
    fn health_tab_reports_a_stall_with_its_next_step() {
        let mut t = torrent();
        t.download_speed = 0;
        t.health.peers = PeerBreakdown {
            seen: 52,
            dead: 49,
            connecting: 3,
            ..Default::default()
        };
        let mut app = App::new();
        // The renderer reads the real clock; make the stall old enough by
        // back-dating the first push (which starts the clock) rather than
        // sleeping.
        let long_ago = std::time::Instant::now() - std::time::Duration::from_secs(120);
        app.handle_state_push_at(vec![t], long_ago);
        app.network_health = Some(crate::types::NetworkHealth {
            listen_port: Some(6881),
            dht: Some(crate::types::DhtHealth {
                routing_table_size: 300,
                ..Default::default()
            }),
            connect: crate::types::ConnectHealth {
                tcp: crate::types::TransportStats {
                    attempts: 214,
                    successes: 0,
                    errors: 214,
                },
                ..Default::default()
            },
            ..Default::default()
        });
        app.detail_tab = DetailTab::Health;
        let text = draw(&mut app, 140, 40);
        assert!(text.contains("\u{26a0} Stalled"), "{text}");
        assert!(text.contains("none reachable"), "{text}");
        assert!(
            text.contains("Every outgoing TCP connection failed"),
            "{text}"
        );
        assert!(text.contains("\u{2192} Something is blocking"), "{text}");
    }

    #[test]
    fn health_tab_lists_pending_adds_from_the_session() {
        let mut app = app_showing(torrent(), DetailTab::Health);
        app.network_health = Some(crate::types::NetworkHealth {
            pending_adds: vec![crate::types::PendingAdd {
                label: "ubuntu.iso".to_string(),
                secs: 45,
            }],
            ..Default::default()
        });
        let text = draw(&mut app, 140, 40);
        assert!(text.contains("Resolving \"ubuntu.iso\" for 45 s"), "{text}");
        assert!(text.contains("no peer has sent the metadata yet"), "{text}");
    }

    #[test]
    fn health_tab_scroll_is_clamped_to_the_content() {
        let mut t = torrent();
        t.trackers = (0..30)
            .map(|i| {
                tracker(
                    &format!("https://t{i}.example/announce"),
                    TrackerStatus::Pending,
                )
            })
            .collect();
        let mut app = app_showing(t, DetailTab::Health);
        app.detail_health_scroll = 500;
        let text = draw(&mut app, 120, 20);
        assert!(
            app.detail_health_scroll < 60,
            "{}",
            app.detail_health_scroll
        );
        // Scrolled to the end: the last tracker is on screen, the first is not.
        assert!(text.contains("t29.example"), "{text}");
        assert!(!text.contains("t0.example"), "{text}");
    }

    #[test]
    fn peers_tab_shows_client_names_only_on_a_wide_pane() {
        let mut t = torrent();
        t.peers = peers(2);
        let mut app = app_showing(t.clone(), DetailTab::Peers);
        assert!(draw(&mut app, 120, 30).contains("qBittorrent"));
        let mut app = app_showing(t, DetailTab::Peers);
        assert!(!draw(&mut app, 80, 30).contains("qBittorrent"));
    }

    #[test]
    fn peers_tab_lists_live_peers_before_dead_ones() {
        let mut t = torrent();
        let mut ps = peers(2);
        ps[0].state = "dead".to_string(); // the one with the most bytes
        ps[1].state = "live".to_string();
        t.peers = ps;
        let mut app = app_showing(t, DetailTab::Peers);
        let text = draw(&mut app, 120, 30);
        let live = text.find("10.0.0.2:6881").unwrap();
        let dead = text.find("10.0.0.1:6881").unwrap();
        assert!(live < dead, "live peer should sort first: {text}");
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

    // -- the file scroll invariant ------------------------------------------

    fn files(n: usize) -> Vec<FileInfo> {
        (0..n)
            .map(|i| FileInfo {
                name: format!("file-{:03}.bin", i),
                size_bytes: 1024,
                progress_bytes: (i * 8) as u64,
            })
            .collect()
    }

    /// Same invariant the peer list has: the highlighted row must lie inside
    /// the window actually drawn. Before this the Files tab had no scrolling
    /// at all — a long list ran off the bottom and `j` moved an invisible
    /// cursor.
    #[test]
    fn file_scroll_keeps_the_cursor_inside_the_window() {
        let mut t = torrent();
        t.files = files(60);
        let mut app = app_showing(t, DetailTab::Files);

        let height: u16 = 20;
        let visible = (height - 2) as usize;
        for index in [0usize, 1, 17, 18, 40, 59] {
            app.detail_file_index = index;
            draw(&mut app, 120, height);
            let offset = app.detail_file_scroll_offset;
            assert!(
                offset <= index && index < offset + visible,
                "index {index} outside window [{offset}, {})",
                offset + visible
            );
        }
    }

    #[test]
    fn a_file_past_the_fold_is_actually_drawn() {
        // The regression in one assertion: file 059 exists, so selecting it
        // must put it on screen.
        let mut t = torrent();
        t.files = files(60);
        let mut app = app_showing(t, DetailTab::Files);
        app.detail_file_index = 59;
        let text = draw(&mut app, 120, 20);
        assert!(text.contains("file-059.bin"), "last file not drawn: {text}");
        assert!(
            !text.contains("file-000.bin"),
            "should have scrolled past the top"
        );
    }

    #[test]
    fn file_scroll_follows_the_cursor_back_up() {
        let mut t = torrent();
        t.files = files(60);
        let mut app = app_showing(t, DetailTab::Files);
        app.detail_file_index = 59;
        draw(&mut app, 120, 20);
        assert!(app.detail_file_scroll_offset > 0);

        app.detail_file_index = 0;
        let text = draw(&mut app, 120, 20);
        assert_eq!(app.detail_file_scroll_offset, 0);
        assert!(text.contains("file-000.bin"), "{text}");
    }

    #[test]
    fn file_scroll_offset_is_bounded_when_the_list_shrinks() {
        let mut t = torrent();
        t.files = files(60);
        let mut app = app_showing(t, DetailTab::Files);
        app.detail_file_index = 59;
        draw(&mut app, 120, 20);
        assert!(app.detail_file_scroll_offset > 0);

        let mut t = torrent();
        t.files = files(3);
        app.handle_state_push(vec![t]);
        draw(&mut app, 120, 20);
        assert!(
            app.detail_file_scroll_offset < 3,
            "offset {} points past a 3-file list",
            app.detail_file_scroll_offset
        );
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
        t.trackers = vec![tracker(
            "https://tracker.example/announce",
            TrackerStatus::Pending,
        )];

        for tab in [
            DetailTab::Stats,
            DetailTab::Info,
            DetailTab::Files,
            DetailTab::Peers,
            DetailTab::Health,
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
