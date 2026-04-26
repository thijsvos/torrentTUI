use ratatui::style::Color;

const FILLED: char = '█';
const EMPTY: char = '░';

pub fn render_progress_bar(percent: f64, width: usize) -> String {
    let percent = percent.clamp(0.0, 100.0);
    let filled = (((percent / 100.0) * width as f64).round() as usize).min(width);
    let empty = width - filled;
    // Each char above is 3 bytes UTF-8; +8 for trailing " 100.0%".
    let mut bar = String::with_capacity(width * 3 + 8);
    for _ in 0..filled {
        bar.push(FILLED);
    }
    for _ in 0..empty {
        bar.push(EMPTY);
    }
    use std::fmt::Write;
    let _ = write!(bar, " {:>5.1}%", percent);
    bar
}

pub fn progress_color(percent: f64) -> Color {
    if percent >= 100.0 {
        Color::Green
    } else if percent >= 75.0 {
        Color::LightGreen
    } else if percent >= 50.0 {
        Color::Yellow
    } else if percent >= 25.0 {
        Color::Rgb(255, 165, 0) // Orange
    } else {
        Color::Red
    }
}

pub const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_bar_zero() {
        let bar = render_progress_bar(0.0, 10);
        assert!(bar.contains("0.0%"));
        assert!(bar.starts_with("░░░░░░░░░░"));
    }

    #[test]
    fn progress_bar_fifty() {
        let bar = render_progress_bar(50.0, 10);
        assert!(bar.contains("50.0%"));
        assert!(bar.starts_with("█████░░░░░"));
    }

    #[test]
    fn progress_bar_hundred() {
        let bar = render_progress_bar(100.0, 10);
        assert!(bar.contains("100.0%"));
        assert!(bar.starts_with("██████████"));
    }

    #[test]
    fn progress_bar_zero_width() {
        let bar = render_progress_bar(50.0, 0);
        assert!(bar.contains("50.0%"));
    }

    #[test]
    fn progress_bar_clamps_over_100() {
        // Defensive clamp: filled must never exceed width.
        let bar = render_progress_bar(150.0, 10);
        assert!(bar.starts_with("██████████"));
        assert!(bar.contains("100.0%"));
    }

    #[test]
    fn progress_bar_clamps_negative() {
        let bar = render_progress_bar(-10.0, 10);
        assert!(bar.starts_with("░░░░░░░░░░"));
        assert!(bar.contains("0.0%"));
    }

    #[test]
    fn color_thresholds() {
        assert_eq!(progress_color(0.0), Color::Red);
        assert_eq!(progress_color(24.9), Color::Red);
        assert_eq!(progress_color(25.0), Color::Rgb(255, 165, 0));
        assert_eq!(progress_color(49.9), Color::Rgb(255, 165, 0));
        assert_eq!(progress_color(50.0), Color::Yellow);
        assert_eq!(progress_color(74.9), Color::Yellow);
        assert_eq!(progress_color(75.0), Color::LightGreen);
        assert_eq!(progress_color(99.9), Color::LightGreen);
        assert_eq!(progress_color(100.0), Color::Green);
    }
}
