//! Frame skeleton, the status/filter/throttle bars, and the human-readable
//! formatters (`format_size`, `format_speed`, `format_eta`) shared by every
//! widget.
//!
//! The formatters live here rather than in `util` because the bars are their
//! main consumer and their rounding choices are display decisions rather than
//! general-purpose utilities.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::App;
use crate::types::AppMode;

/// Split the frame into the three regions every mode shares: a 3-row header, a
/// flexible main area (table or detail view), and a 3-row bottom bar (status,
/// filter, throttle prompt or add-torrent input, depending on mode).
///
/// Always three elements, in that order — main.rs indexes them positionally, so
/// returning a different count panics rather than misrendering. The main area
/// is `Min(5)`, so on a very short terminal ratatui shrinks the fixed bars
/// instead and downstream renderers must tolerate a zero-height inner area.
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

pub fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![
        Span::styled("TorrentTUI", Style::default().fg(Color::Cyan)),
        Span::raw(concat!(" v", env!("CARGO_PKG_VERSION"))),
    ];
    // Privacy badge — rendered only from the engine-reported posture (what
    // the session actually started with), never from the raw config.
    if let Some(ref privacy) = app.privacy {
        let mut parts: Vec<&str> = Vec::new();
        if privacy.proxy {
            parts.push("proxy");
        }
        if let Some(ref iface) = privacy.bind_interface {
            parts.push(iface);
        }
        if privacy.blocklist_ranges.is_some() {
            parts.push("blocklist");
        }
        if !parts.is_empty() {
            spans.push(Span::styled(
                format!("  [{}]", parts.join("+")),
                Style::default().fg(Color::Green),
            ));
        }
    }
    let title = Paragraph::new(Line::from(spans))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .centered();
    f.render_widget(title, area);
}

/// Below this, the free-space readout is marked as a warning. Matches the
/// README's "low-space warnings".
const LOW_DISK_SPACE_BYTES: u64 = 1_073_741_824;

