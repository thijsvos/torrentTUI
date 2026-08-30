//! The action registry — the single source of truth for every user-invokable
//! action, its keybinding, and its documentation.
//!
//! Four consumers read it: the command palette searches it, the help overlay
//! renders from it, the status-bar hint lines are assembled from it, and a
//! test in this module regenerates the README keybinding tables from it and
//! fails when they drift. Before this registry existed those four surfaces
//! were hand-synced ("nothing here fails if they drift" — and three real
//! drift bugs proved it); now a new action is one row here, and the docs
//! cannot rot.
//!
//! Rows without an [`ActionId`] are documentation-only (pure navigation like
//! `j`/`k`, or chords handled before dispatch like `Ctrl+C`): they appear in
//! help and the README but not in the palette.

use crate::app::App;
use crate::types::{AppMode, DetailTab};

/// Every action the palette can execute. Dispatch lives in `main.rs`'s
/// `execute_action`, which routes each id to the same function the key
/// handler for that binding calls — the palette is an alternate front door,
/// never a second implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionId {
    AddTorrent,
    OpenSearch,
    PauseToggle,
    PauseAll,
    Delete,
    RevealFolder,
    OpenDetail,
    CycleSort,
    ReverseSort,
    OpenFilter,
    OpenThrottle,
    ToggleMark,
    MarkAll,
    ClearMarks,
    ToggleHelp,
    Quit,
    // Detail view
    CycleTab,
    ToggleFileSelection,
    ApplyFileSelection,
    StreamFile,
    DetailBack,
    // Search results
    DownloadResult,
    EditQuery,
    RetrySearch,
    CycleResultSort,
    ReverseResultSort,
    SearchBack,
}

/// Where an action lives. `Global` actions are advertised in the main
/// keybinding table and offered by the palette from every mode it opens in;
/// the others only in their own view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Normal,
    Detail,
    SearchResults,
}

/// Which help-overlay / README section a row renders into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Main,
    Search,
    Detail,
}

pub struct ActionInfo {
    /// `None` = documentation-only row (navigation, pre-dispatch chords).
    pub id: Option<ActionId>,
    pub scope: Scope,
    pub section: Section,
    /// Key cell exactly as the README renders it (backticks included). The
    /// help overlay and palette strip the backticks for display.
    pub keys: &'static str,
    /// Canonical description, used verbatim in the README tables.
    pub description: &'static str,
    /// Shorter wording for the help overlay and palette when the canonical
    /// description is too long for a popup column.
    pub short: Option<&'static str>,
    /// Status-bar hint chip ("a:add"); `None` = not in the hint line.
    pub hint: Option<&'static str>,
    /// Whether the hint also shows in Normal mode's empty-list hint line.
    pub hint_when_empty: bool,
    /// Whether the palette offers it (documentation-only rows never do).
    pub in_palette: bool,
    /// Availability beyond scope, evaluated against live state. Unavailable
    /// actions are hidden from the palette; docs always show the row.
    pub available: fn(&App) -> bool,
}

fn always(_: &App) -> bool {
    true
}
fn has_visible_torrents(app: &App) -> bool {
    !app.sorted_torrents().is_empty()
}
fn has_visible_or_marks(app: &App) -> bool {
    !app.sorted_torrents().is_empty() || app.has_marks()
}
fn has_torrents(app: &App) -> bool {
    !app.torrents.is_empty()
}
fn has_selection(app: &App) -> bool {
    app.selected_torrent().is_some()
}
fn has_marks(app: &App) -> bool {
    app.has_marks()
}
fn on_files_tab_with_files(app: &App) -> bool {
    app.detail_tab == DetailTab::Files
        && app.selected_torrent().is_some_and(|t| !t.files.is_empty())
}
fn has_search_result(app: &App) -> bool {
    app.selected_search_result().is_some() && !app.search.in_flight
}
fn can_retry_search(app: &App) -> bool {
    !app.search.in_flight && !app.search.query.is_empty()
}

