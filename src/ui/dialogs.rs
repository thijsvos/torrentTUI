use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::theme::Theme;
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
///
/// `leftovers` is how many files `[K]eep files` cleans up (what is left of
/// unselected files, #109); when there are any, a line says so, because
/// "keep" removing files must not come as a surprise.
pub fn render_delete_dialog(
    f: &mut Frame,
    area: Rect,
    torrent_name: &str,
    watch_dir_configured: bool,
    leftovers: usize,
    theme: Theme,
) {
    let mut popup = centered_rect(50, 25, area);
    // The percentage height is only 6 rows on an 80x24 terminal, which is
    // exactly the borders plus the four base lines. Grow it so the optional
    // notes are not clipped off the bottom.
    let needed = 6 + 2 * u16::from(watch_dir_configured) + u16::from(leftovers > 0);
    popup.height = popup.height.max(needed).min(area.height);
    // Half of an 80-column terminal is 40 columns, which cuts either note off
    // mid-sentence; same idea as the quit dialog's floor.
    if watch_dir_configured || leftovers > 0 {
        popup.width = popup.width.max(56.min(area.width));
    }
    // Re-centre: `centered_rect` placed the popup for its original size, and
    // a grown one would otherwise run off the right or bottom edge.
    popup.x = area.x + area.width.saturating_sub(popup.width) / 2;
    popup.y = area.y + area.height.saturating_sub(popup.height) / 2;
    f.render_widget(Clear, popup);

    let mut text = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  Delete \"{}\"?", truncate(torrent_name, 40)),
            Style::default().fg(theme.text),
        )),
    ];

    if watch_dir_configured {
        text.push(Line::from(Span::styled(
            "  Also removes the .torrent from your watch folder",
            Style::default().fg(theme.muted),
        )));
    }

    match leftovers {
        0 => {}
        1 => text.push(Line::from(Span::styled(
            "  Keep also cleans up 1 file you didn't select",
            Style::default().fg(theme.muted),
        ))),
        n => text.push(Line::from(Span::styled(
            format!("  Keep also cleans up {n} files you didn't select"),
            Style::default().fg(theme.muted),
        ))),
    }

    text.extend([
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  [K]",
                Style::default()
                    .fg(theme.success)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("eep files   "),
            Span::styled(
                "[D]",
                Style::default()
                    .fg(theme.error)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("elete files   "),
            Span::styled(
                "[C]",
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("ancel"),
        ]),
    ]);

    let dialog = Paragraph::new(text).block(
        Block::default()
            .title(" Confirm Delete ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.error)),
    );
    f.render_widget(dialog, popup);
}

/// `resolving` is how many magnets are still waiting for metadata: quitting
/// drops them, and they are not "downloads" — say what is actually at stake.
pub fn render_quit_dialog(f: &mut Frame, area: Rect, resolving: usize, theme: Theme) {
    let mut popup = centered_rect(40, 20, area);
    // 40% of an 80-column terminal is 32 columns, which clips the resolving
    // sentence mid-word; same floor the detach dialog uses.
    popup.width = popup.width.max(58.min(area.width));
    popup.height = popup.height.max(7).min(area.height);
    f.render_widget(Clear, popup);

    let what = match resolving {
        0 => "  Quitting stops all downloading and seeding.".to_string(),
        1 => "  A magnet is still resolving; quitting drops it.".to_string(),
        n => format!("  {n} magnets are still resolving; quitting drops them."),
    };
    let text = vec![
        Line::from(""),
        Line::from(Span::styled(what, Style::default().fg(theme.text))),
        Line::from(Span::styled(
            "  Really quit?",
            Style::default().fg(theme.text),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  [Y]",
                Style::default()
                    .fg(theme.error)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("es   "),
            Span::styled(
                "[N]",
                Style::default()
                    .fg(theme.success)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("o"),
        ]),
    ];

    let dialog = Paragraph::new(text).block(
        Block::default()
            .title(" Confirm Quit ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.warning)),
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
pub fn render_detach_dialog(
    f: &mut Frame,
    area: Rect,
    torrent_count: usize,
    streaming: bool,
    theme: Theme,
) {
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
    // Wide enough for the stop command whole, which grows with a profile's
    // name — a cut-off command is no way out at all.
    let reattach = format!("  Reattach:  {}", crate::config::command(""));
    let stop = format!("  Stop:      {}", crate::config::command("--stop"));
    let wide = u16::try_from(stop.chars().count() + 3).unwrap_or(u16::MAX);
    popup.width = popup.width.max(wide.max(46)).min(area.width);
    popup.x = area.x + (area.width - popup.width) / 2;
    f.render_widget(Clear, popup);

    let dim = Style::default().fg(theme.muted);
    let mut text = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Keep downloading after TorrentTUI closes?",
            Style::default().fg(theme.text),
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
        Line::from(Span::styled(reattach, dim)),
        Line::from(Span::styled(stop, dim)),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  [Y]",
                Style::default()
                    .fg(theme.success)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("es   "),
            Span::styled(
                "[N]",
                Style::default()
                    .fg(theme.error)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("o"),
        ]),
    ]);

    let dialog = Paragraph::new(text).block(
        Block::default()
            .title(" Detach to Background ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.warning)),
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
            .draw(|f| render_detach_dialog(f, f.area(), 3, streaming, Theme::system()))
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

    fn delete_screen(watch: bool, leftovers: usize) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|f| render_delete_dialog(f, f.area(), "Show", watch, leftovers, Theme::system()))
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
    fn delete_dialog_says_when_keep_cleans_up_unselected_files() {
        // #109: "keep" removing files must not come as a surprise.
        let none = delete_screen(false, 0);
        assert!(!none.contains("cleans up"), "{none}");
        let one = delete_screen(false, 1);
        assert!(
            one.contains("Keep also cleans up 1 file you didn't select"),
            "{one}"
        );
        let many = delete_screen(true, 3);
        assert!(
            many.contains("Keep also cleans up 3 files you didn't select"),
            "{many}"
        );
        assert!(many.contains("watch folder"), "{many}");
        // The answer keys survive both notes.
        assert!(
            many.contains("[K]") && many.contains("[D]") && many.contains("[C]"),
            "{many}"
        );
    }

    #[test]
    fn delete_dialog_fits_both_notes_on_a_small_terminal() {
        // 80x24: the percentage height alone is 6 rows, the base content.
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| render_delete_dialog(f, f.area(), "Show", true, 2, Theme::system()))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("[K]"), "{text}");
        assert!(
            text.contains("Keep also cleans up 2 files you didn't select"),
            "{text}"
        );
        assert!(
            text.contains("Also removes the .torrent from your watch folder"),
            "{text}"
        );
    }

    #[test]
    fn delete_dialog_stays_on_screen_on_a_narrow_terminal() {
        for (w, h) in [(60, 20), (30, 8), (10, 4), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|f| {
                    let area = f.area();
                    render_delete_dialog(f, area, "Show", true, 5, Theme::system());
                })
                .unwrap();
        }
    }

    fn quit_screen(resolving: usize) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|f| render_quit_dialog(f, f.area(), resolving, Theme::system()))
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
        // A seed-only session prompts too, so "downloads" alone was wrong.
        assert!(quit_screen(0).contains("Quitting stops all downloading and seeding."));
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
