//! UI-side application state and the operations key handlers perform on it.
//!
//! `App` deliberately holds no engine handle: everything it knows about
//! torrents arrives as a snapshot over the state channel, and everything it
//! wants done leaves as an `EngineCommand`. That makes it trivially
//! constructible in tests and keeps any render path from blocking on the
//! session.
//!
//! The recurring pattern here is the sort cache. `sorted_torrents()` is called
//! several times per frame, and lowercasing names for the filter plus sorting
//! dominated CPU at hundreds of torrents. Anything that changes the filter, the
//! sort, or the torrent list must therefore invalidate the cache, rebuild it,
//! and then restore the selection — which is why `change_sort_column`,
//! `push_filter_char` and friends exist as wrappers instead of callers poking
//! the fields directly.
//!
//! `torrents` is replaced wholesale on every push, so state keyed by torrent id
//! (`marked_ids`, `deselected_files`) can outlive the torrent it refers to.
//! `prune_stale_state` is what stops that accumulating, and `handle_state_push`
//! is the one entry point that runs it.

use std::collections::{HashMap, HashSet};

use crate::actions::{fuzzy_score, palette_scope, tui_description, ActionInfo, Scope, ACTIONS};
use crate::config::{PlayerConfig, SearchConfig};
use crate::search::{SearchOutcome, SearchResult};
use crate::types::{AppMode, DetailTab, SortColumn, TorrentInfo, TorrentStatus};
use ratatui::widgets::TableState;

/// Cap on the palette's filter buffer; action names are short, so anything
/// longer than this can't match and only wastes redraw work.
const MAX_PALETTE_QUERY_CHARS: usize = 64;

/// Cap on the torrent-table filter (mirrors `search::MAX_QUERY_CHARS`).
/// Torrent names are bounded, so a longer needle can't match anything — and
/// without a cap a stray paste of the wrong clipboard buffer grows
/// `filter_text` without limit.
const MAX_FILTER_CHARS: usize = 200;

/// State for the command-palette overlay. Ephemeral by design: opening the
/// palette resets the filter, so it always starts from "show me everything".
pub struct PaletteState {
    /// Live filter text (control/bidi chars rejected on entry, like the
    /// search query).
    pub input: String,
    /// The action the cursor is on, tracked by identity rather than by index:
    /// availability predicates re-filter the match list on background events
    /// (a search outcome landing, metadata arriving), and an index would let
    /// a row shift under the cursor between deciding and pressing Enter —
    /// firing an action the user never chose. `None` = the top match.
    pub anchor: Option<&'static ActionInfo>,
    /// Mode the palette opened over — rendered underneath, returned to on
    /// close, and the selector for which actions are offered.
    pub return_mode: AppMode,
    pub table_state: TableState,
}

impl PaletteState {
    fn new() -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        Self {
            input: String::new(),
            anchor: None,
            return_mode: AppMode::Normal,
            table_state,
        }
    }
}

/// Sort column for the search-results table. Distinct from the torrent
/// table's `SortColumn`: the columns differ, and so does the model — each
/// column has a *natural* direction it lands in when first selected
/// (numeric columns biggest-first, title A→Z), and `sort_reversed` flips
/// that, rather than reversal being a direction in itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResultSortColumn {
    Seeders,
    Size,
    Title,
    Leechers,
}

impl ResultSortColumn {
    pub fn next(self) -> Self {
        match self {
            ResultSortColumn::Seeders => ResultSortColumn::Size,
            ResultSortColumn::Size => ResultSortColumn::Title,
            ResultSortColumn::Title => ResultSortColumn::Leechers,
            ResultSortColumn::Leechers => ResultSortColumn::Seeders,
        }
    }

    /// Position of this column in the rendered results table (column 0 is
    /// the ✓ added-mark). Indexes `ui/search.rs`'s header labels, so the two
    /// must stay in the same order — the same contract `SortColumn` has with
    /// `table.rs`.
    pub fn column_index(self) -> usize {
        match self {
            ResultSortColumn::Title => 1,
            ResultSortColumn::Size => 2,
            ResultSortColumn::Seeders => 3,
            ResultSortColumn::Leechers => 4,
        }
    }

    /// The direction this column lands in when first selected: you sort by
    /// seeders to find the best-seeded result, by title to read A→Z.
    pub fn natural_descending(self) -> bool {
        !matches!(self, ResultSortColumn::Title)
    }
}

/// State for the indexer-search feature, grouped because it lives and dies
/// together: it persists for the whole session (leaving the search views and
/// coming back shows the last query and results untouched) and only firing a
/// new search replaces it.
pub struct SearchState {
    /// Live edit buffer for the query bar. A plain `String` like
    /// `filter_text`, deliberately not the shared `InputWidget` — the add
    /// dialog clears that widget on entry, which would eat the remembered
    /// query, and a plain field keeps the handlers testable.
    pub input: String,
    /// The last *fired* query, for the results title and the `r` retry.
    pub query: String,
    /// Monotonic query id, bumped only when a search fires. Outcomes carrying
    /// any other value are stale and get dropped, which is the entire
    /// staleness story — in-flight tasks are never aborted, their results
    /// just die at this check.
    pub generation: u64,
    /// True from fire until the matching-generation outcome lands.
    pub in_flight: bool,
    /// Distinguishes "never searched" from "searched and found nothing".
    pub searched_once: bool,
    pub results: Vec<SearchResult>,
    /// Short per-provider failure notes from the last outcome; partial
    /// results still render, these annotate the title line.
    pub provider_errors: Vec<String>,
    pub selected: usize,
    pub table_state: TableState,
    /// Results-table sort. Sticky for the session (a retry or a new query
    /// keeps it), like the torrent table's sort.
    pub sort_column: ResultSortColumn,
    /// Flips the column's natural direction — `false` means "as
    /// [`ResultSortColumn::natural_descending`] says", not "ascending".
    pub sort_reversed: bool,
    /// Info hashes sent to `AddTorrent` this session, for the ✓ mark. Keyed
    /// by hash (not row) so the mark survives re-searching. The double-Enter
    /// guard blocks a re-send only while the torrent is actually present in
    /// the session, so a deleted torrent or a failed add stays re-addable;
    /// rapid double-presses in the window before the first state push land on
    /// the engine's `AlreadyManaged` reply instead.
    pub added: HashSet<String>,
}

impl SearchState {
    fn new() -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        Self {
            input: String::new(),
            query: String::new(),
            generation: 0,
            in_flight: false,
            searched_once: false,
            results: Vec::new(),
            provider_errors: Vec::new(),
            selected: 0,
            table_state,
            sort_column: ResultSortColumn::Seeders,
            sort_reversed: false,
            added: HashSet::new(),
        }
    }
}

/// All UI-side state. Constructed once in `run_app`; every field is either
/// derived from an engine snapshot or owned by a key handler.
pub struct App {
    pub torrents: Vec<TorrentInfo>,
    pub table_state: TableState,
    pub selected_index: usize,
    pub selected_torrent_id: Option<usize>,
    pub mode: AppMode,
    pub detail_tab: DetailTab,
    pub sort_column: SortColumn,
    pub sort_reversed: bool,
    pub error_message: Option<String>,
    pub error_timer: Option<std::time::Instant>,
    pub info_message: Option<String>,
    pub info_timer: Option<std::time::Instant>,
    pub spinner_tick: usize,
    pub should_quit: bool,
    /// Set with `should_quit` when the user confirmed a detach, so `main` knows
    /// to hand the session to a background process instead of ending it.
    pub detach_requested: bool,
    pub filter_text: String,
    /// Counts `rebuild_sort_cache` runs so tests can pin batching claims —
    /// e.g. that one paste re-sorts once, not once per character.
    #[cfg(test)]
    pub sort_rebuilds: usize,
    pub free_disk_space: Option<u64>,
    pub disk_space_timer: Option<std::time::Instant>,
    pub throttle_step: u8, // 0 = download, 1 = upload
    pub throttle_input_buf: String,
    pub throttle_download_value: u64,
    pub throttle_upload_value: u64,
    pub speed_limit_download_kbps: u64,
    pub speed_limit_upload_kbps: u64,
    /// Where the torrent table was drawn last frame, so mouse clicks can be
    /// mapped back to rows. Set by the renderer and cleared to `None` in
    /// Detail mode, which is what makes clicks inert there.
    pub table_area: Option<ratatui::layout::Rect>,
    pub detail_file_index: usize,
    /// Per torrent id, the file indices the user has *deselected*. Stored
    /// negatively so a torrent whose metadata has not arrived yet — and which
    /// therefore has no known file count — still defaults to "download
    /// everything". Pruned by `prune_stale_state`.
    pub deselected_files: HashMap<usize, HashSet<usize>>,
    pub marked_ids: HashSet<usize>,
    pub detail_peer_index: usize,
    /// Top peer-row index currently visible in the Peers tab. The renderer
    /// recomputes it each frame to keep `detail_peer_index` in view and to
    /// clamp it when the peer list shrinks; the key handlers reset it to 0
    /// whenever the Peers list is re-entered (opening Detail, switching tab).
    pub detail_peer_scroll_offset: usize,
    /// Cached order of indices into `self.torrents` after applying the
    /// current filter and sort. Rebuilt when `sort_dirty` is true. The cache
    /// exists because `sorted_torrents()` is called multiple times per frame
    /// (table, detail, key handlers) and the lowercase-for-filter + sort
    /// work dominated CPU at hundreds of torrents.
    sort_cache: Vec<usize>,
    sort_dirty: bool,
    /// Wired from `config.general.confirm_on_quit`; when false, `q` quits
    /// immediately instead of opening the confirmation dialog.
    pub confirm_on_quit: bool,
    /// Base URL of the engine's embedded HTTP API (e.g.
    /// `http://127.0.0.1:34567`).
    /// `None` until the engine reports `EngineInfo::HttpApiReady`; if the
    /// bind failed at startup, stays `None` forever and the `s` stream
    /// keybinding shows an error.
    pub http_api_base: Option<String>,
    /// The privacy posture the session actually started with — `None` until
    /// (and unless) the engine reports `EngineInfo::Privacy`, so the header
    /// badge can never show a protection that failed to apply.
    pub privacy: Option<crate::engine::torrent::PrivacyStatus>,
    /// External player command + args for the `s` stream keybinding. Loaded
    /// once from `config.player` at startup.
    pub player_config: PlayerConfig,
    /// Name last seen behind each torrent id, so `prune_stale_state` can tell
    /// "same torrent" from "recycled id". librqbit hands out `max(ids) + 1`, so
    /// a freed id can be reused by a concurrent add.
    torrent_identities: HashMap<usize, String>,
    /// Wired from `config.general.watch_dir.is_some()`. Only affects the
    /// delete dialog, which warns that deleting also removes the `.torrent`
    /// from the watch folder.
    pub watch_dir_configured: bool,
    /// Indexer-search state; see [`SearchState`].
    pub search: SearchState,
    /// Copied once from `config.search` at startup, like `player_config`.
    pub search_config: SearchConfig,
    /// Copied once from `config.privacy.proxy_url()` at startup. When set,
    /// the lazily built search client routes indexer queries through the same
    /// SOCKS5 proxy as the torrent traffic — a proxied session that still
    /// sent search queries directly would undercut the badge's promise.
    pub search_proxy_url: Option<String>,
    /// Copied once from `config.general.download_dir` at startup; the `o`
    /// keybinding resolves the selected torrent's on-disk location against it.
    pub download_dir: String,
    /// Command-palette overlay state; see [`PaletteState`].
    pub palette: PaletteState,
    /// Scroll offset of the help overlay. The registry-driven table outgrew
    /// a 24-line terminal, so the overlay scrolls; reset when help opens and
    /// clamped against the visible height by the renderer.
    pub help_scroll: u16,
}

