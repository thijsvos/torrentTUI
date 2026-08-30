//! Renderer for the indexer-search results view (main area). Four states:
//! loading spinner, all-providers-failed, zero results, and the results
//! table. Follows `table.rs`'s stateful-table pattern so navigation and
//! scrolling behave identically to the torrent table.

use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame,
};

use crate::app::App;
use crate::ui::layout::format_size;
use crate::ui::progress::SPINNER_FRAMES;

pub fn render_search_view(f: &mut Frame, area: Rect, app: &mut App) {
    let title = block_title(app);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    if app.search.in_flight {
        let spinner = SPINNER_FRAMES[app.spinner_tick];
        let msg = format!("{} Searching for \"{}\"...", spinner, app.search.query);
        let widget = Paragraph::new(Line::from(vec![Span::styled(
            msg,
            Style::default().fg(Color::Magenta),
        )]))
        .block(block)
        .centered();
        f.render_widget(widget, area);
        return;
    }

    if app.search.results.is_empty() {
        // Both "found nothing" and "every provider failed" end here; the
        // failure case gets its notes and a retry hint in red. There is no
        // never-searched state to draw: this view is only entered once a
        // search has fired (run_app's render dispatch falls back to the
        // torrent table before then).
        let (lines, style) = if app.search.provider_errors.is_empty() {
            let msg = format!("No results for \"{}\".", app.search.query);
            (
                vec![Line::from(msg), Line::from(""), Line::from(EMPTY_HINTS)],
                Style::default().fg(Color::DarkGray),
            )
        } else {
            let mut lines: Vec<Line> = app
                .search
                .provider_errors
                .iter()
                .map(|e| Line::from(e.clone()))
                .collect();
            lines.push(Line::from(""));
            lines.push(Line::from(EMPTY_HINTS));
            (lines, Style::default().fg(Color::Red))
        };
        let widget = Paragraph::new(lines).style(style).block(block).centered();
        f.render_widget(widget, area);
        return;
    }

    // Sort indicator on the active column, same language as the torrent
    // table: ▼ descending, ▲ ascending.
    let sort_index = app.search.sort_column.column_index();
    let arrow = if app.result_sort_descending() {
        "\u{25bc}"
    } else {
        "\u{25b2}"
    };
    let header = Row::new(
        ["", "Title", "Size", "Seed", "Leech", "Source"]
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let label = if i == sort_index {
                    format!("{} {}", h, arrow)
                } else {
                    h.to_string()
                };
                Cell::from(label).style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            }),
    )
    .height(1);

    let rows: Vec<Row> = app
        .search
        .results
        .iter()
        .map(|r| {
            let added = app.search.added.contains(&r.info_hash);
            let mark = if added { "\u{2713}" } else { "" };
            let size = match r.size_bytes {
                Some(bytes) => format_size(bytes),
                None => "?".to_string(),
            };
            Row::new(vec![
                Cell::from(mark).style(Style::default().fg(Color::Green)),
                Cell::from(r.title.clone()),
                Cell::from(size),
                Cell::from(r.seeders.to_string()).style(Style::default().fg(Color::Green)),
                Cell::from(r.leechers.to_string()).style(Style::default().fg(Color::Red)),
                Cell::from(r.source.label()).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),  // added mark
            Constraint::Min(20),    // Title
            Constraint::Length(10), // Size
            Constraint::Length(7),  // Seeders (room for the sort arrow)
            Constraint::Length(8),  // Leechers (room for the sort arrow)
            Constraint::Length(7),  // Source
        ],
    )
    .header(header)
    .block(block)
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("\u{25b6} ");

    f.render_stateful_widget(table, area, &mut app.search.table_state);
}

const EMPTY_HINTS: &str = "r: retry   s: edit query   Esc: back";

/// The block title carries the query, the count, and any partial-failure note
/// so a degraded search is visibly degraded without blocking anything.
fn block_title(app: &App) -> String {
    if app.search.in_flight || !app.search.searched_once {
        return " Search ".to_string();
    }
    let note = if app.search.provider_errors.is_empty() || app.search.results.is_empty() {
        String::new()
    } else {
        format!(" ({})", app.search.provider_errors.join(", "))
    };
    format!(
        " Search: \"{}\" \u{2014} {} results{} ",
        app.search.query,
        app.search.results.len(),
        note
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{SearchResult, SourceSet};

    fn app_with_results(results: Vec<SearchResult>) -> App {
        let mut app = App::new();
        app.search.query = "q".to_string();
        app.search.searched_once = true;
        app.search.results = results;
        app
    }

    fn result() -> SearchResult {
        SearchResult {
            title: "t".to_string(),
            info_hash: "88066b90278f2de655ee2dd44e784c340b54e45c".to_string(),
            size_bytes: None,
            seeders: 0,
            leechers: 0,
            source: SourceSet::default(),
        }
    }

    #[test]
    fn header_marks_the_sorted_column_with_a_direction_arrow() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = app_with_results(vec![result()]);
        let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();

        let screen = |t: &Terminal<TestBackend>| -> String {
            t.backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect()
        };

        terminal
            .draw(|f| render_search_view(f, f.area(), &mut app))
            .unwrap();
        assert!(screen(&terminal).contains("Seed \u{25bc}"));

        app.cycle_result_sort(); // -> Size, natural descending
        terminal
            .draw(|f| render_search_view(f, f.area(), &mut app))
            .unwrap();
        let s = screen(&terminal);
        assert!(s.contains("Size \u{25bc}"), "{s}");
        assert!(!s.contains("Seed \u{25bc}"));

        app.reverse_result_sort();
        terminal
            .draw(|f| render_search_view(f, f.area(), &mut app))
            .unwrap();
        assert!(screen(&terminal).contains("Size \u{25b2}"));
    }

    #[test]
    fn title_carries_count_and_partial_failure_note() {
        let mut app = app_with_results(vec![result()]);
        assert_eq!(block_title(&app), " Search: \"q\" \u{2014} 1 results ");
        app.search.provider_errors = vec!["apibay: timed out".to_string()];
        assert_eq!(
            block_title(&app),
            " Search: \"q\" \u{2014} 1 results (apibay: timed out) "
        );
    }

    #[test]
    fn title_is_plain_while_loading_or_before_first_search() {
        let mut app = App::new();
        assert_eq!(block_title(&app), " Search ");
        app.search.in_flight = true;
        assert_eq!(block_title(&app), " Search ");
    }

    #[test]
    fn empty_results_with_errors_keeps_note_out_of_title() {
        // The error body already shows the notes; doubling them in the title
        // would be noise.
        let mut app = app_with_results(Vec::new());
        app.search.provider_errors = vec!["apibay: HTTP 403".to_string()];
        assert_eq!(block_title(&app), " Search: \"q\" \u{2014} 0 results ");
    }
}