/// The registry. Row order is meaningful: it is the README table order, the
/// help overlay order, and the order hint chips appear in.
pub const ACTIONS: &[ActionInfo] = &[
    // ---- Main table: global + torrent list ----
    ActionInfo {
        id: None, // opens the palette itself; handled before dispatch
        scope: Scope::Global,
        section: Section::Main,
        keys: "`:` / `Ctrl+P`",
        description: "Open the command palette (fuzzy-search every action)",
        short: Some("Open the command palette"),
        hint: Some(":palette"),
        hint_when_empty: true,
        in_palette: false,
        available: always,
    },
    ActionInfo {
        id: Some(ActionId::AddTorrent),
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`a`",
        description: "Add magnet link or .torrent file",
        short: None,
        hint: Some("a:add"),
        hint_when_empty: true,
        in_palette: true,
        available: always,
    },
    ActionInfo {
        id: Some(ActionId::OpenSearch),
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`s`",
        description: "Search torrent indexers",
        short: None,
        hint: Some("s:search"),
        hint_when_empty: true,
        in_palette: true,
        available: always,
    },
    ActionInfo {
        id: Some(ActionId::PauseToggle),
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`p`",
        description: "Pause/unpause selected (or all marked) torrents",
        short: None,
        hint: Some("p:(un)pause"),
        hint_when_empty: false,
        in_palette: true,
        available: has_visible_or_marks,
    },
    ActionInfo {
        id: Some(ActionId::PauseAll),
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`P`",
        description: "Pause/unpause all torrents",
        short: None,
        hint: None,
        hint_when_empty: false,
        in_palette: true,
        available: has_torrents,
    },
    ActionInfo {
        id: Some(ActionId::Delete),
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`d`",
        description: "Delete selected (or all marked) torrents",
        short: None,
        hint: Some("d:delete"),
        hint_when_empty: false,
        in_palette: true,
        available: has_visible_or_marks,
    },
    ActionInfo {
        id: Some(ActionId::RevealFolder),
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`o`",
        description: "Reveal the selected torrent in your file manager (Finder/Explorer/xdg-open); falls back to the download folder while data is still arriving",
        short: Some("Reveal torrent in file manager"),
        hint: Some("o:folder"),
        hint_when_empty: false,
        in_palette: true,
        available: has_selection,
    },
    ActionInfo {
        id: Some(ActionId::OpenDetail),
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`Enter`",
        description: "Open detail view",
        short: None,
        hint: Some("Enter:detail"),
        hint_when_empty: false,
        in_palette: true,
        available: has_visible_torrents,
    },
    ActionInfo {
        id: None,
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`j` / `k` (or `↓` / `↑`)",
        description: "Move selection down/up",
        short: None,
        hint: None,
        hint_when_empty: false,
        in_palette: false,
        available: always,
    },
    ActionInfo {
        id: Some(ActionId::CycleSort),
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`Tab`",
        description: "Cycle sort column",
        short: None,
        hint: None,
        hint_when_empty: false,
        in_palette: true,
        available: always,
    },
    ActionInfo {
        id: Some(ActionId::ReverseSort),
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`r`",
        description: "Reverse sort order",
        short: None,
        hint: None,
        hint_when_empty: false,
        in_palette: true,
        available: always,
    },
    ActionInfo {
        id: Some(ActionId::OpenFilter),
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`/`",
        description: "Filter torrent list",
        short: None,
        hint: Some("/:filter"),
        hint_when_empty: true,
        in_palette: true,
        available: always,
    },
    ActionInfo {
        id: Some(ActionId::OpenThrottle),
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`t`",
        description: "Set speed limits",
        short: None,
        hint: Some("t:throttle"),
        hint_when_empty: false,
        in_palette: true,
        available: always,
    },
    ActionInfo {
        id: Some(ActionId::ToggleMark),
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`Space`",
        description: "Mark/unmark current torrent (then advances selection)",
        short: Some("Mark/unmark current torrent"),
        hint: Some("Space:mark"),
        hint_when_empty: false,
        in_palette: true,
        available: has_visible_torrents,
    },
    ActionInfo {
        id: Some(ActionId::MarkAll),
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`v`",
        description: "Mark all visible torrents",
        short: None,
        hint: None,
        hint_when_empty: false,
        in_palette: true,
        available: has_visible_torrents,
    },
    ActionInfo {
        id: Some(ActionId::ClearMarks),
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`V`",
        description: "Clear all marks",
        short: None,
        hint: None,
        hint_when_empty: false,
        in_palette: true,
        available: has_marks,
    },
    ActionInfo {
        id: None,
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`Esc`",
        description: "Clear marks (or close current dialog)",
        short: None,
        hint: None,
        hint_when_empty: false,
        in_palette: false,
        available: always,
    },
    ActionInfo {
        id: Some(ActionId::ToggleHelp),
        // Normal, not Global: offering Help/Quit from the Detail palette
        // would leave the engine's SetDetailTorrent materialization active
        // after the mode jump — the same leak Ctrl+C explicitly clears.
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`?`",
        description: "Toggle help",
        short: Some("Toggle help overlay"),
        hint: Some("?:help"),
        hint_when_empty: true,
        in_palette: true,
        available: always,
    },
    ActionInfo {
        id: Some(ActionId::Quit),
        scope: Scope::Normal,
        section: Section::Main,
        keys: "`q`",
        description: "Quit",
        short: None,
        hint: Some("q:quit"),
        hint_when_empty: true,
        in_palette: true,
        available: always,
    },
    ActionInfo {
        id: None, // intercepted before mode dispatch
        scope: Scope::Global,
        section: Section::Main,
        keys: "`Ctrl+C`",
        description: "Quit (double press to force)",
        short: None,
        hint: None,
        hint_when_empty: false,
        in_palette: false,
        available: always,
    },
    // ---- Search results ----
    ActionInfo {
        id: Some(ActionId::DownloadResult),
        scope: Scope::SearchResults,
        section: Section::Search,
        keys: "`Enter`",
        description: "Download the selected result (stays in results for multi-grab)",
        short: Some("Download the selected result"),
        hint: Some("Enter:download"),
        hint_when_empty: false,
        in_palette: true,
        available: has_search_result,
    },
    ActionInfo {
        id: None,
        scope: Scope::SearchResults,
        section: Section::Search,
        keys: "`j` / `k` (or `↓` / `↑`)",
        description: "Move selection down/up",
        short: None,
        hint: Some("j/k:navigate"),
        hint_when_empty: false,
        in_palette: false,
        available: always,
    },
    ActionInfo {
        id: Some(ActionId::CycleResultSort),
        scope: Scope::SearchResults,
        section: Section::Search,
        keys: "`Tab`",
        description: "Cycle sort column (Seeders → Size → Title → Leechers)",
        short: Some("Cycle result sort column"),
        hint: Some("Tab:sort"),
        hint_when_empty: false,
        in_palette: true,
        available: has_search_result,
    },
    ActionInfo {
        id: Some(ActionId::ReverseResultSort),
        scope: Scope::SearchResults,
        section: Section::Search,
        keys: "`R`",
        description: "Reverse sort order",
        short: Some("Reverse result sort order"),
        hint: None,
        hint_when_empty: false,
        in_palette: true,
        available: has_search_result,
    },
    ActionInfo {
        id: Some(ActionId::EditQuery),
        scope: Scope::SearchResults,
        section: Section::Search,
        keys: "`s`",
        description: "Edit the query (pre-filled)",
        short: None,
        hint: Some("s:edit query"),
        hint_when_empty: false,
        in_palette: true,
        available: always,
    },
    ActionInfo {
        id: Some(ActionId::RetrySearch),
        scope: Scope::SearchResults,
        section: Section::Search,
        keys: "`r`",
        description: "Retry the same query",
        short: None,
        hint: Some("r:retry"),
        hint_when_empty: false,
        in_palette: true,
        available: can_retry_search,
    },
    ActionInfo {
        id: Some(ActionId::SearchBack),
        scope: Scope::SearchResults,
        section: Section::Search,
        keys: "`Esc` / `q`",
        description: "Back to the torrent list (results are kept)",
        short: Some("Back to the torrent list"),
        hint: Some("Esc:back"),
        hint_when_empty: false,
        in_palette: true,
        available: always,
    },
    // ---- Detail view ----
    ActionInfo {
        id: Some(ActionId::CycleTab),
        scope: Scope::Detail,
        section: Section::Detail,
        keys: "`Tab`",
        description: "Cycle tabs (Stats → Info → Files → Peers)",
        short: Some("Cycle detail tabs"),
        hint: Some("Tab:switch tab"),
        hint_when_empty: false,
        in_palette: true,
        available: always,
    },
    ActionInfo {
        id: None,
        scope: Scope::Detail,
        section: Section::Detail,
        keys: "`j` / `k`",
        description: "Navigate files (Files tab) or peers (Peers tab)",
        short: None,
        hint: Some("j/k:navigate"),
        hint_when_empty: false,
        in_palette: false,
        available: always,
    },
    ActionInfo {
        id: Some(ActionId::ToggleFileSelection),
        scope: Scope::Detail,
        section: Section::Detail,
        keys: "`Space`",
        description: "Toggle file selection (Files tab)",
        short: None,
        hint: Some("Space:toggle"),
        hint_when_empty: false,
        in_palette: true,
        available: on_files_tab_with_files,
    },
    ActionInfo {
        id: Some(ActionId::ApplyFileSelection),
        scope: Scope::Detail,
        section: Section::Detail,
        keys: "`S`",
        description: "Apply current file selection to engine (Files tab)",
        short: Some("Apply file selection (Files tab)"),
        hint: Some("S:apply"),
        hint_when_empty: false,
        in_palette: true,
        available: on_files_tab_with_files,
    },
    ActionInfo {
        id: Some(ActionId::StreamFile),
        scope: Scope::Detail,
        section: Section::Detail,
        keys: "`s`",
        description: "Stream selected file in default media player (Files tab)",
        short: Some("Stream selected file (Files tab)"),
        hint: None,
        hint_when_empty: false,
        in_palette: true,
        available: on_files_tab_with_files,
    },
    ActionInfo {
        id: Some(ActionId::RevealFolder),
        scope: Scope::Detail,
        section: Section::Detail,
        keys: "`o`",
        description: "Reveal this torrent in your file manager",
        short: None,
        hint: Some("o:folder"),
        hint_when_empty: false,
        in_palette: true,
        available: has_selection,
    },
    ActionInfo {
        id: Some(ActionId::DetailBack),
        scope: Scope::Detail,
        section: Section::Detail,
        keys: "`Esc` / `q`",
        description: "Back to list",
        short: Some("Back to the torrent list"),
        hint: Some("Esc:back"),
        hint_when_empty: false,
        in_palette: true,
        available: always,
    },
];