pub fn render_status_bar(f: &mut Frame, area: Rect, app: &App) {
    if let Some(ref err) = app.error_message {
        // Prefixed, not just reddened. An error and an informational notice
        // rendered in structurally identical widgets and were told apart by
        // hue alone — the one distinction that vanishes without colour.
        let error = Paragraph::new(Line::from(vec![Span::styled(
            format!("\u{2716} {}", err),
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

    // The Normal/Detail/SearchResults lines are assembled from the action
    // registry so they can never drift from the real bindings; the modal and
    // text-input modes keep literal strings (their keys aren't actions).
    let hints = match app.mode {
        AppMode::Normal => {
            crate::actions::hint_line(crate::actions::Scope::Normal, app.torrents.is_empty())
        }
        AppMode::Input => "Enter:submit  Esc:cancel".to_string(),
        AppMode::Detail => crate::actions::hint_line(crate::actions::Scope::Detail, false),
        AppMode::Help => "j/k:scroll  Esc/?:close".to_string(),
        AppMode::ConfirmDelete => "k:keep files  d:delete files  c:cancel".to_string(),
        AppMode::ConfirmQuit => "y:quit  n:cancel".to_string(),
        AppMode::ConfirmDetach => "y:detach  n:cancel".to_string(),
        AppMode::Filter => "Enter:apply  Esc:clear & close".to_string(),
        AppMode::ThrottleInput => "Enter:confirm  Esc:cancel".to_string(),
        AppMode::Search => "Enter:search  Esc:back".to_string(),
        AppMode::SearchResults => {
            crate::actions::hint_line(crate::actions::Scope::SearchResults, false)
        }
        AppMode::Palette => {
            "type to filter  Enter:run  \u{2191}/\u{2193}:navigate  Esc:close".to_string()
        }
    };

    // Build right-aligned speed section
    let mut right_spans = vec![Span::styled(
        format!("\u{2193} {}", down_speed),
        Style::default().fg(Color::Green),
    )];
    if app.speed_limit_download_kbps > 0 {
        right_spans.push(Span::styled(
            format!(
                " [{}]",
                format_speed(app.speed_limit_download_kbps.saturating_mul(1024))
            ),
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
            format!(
                "[{}] ",
                format_speed(app.speed_limit_upload_kbps.saturating_mul(1024))
            ),
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Calculate width of speed section for the right column
    // Use character count, not byte length: the ↓/↑ arrows are 3 UTF-8 bytes
    // but occupy a single terminal column, so `.len()` over-reserves the right
    // column by ~4 columns. All glyphs here are single-width, so chars().count()
    // is exact (use unicode_width if wide CJK ever appears in this section).
    let right_text_width: u16 = right_spans
        .iter()
        .map(|s| s.content.chars().count() as u16)
        .sum();

    // Build left section: hints, counts, disk, filter
    let mut left_spans = vec![
        Span::styled(format!(" {}", hints), Style::default().fg(Color::Gray)),
        Span::raw("  \u{2502}  "),
        Span::raw(format!("{} active / {} total", active, total)),
    ];

    if let Some(space) = app.free_disk_space {
        let space_str = format_size(space);
        // Both branches used to render the identical string and differ only in
        // colour, so the low-space warning did not exist at all on a monochrome
        // terminal, under NO_COLOR, or for a reader who cannot separate red
        // from grey. The glyph carries the signal; the colour reinforces it.
        let low = space < LOW_DISK_SPACE_BYTES;
        let style = if low {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Gray)
        };
        let text = if low {
            format!("\u{26a0} {} free", space_str)
        } else {
            format!("{} free", space_str)
        };
        left_spans.push(Span::raw("  \u{2502}  "));
        left_spans.push(Span::styled(text, style));
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

    // On a very narrow terminal the bordered inner area collapses to zero
    // width/height. ratatui renders that safely, but there's nothing to lay
    // out, so skip the column split and paragraph rendering entirely.
    if inner.width == 0 || inner.height == 0 {
        return;
    }

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

pub fn render_search_bar(f: &mut Frame, area: Rect, query: &str) {
    let line = Line::from(vec![
        Span::styled(" Search torrents: ", Style::default().fg(Color::Cyan)),
        Span::raw(query),
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

/// Format a byte-per-second rate for display. Divides by 1024 at each step but
/// labels the result KB/MB/GB, matching what other torrent clients show rather
/// than being strictly correct about KiB. Callers holding a KB/s value from the
/// throttle config must multiply by 1024 first.
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

/// Format a byte count for display, with the same 1024-per-step, KB-labelled
/// convention as `format_speed`. Anything under 1 MB is rendered as whole KB,
/// so sub-kilobyte files show as `0 KB` rather than in bytes — fine for torrent
/// payloads, wrong if this is ever reused for small values.
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
    fn header_shows_the_privacy_badge_only_when_the_engine_reported_one() {
        use crate::engine::torrent::PrivacyStatus;
        use ratatui::{backend::TestBackend, Terminal};

        let screen = |t: &Terminal<TestBackend>| -> String {
            t.backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect()
        };

        let mut app = App::new();
        let mut terminal = Terminal::new(TestBackend::new(80, 3)).unwrap();
        terminal.draw(|f| render_header(f, f.area(), &app)).unwrap();
        assert!(!screen(&terminal).contains('['), "no badge before report");

        app.privacy = Some(PrivacyStatus {
            proxy: true,
            bind_interface: Some("wg0".to_string()),
            blocklist_ranges: Some(3),
        });
        terminal.draw(|f| render_header(f, f.area(), &app)).unwrap();
        assert!(
            screen(&terminal).contains("[proxy+wg0+blocklist]"),
            "screen: {}",
            screen(&terminal)
        );
    }

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

    /// Render the status bar and flatten it to text, so an assertion can ask
    /// what a reader actually sees rather than what style was applied.
    fn status_text(app: &App) -> String {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(120, 3)).unwrap();
        terminal
            .draw(|f| render_status_bar(f, f.area(), app))
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
    fn low_disk_space_is_marked_without_relying_on_colour() {
        // Both branches used to render identical text and differ only in hue,
        // so the warning did not survive NO_COLOR, a monochrome terminal, or a
        // reader who cannot separate red from grey.
        let mut app = App::new();
        app.free_disk_space = Some(LOW_DISK_SPACE_BYTES - 1);
        let low = status_text(&app);
        assert!(low.contains('\u{26a0}'), "no warning glyph: {low}");

        app.free_disk_space = Some(LOW_DISK_SPACE_BYTES * 8);
        let plenty = status_text(&app);
        assert!(!plenty.contains('\u{26a0}'), "spurious warning: {plenty}");
    }

    #[test]
    fn an_error_is_distinguishable_from_a_notice_without_colour() {
        // Same widget, same borders; previously only the hue differed.
        let mut app = App::new();
        app.set_error("disk full".to_string());
        let err = status_text(&app);
        assert!(err.contains('\u{2716}'), "no error marker: {err}");

        let mut app = App::new();
        app.set_info("took over the background session".to_string());
        let info = status_text(&app);
        assert!(
            !info.contains('\u{2716}'),
            "notice marked as an error: {info}"
        );
    }
}
