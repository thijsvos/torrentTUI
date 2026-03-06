use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Tabs},
    Frame,
};

use crate::app::App;
use crate::ui::layout::{format_eta, format_size, format_speed};
use crate::ui::progress::render_progress_bar;

pub fn render_detail(f: &mut Frame, area: Rect, app: &App) {
    let torrent = match app.selected_torrent() {
        Some(t) => t,
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
        &torrent.name,
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
        .select(app.detail_tab_index)
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
    match app.detail_tab_index {
        0 => render_stats_tab(f, chunks[2], app),
        1 => render_info_tab(f, chunks[2], app),
        2 => render_files_tab(f, chunks[2], app),
        3 => render_peers_tab(f, chunks[2], app),
        _ => {}
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
                format!("{:.2}", torrent.uploaded_bytes as f64 / torrent.downloaded_bytes as f64)
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
        format!("{:.2}", torrent.uploaded_bytes as f64 / torrent.downloaded_bytes as f64)
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
            Span::raw(format!("{}  (ratio: {})", format_size(torrent.uploaded_bytes), ratio)),
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
            file.progress_bytes as f64 / file.size_bytes as f64 * 100.0
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

        lines.push(Line::from(vec![
            Span::styled(if is_highlighted { "> " } else { "  " }, highlight_style),
            Span::styled(format!("{} ", checkbox), checkbox_style),
            Span::styled(format!("{:<45}", truncate(&file.name, 45)), file_style),
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
                " Files ({}) - Space:toggle  S:apply selection ",
                torrent.files.len()
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(files_widget, area);
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        format!("{}...", &s[..max_len - 3])
    } else {
        s.to_string()
    }
}

fn render_peers_tab(f: &mut Frame, area: Rect, app: &App) {
    let torrent = match app.selected_torrent() {
        Some(t) => t,
        None => return,
    };

    if torrent.peers.is_empty() {
        let text = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("  Connected:  ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{}", torrent.peers_connected)),
            ]),
            Line::from(vec![
                Span::styled("  Total seen: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{}", torrent.peers_total)),
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

    let mut lines = vec![
        Line::from(vec![
            Span::styled("  Connected: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}", torrent.peers_connected)),
            Span::styled("  /  Total seen: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}", torrent.peers_total)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                format!("  {:<22} {:<12} {:>12} {:>8} {:>6}",
                    "Address", "State", "Downloaded", "Pieces", "Errs"),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    // Respect scroll offset
    let visible_height = area.height.saturating_sub(6) as usize; // borders + header lines
    let scroll_offset = if torrent.peers.len() > visible_height {
        app.detail_peer_index.min(torrent.peers.len().saturating_sub(visible_height))
    } else {
        0
    };

    for (i, peer) in torrent.peers.iter().enumerate().skip(scroll_offset).take(visible_height) {
        let is_selected = i == app.detail_peer_index;
        let prefix = if is_selected { "> " } else { "  " };
        let style = if is_selected {
            Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        lines.push(Line::from(Span::styled(
            format!("{}{:<22} {:<12} {:>12} {:>8} {:>6}",
                prefix,
                truncate(&peer.address, 22),
                truncate(&peer.state, 12),
                format_size(peer.downloaded_bytes),
                peer.pieces,
                peer.errors),
            style,
        )));
    }

    let peers_widget = Paragraph::new(lines).block(
        Block::default()
            .title(format!(" Peers ({}) - j/k:scroll ", torrent.peers.len()))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    f.render_widget(peers_widget, area);
}
