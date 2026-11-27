const FILLED: char = '█';
const EMPTY: char = '░';

use ratatui::{
    style::{Color, Style},
    text::Span,
};

/// Render a block-glyph bar `width` cells wide with the numeric percentage
/// centred inside it. The column title carries the `%` unit so this keeps
/// all of the percentage space in the bar itself.
///
/// `percent` is clamped to 0..=100 before use: librqbit can briefly report
/// more downloaded than total after a recheck, and an unclamped value would
/// overrun the cell.
pub fn render_progress_bar(percent: f64, width: usize) -> String {
    let percent = percent.clamp(0.0, 100.0);
    if width == 0 {
        return format!("{:.1}", percent);
    }

    let filled = (((percent / 100.0) * width as f64).round() as usize).min(width);
    let empty = width - filled;
    let mut bar = String::with_capacity(width * 3);
    for _ in 0..filled {
        bar.push(FILLED);
    }
    for _ in 0..empty {
        bar.push(EMPTY);
    }

    let label = format!("{:.1}", percent);
    let label: String = label.chars().take(width).collect();
    let start = (width.saturating_sub(label.chars().count())) / 2;
    let mut chars: Vec<char> = bar.chars().collect();
    for (offset, ch) in label.chars().enumerate() {
        chars[start + offset] = ch;
    }
    chars.into_iter().collect()
}

/// Render a bar as styled spans so the numeric percentage can be inverted
/// over filled cells: black text on the progress fill, without changing the
/// rest of the bar.
pub fn render_progress_spans(
    percent: f64,
    width: usize,
    fill: Color,
    base_bg: Option<Color>,
) -> Vec<Span<'static>> {
    let percent = percent.clamp(0.0, 100.0);
    if width == 0 {
        return vec![Span::raw(format!("{:.1}", percent))];
    }

    let filled = (((percent / 100.0) * width as f64).round() as usize).min(width);
    let label = format!("{:.1}", percent);
    let label_chars: Vec<char> = label.chars().take(width).collect();
    let start = width.saturating_sub(label_chars.len()) / 2;

    let mut spans = Vec::with_capacity(width);
    for index in 0..width {
        let label_offset = index
            .checked_sub(start)
            .filter(|offset| *offset < label_chars.len());
        let character = if let Some(offset) = label_offset {
            label_chars[offset]
        } else {
            if index < filled {
                FILLED
            } else {
                EMPTY
            }
        };

        let style = if label_offset.is_some() {
            Style::default().fg(Color::Black).bg(fill)
        } else if let Some(background) = base_bg {
            Style::default().fg(fill).bg(background)
        } else {
            Style::default().fg(fill)
        };
        spans.push(Span::styled(character.to_string(), style));
    }
    spans
}

/// Braille spinner frames for the "fetching metadata" cell. The length must
/// stay at 10: `App::tick_spinner` advances `spinner_tick` with `% 10` and
/// `table.rs` indexes this slice with it unchecked, so shortening this array
/// panics the render. A test in this module pins the length.
pub const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_bar_zero() {
        let bar = render_progress_bar(0.0, 10);
        assert!(bar.contains("0.0"));
        assert_eq!(bar.chars().count(), 10);
    }

    #[test]
    fn progress_bar_fifty() {
        let bar = render_progress_bar(50.0, 10);
        assert!(bar.contains("50.0"));
        assert_eq!(bar.chars().count(), 10);
    }

    #[test]
    fn progress_bar_hundred() {
        let bar = render_progress_bar(100.0, 10);
        assert!(bar.contains("100.0"));
        assert_eq!(bar.chars().count(), 10);
    }

    #[test]
    fn progress_bar_zero_width() {
        let bar = render_progress_bar(50.0, 0);
        assert_eq!(bar, "50.0");
    }

    #[test]
    fn progress_bar_clamps_over_100() {
        // Defensive clamp: filled must never exceed width.
        let bar = render_progress_bar(150.0, 10);
        assert!(bar.contains("100.0"));
    }

    #[test]
    fn progress_bar_clamps_negative() {
        let bar = render_progress_bar(-10.0, 10);
        assert!(bar.contains("0.0"));
    }

    #[test]
    fn progress_spans_invert_filled_label_positions() {
        let spans = render_progress_spans(100.0, 11, Color::Green, None);
        assert_eq!(spans.len(), 11);
        let label_spans: Vec<&Span<'static>> = spans
            .iter()
            .filter(|span| span.style.fg == Some(Color::Black))
            .collect();
        assert_eq!(label_spans.len(), 5);
        assert!(label_spans
            .iter()
            .all(|span| span.style.bg == Some(Color::Green)));
    }

    #[test]
    fn progress_spans_keep_the_partial_label_as_one_inverted_block() {
        let spans = render_progress_spans(50.0, 11, Color::Yellow, Some(Color::DarkGray));
        let label_spans: Vec<&Span<'static>> = spans
            .iter()
            .filter(|span| span.style.fg == Some(Color::Black))
            .collect();
        assert_eq!(label_spans.len(), 4);
        assert!(label_spans
            .iter()
            .all(|span| span.style.bg == Some(Color::Yellow)));
    }

    #[test]
    fn spinner_frame_count_matches_app_modulo() {
        // app.rs::tick_spinner cycles spinner_tick with `% 10`, and table.rs
        // indexes SPINNER_FRAMES[spinner_tick]. If this length ever drifts from
        // 10, that index panics — keep them in lockstep.
        assert_eq!(SPINNER_FRAMES.len(), 10);
    }
}
