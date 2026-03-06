use ratatui::style::Color;

pub fn render_progress_bar(percent: f64, width: usize) -> String {
    let filled = ((percent / 100.0) * width as f64).round() as usize;
    let empty = width.saturating_sub(filled);
    let bar: String = "█".repeat(filled) + &"░".repeat(empty);
    format!("{} {:>5.1}%", bar, percent)
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
