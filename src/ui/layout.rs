use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::App;
use crate::types::AppMode;

pub fn get_layout(area: Rect) -> Vec<Rect> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(5),    // Main area
            Constraint::Length(3), // Status bar
        ])
        .split(area)
        .to_vec()
}

pub fn render_header(f: &mut Frame, area: Rect) {
    let title = Paragraph::new(Line::from(vec![
        Span::styled("TorrentTUI", Style::default().fg(Color::Cyan)),
        Span::raw(" v0.3.0"),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    )
    .centered();
    f.render_widget(title, area);
}

pub fn render_status_bar(f: &mut Frame, area: Rect, app: &App) {
    if let Some(ref err) = app.error_message {
        let error = Paragraph::new(Line::from(vec![Span::styled(
            err.clone(),
            Style::default().fg(Color::Red),
        )]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red)),
        );
        f.render_widget(error, area);
        return;
    }

    if let Some(ref info) = app.info_message {
        let info_widget = Paragraph::new(Line::from(vec![Span::styled(
            info.clone(),
            Style::default().fg(Color::Yellow),
        )]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        );
        f.render_widget(info_widget, area);
        return;
    }

    let down_speed = format_speed(app.total_download_speed());
    let up_speed = format_speed(app.total_upload_speed());
    let active = app.active_count();
    let total = app.torrents.len();

    let hints = match app.mode {
        AppMode::Normal => {
            if app.torrents.is_empty() {
                "a:add  /:filter  ?:help  q:quit"
            } else {
                "a:add  Space:mark  p:(un)pause  d:delete  Enter:detail  /:filter  t:throttle  ?:help  q:quit"
            }
        }
        AppMode::Input => "Enter:submit  Esc:cancel",
        AppMode::Detail => "Tab:switch tab  j/k:navigate  Space:toggle  S:apply  Esc:back",
        AppMode::Help => "Esc/?:close",
        AppMode::ConfirmDelete => "k:keep files  d:delete files  c:cancel",
        AppMode::ConfirmQuit => "y:quit  n:cancel",
        AppMode::Filter => "Enter:apply  Esc:clear & close",
        AppMode::ThrottleInput => "Enter:confirm  Esc:cancel",
    };

    // Build right-aligned speed section
    let mut right_spans = vec![Span::styled(
        format!("\u{2193} {}", down_speed),
        Style::default().fg(Color::Green),
    )];
    if app.speed_limit_download_kbps > 0 {
        right_spans.push(Span::styled(
            format!(" [{}]", format_speed(app.speed_limit_download_kbps * 1024)),
            Style::default().fg(Color::DarkGray),
        ));
    }
    right_spans.push(Span::raw("  "));

    let total_up = app.total_uploaded_bytes();
    let total_down = app.total_downloaded_bytes();
    if total_up > 0 || total_down > 0 {
        let ratio = if total_down > 0 {
            total_up as f64 / total_down as f64
        } else {
            0.0
        };
        right_spans.push(Span::styled(
            format!("R:{:.2}  ", ratio),
            Style::default().fg(Color::Gray),
        ));
    }

    right_spans.push(Span::styled(
        format!("\u{2191} {} ", up_speed),
        Style::default().fg(Color::Magenta),
    ));
    if app.speed_limit_upload_kbps > 0 {
        right_spans.push(Span::styled(
            format!("[{}] ", format_speed(app.speed_limit_upload_kbps * 1024)),
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Calculate width of speed section for the right column
    let right_text_width: u16 = right_spans.iter().map(|s| s.content.len() as u16).sum();

    // Build left section: hints, counts, disk, filter
    let mut left_spans = vec![
        Span::styled(format!(" {}", hints), Style::default().fg(Color::Gray)),
        Span::raw("  \u{2502}  "),
        Span::raw(format!("{} active / {} total", active, total)),
    ];

    if let Some(space) = app.free_disk_space {
        let space_str = format_size(space);
        let style = if space < 1_073_741_824 {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Gray)
        };
        left_spans.push(Span::raw("  \u{2502}  "));
        left_spans.push(Span::styled(format!("{} free", space_str), style));
    }

    if !app.filter_text.is_empty() && app.mode != AppMode::Filter {
        left_spans.push(Span::raw("  \u{2502}  "));
        left_spans.push(Span::styled(
            format!("filter: \"{}\"", app.filter_text),
            Style::default().fg(Color::Yellow),
        ));
    }

    if app.has_marks() {
        left_spans.push(Span::raw("  \u{2502}  "));
        left_spans.push(Span::styled(
            format!("{} marked", app.marked_count()),
            Style::default().fg(Color::Cyan),
        ));
    }

    // Split into two columns: left fills, right is fixed-width for speeds
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(right_text_width)])
        .split(inner);

    let left_widget = Paragraph::new(Line::from(left_spans));
    let right_widget =
        Paragraph::new(Line::from(right_spans)).alignment(ratatui::layout::Alignment::Right);

    f.render_widget(left_widget, columns[0]);
    f.render_widget(right_widget, columns[1]);
}

pub fn render_filter_bar(f: &mut Frame, area: Rect, filter_text: &str) {
    let line = Line::from(vec![
        Span::styled(" Filter: ", Style::default().fg(Color::Cyan)),
        Span::raw(filter_text),
        Span::styled("\u{2588}", Style::default().fg(Color::White)), // cursor
    ]);

    let bar = Paragraph::new(line).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(bar, area);
}

pub fn render_throttle_bar(f: &mut Frame, area: Rect, step: u8, input_buf: &str) {
    let prompt = if step == 0 {
        " Download limit (KB/s, 0=unlimited): "
    } else {
        " Upload limit (KB/s, 0=unlimited): "
    };

    let line = Line::from(vec![
        Span::styled(prompt, Style::default().fg(Color::Cyan)),
        Span::raw(input_buf),
        Span::styled("\u{2588}", Style::default().fg(Color::White)),
    ]);

    let bar = Paragraph::new(line).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    f.render_widget(bar, area);
}

pub fn format_speed(bytes_per_sec: u64) -> String {
    if bytes_per_sec == 0 {
        return "0 B/s".to_string();
    }
    let kb = bytes_per_sec as f64 / 1024.0;
    if kb < 1024.0 {
        return format!("{:.1} KB/s", kb);
    }
    let mb = kb / 1024.0;
    if mb < 1024.0 {
        return format!("{:.1} MB/s", mb);
    }
    let gb = mb / 1024.0;
    format!("{:.2} GB/s", gb)
}

pub fn format_size(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".to_string();
    }
    let kb = bytes as f64 / 1024.0;
    if kb < 1024.0 {
        return format!("{:.0} KB", kb);
    }
    let mb = kb / 1024.0;
    if mb < 1024.0 {
        return format!("{:.1} MB", mb);
    }
    let gb = mb / 1024.0;
    format!("{:.2} GB", gb)
}

pub fn format_eta(seconds: Option<u64>) -> String {
    match seconds {
        None => "\u{2014}".to_string(),
        Some(0) => "\u{2014}".to_string(),
        Some(s) => {
            let hours = s / 3600;
            let mins = (s % 3600) / 60;
            let secs = s % 60;
            if hours > 0 {
                format!("{}h {}m", hours, mins)
            } else if mins > 0 {
                format!("{}m {}s", mins, secs)
            } else {
                format!("{}s", secs)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_zero() {
        assert_eq!(format_speed(0), "0 B/s");
    }

    #[test]
    fn speed_kilobytes() {
        assert_eq!(format_speed(1536), "1.5 KB/s");
        assert_eq!(format_speed(1024), "1.0 KB/s");
    }

    #[test]
    fn speed_megabytes() {
        assert_eq!(format_speed(1024 * 1024), "1.0 MB/s");
    }

    #[test]
    fn speed_gigabytes() {
        assert_eq!(format_speed(1024 * 1024 * 1024), "1.00 GB/s");
    }

    #[test]
    fn size_zero() {
        assert_eq!(format_size(0), "0 B");
    }

    #[test]
    fn size_kilobytes() {
        assert_eq!(format_size(2048), "2 KB");
    }

    #[test]
    fn size_megabytes() {
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
    }

    #[test]
    fn size_gigabytes() {
        assert_eq!(format_size(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn eta_none() {
        assert_eq!(format_eta(None), "\u{2014}");
    }

    #[test]
    fn eta_zero() {
        assert_eq!(format_eta(Some(0)), "\u{2014}");
    }

    #[test]
    fn eta_seconds_only() {
        assert_eq!(format_eta(Some(45)), "45s");
    }

    #[test]
    fn eta_minutes_seconds() {
        assert_eq!(format_eta(Some(125)), "2m 5s");
    }

    #[test]
    fn eta_hours_minutes() {
        assert_eq!(format_eta(Some(3661)), "1h 1m");
    }
}
