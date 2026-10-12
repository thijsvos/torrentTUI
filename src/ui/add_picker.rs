//! The add picker overlay (`A`). While the engine fetches the file list it is
//! a small "fetching" box; then a centered popup with the file checklist, the
//! filter, the save folder, and what the selection needs against what the
//! folder's volume has free.
//!
//! The state and every rule live in `crate::add_picker`; this only draws it.
//! Keys are hinted in the status bar, which has the full width for them.

use std::time::Instant;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table},
    Frame,
};
use unicode_width::UnicodeWidthChar;

use crate::add_picker::{AddPickerState, Focus, Picker};
use crate::app::App;
use crate::theme::Theme;
use crate::ui::layout::format_size;
use crate::ui::util::truncate;

const CURSOR: &str = "\u{2588}";

pub fn render_add_picker(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = app.theme;
    match app.add_picker.as_mut() {
        None => {}
        Some(AddPickerState::Resolving { label, since, .. }) => {
            render_resolving(f, area, label, *since, theme)
        }
        Some(AddPickerState::Ready(picker)) => render_picker(f, area, picker, theme),
    }
}

/// A popup of `pct` of `area`, grown to at least `min` and centred again —
/// growing in place would push it past the bottom edge of a short terminal.
fn popup_rect(area: Rect, pct: (u16, u16), min: (u16, u16)) -> Rect {
    let w = (area.width as u32 * pct.0 as u32 / 100) as u16;
    let h = (area.height as u32 * pct.1 as u32 / 100) as u16;
    let w = w.max(min.0).min(area.width);
    let h = h.max(min.1).min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn render_resolving(f: &mut Frame, area: Rect, label: &str, since: Instant, theme: Theme) {
    let popup = popup_rect(area, (60, 20), (44, 6));
    f.render_widget(Clear, popup);
    let block = Block::default()
        .title(" Add torrent ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.primary));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let secs = since.elapsed().as_secs();
    let room = (inner.width as usize).saturating_sub(34).max(8);
    let text = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  Fetching the file list for ",
                Style::default().fg(theme.text),
            ),
            Span::styled(
                format!("\"{}\"", truncate(label, room)),
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}:{:02}", secs / 60, secs % 60),
                Style::default().fg(theme.muted),
            ),
        ]),
        Line::from(Span::styled(
            "  Nothing downloads until you choose. Esc cancels.",
            Style::default().fg(theme.muted),
        )),
    ];
    f.render_widget(Paragraph::new(text), inner);
}

fn render_picker(f: &mut Frame, area: Rect, picker: &mut Picker, theme: Theme) {
    let popup = popup_rect(area, (84, 84), (60, 14));
    f.render_widget(Clear, popup);
    let title_room = (popup.width as usize).saturating_sub(10).max(3);
    let block = Block::default()
        .title(format!(
            " Add \u{b7} {} ",
            truncate(&picker.name, title_room)
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.primary));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // file count, total, sort
            Constraint::Length(1), // filter
            Constraint::Min(1),    // files
            Constraint::Length(1), // save to
            Constraint::Length(1), // where the files land
            Constraint::Length(1), // selected vs free
            Constraint::Length(1), // message
        ])
        .split(inner);
    let width = inner.width as usize;

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(
                " {} file{} \u{b7} {} \u{b7} sorted by {}",
                picker.files.len(),
                if picker.files.len() == 1 { "" } else { "s" },
                format_size(picker.total_bytes()),
                picker.sort.label()
            ),
            Style::default().fg(theme.muted),
        ))),
        rows[0],
    );

    f.render_widget(Paragraph::new(filter_line(picker, theme)), rows[1]);
    render_files(f, rows[2], picker, theme);
    f.render_widget(Paragraph::new(folder_line(picker, width, theme)), rows[3]);
    f.render_widget(Paragraph::new(output_line(picker, width, theme)), rows[4]);
    f.render_widget(Paragraph::new(summary_line(picker, theme)), rows[5]);
    if let Some(msg) = &picker.message {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {}", truncate(msg, width.saturating_sub(1).max(3))),
                Style::default().fg(theme.warning),
            ))),
            rows[6],
        );
    }
}

