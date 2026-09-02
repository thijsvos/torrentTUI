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

/// `resolving` is how many magnets are still waiting for metadata: quitting
/// drops them, and they are not "downloads" — say what is actually at stake.
pub fn render_quit_dialog(f: &mut Frame, area: Rect, resolving: usize) {
    let mut popup = centered_rect(40, 20, area);
    // 40% of an 80-column terminal is 32 columns, which clips the resolving
    // sentence mid-word; same floor the detach dialog uses.
    popup.width = popup.width.max(58.min(area.width));
    popup.height = popup.height.max(7).min(area.height);
    f.render_widget(Clear, popup);

    let what = match resolving {
        0 => "  Active downloads in progress.".to_string(),
        1 => "  A magnet is still resolving; quitting drops it.".to_string(),
        n => format!("  {n} magnets are still resolving; quitting drops them."),
    };
    let text = vec![
        Line::from(""),
        Line::from(Span::styled(what, Style::default().fg(Color::White))),
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

/// Confirm handing the session to a background process.
///
/// The word **seeding** is load-bearing, not padding: it is the difference
/// between an informed choice and the silent-seeding failure the project's
/// privacy posture exists to prevent. So are the two commands — a user who is
/// told a process will outlive their window must be told how to end it in the
/// same breath.
///
/// `torrent_count` is stated because "N torrents" is what makes the consequence
/// concrete, and `streaming` warns that an open player will stop: the HTTP API
/// belongs to this process, and the background copy binds a fresh port with a
/// fresh per-run password.
pub fn render_detach_dialog(f: &mut Frame, area: Rect, torrent_count: usize, streaming: bool) {
    let mut popup = centered_rect(58, 40, area);
    // Percentage height collapses to a handful of rows on an 80x24 terminal,
    // which would clip the bottom of the dialog — and the bottom is where the
    // answer keys and the two commands live.
    //
    // Counted, not guessed: blank, question, blank, two consequence lines,
    // the optional stream warning, blank, Reattach, Stop, blank, [Y]es/[N]o —
    // plus two border rows. Getting this one row short shipped a dialog whose
    // [Y]es/[N]o line was cut off, which is only visible by looking at it.
    let needed = if streaming { 13 } else { 12 };
    popup.height = popup.height.max(needed).min(area.height);
    popup.width = popup.width.max(46.min(area.width));
    f.render_widget(Clear, popup);

    let dim = Style::default().fg(Color::DarkGray);
    let mut text = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Keep downloading after TorrentTUI closes?",
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!(
                "  {} torrent{} will keep downloading AND",
                torrent_count,
                if torrent_count == 1 { "" } else { "s" }
            ),
            dim,
        )),
        Line::from(Span::styled(
            "  seeding in the background until you stop it.",
            dim,
        )),
    ];

    if streaming {
        text.push(Line::from(Span::styled(
            "  An open stream will stop playing.",
            dim,
        )));
    }

    text.extend([
        Line::from(""),
        Line::from(Span::styled("  Reattach:  torrenttui", dim)),
        Line::from(Span::styled("  Stop:      torrenttui --stop", dim)),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  [Y]",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("es   "),
            Span::styled(
                "[N]",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw("o"),
        ]),
    ]);

    let dialog = Paragraph::new(text).block(
        Block::default()
            .title(" Detach to Background ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    );
    f.render_widget(dialog, popup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn screen(width: u16, height: u16, streaming: bool) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| render_detach_dialog(f, f.area(), 3, streaming))
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
    fn detach_dialog_states_the_consequence_and_the_way_out() {
        let text = screen(100, 30, false);
        assert!(text.contains("Detach"), "{text}");
        // The three things a user must not have to guess.
        assert!(text.contains("seeding"), "{text}");
        assert!(text.contains("torrenttui --stop"), "{text}");
        assert!(text.contains("3 torrents"), "{text}");
    }

    #[test]
    fn detach_dialog_shows_the_answer_keys() {
        // Regression: the popup was one row short, so the line telling the user
        // which key answers the question was clipped off the bottom.
        for streaming in [true, false] {
            let text = screen(100, 30, streaming);
            assert!(text.contains("[Y]"), "streaming={streaming}: {text}");
            assert!(text.contains("[N]"), "streaming={streaming}: {text}");
        }
    }

    #[test]
    fn detach_dialog_warns_about_an_open_stream() {
        assert!(screen(100, 30, true).contains("stream"));
        assert!(!screen(100, 30, false).contains("stream"));
    }

    #[test]
    fn detach_dialog_does_not_panic_on_a_tiny_terminal() {
        // Mirrors the help overlay's 10x4 guard: the layout must clamp rather
        // than index past the area.
        let _ = screen(10, 4, true);
        let _ = screen(1, 1, false);
    }

    fn quit_screen(resolving: usize) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|f| render_quit_dialog(f, f.area(), resolving))
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
    fn quit_dialog_says_what_a_quit_would_drop() {
        assert!(quit_screen(0).contains("Active downloads in progress."));
        let one = quit_screen(1);
        assert!(
            one.contains("A magnet is still resolving; quitting drops it."),
            "{one}"
        );
        let two = quit_screen(2);
        assert!(two.contains("2 magnets are still resolving"), "{two}");
        assert!(two.contains("[Y]") && two.contains("[N]"), "{two}");
    }
}
