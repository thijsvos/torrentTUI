use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::ui::util::centered_rect;
use crate::ui::util::truncate;

/// Render the delete-confirmation popup. `watch_dir_configured` adds a line
/// making clear that both answers remove the `.torrent` from the watch folder —
/// otherwise `[K]eep files` reads as a promise the delete path no longer keeps.
///
/// That flag is wired from `config.general.watch_dir.is_some()` alone, so the
/// note also shows in the cases where the engine disables cleanup at startup
/// (watch folder equal to the download dir, unavailable, or a home/root path).
/// Erring toward the warning is deliberate; it is not a guarantee.
pub fn render_delete_dialog(
    f: &mut Frame,
    area: Rect,
    torrent_name: &str,
    watch_dir_configured: bool,
) {
    let mut popup = centered_rect(50, 25, area);
    // The percentage height is only 6 rows on an 80x24 terminal, which is
    // exactly the borders plus the four base lines. Grow it so the watch-folder
    // note is not clipped off the bottom.
    let needed = if watch_dir_configured { 8 } else { 6 };
    popup.height = popup.height.max(needed).min(area.height);
    f.render_widget(Clear, popup);

    let mut text = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  Delete \"{}\"?", truncate(torrent_name, 40)),
            Style::default().fg(Color::White),
        )),
    ];

    if watch_dir_configured {
        text.push(Line::from(Span::styled(
            "  Also removes the .torrent from your watch folder",
            Style::default().fg(Color::DarkGray),
        )));
    }

    text.extend([
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  [K]",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("eep files   "),
            Span::styled(
                "[D]",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw("elete files   "),
            Span::styled(
                "[C]",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("ancel"),
        ]),
    ]);

    let dialog = Paragraph::new(text).block(
        Block::default()
            .title(" Confirm Delete ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Red)),
    );
    f.render_widget(dialog, popup);
}

pub fn render_quit_dialog(f: &mut Frame, area: Rect) {
    let popup = centered_rect(40, 20, area);
    f.render_widget(Clear, popup);

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Active downloads in progress.",
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            "  Really quit?",
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  [Y]",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw("es   "),
            Span::styled(
                "[N]",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("o"),
        ]),
    ];

    let dialog = Paragraph::new(text).block(
        Block::default()
            .title(" Confirm Quit ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    );
    f.render_widget(dialog, popup);
}