fn filter_line(picker: &Picker, theme: Theme) -> Line<'static> {
    let query = picker.filter.as_str();
    let focused = picker.focus == Focus::Filter;
    if !focused && query.is_empty() {
        return Line::from(Span::styled(
            " / to filter",
            Style::default().fg(theme.muted),
        ));
    }
    let mut spans = vec![
        Span::styled(" / ", Style::default().fg(theme.primary)),
        Span::styled(query.to_string(), Style::default().fg(theme.text)),
    ];
    if focused {
        spans.push(Span::styled(CURSOR, Style::default().fg(theme.text)));
    }
    if !query.is_empty() {
        spans.push(Span::styled(
            format!(
                "  {} of {} shown{}",
                picker.visible().len(),
                picker.files.len(),
                // The bulk keys act on these, so say when they are a loose
                // match rather than files containing the text.
                if picker.fuzzy { " (fuzzy)" } else { "" }
            ),
            Style::default().fg(theme.muted),
        ));
    }
    Line::from(spans)
}

fn render_files(f: &mut Frame, area: Rect, picker: &mut Picker, theme: Theme) {
    picker.page_size = area.height.max(1) as usize;
    if picker.visible().is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  No file matches the filter",
                Style::default().fg(theme.muted),
            ))),
            area,
        );
        return;
    }
    // Highlight symbol (2) + checkbox (3) + size (10) + two column gaps.
    let path_width = (area.width as usize).saturating_sub(17).max(3);
    let rows: Vec<Row> = picker
        .visible()
        .iter()
        .map(|&pos| {
            let file = &picker.files[pos];
            let on = picker.is_selected(pos);
            let (check, check_style) = if on {
                ("[\u{2713}]", Style::default().fg(theme.success))
            } else {
                ("[ ]", Style::default().fg(theme.muted))
            };
            let ink = if on { theme.text } else { theme.text_dim };
            Row::new(vec![
                Cell::from(check).style(check_style),
                Cell::from(truncate(&file.path, path_width)).style(Style::default().fg(ink)),
                Cell::from(Line::from(format_size(file.size)).right_aligned())
                    .style(Style::default().fg(theme.muted)),
            ])
        })
        .collect();
    // The cursor row stands out only while the list has the keys, so it is
    // clear which part of the picker typing goes to.
    let highlight = if picker.focus == Focus::List {
        Style::default()
            .bg(theme.selection_bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(theme.bg_element)
    };
    let table = Table::new(
        rows,
        [
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(10),
        ],
    )
    .row_highlight_style(highlight)
    .highlight_symbol("\u{25b6} ");
    f.render_stateful_widget(table, area, &mut picker.table_state);
}

/// The last `width` columns of `s`, with a leading `…` when cut: the end of
/// a path is the part being typed, so that is the part to keep in view.
fn tail(s: &str, width: usize) -> String {
    let mut used = 0;
    let mut start = s.len();
    for (i, c) in s.char_indices().rev() {
        let w = c.width().unwrap_or(0);
        if used + w > width.saturating_sub(1) {
            return format!("\u{2026}{}", &s[start..]);
        }
        used += w;
        start = i;
    }
    s.to_string()
}

fn folder_line(picker: &Picker, width: usize, theme: Theme) -> Line<'static> {
    let focused = picker.focus == Focus::Folder;
    let label_style = if focused {
        Style::default()
            .fg(theme.primary)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted)
    };
    let room = width.saturating_sub(12).max(4);
    let mut spans = vec![
        Span::styled(" Save to: ", label_style),
        Span::styled(
            tail(picker.folder.as_str(), room),
            Style::default().fg(theme.text),
        ),
    ];
    if focused {
        spans.push(Span::styled(CURSOR, Style::default().fg(theme.text)));
    }
    Line::from(spans)
}

fn output_line(picker: &Picker, width: usize, theme: Theme) -> Line<'static> {
    let path = match picker.output_folder() {
        Ok(path) => path,
        Err(why) => {
            return Line::from(Span::styled(
                format!("   {why}"),
                Style::default().fg(theme.warning),
            ))
        }
    };
    let (note, note_style) = match picker.current_probe() {
        Some(p) if p.not_a_dir => (" (a file, not a folder)", Style::default().fg(theme.error)),
        Some(p) if !p.exists => (" (new folder)", Style::default().fg(theme.info)),
        _ => ("", Style::default()),
    };
    let room = width.saturating_sub(5 + note.len()).max(4);
    Line::from(vec![
        Span::styled("   \u{2192} ", Style::default().fg(theme.muted)),
        Span::styled(
            tail(&path.to_string_lossy(), room),
            Style::default().fg(theme.text_dim),
        ),
        Span::styled(note, note_style),
    ])
}