impl App {
    /// A blank app with row 0 selected and the sort cache marked dirty.
    ///
    /// The config-derived fields (speed limits, `confirm_on_quit`,
    /// `player_config`, `watch_dir_configured`) get placeholder values here and
    /// are overwritten by `run_app` immediately afterwards — do not read the
    /// real defaults off this function.
    pub fn new() -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        Self {
            torrents: Vec::new(),
            table_state,
            selected_index: 0,
            selected_torrent_id: None,
            mode: AppMode::Normal,
            detail_tab: DetailTab::Stats,
            sort_column: SortColumn::Index,
            sort_reversed: false,
            error_message: None,
            error_timer: None,
            info_message: None,
            info_timer: None,
            spinner_tick: 0,
            should_quit: false,
            detach_requested: false,
            filter_text: String::new(),
            #[cfg(test)]
            sort_rebuilds: 0,
            free_disk_space: None,
            disk_space_timer: None,
            throttle_step: 0,
            throttle_input_buf: String::new(),
            throttle_download_value: 0,
            throttle_upload_value: 0,
            speed_limit_download_kbps: 0,
            speed_limit_upload_kbps: 0,
            table_area: None,
            detail_file_index: 0,
            deselected_files: HashMap::new(),
            marked_ids: HashSet::new(),
            detail_peer_index: 0,
            detail_peer_scroll_offset: 0,
            sort_cache: Vec::new(),
            sort_dirty: true,
            confirm_on_quit: true,
            http_api_base: None,
            privacy: None,
            player_config: PlayerConfig::default(),
            torrent_identities: HashMap::new(),
            watch_dir_configured: false,
            search: SearchState::new(),
            search_config: SearchConfig::default(),
            search_proxy_url: None,
            download_dir: String::new(),
            palette: PaletteState::new(),
            help_scroll: 0,
        }
    }

    /// Whether `q` should open the confirmation dialog instead of quitting
    /// outright. True for anything mid-work: downloading, resolving metadata,
    /// or actively seeding. Seeding counts because quitting still cuts peers
    /// abruptly, even though nothing is being downloaded.
    pub fn confirm_on_quit_required(&self) -> bool {
        self.confirm_on_quit
            && self.torrents.iter().any(|t| {
                matches!(
                    t.status,
                    TorrentStatus::Downloading
                        | TorrentStatus::FetchingMetadata
                        | TorrentStatus::Seeding
                )
            })
    }

    /// Mark the cached sort order as dirty. Call from anywhere that mutates
    /// `torrents`, `sort_column`, `sort_reversed`, or `filter_text`.
    pub fn invalidate_sort(&mut self) {
        self.sort_dirty = true;
    }

    /// Replace the torrent list and refresh derived state (sort cache,
    /// selection bookkeeping, pruned marks). One canonical entry point so
    /// callers can't forget to invalidate.
    /// Returns whether anything the UI draws actually changed. The engine
    /// pushes a snapshot every 100 ms whether or not it differs, so repainting
    /// unconditionally meant a session of idle torrents redrew the terminal ten
    /// times a second forever — and rebuilt the sort cache each time.
    pub fn handle_state_push(&mut self, torrents: Vec<TorrentInfo>) -> bool {
        let changed = Self::render_state_differs(&self.torrents, &torrents);
        self.torrents = torrents;
        if !changed {
            // Same rows, same values: the cached order is still correct and
            // nothing on screen would move.
            return false;
        }
        self.invalidate_sort();
        self.prune_stale_state();
        self.ensure_sort_cache();
        self.restore_selection();
        true
    }

    /// Compare only the fields that reach the screen. Deliberately ignores
    /// `files`/`peers`/`trackers`, which are populated for the Detail torrent
    /// only and are compared by the Detail view's own redraw trigger.
    fn render_state_differs(old: &[TorrentInfo], new: &[TorrentInfo]) -> bool {
        if old.len() != new.len() {
            return true;
        }
        old.iter().zip(new.iter()).any(|(a, b)| {
            a.id != b.id
                || a.name != b.name
                || a.status != b.status
                || a.size_bytes != b.size_bytes
                || a.downloaded_bytes != b.downloaded_bytes
                || a.uploaded_bytes != b.uploaded_bytes
                || a.download_speed != b.download_speed
                || a.upload_speed != b.upload_speed
                || a.peers_connected != b.peers_connected
                || a.peers_total != b.peers_total
                || a.eta_seconds != b.eta_seconds
                || a.files.len() != b.files.len()
                || a.peers.len() != b.peers.len()
        })
    }

    /// Set a new sort column and refresh the cache. Prefer these wrappers
    /// over assigning `sort_column` / `sort_reversed` / `filter_text` directly:
    /// each has to pair `invalidate_sort` with `ensure_sort_cache` *and*
    /// `restore_selection`, and skipping the last one leaves the user's cursor
    /// on whatever torrent happens to land at the old index.
    pub fn change_sort_column(&mut self, next: SortColumn) {
        self.sort_column = next;
        self.invalidate_sort();
        self.ensure_sort_cache();
        self.restore_selection();
    }

    /// Toggle the sort direction and refresh — see `change_sort_column`.
    pub fn toggle_sort_reversed(&mut self) {
        self.sort_reversed = !self.sort_reversed;
        self.invalidate_sort();
        self.ensure_sort_cache();
        self.restore_selection();
    }

    /// Append a character to the filter and refresh — see `change_sort_column`.
    /// Owns the same trims typing and paste share: control and bidi-override
    /// characters are dropped (`filter_text` renders raw in the filter bar
    /// and status line, so a pasted U+202E would garble both — same rationale
    /// as `search_push_char`) and the filter is capped at `MAX_FILTER_CHARS`.
    pub fn push_filter_char(&mut self, c: char) {
        if c.is_control()
            || crate::ui::util::is_bidi_control(c)
            || self.filter_text.chars().count() >= MAX_FILTER_CHARS
        {
            return;
        }
        self.filter_text.push(c);
        self.invalidate_sort();
        self.ensure_sort_cache();
        self.restore_selection();
    }

    /// Append a whole string (a bracketed-paste payload) and refresh ONCE.
    /// The per-character wrapper re-sorts the table on every push, which
    /// turned a large accidental paste into an
    /// O(chars × torrents·log(torrents)) freeze of the whole event loop.
    /// Same control-char filter and `MAX_FILTER_CHARS` cap as
    /// `push_filter_char`.
    pub fn push_filter_str(&mut self, s: &str) {
        let mut len = self.filter_text.chars().count();
        let mut changed = false;
        for c in s.chars() {
            if len >= MAX_FILTER_CHARS {
                break;
            }
            if c.is_control() || crate::ui::util::is_bidi_control(c) {
                continue;
            }
            self.filter_text.push(c);
            len += 1;
            changed = true;
        }
        if changed {
            self.invalidate_sort();
            self.ensure_sort_cache();
            self.restore_selection();
        }
    }

    /// Drop the trailing filter character and refresh — see
    /// `change_sort_column`.
    pub fn pop_filter_char(&mut self) {
        self.filter_text.pop();
        self.invalidate_sort();
        self.ensure_sort_cache();
        self.restore_selection();
    }

    /// Empty the filter and refresh — see `change_sort_column`.
    pub fn clear_filter(&mut self) {
        if !self.filter_text.is_empty() {
            self.filter_text.clear();
            self.invalidate_sort();
            self.ensure_sort_cache();
        }
        self.restore_selection();
    }

    fn rebuild_sort_cache(&mut self) {
        #[cfg(test)]
        {
            self.sort_rebuilds += 1;
        }
        let filter_lower = self.filter_text.to_lowercase();
        // Collect indices into `self.torrents` after applying the filter.
        let mut indices: Vec<usize> = self
            .torrents
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                if filter_lower.is_empty() {
                    true
                } else {
                    t.name.to_lowercase().contains(&filter_lower)
                }
            })
            .map(|(i, _)| i)
            .collect();

        if self.sort_column == SortColumn::Name {
            // Lowercase keys precomputed so each name is lowered once, not
            // O(N log N) times during sort comparisons.
            let lc: Vec<String> = indices
                .iter()
                .map(|&i| self.torrents[i].name.to_lowercase())
                .collect();
            let mut pos: Vec<usize> = (0..indices.len()).collect();
            pos.sort_by(|&a, &b| {
                let cmp = lc[a].cmp(&lc[b]);
                if self.sort_reversed {
                    cmp.reverse()
                } else {
                    cmp
                }
            });
            indices = pos.into_iter().map(|i| indices[i]).collect();
        } else {
            let sort_column = self.sort_column;
            let sort_reversed = self.sort_reversed;
            let torrents = &self.torrents;
            indices.sort_by(|&a, &b| {
                let ta = &torrents[a];
                let tb = &torrents[b];
                let cmp = match sort_column {
                    SortColumn::Index => ta.id.cmp(&tb.id),
                    SortColumn::Name => {
                        // Handled by the early-return above; degrade gracefully.
                        debug_assert!(false, "SortColumn::Name should hit the early return");
                        std::cmp::Ordering::Equal
                    }
                    SortColumn::Size => ta.size_bytes.cmp(&tb.size_bytes),
                    SortColumn::Progress => ta
                        .progress_percent()
                        .partial_cmp(&tb.progress_percent())
                        .unwrap_or(std::cmp::Ordering::Equal),
                    SortColumn::Speed => ta.download_speed.cmp(&tb.download_speed),
                    SortColumn::Peers => ta.peers_connected.cmp(&tb.peers_connected),
                    SortColumn::Eta => match (ta.eta_seconds, tb.eta_seconds) {
                        (Some(a_eta), Some(b_eta)) => a_eta.cmp(&b_eta),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => std::cmp::Ordering::Equal,
                    },
                    SortColumn::Status => ta.status.sort_key().cmp(&tb.status.sort_key()),
                };
                if sort_reversed {
                    cmp.reverse()
                } else {
                    cmp
                }
            });
        }

        self.sort_cache = indices;
        self.sort_dirty = false;
    }

    /// The current filtered and sorted view, borrowed from the cache. Takes
    /// `&self` so render paths can call it, which means it will never rebuild a
    /// cache it finds stale: if `sort_dirty` is set it falls back to a full
    /// live sort, correct but O(N log N) on every call, and this runs several
    /// times per frame.
    ///
    /// If the cache is *not* dirty but `torrents` was mutated behind its back,
    /// indices that no longer resolve are dropped and you get a quietly
    /// shortened list with no error. Mutate through `handle_state_push` (or
    /// pair `invalidate_sort` with `ensure_sort_cache`) and neither happens.
    pub fn sorted_torrents(&self) -> Vec<&TorrentInfo> {
        // Read path: never rebuild. Callers that need a fresh cache must
        // ensure invalidate_sort + ensure_sort_cache ran first; for the
        // hot path this is wrapped by the convenience methods below.
        if self.sort_dirty {
            // Fall back to the live sort when called before the cache has
            // been (re)built. Avoids needing `&mut self` in render paths.
            // This branch is also what tests rely on — they construct an
            // App and immediately call sorted_torrents without a state push.
            return self.sorted_torrents_live();
        }
        self.sort_cache
            .iter()
            .filter_map(|&i| self.torrents.get(i))
            .collect()
    }

    /// Refresh the sort cache if dirty. Call before render paths that need
    /// fresh data. The hot path (run_app) keeps this cheap by invalidating
    /// only when the underlying inputs change.
    pub fn ensure_sort_cache(&mut self) {
        if self.sort_dirty {
            self.rebuild_sort_cache();
        }
    }

    /// Live (uncached) sort. Used as a fallback in `sorted_torrents()` when
    /// the cache is dirty and the caller only has `&self`. Keeps the original
    /// O(N log N) cost but skips the lowercase keys allocation when not Name.
    fn sorted_torrents_live(&self) -> Vec<&TorrentInfo> {
        let filter_lower = self.filter_text.to_lowercase();
        let mut torrents: Vec<&TorrentInfo> = self
            .torrents
            .iter()
            .filter(|t| {
                if filter_lower.is_empty() {
                    true
                } else {
                    t.name.to_lowercase().contains(&filter_lower)
                }
            })
            .collect();

        if self.sort_column == SortColumn::Name {
            let lc: Vec<String> = torrents.iter().map(|t| t.name.to_lowercase()).collect();
            let mut indices: Vec<usize> = (0..torrents.len()).collect();
            indices.sort_by(|&a, &b| {
                let cmp = lc[a].cmp(&lc[b]);
                if self.sort_reversed {
                    cmp.reverse()
                } else {
                    cmp
                }
            });
            return indices.into_iter().map(|i| torrents[i]).collect();
        }

        torrents.sort_by(|a, b| {
            let cmp = match self.sort_column {
                SortColumn::Index => a.id.cmp(&b.id),
                SortColumn::Name => {
                    debug_assert!(false, "SortColumn::Name should hit the early return");
                    std::cmp::Ordering::Equal
                }
                SortColumn::Size => a.size_bytes.cmp(&b.size_bytes),
                SortColumn::Progress => a
                    .progress_percent()
                    .partial_cmp(&b.progress_percent())
                    .unwrap_or(std::cmp::Ordering::Equal),
                SortColumn::Speed => a.download_speed.cmp(&b.download_speed),
                SortColumn::Peers => a.peers_connected.cmp(&b.peers_connected),
                SortColumn::Eta => match (a.eta_seconds, b.eta_seconds) {
                    (Some(a_eta), Some(b_eta)) => a_eta.cmp(&b_eta),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                },
                SortColumn::Status => a.status.sort_key().cmp(&b.status.sort_key()),
            };
            if self.sort_reversed {
                cmp.reverse()
            } else {
                cmp
            }
        });

        torrents
    }

    pub fn next(&mut self) {
        let count = self.sorted_torrents().len();
        if count == 0 {
            return;
        }
        self.selected_index = (self.selected_index + 1).min(count - 1);
        self.update_selected_id();
        self.table_state.select(Some(self.selected_index));
    }

    pub fn previous(&mut self) {
        if self.sorted_torrents().is_empty() {
            return;
        }
        self.selected_index = self.selected_index.saturating_sub(1);
        self.update_selected_id();
        self.table_state.select(Some(self.selected_index));
    }

    pub fn update_selected_id(&mut self) {
        let sorted = self.sorted_torrents();
        self.selected_torrent_id = sorted.get(self.selected_index).map(|t| t.id);
    }

    pub fn selected_torrent(&self) -> Option<&TorrentInfo> {
        // Index the sort cache directly instead of materializing the whole
        // `Vec<&TorrentInfo>` just to read one element — this is called several
        // times per frame. Falls back to the live sort when the cache is dirty.
        if self.sort_dirty {
            return self
                .sorted_torrents_live()
                .get(self.selected_index)
                .copied();
        }
        self.sort_cache
            .get(self.selected_index)
            .and_then(|&i| self.torrents.get(i))
    }

    /// Re-anchor the selection after the visible list changed. Follows the
    /// selected torrent by id where it still exists, so re-sorting or filtering
    /// keeps the cursor on the same *torrent* rather than the same row number;
    /// when it is gone, the old index is clamped into the new bounds.
    ///
    /// Reads the sort cache, so run `ensure_sort_cache()` first or you anchor
    /// against the previous ordering. Also refreshes `selected_torrent_id`,
    /// syncs `table_state`, and clamps the detail-view cursors.
    pub fn restore_selection(&mut self) {
        let (new_index, new_id) = {
            let sorted = self.sorted_torrents();
            let len = sorted.len();
            let new_index = if let Some(id) = self.selected_torrent_id {
                if let Some(pos) = sorted.iter().position(|t| t.id == id) {
                    pos
                } else if len > 0 {
                    self.selected_index.min(len - 1)
                } else {
                    0
                }
            } else if len > 0 {
                self.selected_index.min(len - 1)
            } else {
                0
            };
            // Refresh the cached id from the resolved index so callers don't
            // see a stale value between user actions.
            let new_id = sorted.get(new_index).map(|t| t.id);
            (new_index, new_id)
        };
        self.selected_index = new_index;
        self.selected_torrent_id = new_id;
        self.table_state.select(Some(self.selected_index));
        self.clamp_detail_indices();
    }

    /// Clamp detail-view indices against the currently selected torrent's
    /// file/peer counts. Called after every state push so a torrent that
    /// briefly drops to FetchingMetadata (files emptied) or that gains/loses
    /// peers can't leave the cursor pointing past the end of the list.
    pub fn clamp_detail_indices(&mut self) {
        // Extract bounds in a scope so the immutable borrow of `self` is
        // released before we mutate the index fields.
        let bounds = self
            .selected_torrent()
            .map(|t| (t.files.len(), t.peers.len()));
        match bounds {
            Some((file_count, peer_count)) => {
                let file_max = file_count.saturating_sub(1);
                if self.detail_file_index > file_max {
                    self.detail_file_index = file_max;
                }
                let peer_max = peer_count.saturating_sub(1);
                if self.detail_peer_index > peer_max {
                    self.detail_peer_index = peer_max;
                }
            }
            None => {
                self.detail_file_index = 0;
                self.detail_peer_index = 0;
            }
        }
    }

    /// Drop UI-side bookkeeping for torrents that are no longer in the list.
    /// The engine prunes its own maps; this mirrors the same pattern on the UI
    /// side (called from the state-update arm of the main loop).
    pub fn prune_stale_state(&mut self) {
        let current: HashMap<usize, &str> = self
            .torrents
            .iter()
            .map(|t| (t.id, t.name.as_str()))
            .collect();
        // Drop state for ids that are gone, and — because librqbit recycles
        // ids as `max + 1` — also for ids whose torrent has been replaced by a
        // different one. Without the identity check, deleting the highest-id
        // torrent and letting the watch folder add a new one into the freed id
        // silently carried the old file deselections onto the new torrent.
        let identities = &self.torrent_identities;
        self.marked_ids.retain(|id| {
            current
                .get(id)
                .is_some_and(|name| identities.get(id).is_none_or(|prev| prev == name))
        });
        self.deselected_files.retain(|id, _| {
            current
                .get(id)
                .is_some_and(|name| identities.get(id).is_none_or(|prev| prev == name))
        });
        self.torrent_identities = current
            .into_iter()
            .map(|(id, name)| (id, name.to_string()))
            .collect();
    }

    pub fn total_download_speed(&self) -> u64 {
        self.torrents.iter().map(|t| t.download_speed).sum()
    }

    pub fn total_upload_speed(&self) -> u64 {
        self.torrents.iter().map(|t| t.upload_speed).sum()
    }

    pub fn total_uploaded_bytes(&self) -> u64 {
        self.torrents.iter().map(|t| t.uploaded_bytes).sum()
    }

    pub fn total_downloaded_bytes(&self) -> u64 {
        self.torrents.iter().map(|t| t.downloaded_bytes).sum()
    }

    pub fn active_count(&self) -> usize {
        self.torrents
            .iter()
            .filter(|t| matches!(t.status, TorrentStatus::Downloading))
            .count()
    }

    pub fn has_fetching_metadata(&self) -> bool {
        self.torrents
            .iter()
            .any(|t| matches!(t.status, TorrentStatus::FetchingMetadata))
    }

    /// Show an error in the status bar for 3 seconds. Replaces any error
    /// already showing — unlike `set_info`, which concatenates — because the
    /// newest failure is the one the user needs.
    pub fn set_error(&mut self, msg: String) {
        self.error_message = Some(msg);
        self.error_timer = Some(std::time::Instant::now());
    }

    /// Show an informational message for 5 seconds. If one is still on screen
    /// the new text is appended after a `|` rather than replacing it, so a
    /// burst does not silently swallow all but the last. The combined line is
    /// rendered unwrapped, which is why the engine batches its own bursts into
    /// at most two messages.
    pub fn set_info(&mut self, msg: String) {
        // If a previous info message is still on screen, append the new one so
        // burst notifications (e.g. multiple completions in one tick) don't
        // overwrite each other silently.
        if let Some(existing) = self.info_message.take() {
            self.info_message = Some(format!("{} | {}", existing, msg));
        } else {
            self.info_message = Some(msg);
        }
        self.info_timer = Some(std::time::Instant::now());
    }

    /// Age out status-bar messages: errors after 3 seconds, info after 5.
    /// There is no timer task behind this — the main loop calls it every pass,
    /// so a message set outside that loop stays on screen until the next
    /// iteration, and one set during shutdown never clears at all.
    pub fn clear_expired_messages(&mut self) {
        if let Some(timer) = self.error_timer {
            if timer.elapsed() > std::time::Duration::from_secs(3) {
                self.error_message = None;
                self.error_timer = None;
            }
        }
        if let Some(timer) = self.info_timer {
            if timer.elapsed() > std::time::Duration::from_secs(5) {
                self.info_message = None;
                self.info_timer = None;
            }
        }
    }

    pub fn tick_spinner(&mut self) {
        self.spinner_tick = (self.spinner_tick + 1) % 10;
    }

    /// Append a character to the search query. Rejects control characters
    /// (like the add dialog's `InputWidget::push`) and bidi controls, plus a
    /// length cap. Bidi controls matter here because the query is rendered raw
    /// in the search bar and results title — a pasted U+202E would visually
    /// reverse the rest of the title line on bidi-aware terminals. Paste goes
    /// through here too, so typing and pasting obey the same rules.
    pub fn search_push_char(&mut self, c: char) {
        if c.is_control() || crate::ui::util::is_bidi_control(c) {
            return;
        }
        if self.search.input.chars().count() >= crate::search::MAX_QUERY_CHARS {
            return;
        }
        self.search.input.push(c);
    }

    pub fn search_pop_char(&mut self) {
        self.search.input.pop();
    }

    /// Arm a search for the current input: bump the generation, remember the
    /// query, raise the in-flight flag. Returns what the caller needs to spawn
    /// the task, or `None` for an empty/whitespace query (a silent no-op, like
    /// the filter's tolerance for empties). No I/O happens here — the caller
    /// owns the spawn — which is what keeps every Esc/Enter path unit-testable.
    pub fn fire_search(&mut self) -> Option<(String, u64)> {
        let query = self.search.input.trim().to_string();
        if query.is_empty() {
            return None;
        }
        self.search.query = query.clone();
        Some((query, self.arm_search()))
    }

    /// Re-arm the previously fired query (the `r` retry): same generation
    /// bump, no dependence on the edit buffer.
    pub fn refire_search(&mut self) -> Option<(String, u64)> {
        if self.search.query.is_empty() {
            return None;
        }
        Some((self.search.query.clone(), self.arm_search()))
    }

    fn arm_search(&mut self) -> u64 {
        self.search.generation += 1;
        self.search.in_flight = true;
        self.search.provider_errors.clear();
        self.search.generation
    }

    /// Apply a search outcome; returns whether anything on screen changed.
    /// The discard rule for superseded queries lives here: a generation
    /// mismatch drops the outcome without touching any state. A matching
    /// outcome arriving while the user is elsewhere is stored silently (they
    /// see it when they return via `s`) — the `false` return only suppresses
    /// the repaint.
    pub fn apply_search_outcome(&mut self, outcome: SearchOutcome) -> bool {
        if outcome.generation != self.search.generation {
            return false;
        }
        self.search.in_flight = false;
        self.search.results = outcome.results;
        self.search.provider_errors = outcome.provider_errors;
        self.search.searched_once = true;
        // Fresh results honor the session's sticky sort, and the cursor
        // starts at the top of the *sorted* list rather than following
        // whatever the previous selection was.
        self.resort_search_results();
        self.search.selected = 0;
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        self.search.table_state = table_state;
        self.in_search_view()
    }

    /// Effective sort direction of the results table, for the header arrow:
    /// `true` = descending (▼).
    pub fn result_sort_descending(&self) -> bool {
        self.search.sort_column.natural_descending() != self.search.sort_reversed
    }

    /// Sorting is a no-op while the spinner hides the table or there is
    /// nothing to sort — the same condition as the registry rows'
    /// `has_search_result` availability, so the raw keys agree with what the
    /// palette offers. Without this, Tab/R during an in-flight retry would
    /// silently mutate the sticky sort with zero visible feedback and the
    /// landing results would arrive ordered by a column never visibly chosen.
    fn result_sort_locked(&self) -> bool {
        self.search.in_flight || self.search.results.is_empty()
    }

    /// `Tab` in the results view: next column, landing in that column's
    /// natural direction (mirroring how selecting a column behaves in
    /// desktop torrent clients), not carrying the previous reversal over.
    pub fn cycle_result_sort(&mut self) {
        if self.result_sort_locked() {
            return;
        }
        self.search.sort_column = self.search.sort_column.next();
        self.search.sort_reversed = false;
        self.resort_search_results();
    }

    /// `R` in the results view: flip the current column's direction.
    pub fn reverse_result_sort(&mut self) {
        if self.result_sort_locked() {
            return;
        }
        self.search.sort_reversed = !self.search.sort_reversed;
        self.resort_search_results();
    }

    /// Re-sort the results in place, keeping the cursor on the result it was
    /// on (found again by info hash — the row moves, the selection follows).
    /// ≤ 500 rows and only runs on sort changes and outcome arrival, so no
    /// cache like the torrent table's is warranted.
    fn resort_search_results(&mut self) {
        let anchor = self.selected_search_result().map(|r| r.info_hash.clone());
        let col = self.search.sort_column;
        let descending = self.result_sort_descending();
        self.search.results.sort_by(|a, b| {
            let ord = match col {
                ResultSortColumn::Seeders => a.seeders.cmp(&b.seeders),
                ResultSortColumn::Leechers => a.leechers.cmp(&b.leechers),
                // Unknown sizes count as 0, sinking "?" rows to the bottom
                // in the default (descending) direction.
                ResultSortColumn::Size => a.size_bytes.unwrap_or(0).cmp(&b.size_bytes.unwrap_or(0)),
                ResultSortColumn::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            };
            let ord = if descending { ord.reverse() } else { ord };
            // Tiebreak after the flip, so equal keys always read A→Z
            // whichever direction the column sorts in.
            ord.then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
        });
        if let Some(hash) = anchor {
            if let Some(pos) = self.search.results.iter().position(|r| r.info_hash == hash) {
                self.search.selected = pos;
            }
        }
        self.search.selected = self
            .search
            .selected
            .min(self.search.results.len().saturating_sub(1));
        self.search.table_state.select(Some(self.search.selected));
    }

    pub fn search_next(&mut self) {
        let count = self.search.results.len();
        if count == 0 {
            return;
        }
        self.search.selected = (self.search.selected + 1).min(count - 1);
        self.search.table_state.select(Some(self.search.selected));
    }

    pub fn search_previous(&mut self) {
        if self.search.results.is_empty() {
            return;
        }
        self.search.selected = self.search.selected.saturating_sub(1);
        self.search.table_state.select(Some(self.search.selected));
    }

    pub fn selected_search_result(&self) -> Option<&SearchResult> {
        self.search.results.get(self.search.selected)
    }

    /// Whether a torrent with this info hash is currently in the session.
    /// Case-insensitive because search hashes are normalized to lowercase
    /// while the engine reports librqbit's formatting.
    pub fn torrent_in_session(&self, info_hash: &str) -> bool {
        self.torrents
            .iter()
            .any(|t| t.info_hash.eq_ignore_ascii_case(info_hash))
    }

    /// Whether a search view is what the user currently sees — directly, or
    /// underneath the palette overlay.
    pub fn in_search_view(&self) -> bool {
        match &self.mode {
            AppMode::Search | AppMode::SearchResults => true,
            AppMode::Palette => matches!(
                self.palette.return_mode,
                AppMode::Search | AppMode::SearchResults
            ),
            _ => false,
        }
    }

    /// Whether the frame tick should animate the spinner for a search in
    /// flight — only while a search view is actually on screen.
    pub fn search_spinner_active(&self) -> bool {
        self.search.in_flight && self.in_search_view()
    }

    /// Whether a detail view is what the user currently sees — directly, or
    /// underneath the palette overlay. The Ctrl+C intercept uses this to know
    /// the engine's `SetDetailTorrent` materialization must be cleared; the
    /// plain `mode == Detail` check missed the palette-over-Detail case and
    /// leaked per-tick file/peer building.
    pub fn in_detail_view(&self) -> bool {
        match &self.mode {
            AppMode::Detail => true,
            AppMode::Palette => self.palette.return_mode == AppMode::Detail,
            _ => false,
        }
    }

    /// Open the command palette over the current mode. Returns whether it
    /// opened — text-input modes refuse, because their keys (including `:`)
    /// must stay typeable.
    pub fn open_palette(&mut self) -> bool {
        if palette_scope(&self.mode).is_none() {
            return false;
        }
        self.palette.return_mode = self.mode.clone();
        self.palette.input.clear();
        self.palette.anchor = None;
        self.palette.table_state = {
            let mut ts = TableState::default();
            ts.select(Some(0));
            ts
        };
        self.mode = AppMode::Palette;
        true
    }

    /// Close the palette and return to the mode it opened over.
    pub fn close_palette(&mut self) {
        self.mode = self.palette.return_mode.clone();
    }

    /// The actions the palette currently offers: in-palette registry rows for
    /// the scope it opened from (plus Global), filtered by availability and
    /// the fuzzy query, best score first (name as the tiebreak so ordering is
    /// deterministic).
    pub fn palette_matches(&self) -> Vec<&'static ActionInfo> {
        let Some(scope) = palette_scope(&self.palette.return_mode) else {
            return Vec::new();
        };
        let mut scored: Vec<(u32, &'static ActionInfo)> = ACTIONS
            .iter()
            .filter(|a| a.in_palette && (a.scope == scope || a.scope == Scope::Global))
            .filter(|a| (a.available)(self))
            .filter_map(|a| fuzzy_score(&self.palette.input, tui_description(a)).map(|s| (s, a)))
            .collect();
        scored.sort_by(|(sa, a), (sb, b)| {
            sb.cmp(sa)
                .then_with(|| tui_description(a).cmp(tui_description(b)))
        });
        scored.into_iter().map(|(_, a)| a).collect()
    }

    /// Where the cursor sits in the given match list: the anchored action's
    /// position, or the top when nothing is anchored or the anchor vanished
    /// (its availability changed while the palette was open).
    pub fn palette_selected_index(&self, matches: &[&'static ActionInfo]) -> usize {
        self.palette
            .anchor
            .and_then(|anchor| matches.iter().position(|a| std::ptr::eq(*a, anchor)))
            .unwrap_or(0)
    }

    /// Append a character to the palette filter, mirroring the search query's
    /// control/bidi hygiene. Any edit resets the cursor to the top hit.
    pub fn palette_push_char(&mut self, c: char) {
        if c.is_control() || crate::ui::util::is_bidi_control(c) {
            return;
        }
        if self.palette.input.chars().count() >= MAX_PALETTE_QUERY_CHARS {
            return;
        }
        self.palette.input.push(c);
        self.palette.anchor = None;
        self.palette.table_state.select(Some(0));
    }

    pub fn palette_pop_char(&mut self) {
        self.palette.input.pop();
        self.palette.anchor = None;
        self.palette.table_state.select(Some(0));
    }

    pub fn palette_next(&mut self) {
        let matches = self.palette_matches();
        if matches.is_empty() {
            return;
        }
        let idx = (self.palette_selected_index(&matches) + 1).min(matches.len() - 1);
        self.palette.anchor = Some(matches[idx]);
        self.palette.table_state.select(Some(idx));
    }

    pub fn palette_previous(&mut self) {
        let matches = self.palette_matches();
        if matches.is_empty() {
            return;
        }
        let idx = self.palette_selected_index(&matches).saturating_sub(1);
        self.palette.anchor = Some(matches[idx]);
        self.palette.table_state.select(Some(idx));
    }

    /// Whether a file is included in the download. Returns `true` for torrents
    /// and indices this has never seen, because selection is tracked as the set
    /// of *exclusions* — so an out-of-range `file_index` also reports selected.
    /// Bound the index against the torrent's real file count before trusting
    /// it.
    pub fn is_file_selected(&self, torrent_id: usize, file_index: usize) -> bool {
        !self
            .deselected_files
            .get(&torrent_id)
            .is_some_and(|s| s.contains(&file_index))
    }

    pub fn toggle_file_selection(&mut self, torrent_id: usize, file_index: usize) {
        let set = self.deselected_files.entry(torrent_id).or_default();
        if set.contains(&file_index) {
            set.remove(&file_index);
        } else {
            set.insert(file_index);
        }
    }

    pub fn selected_file_indices(&self, torrent_id: usize, total_files: usize) -> Vec<usize> {
        (0..total_files)
            .filter(|i| self.is_file_selected(torrent_id, *i))
            .collect()
    }

    /// Whether the free-space figure is due for a refresh (at most once every
    /// 5 seconds). Safe and intended to call every frame — the rate limit lives
    /// here rather than at the call site. Marks the attempt immediately, so a
    /// slow probe can't queue up behind itself.
    pub fn disk_space_due(&mut self) -> bool {
        let due = match self.disk_space_timer {
            None => true,
            Some(t) => t.elapsed() > std::time::Duration::from_secs(5),
        };
        if due {
            self.disk_space_timer = Some(std::time::Instant::now());
        }
        due
    }

    /// Record a probe result. `None` (directory removed, network share
    /// unmounted) clears the cached value rather than keeping a stale reading,
    /// so the indicator disappears instead of lying.
    pub fn set_disk_space(&mut self, free: Option<u64>) {
        self.free_disk_space = free;
    }

    pub fn toggle_mark(&mut self) {
        if let Some(torrent) = self.selected_torrent() {
            let id = torrent.id;
            if self.marked_ids.contains(&id) {
                self.marked_ids.remove(&id);
            } else {
                self.marked_ids.insert(id);
            }
        }
    }

    pub fn clear_marks(&mut self) {
        self.marked_ids.clear();
    }

    /// Mark every torrent currently *visible* — the active filter applies —
    /// and add to the existing marks rather than replacing them, so narrowing
    /// the filter and pressing `v` repeatedly accumulates a selection across
    /// searches. Marks for torrents hidden by the filter survive untouched,
    /// including through a subsequent bulk delete.
    pub fn mark_all(&mut self) {
        let ids: Vec<usize> = self.sorted_torrents().iter().map(|t| t.id).collect();
        self.marked_ids.extend(ids);
    }

    pub fn has_marks(&self) -> bool {
        !self.marked_ids.is_empty()
    }

    pub fn marked_count(&self) -> usize {
        self.marked_ids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TorrentStatus;

    fn make_torrent(id: usize, name: &str, size: u64, status: TorrentStatus) -> TorrentInfo {
        TorrentInfo {
            id,
            name: name.to_string(),
            size_bytes: size,
            downloaded_bytes: 0,
            uploaded_bytes: 0,
            download_speed: 0,
            upload_speed: 0,
            peers_connected: 0,
            peers_total: 0,
            status,
            eta_seconds: None,
            files: Vec::new(),
            peers: Vec::new(),
            info_hash: String::new(),
            trackers: Vec::new(),
            piece_length: None,
            content_path: None,
        }
    }

    /// Build an App the way the running app does — through `handle_state_push`,
    /// so the sort cache is built and every test below exercises
    /// `rebuild_sort_cache` rather than the dirty-cache fallback.
    fn app_with_torrents(torrents: Vec<TorrentInfo>) -> App {
        let mut app = App::new();
        app.handle_state_push(torrents);
        app
    }

    /// The fallback path: torrents assigned directly, cache left dirty. Only
    /// for the two tests that deliberately cover `sorted_torrents_live`.
    fn app_with_dirty_cache(torrents: Vec<TorrentInfo>) -> App {
        let mut app = App::new();
        app.torrents = torrents;
        app
    }

    #[test]
    fn sorted_torrents_no_filter() {
        let app = app_with_torrents(vec![
            make_torrent(0, "Alpha", 100, TorrentStatus::Downloading),
            make_torrent(1, "Beta", 200, TorrentStatus::Paused),
        ]);
        assert_eq!(app.sorted_torrents().len(), 2);
    }

    #[test]
    fn sorted_torrents_filter_case_insensitive() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "Alpha", 100, TorrentStatus::Downloading),
            make_torrent(1, "Beta", 200, TorrentStatus::Paused),
        ]);
        for c in "alpha".chars() {
            app.push_filter_char(c);
        }
        let sorted = app.sorted_torrents();
        assert_eq!(sorted.len(), 1);
        assert_eq!(sorted[0].name, "Alpha");
    }

    #[test]
    fn filter_paste_drops_control_and_bidi_chars_and_applies() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "Alpha", 100, TorrentStatus::Downloading),
            make_torrent(1, "Beta", 200, TorrentStatus::Paused),
        ]);
        // Escape, newline and a bidi override smuggled into the payload must
        // not survive — filter_text renders raw in the filter bar.
        app.push_filter_str("al\u{1b}ph\n\u{202e}a");
        assert_eq!(app.filter_text, "alpha");
        let sorted = app.sorted_torrents();
        assert_eq!(sorted.len(), 1);
        assert_eq!(sorted[0].name, "Alpha");
    }

    #[test]
    fn filter_paste_resorts_once_while_typing_resorts_per_char() {
        // The freeze regression this pins: the paste path used to route
        // through push_filter_char per character, re-sorting the table each
        // time. The counter makes the batching claim testable.
        let mut app = app_with_torrents(vec![
            make_torrent(0, "Alpha", 100, TorrentStatus::Downloading),
            make_torrent(1, "Beta", 200, TorrentStatus::Paused),
        ]);
        let base = app.sort_rebuilds;
        app.push_filter_str("alpha");
        assert_eq!(app.sort_rebuilds, base + 1, "one paste = one resort");

        let base = app.sort_rebuilds;
        for c in "beta".chars() {
            app.push_filter_char(c);
        }
        assert_eq!(app.sort_rebuilds, base + 4, "typing resorts per char");
    }

    #[test]
    fn filter_paste_larger_than_the_cap_is_truncated() {
        let mut app = app_with_torrents(vec![make_torrent(
            0,
            "Alpha",
            100,
            TorrentStatus::Downloading,
        )]);
        // Pins the cap and cache coherence for a huge accidental paste (the
        // once-per-character resort half of that regression is pinned by
        // `filter_paste_resorts_once_while_typing_resorts_per_char`).
        app.push_filter_str(&"x".repeat(10_000));
        assert_eq!(app.filter_text.chars().count(), MAX_FILTER_CHARS);
        assert!(app.sorted_torrents().is_empty());
    }

    #[test]
    fn typed_filter_chars_respect_the_cap_too() {
        let mut app = app_with_torrents(Vec::new());
        for _ in 0..(MAX_FILTER_CHARS + 5) {
            app.push_filter_char('x');
        }
        assert_eq!(app.filter_text.chars().count(), MAX_FILTER_CHARS);
    }

    #[test]
    fn sorted_torrents_filter_no_matches() {
        let mut app = app_with_torrents(vec![make_torrent(
            0,
            "Alpha",
            100,
            TorrentStatus::Downloading,
        )]);
        for c in "zzz".chars() {
            app.push_filter_char(c);
        }
        assert!(app.sorted_torrents().is_empty());
    }

    #[test]
    fn recycled_torrent_id_drops_stale_marks_and_deselections() {
        // librqbit reuses `max(ids) + 1`, so deleting the highest-id torrent
        // frees its id for a concurrent watch-folder add. Without the identity
        // check the old deselections silently attached to the new torrent.
        let mut app = App::new();
        app.handle_state_push(vec![make_torrent(
            7,
            "Old",
            100,
            TorrentStatus::Downloading,
        )]);
        app.marked_ids.insert(7);
        app.deselected_files.insert(7, HashSet::from([2, 5]));

        // Same id, different torrent.
        app.handle_state_push(vec![make_torrent(
            7,
            "New",
            100,
            TorrentStatus::Downloading,
        )]);
        assert!(!app.marked_ids.contains(&7), "mark must not survive");
        assert!(
            !app.deselected_files.contains_key(&7),
            "deselection must not survive"
        );
        assert!(
            app.is_file_selected(7, 2),
            "new torrent downloads everything"
        );
    }

    #[test]
    fn same_torrent_keeps_its_marks_across_pushes() {
        let mut app = App::new();
        let rows = vec![make_torrent(7, "Same", 100, TorrentStatus::Downloading)];
        app.handle_state_push(rows.clone());
        app.marked_ids.insert(7);
        app.deselected_files.insert(7, HashSet::from([1]));

        let mut moved = rows.clone();
        moved[0].downloaded_bytes += 10;
        app.handle_state_push(moved);
        assert!(app.marked_ids.contains(&7));
        assert!(!app.is_file_selected(7, 1));
    }

    #[test]
    fn identical_state_push_reports_no_change() {
        // The engine pushes every 100 ms regardless. Repainting on each one
        // meant idle torrents redrew the terminal ten times a second.
        let rows = vec![
            make_torrent(0, "Alpha", 100, TorrentStatus::Downloading),
            make_torrent(1, "Beta", 200, TorrentStatus::Paused),
        ];
        let mut app = App::new();
        assert!(
            app.handle_state_push(rows.clone()),
            "first push is a change"
        );
        assert!(
            !app.handle_state_push(rows.clone()),
            "identical push is not"
        );

        // ...but anything the table draws must still trigger a repaint.
        let mut moved = rows.clone();
        moved[0].downloaded_bytes += 1;
        assert!(app.handle_state_push(moved), "progress moved");

        let mut renamed = rows.clone();
        renamed[1].status = TorrentStatus::Seeding;
        assert!(app.handle_state_push(renamed), "status changed");

        assert!(
            app.handle_state_push(vec![rows[0].clone()]),
            "a torrent disappearing is a change"
        );
    }

    #[test]
    fn sorted_torrents_falls_back_to_a_live_sort_when_the_cache_is_dirty() {
        // The `&self` read path can't rebuild, so it sorts live instead of
        // returning a stale order. Mutating the fields directly (rather than
        // through `change_sort_column`) is what leaves the cache dirty.
        let mut app = app_with_dirty_cache(vec![
            make_torrent(0, "Zeta", 100, TorrentStatus::Downloading),
            make_torrent(1, "Alpha", 200, TorrentStatus::Downloading),
        ]);
        app.sort_column = SortColumn::Name;
        let sorted = app.sorted_torrents();
        assert_eq!(sorted[0].name, "Alpha");
        assert_eq!(sorted[1].name, "Zeta");
    }

    #[test]
    fn cached_and_live_comparators_agree() {
        // `rebuild_sort_cache` and `sorted_torrents_live` carry separate copies
        // of the comparator. Pin them against each other so adding a sort
        // column to one and not the other fails here.
        for column in [
            SortColumn::Index,
            SortColumn::Name,
            SortColumn::Size,
            SortColumn::Progress,
            SortColumn::Status,
            SortColumn::Speed,
            SortColumn::Eta,
        ] {
            for reversed in [false, true] {
                let torrents = vec![
                    make_torrent(0, "Zeta", 100, TorrentStatus::Downloading),
                    make_torrent(1, "Alpha", 300, TorrentStatus::Paused),
                    make_torrent(2, "Mid", 200, TorrentStatus::Seeding),
                ];
                let mut cached = app_with_torrents(torrents.clone());
                cached.change_sort_column(column);
                if reversed {
                    cached.toggle_sort_reversed();
                }
                let mut live = app_with_dirty_cache(torrents);
                live.sort_column = column;
                live.sort_reversed = reversed;

                let a: Vec<usize> = cached.sorted_torrents().iter().map(|t| t.id).collect();
                let b: Vec<usize> = live.sorted_torrents().iter().map(|t| t.id).collect();
                assert_eq!(a, b, "column {column:?} reversed={reversed}");
            }
        }
    }

    #[test]
    fn sorted_by_name() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "Zeta", 100, TorrentStatus::Downloading),
            make_torrent(1, "Alpha", 200, TorrentStatus::Downloading),
        ]);
        app.change_sort_column(SortColumn::Name);
        let sorted = app.sorted_torrents();
        assert_eq!(sorted[0].name, "Alpha");
        assert_eq!(sorted[1].name, "Zeta");
    }

    #[test]
    fn sorted_by_name_reversed() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "Alpha", 100, TorrentStatus::Downloading),
            make_torrent(1, "Zeta", 200, TorrentStatus::Downloading),
        ]);
        app.change_sort_column(SortColumn::Name);
        app.toggle_sort_reversed();
        let sorted = app.sorted_torrents();
        assert_eq!(sorted[0].name, "Zeta");
    }

    #[test]
    fn sorted_by_size() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "Big", 1000, TorrentStatus::Downloading),
            make_torrent(1, "Small", 100, TorrentStatus::Downloading),
        ]);
        app.change_sort_column(SortColumn::Size);
        let sorted = app.sorted_torrents();
        assert_eq!(sorted[0].name, "Small");
        assert_eq!(sorted[1].name, "Big");
    }

    #[test]
    fn sorted_eta_none_last() {
        let mut t1 = make_torrent(0, "A", 100, TorrentStatus::Downloading);
        t1.eta_seconds = Some(60);
        let mut t2 = make_torrent(1, "B", 100, TorrentStatus::Downloading);
        t2.eta_seconds = None;
        let mut app = app_with_torrents(vec![t2, t1]);
        app.sort_column = SortColumn::Eta;
        let sorted = app.sorted_torrents();
        assert_eq!(sorted[0].name, "A"); // Some(60) first
        assert_eq!(sorted[1].name, "B"); // None last
    }

    #[test]
    fn next_empty_list() {
        let mut app = App::new();
        app.next(); // should not panic
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn next_single_item() {
        let mut app =
            app_with_torrents(vec![make_torrent(0, "A", 100, TorrentStatus::Downloading)]);
        app.next();
        assert_eq!(app.selected_index, 0); // stays at 0
    }

    #[test]
    fn next_advances() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "A", 100, TorrentStatus::Downloading),
            make_torrent(1, "B", 100, TorrentStatus::Downloading),
            make_torrent(2, "C", 100, TorrentStatus::Downloading),
        ]);
        app.next();
        assert_eq!(app.selected_index, 1);
        app.next();
        assert_eq!(app.selected_index, 2);
        app.next();
        assert_eq!(app.selected_index, 2); // clamped at end
    }

    #[test]
    fn previous_at_zero() {
        let mut app =
            app_with_torrents(vec![make_torrent(0, "A", 100, TorrentStatus::Downloading)]);
        app.previous();
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn previous_moves_up() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "A", 100, TorrentStatus::Downloading),
            make_torrent(1, "B", 100, TorrentStatus::Downloading),
        ]);
        app.selected_index = 1;
        app.previous();
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn selected_torrent_returns_correct() {
        let app = app_with_torrents(vec![
            make_torrent(0, "A", 100, TorrentStatus::Downloading),
            make_torrent(1, "B", 200, TorrentStatus::Downloading),
        ]);
        let t = app.selected_torrent().unwrap();
        assert_eq!(t.name, "A");
    }

    #[test]
    fn toggle_mark() {
        let mut app =
            app_with_torrents(vec![make_torrent(0, "A", 100, TorrentStatus::Downloading)]);
        assert!(!app.has_marks());
        app.toggle_mark();
        assert!(app.has_marks());
        assert_eq!(app.marked_count(), 1);
        app.toggle_mark();
        assert!(!app.has_marks());
    }

    #[test]
    fn mark_all_and_clear() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "A", 100, TorrentStatus::Downloading),
            make_torrent(1, "B", 100, TorrentStatus::Downloading),
            make_torrent(2, "C", 100, TorrentStatus::Downloading),
        ]);
        app.mark_all();
        assert_eq!(app.marked_count(), 3);
        app.clear_marks();
        assert!(!app.has_marks());
    }

    #[test]
    fn marks_tracked_by_id() {
        let mut app = app_with_torrents(vec![
            make_torrent(5, "A", 100, TorrentStatus::Downloading),
            make_torrent(10, "B", 100, TorrentStatus::Downloading),
        ]);
        app.toggle_mark(); // marks id=5 (first in sorted)
        assert!(app.marked_ids.contains(&5));
        assert!(!app.marked_ids.contains(&10));
    }

    #[test]
    fn restore_selection_refreshes_cached_id() {
        let mut app = app_with_torrents(vec![
            make_torrent(5, "A", 100, TorrentStatus::Downloading),
            make_torrent(10, "B", 100, TorrentStatus::Downloading),
        ]);
        app.selected_index = 1;
        app.update_selected_id();
        assert_eq!(app.selected_torrent_id, Some(10));

        // Remove the torrent currently selected and call restore_selection.
        app.torrents.retain(|t| t.id != 10);
        app.restore_selection();
        assert_eq!(app.selected_index, 0);
        // The cached id must follow the new selection, not stay stale on 10.
        assert_eq!(app.selected_torrent_id, Some(5));
    }

    #[test]
    fn prune_stale_state_drops_marks_and_deselections() {
        let mut app = app_with_torrents(vec![
            make_torrent(1, "A", 100, TorrentStatus::Downloading),
            make_torrent(2, "B", 100, TorrentStatus::Downloading),
        ]);
        app.marked_ids.insert(1);
        app.marked_ids.insert(2);
        app.marked_ids.insert(99); // stale, no torrent with id 99
        app.deselected_files.insert(2, HashSet::new());
        app.deselected_files.insert(42, HashSet::new()); // stale

        app.prune_stale_state();

        assert!(app.marked_ids.contains(&1));
        assert!(app.marked_ids.contains(&2));
        assert!(!app.marked_ids.contains(&99));
        assert!(app.deselected_files.contains_key(&2));
        assert!(!app.deselected_files.contains_key(&42));
    }

    #[test]
    fn set_info_concatenates_when_unread() {
        let mut app = App::new();
        app.set_info("first".to_string());
        app.set_info("second".to_string());
        assert_eq!(app.info_message.as_deref(), Some("first | second"));
    }

    fn file(name: &str) -> crate::types::FileInfo {
        crate::types::FileInfo {
            name: name.to_string(),
            size_bytes: 100,
            progress_bytes: 0,
        }
    }

    fn peer(addr: &str) -> crate::types::PeerInfo {
        crate::types::PeerInfo {
            address: addr.to_string(),
            state: "Live".to_string(),
            downloaded_bytes: 0,
            pieces: 0,
            errors: 0,
        }
    }

    #[test]
    fn tick_spinner_wraps_at_ten() {
        let mut app = App::new();
        assert_eq!(app.spinner_tick, 0);
        app.tick_spinner();
        assert_eq!(app.spinner_tick, 1);
        for _ in 0..9 {
            app.tick_spinner();
        }
        // 10 ticks from 0 wraps back to 0.
        assert_eq!(app.spinner_tick, 0);
        // The `% 10` in tick_spinner must match SPINNER_FRAMES.len(), or
        // table.rs panics when it indexes SPINNER_FRAMES[spinner_tick].
        assert_eq!(crate::ui::progress::SPINNER_FRAMES.len(), 10);
    }

    #[test]
    fn clamp_detail_indices_clamps_to_last_when_lists_shrink() {
        let mut t = make_torrent(0, "A", 100, TorrentStatus::Downloading);
        t.files = vec![file("a"), file("b")];
        t.peers = vec![peer("1.1.1.1:1"), peer("2.2.2.2:2"), peer("3.3.3.3:3")];
        let mut app = app_with_torrents(vec![t]);
        app.update_selected_id();
        app.detail_file_index = 9;
        app.detail_peer_index = 9;
        app.clamp_detail_indices();
        assert_eq!(app.detail_file_index, 1); // 2 files -> max index 1
        assert_eq!(app.detail_peer_index, 2); // 3 peers -> max index 2
    }

    #[test]
    fn clamp_detail_indices_resets_when_no_selection() {
        let mut app = App::new(); // no torrents -> selected_torrent() is None
        app.detail_file_index = 5;
        app.detail_peer_index = 7;
        app.clamp_detail_indices();
        assert_eq!(app.detail_file_index, 0);
        assert_eq!(app.detail_peer_index, 0);
    }

    #[test]
    fn confirm_on_quit_required_true_when_active() {
        let mut app =
            app_with_torrents(vec![make_torrent(0, "A", 100, TorrentStatus::Downloading)]);
        app.confirm_on_quit = true;
        assert!(app.confirm_on_quit_required());
    }

    #[test]
    fn confirm_on_quit_required_false_when_idle() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "A", 100, TorrentStatus::Complete),
            make_torrent(1, "B", 100, TorrentStatus::Paused),
        ]);
        app.confirm_on_quit = true;
        assert!(!app.confirm_on_quit_required());
    }

    #[test]
    fn confirm_on_quit_required_false_when_disabled() {
        let mut app =
            app_with_torrents(vec![make_torrent(0, "A", 100, TorrentStatus::Downloading)]);
        app.confirm_on_quit = false;
        assert!(!app.confirm_on_quit_required());
    }

    #[test]
    fn file_selection_toggle_and_query() {
        let mut app = App::new();
        // All files selected by default.
        assert!(app.is_file_selected(0, 0));
        assert_eq!(app.selected_file_indices(0, 3), vec![0, 1, 2]);
        // Deselect file 1.
        app.toggle_file_selection(0, 1);
        assert!(!app.is_file_selected(0, 1));
        assert_eq!(app.selected_file_indices(0, 3), vec![0, 2]);
        // Toggle it back.
        app.toggle_file_selection(0, 1);
        assert!(app.is_file_selected(0, 1));
        assert_eq!(app.selected_file_indices(0, 3), vec![0, 1, 2]);
    }

    #[test]
    fn messages_expire_after_their_window() {
        use std::time::{Duration, Instant};
        let mut app = App::new();
        app.set_error("boom".to_string());
        app.set_info("note".to_string());
        // Fresh messages are not cleared.
        app.clear_expired_messages();
        assert_eq!(app.error_message.as_deref(), Some("boom"));
        assert_eq!(app.info_message.as_deref(), Some("note"));
        // Age the timers past their windows (3s errors, 5s info).
        app.error_timer = Some(
            Instant::now()
                .checked_sub(Duration::from_secs(4))
                .expect("system uptime should exceed a few seconds"),
        );
        app.info_timer = Some(
            Instant::now()
                .checked_sub(Duration::from_secs(6))
                .expect("system uptime should exceed a few seconds"),
        );
        app.clear_expired_messages();
        assert!(app.error_message.is_none());
        assert!(app.info_message.is_none());
    }

    fn search_result(hash: &str, seeders: u64) -> SearchResult {
        SearchResult {
            title: format!("t-{hash}"),
            info_hash: hash.to_string(),
            size_bytes: Some(100),
            seeders,
            leechers: 0,
            source: crate::search::SourceSet {
                apibay: true,
                torrents_csv: false,
            },
        }
    }

    fn outcome(generation: u64, results: Vec<SearchResult>) -> SearchOutcome {
        SearchOutcome {
            generation,
            results,
            provider_errors: Vec::new(),
        }
    }

    /// Fully-specified result for the sort tests.
    fn sortable(hash: &str, title: &str, size: Option<u64>, seeders: u64, l: u64) -> SearchResult {
        SearchResult {
            title: title.to_string(),
            info_hash: hash.to_string(),
            size_bytes: size,
            seeders,
            leechers: l,
            source: crate::search::SourceSet {
                apibay: true,
                torrents_csv: false,
            },
        }
    }

    fn app_with_sortable_results() -> App {
        let mut app = App::new();
        app.search.input = "q".to_string();
        let (_, generation) = app.fire_search().expect("fires");
        app.apply_search_outcome(outcome(
            generation,
            vec![
                sortable("h1", "banana", Some(10), 5, 9),
                sortable("h2", "Apple", None, 50, 1),
                sortable("h3", "cherry", Some(999), 20, 4),
            ],
        ));
        app
    }

    #[test]
    fn results_default_to_seeders_descending_and_tab_cycles_all_columns() {
        let mut app = app_with_sortable_results();
        assert_eq!(app.search.sort_column, ResultSortColumn::Seeders);
        assert!(app.result_sort_descending());
        let seeders: Vec<u64> = app.search.results.iter().map(|r| r.seeders).collect();
        assert_eq!(seeders, vec![50, 20, 5]);

        app.cycle_result_sort();
        assert_eq!(app.search.sort_column, ResultSortColumn::Size);
        app.cycle_result_sort();
        assert_eq!(app.search.sort_column, ResultSortColumn::Title);
        app.cycle_result_sort();
        assert_eq!(app.search.sort_column, ResultSortColumn::Leechers);
        app.cycle_result_sort();
        assert_eq!(app.search.sort_column, ResultSortColumn::Seeders);
    }

    #[test]
    fn size_sort_descends_by_default_with_unknown_sizes_last() {
        let mut app = app_with_sortable_results();
        app.cycle_result_sort(); // Seeders -> Size
        let hashes: Vec<&str> = app
            .search
            .results
            .iter()
            .map(|r| r.info_hash.as_str())
            .collect();
        // 999, 10, then the unknown ("?") size at the bottom.
        assert_eq!(hashes, vec!["h3", "h1", "h2"]);
    }

    #[test]
    fn title_sort_is_case_insensitive_ascending_until_reversed() {
        let mut app = app_with_sortable_results();
        app.cycle_result_sort();
        app.cycle_result_sort(); // -> Title, natural ascending
        assert!(!app.result_sort_descending());
        let titles: Vec<&str> = app
            .search
            .results
            .iter()
            .map(|r| r.title.as_str())
            .collect();
        assert_eq!(titles, vec!["Apple", "banana", "cherry"]);

        app.reverse_result_sort();
        assert!(app.result_sort_descending());
        let titles: Vec<&str> = app
            .search
            .results
            .iter()
            .map(|r| r.title.as_str())
            .collect();
        assert_eq!(titles, vec!["cherry", "banana", "Apple"]);
    }

    #[test]
    fn reverse_flips_the_natural_direction_and_cycling_resets_it() {
        let mut app = app_with_sortable_results();
        app.reverse_result_sort(); // Seeders ascending now
        assert!(!app.result_sort_descending());
        let seeders: Vec<u64> = app.search.results.iter().map(|r| r.seeders).collect();
        assert_eq!(seeders, vec![5, 20, 50]);
        // Moving to another column lands in ITS natural direction, not the
        // reversed state left behind.
        app.cycle_result_sort();
        assert!(app.result_sort_descending());
    }

    #[test]
    fn sort_keys_are_inert_while_in_flight_or_without_results() {
        // Matches the registry rows' `has_search_result` availability: while
        // the spinner hides the table (or with nothing to sort) Tab/R must
        // not silently mutate the sticky sort.
        let mut app = app_with_sortable_results();
        app.search.in_flight = true;
        app.cycle_result_sort();
        app.reverse_result_sort();
        assert_eq!(app.search.sort_column, ResultSortColumn::Seeders);
        assert!(!app.search.sort_reversed);

        let mut empty = App::new();
        empty.cycle_result_sort();
        empty.reverse_result_sort();
        assert_eq!(empty.search.sort_column, ResultSortColumn::Seeders);
        assert!(!empty.search.sort_reversed);
    }

    #[test]
    fn cursor_follows_the_selected_result_across_a_resort() {
        let mut app = app_with_sortable_results();
        // Select "banana" (h1): seeders order is h2, h3, h1 -> index 2.
        app.search_next();
        app.search_next();
        assert_eq!(app.selected_search_result().unwrap().info_hash, "h1");
        app.cycle_result_sort();
        app.cycle_result_sort(); // Title asc: Apple, banana, cherry
        assert_eq!(app.selected_search_result().unwrap().info_hash, "h1");
        assert_eq!(app.search.selected, 1);
        assert_eq!(app.search.table_state.selected(), Some(1));
    }

    #[test]
    fn a_new_outcome_honors_the_sticky_sort_and_resets_the_cursor() {
        let mut app = app_with_sortable_results();
        app.cycle_result_sort();
        app.cycle_result_sort(); // Title asc, sticky
        app.search_next();
        app.search.input = "q2".to_string();
        let (_, generation) = app.fire_search().expect("fires");
        app.apply_search_outcome(outcome(
            generation,
            vec![
                sortable("n1", "zebra", None, 1, 0),
                sortable("n2", "aardvark", None, 2, 0),
            ],
        ));
        let titles: Vec<&str> = app
            .search
            .results
            .iter()
            .map(|r| r.title.as_str())
            .collect();
        assert_eq!(titles, vec!["aardvark", "zebra"]);
        assert_eq!(app.search.selected, 0);
        assert_eq!(app.search.table_state.selected(), Some(0));
    }

    #[test]
    fn fire_search_ignores_empty_and_whitespace_queries() {
        let mut app = App::new();
        assert!(app.fire_search().is_none());
        app.search.input = "   ".to_string();
        assert!(app.fire_search().is_none());
        assert!(!app.search.in_flight);
        assert_eq!(app.search.generation, 0);
    }

    #[test]
    fn fire_search_trims_bumps_generation_and_arms_flight() {
        let mut app = App::new();
        app.search.input = "  arch linux  ".to_string();
        let (query, generation) = app.fire_search().expect("should fire");
        assert_eq!(query, "arch linux");
        assert_eq!(generation, 1);
        assert!(app.search.in_flight);
        assert_eq!(app.search.query, "arch linux");
        // The edit buffer keeps the raw text for refining.
        assert_eq!(app.search.input, "  arch linux  ");
    }

    #[test]
    fn stale_generation_outcome_is_dropped_untouched() {
        let mut app = App::new();
        app.search.input = "first".to_string();
        app.fire_search();
        app.search.input = "second".to_string();
        app.fire_search();
        // The gen-1 response arrives after gen-2 fired: dropped, still waiting.
        let hash = "88066b90278f2de655ee2dd44e784c340b54e45c";
        assert!(!app.apply_search_outcome(outcome(1, vec![search_result(hash, 5)])));
        assert!(app.search.results.is_empty());
        assert!(app.search.in_flight);
        assert!(!app.search.searched_once);
        // The current generation's response applies.
        app.mode = AppMode::SearchResults;
        assert!(app.apply_search_outcome(outcome(2, vec![search_result(hash, 5)])));
        assert_eq!(app.search.results.len(), 1);
        assert!(!app.search.in_flight);
        assert!(app.search.searched_once);
    }

    #[test]
    fn outcome_arriving_outside_search_views_is_stored_silently() {
        let mut app = App::new();
        app.search.input = "q".to_string();
        app.fire_search();
        app.mode = AppMode::Normal;
        let hash = "88066b90278f2de655ee2dd44e784c340b54e45c";
        // Applied (stored for when the user returns) but no repaint requested.
        assert!(!app.apply_search_outcome(outcome(1, vec![search_result(hash, 5)])));
        assert_eq!(app.search.results.len(), 1);
        assert!(!app.search.in_flight);
    }

    #[test]
    fn apply_outcome_resets_selection() {
        let mut app = App::new();
        app.mode = AppMode::SearchResults;
        app.search.input = "q".to_string();
        app.fire_search();
        app.search.selected = 7;
        let hash = "88066b90278f2de655ee2dd44e784c340b54e45c";
        app.apply_search_outcome(outcome(1, vec![search_result(hash, 5)]));
        assert_eq!(app.search.selected, 0);
        assert_eq!(app.search.table_state.selected(), Some(0));
    }

    #[test]
    fn refire_search_reuses_the_fired_query_not_the_buffer() {
        let mut app = App::new();
        assert!(app.refire_search().is_none()); // nothing fired yet
        app.search.input = "arch".to_string();
        app.fire_search();
        app.search.input = "edited but not fired".to_string();
        let (query, generation) = app.refire_search().expect("should refire");
        assert_eq!(query, "arch");
        assert_eq!(generation, 2);
        assert!(app.search.in_flight);
    }

    #[test]
    fn search_navigation_clamps_at_both_ends_and_on_empty() {
        let mut app = App::new();
        app.search_next();
        app.search_previous();
        assert_eq!(app.search.selected, 0);
        let h1 = "88066b90278f2de655ee2dd44e784c340b54e45c";
        let h2 = "22b8f63218f1e726ec2f1fb9b38239f95fc6a629";
        app.search.results = vec![search_result(h1, 1), search_result(h2, 2)];
        app.search_next();
        app.search_next();
        app.search_next();
        assert_eq!(app.search.selected, 1);
        app.search_previous();
        app.search_previous();
        app.search_previous();
        assert_eq!(app.search.selected, 0);
    }

    #[test]
    fn search_push_char_filters_controls_and_caps_length() {
        let mut app = App::new();
        app.search_push_char('a');
        app.search_push_char('\u{1b}');
        // Bidi controls are rejected too: the query renders raw in the search
        // bar and results title, where a pasted U+202E would reorder the line.
        app.search_push_char('\u{202E}');
        app.search_push_char('\u{200F}');
        app.search_push_char('b');
        assert_eq!(app.search.input, "ab");
        app.search.input = "x".repeat(crate::search::MAX_QUERY_CHARS);
        app.search_push_char('y');
        assert_eq!(
            app.search.input.chars().count(),
            crate::search::MAX_QUERY_CHARS
        );
    }

    #[test]
    fn torrent_in_session_matches_hashes_case_insensitively() {
        let mut app = App::new();
        let mut t = make_torrent(0, "t", 1, TorrentStatus::Downloading);
        t.info_hash = "88066B90278F2DE655EE2DD44E784C340B54E45C".to_string();
        app.handle_state_push(vec![t]);
        assert!(app.torrent_in_session("88066b90278f2de655ee2dd44e784c340b54e45c"));
        assert!(!app.torrent_in_session("22b8f63218f1e726ec2f1fb9b38239f95fc6a629"));
    }

    #[test]
    fn palette_opens_only_over_non_input_modes_and_returns_there() {
        let mut app = App::new();
        for mode in [AppMode::Normal, AppMode::Detail, AppMode::SearchResults] {
            app.mode = mode.clone();
            assert!(app.open_palette(), "{mode:?} should host the palette");
            assert_eq!(app.mode, AppMode::Palette);
            assert_eq!(app.palette.return_mode, mode);
            app.close_palette();
            assert_eq!(app.mode, mode);
        }
        for mode in [
            AppMode::Input,
            AppMode::Search,
            AppMode::Filter,
            AppMode::Help,
        ] {
            app.mode = mode.clone();
            assert!(!app.open_palette(), "{mode:?} must refuse the palette");
            assert_eq!(app.mode, mode, "refusal must not change mode");
        }
    }

    #[test]
    fn palette_matches_respect_scope_availability_and_query() {
        let mut app = App::new();
        app.mode = AppMode::Normal;
        app.open_palette();
        let names: Vec<&str> = app
            .palette_matches()
            .iter()
            .map(|a| crate::actions::tui_description(a))
            .collect();
        // Normal scope, empty session: add/search always there, delete and
        // pause hidden (nothing to act on), detail actions absent entirely.
        assert!(names.contains(&"Add magnet link or .torrent file"));
        assert!(names.contains(&"Search torrent indexers"));
        assert!(!names.iter().any(|n| n.contains("Delete")));
        assert!(!names.iter().any(|n| n.contains("Cycle detail tabs")));

        // With a torrent, the selection-scoped actions appear.
        app.handle_state_push(vec![make_torrent(0, "t", 10, TorrentStatus::Downloading)]);
        let names: Vec<&str> = app
            .palette_matches()
            .iter()
            .map(|a| crate::actions::tui_description(a))
            .collect();
        assert!(names.iter().any(|n| n.contains("Delete")));

        // The fuzzy query narrows the list and ranks the word-start match
        // first ("delete" is also a scattered subsequence of "Add magnet
        // link or .torrent file" — subsequences are allowed, ranking is what
        // makes the palette feel right).
        for c in "delete".chars() {
            app.palette_push_char(c);
        }
        let matches = app.palette_matches();
        assert!(!matches.is_empty());
        assert!(crate::actions::tui_description(matches[0]).contains("Delete"));
    }

    #[test]
    fn palette_from_detail_offers_detail_actions_only() {
        let mut app = App::new();
        app.handle_state_push(vec![make_torrent(0, "t", 10, TorrentStatus::Downloading)]);
        app.mode = AppMode::Detail;
        app.open_palette();
        let names: Vec<&str> = app
            .palette_matches()
            .iter()
            .map(|a| crate::actions::tui_description(a))
            .collect();
        assert!(names.contains(&"Cycle detail tabs"));
        assert!(names.contains(&"Back to the torrent list"));
        assert!(!names.contains(&"Add magnet link or .torrent file"));
        // File actions need the Files tab with files — hidden on Stats.
        assert!(!names.iter().any(|n| n.contains("Stream")));
    }

    #[test]
    fn palette_input_filters_hostile_chars_and_resets_selection() {
        let mut app = App::new();
        app.mode = AppMode::Normal;
        app.open_palette();
        app.palette_next();
        assert!(app.palette.anchor.is_some());
        app.palette_push_char('\u{1b}');
        app.palette_push_char('\u{202E}');
        assert_eq!(app.palette.input, "");
        app.palette_push_char('a');
        assert_eq!(app.palette.input, "a");
        assert!(app.palette.anchor.is_none(), "edits reset the cursor");
        assert_eq!(app.palette_selected_index(&app.palette_matches()), 0);
    }

    #[test]
    fn palette_cursor_follows_the_anchored_action_when_the_list_shifts() {
        // The misfire the review caught: rows appearing above the cursor
        // (an availability change from a background event) must not change
        // which action the cursor is on.
        let mut app = App::new();
        app.mode = AppMode::SearchResults;
        app.search.in_flight = true; // hides Download/Retry rows
        app.search.searched_once = true;
        app.search.query = "q".to_string();
        app.open_palette();
        app.palette_next(); // anchor the second visible action
        let anchored = app.palette.anchor.expect("anchored");
        let before = app.palette_selected_index(&app.palette_matches());

        // The outcome lands while the palette is open: new rows appear.
        app.search.in_flight = false;
        app.search.results = vec![search_result("88066b90278f2de655ee2dd44e784c340b54e45c", 1)];
        let matches = app.palette_matches();
        assert!(matches.len() > 2, "rows should have appeared");
        let after = app.palette_selected_index(&matches);
        assert!(
            std::ptr::eq(matches[after], anchored),
            "cursor must still be on the anchored action"
        );
        assert_ne!(
            before, after,
            "its index moved — identity tracking did the work"
        );
    }

    #[test]
    fn search_spinner_animates_under_the_palette_too() {
        let mut app = App::new();
        app.search.in_flight = true;
        app.mode = AppMode::SearchResults;
        app.open_palette();
        assert!(app.search_spinner_active());
        // And an arriving outcome still requests a repaint.
        let outcome = SearchOutcome {
            generation: 0,
            results: Vec::new(),
            provider_errors: Vec::new(),
        };
        assert!(app.apply_search_outcome(outcome));
    }

    #[test]
    fn search_spinner_only_animates_in_search_views() {
        let mut app = App::new();
        app.search.in_flight = true;
        app.mode = AppMode::Normal;
        assert!(!app.search_spinner_active());
        app.mode = AppMode::SearchResults;
        assert!(app.search_spinner_active());
        app.mode = AppMode::Search;
        assert!(app.search_spinner_active());
        app.search.in_flight = false;
        assert!(!app.search_spinner_active());
    }
}