/// Keys as the terminal shows them: the README cell without backticks.
pub fn keys_display(info: &ActionInfo) -> String {
    info.keys.replace('`', "")
}

/// The description used inside the TUI (help overlay, palette rows).
pub fn tui_description(info: &ActionInfo) -> &'static str {
    info.short.unwrap_or(info.description)
}

/// Assemble a status-bar hint line for a scope, in registry order. `Normal`
/// hints have an empty-list variant showing only the always-relevant chips.
pub fn hint_line(scope: Scope, empty_list_variant: bool) -> String {
    ACTIONS
        .iter()
        .filter(|a| a.scope == scope || (scope == Scope::Normal && a.scope == Scope::Global))
        .filter(|a| !empty_list_variant || a.hint_when_empty)
        .filter_map(|a| a.hint)
        .collect::<Vec<_>>()
        .join("  ")
}

/// Which palette scope a mode maps to; `None` = the palette does not open
/// from this mode (text-input modes need their keys for typing).
pub fn palette_scope(mode: &AppMode) -> Option<Scope> {
    match mode {
        AppMode::Normal => Some(Scope::Normal),
        AppMode::Detail => Some(Scope::Detail),
        AppMode::SearchResults => Some(Scope::SearchResults),
        _ => None,
    }
}

/// Case-insensitive fuzzy subsequence match. Returns a score (higher =
/// better) when every query char appears in order in `name`; `None`
/// otherwise. Word-start and consecutive matches score extra so "pa" ranks
/// "Pause…" above "…palette". An empty query matches everything at score 0.
pub fn fuzzy_score(query: &str, name: &str) -> Option<u32> {
    let query: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();
    if query.is_empty() {
        return Some(0);
    }
    let name: Vec<char> = name.chars().flat_map(|c| c.to_lowercase()).collect();
    let mut score = 0u32;
    let mut qi = 0usize;
    let mut prev_matched = false;
    for (ni, &nc) in name.iter().enumerate() {
        if qi < query.len() && nc == query[qi] {
            score += 1;
            if prev_matched {
                score += 2; // consecutive run
            }
            let word_start = ni == 0 || !name[ni - 1].is_alphanumeric();
            if word_start {
                score += 3;
            }
            qi += 1;
            prev_matched = true;
        } else {
            prev_matched = false;
        }
    }
    if qi == query.len() {
        Some(score)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_rows_are_well_formed() {
        for a in ACTIONS {
            assert!(!a.keys.is_empty(), "empty keys cell");
            assert!(!a.description.is_empty(), "empty description");
            // Palette rows must be executable.
            if a.in_palette {
                assert!(
                    a.id.is_some(),
                    "palette row without an id: {}",
                    a.description
                );
            }
            // Doc-only rows must not claim palette membership.
            if a.id.is_none() {
                assert!(!a.in_palette, "id-less row in palette: {}", a.description);
            }
        }
    }

    #[test]
    fn descriptions_are_unique_within_a_scope() {
        // The palette shows tui_description; two identical labels in one
        // scope would be indistinguishable.
        for a in ACTIONS.iter().filter(|a| a.in_palette) {
            let dupes = ACTIONS
                .iter()
                .filter(|b| {
                    b.in_palette && b.scope == a.scope && tui_description(b) == tui_description(a)
                })
                .count();
            assert_eq!(dupes, 1, "duplicate palette label: {}", tui_description(a));
        }
    }

    #[test]
    fn hint_lines_render_in_registry_order() {
        let normal = hint_line(Scope::Normal, false);
        assert!(normal.starts_with(":palette  a:add  s:search"));
        assert!(normal.ends_with("?:help  q:quit"));
        let empty = hint_line(Scope::Normal, true);
        assert_eq!(empty, ":palette  a:add  s:search  /:filter  ?:help  q:quit");
        let detail = hint_line(Scope::Detail, false);
        assert!(detail.starts_with("Tab:switch tab  j/k:navigate"));
        assert!(detail.ends_with("o:folder  Esc:back"));
        let search = hint_line(Scope::SearchResults, false);
        assert_eq!(
            search,
            "Enter:download  j/k:navigate  Tab:sort  s:edit query  r:retry  Esc:back"
        );
    }

    #[test]
    fn palette_scope_covers_exactly_the_non_input_modes() {
        assert_eq!(palette_scope(&AppMode::Normal), Some(Scope::Normal));
        assert_eq!(palette_scope(&AppMode::Detail), Some(Scope::Detail));
        assert_eq!(
            palette_scope(&AppMode::SearchResults),
            Some(Scope::SearchResults)
        );
        for mode in [
            AppMode::Input,
            AppMode::Search,
            AppMode::Filter,
            AppMode::ThrottleInput,
            AppMode::Help,
            AppMode::ConfirmDelete,
            AppMode::ConfirmQuit,
            AppMode::Palette,
        ] {
            assert_eq!(palette_scope(&mode), None, "{mode:?}");
        }
    }

    #[test]
    fn fuzzy_scoring_prefers_word_starts_and_runs() {
        assert!(fuzzy_score("", "anything").is_some());
        assert!(fuzzy_score("xyz", "Pause all torrents").is_none());
        // Subsequence, case-insensitive.
        assert!(fuzzy_score("PAT", "Pause all torrents").is_some());
        // "pa" should rank "Pause…" (word start + run) above "…palette"
        // reached mid-word… both start words, so compare run quality:
        let pause = fuzzy_score("pau", "Pause all torrents").unwrap();
        let palette = fuzzy_score("pau", "Open the command palette");
        assert!(palette.is_none() || pause > palette.unwrap());
        // Word-start bonus: "at" matching "all torrents" starts beats a
        // mid-word subsequence of the same length.
        let starts = fuzzy_score("at", "all torrents").unwrap();
        let midword = fuzzy_score("at", "beaten").unwrap();
        assert!(starts > midword);
    }

    /// The data rows of the first markdown key table after `heading`:
    /// everything from the `| Key |` header (exclusive, plus its separator)
    /// to the first non-table line.
    fn readme_table_rows(readme: &str, heading: &str) -> Vec<String> {
        let start = readme
            .find(heading)
            .unwrap_or_else(|| panic!("README heading not found: {heading}"));
        readme[start..]
            .lines()
            .skip(1)
            .skip_while(|l| !l.starts_with("| Key"))
            .skip(2) // column header + separator row
            .take_while(|l| l.starts_with('|'))
            .map(|l| l.trim().to_string())
            .collect()
    }

    /// Regenerate each README keybinding table from the registry and require
    /// exact, ordered equality — per section, so a row duplicated across
    /// tables can't mask its loss from one of them, and stale extra rows
    /// fail too. If this fails: edit the registry first, then make the
    /// README table match it — never the other way around.
    #[test]
    fn readme_keybinding_tables_match_the_registry() {
        let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
            .expect("README.md readable");
        for (section, heading) in [
            (Section::Main, "## Keybindings"),
            (Section::Search, "### Search"),
            (Section::Detail, "### Detail view"),
        ] {
            let expected: Vec<String> = ACTIONS
                .iter()
                .filter(|a| a.section == section)
                .map(|a| format!("| {} | {} |", a.keys, a.description))
                .collect();
            let actual = readme_table_rows(&readme, heading);
            assert_eq!(
                actual, expected,
                "README table under {heading:?} does not match the registry's \
                 {section:?} rows (order included). Fix the registry first, \
                 then mirror it in README.md."
            );
        }
    }
}