fn summary_line(picker: &Picker, theme: Theme) -> Line<'static> {
    let needed = picker.selected_bytes();
    let mut spans = vec![Span::styled(
        format!(
            " Selected {}/{} files \u{b7} {} of {}",
            picker.selected_count(),
            picker.files.len(),
            format_size(needed),
            format_size(picker.total_bytes())
        ),
        Style::default().fg(theme.text),
    )];
    if let Some(free) = picker.current_probe().and_then(|p| p.free) {
        // A warning, not a block: data already on disk counts toward the
        // selection, and the user may be about to free space anyway.
        if needed > free {
            spans.push(Span::styled(
                format!(" \u{b7} \u{26a0} only {} free", format_size(free)),
                Style::default().fg(theme.error),
            ));
        } else {
            spans.push(Span::styled(
                format!(" \u{b7} {} free", format_size(free)),
                Style::default().fg(theme.muted),
            ));
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::add_picker::FolderProbe;
    use crate::types::{PreparedTorrent, PreviewFile, TorrentPreview};
    use ratatui::{backend::TestBackend, Terminal};
    use std::path::PathBuf;

    fn screen(app: &mut App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| render_add_picker(f, f.area(), app))
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn ready_app() -> App {
        let files = ["Season 1/e01.mkv", "Season 1/e02.mkv", "notes.txt"]
            .iter()
            .enumerate()
            .map(|(i, path)| PreviewFile {
                index: i,
                components: path.split('/').map(str::to_string).collect(),
                path: path.to_string(),
                size: 1024 * 1024 * (i as u64 + 1),
            })
            .collect();
        let preview = TorrentPreview {
            name: "The Show".to_string(),
            files,
            preselected: Some(vec![0, 1]),
            subfolder: Some(PathBuf::from("The Show")),
            prepared: PreparedTorrent {
                bytes: Vec::new(),
                initial_peers: Vec::new(),
                stripped_udp: 0,
            },
        };
        let mut app = App::new();
        app.add_picker = Some(AddPickerState::Ready(Box::new(Picker::new(preview, "/dl"))));
        app
    }

    #[test]
    fn shows_files_checkboxes_folder_and_totals() {
        let mut app = ready_app();
        if let Some(AddPickerState::Ready(p)) = app.add_picker.as_mut() {
            p.apply_probe(FolderProbe {
                folder: PathBuf::from("/dl"),
                free: Some(1024 * 1024 * 1024),
                exists: true,
                not_a_dir: false,
            });
        }
        let text = screen(&mut app, 100, 30);
        assert!(text.contains("Add \u{b7} The Show"), "{text}");
        assert!(text.contains("3 files"), "{text}");
        let row = |name: &str| text.lines().find(|l| l.contains(name)).unwrap_or_default();
        assert!(row("Season 1/e01.mkv").contains("[\u{2713}]"), "{text}");
        assert!(row("notes.txt").contains("[ ]"), "{text}");
        assert!(text.contains("Save to: /dl"), "{text}");
        assert!(text.contains("/dl/The Show"), "{text}");
        assert!(text.contains("Selected 2/3 files"), "{text}");
        assert!(text.contains("1.00 GB free"), "{text}");
    }

    #[test]
    fn warns_when_the_selection_will_not_fit() {
        let mut app = ready_app();
        if let Some(AddPickerState::Ready(p)) = app.add_picker.as_mut() {
            p.apply_probe(FolderProbe {
                folder: PathBuf::from("/dl"),
                free: Some(1024),
                exists: false,
                not_a_dir: false,
            });
        }
        let text = screen(&mut app, 100, 30);
        assert!(text.contains("\u{26a0} only"), "{text}");
        assert!(text.contains("(new folder)"), "{text}");
    }

    #[test]
    fn the_fetching_box_names_the_torrent() {
        let mut app = App::new();
        app.add_picker = Some(AddPickerState::Resolving {
            ticket: 1,
            label: "ubuntu.iso".to_string(),
            since: Instant::now(),
        });
        let text = screen(&mut app, 100, 30);
        assert!(text.contains("Fetching the file list"), "{text}");
        assert!(text.contains("ubuntu.iso"), "{text}");
    }

    #[test]
    fn survives_a_tiny_terminal() {
        let mut app = ready_app();
        screen(&mut app, 12, 5);
        let mut app = App::new();
        app.add_picker = Some(AddPickerState::Resolving {
            ticket: 1,
            label: "x".to_string(),
            since: Instant::now(),
        });
        screen(&mut app, 12, 5);
    }

    #[test]
    fn tail_keeps_the_end_of_a_long_path() {
        assert_eq!(tail("/short", 20), "/short");
        assert_eq!(tail("/a/very/long/path", 8), "\u{2026}ng/path");
    }
}
