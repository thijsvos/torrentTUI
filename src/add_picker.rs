//! The add picker (`A`): choose a torrent's files and save folder before it
//! starts downloading.
//!
//! The flow is two engine round trips. `PreviewTorrent` asks librqbit for the
//! file list without adding anything (`list_only`); while it resolves the
//! picker is [`AddPickerState::Resolving`]. The answer opens
//! [`AddPickerState::Ready`], and confirming sends `AddPrepared` with the
//! chosen files and folder — from the bytes the preview kept, so a magnet's
//! metadata is fetched once.
//!
//! Everything here is state and rules with no terminal and no engine, so it
//! is tested directly; `ui::add_picker` draws it and `main` routes keys to it.
//! The only filesystem access is in [`list_subdirs`] (Tab completion) and in
//! [`Picker::confirm`]'s last-moment check, both on an explicit keypress.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ratatui::widgets::TableState;

use crate::actions::fuzzy_score;
use crate::config::expand_tilde;
use crate::file_tree::{Check, FileTree, NodeId, TreeRow};
use crate::types::{PreparedTorrent, PreviewFile, TorrentPreview};
use crate::ui::input::InputWidget;

/// Cap on the filter query; file paths are bounded, so a longer needle can't
/// match anything (mirrors the torrent table's `MAX_FILTER_CHARS`).
const MAX_FILTER_CHARS: usize = 200;

/// Cap on the folder field — PATH_MAX on Linux.
const MAX_FOLDER_CHARS: usize = 4096;

/// How long the folder must sit unchanged before its free space is probed,
/// so typing a path does not queue one `statvfs` per keystroke.
const SPACE_PROBE_DELAY: Duration = Duration::from_millis(300);

/// Directory entries read per Tab press. A completion over more than this is
/// not one anybody is reading through.
const MAX_COMPLETION_ENTRIES: usize = 5000;

/// How many completion candidates the message line lists before "…".
const MAX_SHOWN_CANDIDATES: usize = 8;

const NOTHING_SELECTED: &str = "Select at least one file";

/// Where the picker is in its life.
pub enum AddPickerState {
    /// The engine is fetching the file list.
    Resolving {
        ticket: u64,
        label: String,
        since: Instant,
    },
    Ready(Box<Picker>),
}

/// Which part of the picker takes typing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Filter,
    Folder,
}

/// How the file list is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// One row per file, full paths.
    Flat,
    /// Folders that open and close; one row can stand for a whole season.
    Tree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerSort {
    /// The order the torrent lists its files in.
    TorrentOrder,
    /// Largest first: what usually decides what to skip.
    Size,
    Name,
}

impl PickerSort {
    fn next(self) -> Self {
        match self {
            Self::TorrentOrder => Self::Size,
            Self::Size => Self::Name,
            Self::Name => Self::TorrentOrder,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::TorrentOrder => "torrent order",
            Self::Size => "size",
            Self::Name => "name",
        }
    }
}

/// What the picker hands back when the user confirms.
#[derive(Debug)]
pub struct Confirmed {
    pub prepared: PreparedTorrent,
    /// librqbit file indices.
    pub only_files: Vec<usize>,
    /// Where the torrent's files go: the chosen folder plus the torrent's own
    /// subfolder, as librqbit would have laid it out in the download dir.
    pub output_folder: PathBuf,
    /// The folder as typed, before the subfolder is joined — what goes into
    /// the recent-folders list, so the same base can be picked again.
    pub folder_typed: String,
}

/// The free-space reading for one folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderProbe {
    /// The folder as resolved (tilde expanded, relative joined onto the
    /// download dir) — what the result is matched against.
    pub folder: PathBuf,
    /// Free bytes on the volume of the nearest existing ancestor.
    pub free: Option<u64>,
    pub exists: bool,
    /// Something is there and it is not a directory.
    pub not_a_dir: bool,
}

/// Probe a folder for the picker. Blocking (`statvfs` and `stat`), so callers
/// run it off the reactor: on a hung network mount it blocks for the mount
/// timeout.
pub fn probe_folder(folder: PathBuf) -> FolderProbe {
    let exists = folder.exists();
    let not_a_dir = blocking_file(&folder).is_some();
    // A folder that does not exist yet will be created on the volume of
    // its nearest existing ancestor, so that is the volume to measure.
    let free = folder
        .ancestors()
        .find(|p| p.is_dir())
        .and_then(|p| fs4::available_space(p).ok());
    FolderProbe {
        folder,
        free,
        exists,
        not_a_dir,
    }
}

pub struct Picker {
    pub name: String,
    pub files: Vec<PreviewFile>,
    /// Parallel to `files`.
    selected: Vec<bool>,
    /// What the picker opened with (`r` goes back to it).
    initial: Vec<bool>,
    pub filter: InputWidget,
    pub folder: InputWidget,
    pub focus: Focus,
    pub sort: PickerSort,
    pub table_state: TableState,
    /// Rows a page key moves; the renderer sets it to the visible height.
    pub page_size: usize,
    /// Positions in `files`, in display order: filtered and sorted.
    visible: Vec<usize>,
    /// `visible` as a set while a filter is active: what the tree shows and
    /// what folder keys act on.
    matches: Option<HashSet<usize>>,
    /// The filter fell back to fuzzy matching (nothing contains the text).
    pub fuzzy: bool,
    pub view: View,
    pub tree: FileTree,
    /// Open folders in the tree view.
    expanded: HashSet<NodeId>,
    /// The tree as currently shown.
    tree_rows: Vec<TreeRow>,
    /// The tree row under the cursor, by node — stable across re-filtering
    /// for the same reason `anchor` is.
    tree_anchor: Option<NodeId>,
    /// Recent save folders, newest first, for `↑`/`↓` in the folder field.
    recents: Vec<String>,
    /// Which recent folder the field shows, while stepping through them.
    recent_at: Option<usize>,
    /// What was typed before `↑` started stepping, for `↓` to come back to.
    draft: String,
    /// Position in `files` of the row under the cursor. Tracked by item, not
    /// row number, so re-filtering never slides another file under it.
    anchor: Option<usize>,
    subfolder: Option<PathBuf>,
    download_dir: String,
    /// Taken by `confirm`.
    prepared: Option<PreparedTorrent>,
    /// The newest free-space reading, for whichever folder it was taken for.
    pub probe: Option<FolderProbe>,
    /// When the folder last changed; `None` once it has been probed.
    folder_changed_at: Option<Instant>,
    /// A one-line note: a refusal, or Tab-completion candidates.
    pub message: Option<String>,
}

impl Picker {
    pub fn new(preview: TorrentPreview, download_dir: &str, recents: Vec<String>) -> Self {
        let selected: Vec<bool> = match &preview.preselected {
            Some(wanted) => preview
                .files
                .iter()
                .map(|f| wanted.contains(&f.index))
                .collect(),
            None => vec![true; preview.files.len()],
        };
        let mut folder = InputWidget::with_cap(MAX_FOLDER_CHARS);
        folder.set(download_dir);
        let tree = FileTree::build(&preview.files);
        // Top-level folders open, deeper ones closed: a season pack shows its
        // seasons and their episodes, a huge archive does not unfold at once.
        let expanded: HashSet<NodeId> = tree.root_dirs().collect();
        let view = if tree.has_folders() {
            View::Tree
        } else {
            View::Flat
        };
        let mut picker = Self {
            name: preview.name,
            initial: selected.clone(),
            selected,
            files: preview.files,
            filter: InputWidget::with_cap(MAX_FILTER_CHARS),
            folder,
            focus: Focus::List,
            sort: PickerSort::TorrentOrder,
            table_state: TableState::default(),
            page_size: 10,
            visible: Vec::new(),
            matches: None,
            fuzzy: false,
            view,
            tree,
            expanded,
            tree_rows: Vec::new(),
            tree_anchor: None,
            recents,
            recent_at: None,
            draft: String::new(),
            anchor: None,
            subfolder: preview.subfolder,
            download_dir: download_dir.to_string(),
            prepared: Some(preview.prepared),
            probe: None,
            // Due at once: the first reading should not wait for a keypress.
            folder_changed_at: Instant::now().checked_sub(SPACE_PROBE_DELAY),
            message: None,
        };
        picker.refresh_visible();
        picker
    }

    // -- the list -------------------------------------------------------

    /// Positions in `files`, in display order.
    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    pub fn is_selected(&self, pos: usize) -> bool {
        self.selected.get(pos).copied().unwrap_or(false)
    }

    /// Recompute the filtered, sorted order and put the cursor back on the
    /// file it was on, or on the first row if that file is filtered out.
    ///
    /// The filter is a plain "contains" match first, and only falls back to
    /// fuzzy matching when nothing contains the text. Fuzzy-only would be
    /// wrong here because `a`/`n`/`i` act on whatever is shown: "season 2"
    /// fuzzy-matches `Season 1/e02.mkv` (the `2` of `e02`), so selecting a
    /// season would quietly take an episode of another one with it.
    fn refresh_visible(&mut self) {
        let query = self.filter.as_str().trim();
        let files = &self.files;
        let mut order: Vec<usize> = (0..files.len()).collect();
        self.fuzzy = false;
        if !query.is_empty() {
            let needle = query.to_lowercase();
            order.retain(|&i| files[i].path.to_lowercase().contains(&needle));
        }
        if !query.is_empty() && order.is_empty() {
            // Ranked by match quality; the torrent order breaks ties.
            let mut scored: Vec<(u32, usize)> = (0..files.len())
                .filter_map(|i| fuzzy_score(query, &files[i].path).map(|s| (s, i)))
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            order = scored.into_iter().map(|(_, i)| i).collect();
            self.fuzzy = !order.is_empty();
        } else {
            match self.sort {
                PickerSort::TorrentOrder => {}
                PickerSort::Size => order.sort_by(|&a, &b| files[b].size.cmp(&files[a].size)),
                PickerSort::Name => order.sort_by_cached_key(|&i| files[i].path.to_lowercase()),
            }
        }
        self.visible = order;
        self.matches = (!query.is_empty()).then(|| self.visible.iter().copied().collect());
        if !self.anchor.is_some_and(|a| self.visible.contains(&a)) {
            self.anchor = self.visible.first().copied();
        }
        self.refresh_tree_rows();
    }

    fn refresh_tree_rows(&mut self) {
        self.tree_rows = self
            .tree
            .rows(&self.expanded, self.matches.as_ref(), self.sort);
        if !self
            .tree_anchor
            .is_some_and(|a| self.tree_rows.iter().any(|r| r.node == a))
        {
            self.tree_anchor = self.tree_rows.first().map(|r| r.node);
        }
        self.sync_table_state();
    }

    pub fn tree_rows(&self) -> &[TreeRow] {
        &self.tree_rows
    }

    /// A folder's checkbox in the tree, for the files the filter shows.
    pub fn check(&self, node: NodeId) -> Check {
        self.tree.check(node, &self.selected, self.matches.as_ref())
    }

    pub fn is_expanded(&self, node: NodeId) -> bool {
        self.matches.is_some() || self.expanded.contains(&node)
    }

    /// `t`: switch between the flat list and the tree, keeping the cursor on
    /// the same file (or, coming from a folder, on its first file shown).
    pub fn toggle_view(&mut self) {
        match self.view {
            View::Flat => {
                self.view = View::Tree;
                if let Some(leaf) = self.anchor.and_then(|pos| self.tree.leaf(pos)) {
                    // Open the way to it, or the cursor would land on a
                    // hidden row.
                    let mut at = self.tree.node(leaf).parent;
                    while let Some(dir) = at {
                        self.expanded.insert(dir);
                        at = self.tree.node(dir).parent;
                    }
                    self.tree_anchor = Some(leaf);
                }
                self.refresh_tree_rows();
            }
            View::Tree => {
                self.view = View::Flat;
                if let Some(node) = self.tree_anchor {
                    let visible = &self.visible;
                    if let Some(&pos) = self
                        .tree
                        .node(node)
                        .files
                        .iter()
                        .find(|p| visible.contains(p))
                    {
                        self.anchor = Some(pos);
                    }
                }
                self.sync_table_state();
            }
        }
    }

    /// `l`/`→`: open the folder under the cursor.
    pub fn expand_current(&mut self) {
        if let Some(node) = self.tree_folder_under_cursor() {
            self.expanded.insert(node);
            self.refresh_tree_rows();
        }
    }

    /// `h`/`←`: close the folder under the cursor, or step out to the folder
    /// containing it.
    pub fn collapse_current(&mut self) {
        if self.view != View::Tree {
            return;
        }
        let Some(node) = self.tree_anchor else {
            return;
        };
        // While filtering every folder is shown open, so there is nothing to
        // close; stepping out still works.
        if self.matches.is_none() && self.tree.node(node).is_dir() && self.expanded.remove(&node) {
            self.refresh_tree_rows();
            return;
        }
        if let Some(parent) = self.tree.node(node).parent {
            self.tree_anchor = Some(parent);
            self.sync_table_state();
        }
    }

    fn tree_folder_under_cursor(&self) -> Option<NodeId> {
        if self.view != View::Tree {
            return None;
        }
        self.tree_anchor.filter(|&n| self.tree.node(n).is_dir())
    }

    fn cursor_row(&self) -> Option<usize> {
        match self.view {
            View::Flat => {
                let anchor = self.anchor?;
                self.visible.iter().position(|&i| i == anchor)
            }
            View::Tree => {
                let anchor = self.tree_anchor?;
                self.tree_rows.iter().position(|r| r.node == anchor)
            }
        }
    }

    fn row_count(&self) -> usize {
        match self.view {
            View::Flat => self.visible.len(),
            View::Tree => self.tree_rows.len(),
        }
    }

    fn sync_table_state(&mut self) {
        let row = self.cursor_row();
        self.table_state.select(row);
    }

    /// Position in `files` under the cursor.
    #[cfg(test)]
    fn current(&self) -> Option<usize> {
        self.anchor
    }

    fn move_to_row(&mut self, row: usize) {
        match self.view {
            View::Flat => {
                if let Some(&pos) = self.visible.get(row) {
                    self.anchor = Some(pos);
                }
            }
            View::Tree => {
                if let Some(r) = self.tree_rows.get(row) {
                    self.tree_anchor = Some(r.node);
                    // Keep the flat cursor alongside, for a later `t`.
                    if let Some(pos) = self.tree.node(r.node).file {
                        self.anchor = Some(pos);
                    }
                }
            }
        }
        self.sync_table_state();
    }

    /// Move the cursor by `delta` rows, clamped to the list.
    pub fn move_by(&mut self, delta: isize) {
        let count = self.row_count();
        if count == 0 {
            return;
        }
        let row = self.cursor_row().unwrap_or(0) as isize;
        self.move_to_row((row + delta).clamp(0, count as isize - 1) as usize);
    }

    pub fn page(&mut self, pages: isize) {
        self.move_by(pages * self.page_size.max(1) as isize);
    }

    pub fn top(&mut self) {
        self.move_to_row(0);
    }

    pub fn bottom(&mut self) {
        self.move_to_row(self.row_count().saturating_sub(1));
    }

    /// Space. On a folder: select every file under it (that the filter
    /// shows), or deselect them all when they already are.
    pub fn toggle_current(&mut self) {
        if self.view == View::Tree {
            let Some(node) = self.tree_anchor else {
                return;
            };
            let files = self.tree.files_in_view(node, self.matches.as_ref());
            let on = !files.iter().all(|&f| self.selected[f]);
            for f in files {
                self.selected[f] = on;
            }
            return;
        }
        if let Some(flag) = self.anchor.and_then(|pos| self.selected.get_mut(pos)) {
            *flag = !*flag;
        }
    }

    /// `a` / `n`: select or deselect every file the filter shows — every
    /// file when there is no filter. Filter `season 2`, press `a`, and that
    /// season is in.
    pub fn set_visible(&mut self, on: bool) {
        for &pos in &self.visible {
            self.selected[pos] = on;
        }
    }

    /// `i`: invert the files the filter shows.
    pub fn invert_visible(&mut self) {
        for &pos in &self.visible {
            self.selected[pos] = !self.selected[pos];
        }
    }

    /// `r`: back to what the picker opened with.
    pub fn restore(&mut self) {
        self.selected = self.initial.clone();
    }

    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        self.refresh_visible();
    }

    pub fn selected_count(&self) -> usize {
        self.selected.iter().filter(|&&on| on).count()
    }

    pub fn selected_bytes(&self) -> u64 {
        self.files
            .iter()
            .zip(&self.selected)
            .filter(|(_, &on)| on)
            .map(|(f, _)| f.size)
            .sum()
    }

    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// librqbit indices of the selected files.
    fn only_files(&self) -> Vec<usize> {
        self.files
            .iter()
            .zip(&self.selected)
            .filter(|(_, &on)| on)
            .map(|(f, _)| f.index)
            .collect()
    }

    // -- the filter -----------------------------------------------------

    pub fn filter_push(&mut self, c: char) {
        self.filter.push(c);
        self.refresh_visible();
    }

    pub fn filter_push_str(&mut self, s: &str) {
        self.filter.push_str(s);
        self.refresh_visible();
    }

    pub fn filter_pop(&mut self) {
        self.filter.pop();
        self.refresh_visible();
    }

    pub fn filter_clear(&mut self) {
        self.filter.clear();
        self.refresh_visible();
    }

    // -- the folder -----------------------------------------------------

    fn folder_changed(&mut self) {
        self.folder_changed_at = Some(Instant::now());
        self.message = None;
    }

    /// Typing makes the shown folder the user's own text again.
    fn folder_edited(&mut self) {
        self.recent_at = None;
        self.folder_changed();
    }

    pub fn folder_push(&mut self, c: char) {
        self.folder.push(c);
        self.folder_edited();
    }

    pub fn folder_push_str(&mut self, s: &str) {
        self.folder.push_str(s);
        self.folder_edited();
    }

    pub fn folder_pop(&mut self) {
        self.folder.pop();
        self.folder_edited();
    }

    pub fn folder_clear(&mut self) {
        self.folder.clear();
        self.folder_edited();
    }

    /// Replace the folder outright, as a completion does.
    pub fn folder_set(&mut self, s: &str) {
        self.folder.set(s);
        self.folder_edited();
    }

    /// `↑` in the folder field: the next older recent folder.
    pub fn recent_older(&mut self) {
        let next = self.recent_at.map_or(0, |i| i + 1);
        let Some(dir) = self.recents.get(next).cloned() else {
            return;
        };
        if self.recent_at.is_none() {
            self.draft = self.folder.value();
        }
        self.recent_at = Some(next);
        self.folder.set(&dir);
        self.folder_changed();
    }

    /// `↓`: the next newer one, and after the newest, back to what was typed.
    pub fn recent_newer(&mut self) {
        match self.recent_at {
            None => {}
            Some(0) => {
                self.recent_at = None;
                let draft = std::mem::take(&mut self.draft);
                self.folder.set(&draft);
                self.folder_changed();
            }
            Some(i) => {
                self.recent_at = Some(i - 1);
                let dir = self.recents[i - 1].clone();
                self.folder.set(&dir);
                self.folder_changed();
            }
        }
    }

    pub fn has_recents(&self) -> bool {
        !self.recents.is_empty()
    }

    /// `(n, of)` while the field shows the `n`th recent folder, 1-based.
    pub fn recent_position(&self) -> Option<(usize, usize)> {
        self.recent_at.map(|i| (i + 1, self.recents.len()))
    }

    /// The chosen folder, resolved: `~` expanded, and a relative path taken
    /// as relative to the download dir — "movies" means a `movies` folder in
    /// the usual place, not wherever the process happened to start.
    pub fn folder_base(&self) -> Result<PathBuf, String> {
        resolve_folder(self.folder.as_str(), &self.download_dir)
    }

    /// Where the files will actually land: the chosen folder plus the
    /// torrent's own subfolder, the same layout the download dir would get.
    pub fn output_folder(&self) -> Result<PathBuf, String> {
        let base = self.folder_base()?;
        Ok(match &self.subfolder {
            Some(sub) => base.join(sub),
            None => base,
        })
    }

    /// The folder to probe, once it has sat unchanged long enough.
    pub fn probe_due(&mut self, now: Instant) -> Option<PathBuf> {
        let changed = self.folder_changed_at?;
        if now.duration_since(changed) < SPACE_PROBE_DELAY {
            return None;
        }
        self.folder_changed_at = None;
        self.folder_base().ok()
    }

    /// Take a probe result, unless the folder has moved on since.
    pub fn apply_probe(&mut self, probe: FolderProbe) -> bool {
        if self.folder_base().ok().as_ref() != Some(&probe.folder) {
            return false;
        }
        self.probe = Some(probe);
        true
    }

    /// The probe, if it is for the folder as it stands now.
    pub fn current_probe(&self) -> Option<&FolderProbe> {
        let base = self.folder_base().ok()?;
        self.probe.as_ref().filter(|p| p.folder == base)
    }

    /// Tab in the folder field.
    pub fn complete_folder(&mut self, list_dirs: impl Fn(&Path) -> io::Result<Vec<String>>) {
        let done = complete_dir(self.folder.as_str(), &self.download_dir, list_dirs);
        if let Some(input) = done.input {
            self.folder_set(&input);
        }
        self.message = match done.candidates.len() {
            0 if done.no_match => Some("No matching folder".to_string()),
            0 | 1 => None,
            n => {
                let mut shown = done.candidates[..n.min(MAX_SHOWN_CANDIDATES)].join("  ");
                if n > MAX_SHOWN_CANDIDATES {
                    shown.push_str("  \u{2026}");
                }
                Some(shown)
            }
        };
    }

    // -- confirming -----------------------------------------------------

    /// Enter: everything the engine needs, or why not (nothing changes on a
    /// refusal, so the user can fix it and press Enter again).
    pub fn confirm(&mut self) -> Result<Confirmed, String> {
        let only_files = self.only_files();
        if only_files.is_empty() {
            return Err(NOTHING_SELECTED.to_string());
        }
        // The whole path, so a file above the chosen folder, or one sitting
        // where the torrent's own subfolder goes, is caught here rather than
        // by a failed add after the picker (and the selection) is gone.
        let output_folder = self.output_folder()?;
        if let Some(file) = blocking_file(&output_folder) {
            return Err(format!("{} is a file, not a folder", file.display()));
        }
        let prepared = self
            .prepared
            .take()
            .ok_or_else(|| "Already added".to_string())?;
        Ok(Confirmed {
            prepared,
            only_files,
            output_folder,
            folder_typed: self.folder.as_str().trim().to_string(),
        })
    }
}

/// The nearest part of `path` that exists, when it is not a folder — so no
/// folder can be created at `path`. `exists()` alone misses a file further
/// up: `notes.txt/tv` just reads as "does not exist yet".
fn blocking_file(path: &Path) -> Option<&Path> {
    path.ancestors()
        .find(|p| p.exists())
        .filter(|p| !p.is_dir())
}

/// `dir` made absolute against the working directory, which is where
/// librqbit puts a relative download dir. The picker resolves relative input
/// against the download dir, so a relative one would be applied twice: the
/// default `downloads` would mean `downloads/downloads`.
pub fn absolute_dir(dir: &str) -> String {
    std::path::absolute(dir)
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok())
        .unwrap_or_else(|| dir.to_string())
}

/// See [`Picker::folder_base`].
fn resolve_folder(input: &str, download_dir: &str) -> Result<PathBuf, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Choose a folder to save into".to_string());
    }
    let path = PathBuf::from(expand_tilde(input));
    Ok(if path.is_absolute() {
        path
    } else {
        Path::new(download_dir).join(path)
    })
}

fn is_separator(c: char) -> bool {
    c == '/' || (cfg!(windows) && c == '\\')
}

/// What a Tab press produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Completion {
    /// The new field contents, when the completion moved it forward.
    pub input: Option<String>,
    /// Every matching name, when there is more than one.
    pub candidates: Vec<String>,
    /// Nothing in that folder starts with what was typed.
    pub no_match: bool,
}

/// Shell-style completion of the last path segment against the directories
/// `list_dirs` reports. A unique match completes with a trailing separator,
/// ready for the next segment; several complete as far as they agree and are
/// returned as candidates. What the user typed before the last segment is
/// kept as typed — `~/Dow` becomes `~/Downloads/`, not `/home/me/Downloads/`.
/// Hidden folders only show up once the segment starts with a `.`.
pub fn complete_dir(
    input: &str,
    download_dir: &str,
    list_dirs: impl Fn(&Path) -> io::Result<Vec<String>>,
) -> Completion {
    let (head, fragment) = match input.rfind(is_separator) {
        Some(i) => input.split_at(i + 1),
        None => ("", input),
    };
    // A bare `~` names a folder, but it is only a prefix once it has its
    // separator.
    if head.is_empty() && fragment == "~" {
        return Completion {
            input: Some(format!("~{}", std::path::MAIN_SEPARATOR)),
            ..Default::default()
        };
    }
    let dir = if head.is_empty() {
        PathBuf::from(download_dir)
    } else {
        match resolve_folder(head, download_dir) {
            Ok(dir) => dir,
            Err(_) => return Completion::default(),
        }
    };
    let Ok(names) = list_dirs(&dir) else {
        return Completion {
            no_match: true,
            ..Default::default()
        };
    };
    let mut matches: Vec<String> = names
        .into_iter()
        .filter(|name| name.starts_with(fragment))
        .filter(|name| !name.starts_with('.') || fragment.starts_with('.'))
        .collect();
    matches.sort();
    matches.dedup();
    match matches.as_slice() {
        [] => Completion {
            no_match: true,
            ..Default::default()
        },
        [only] => Completion {
            input: Some(format!("{head}{only}{}", std::path::MAIN_SEPARATOR)),
            ..Default::default()
        },
        [first, rest @ ..] => {
            let common = rest.iter().fold(first.as_str(), |acc, name| {
                let len = acc
                    .char_indices()
                    .zip(name.chars())
                    .take_while(|((_, a), b)| a == b)
                    .last()
                    .map_or(0, |((i, a), _)| i + a.len_utf8());
                &acc[..len]
            });
            Completion {
                input: (common.len() > fragment.len()).then(|| format!("{head}{common}")),
                candidates: matches.clone(),
                no_match: false,
            }
        }
    }
}

/// The real `list_dirs` for [`complete_dir`]: names of the directories in
/// `dir`, symlinks to directories included. Read on a Tab press, capped at
/// `MAX_COMPLETION_ENTRIES` entries.
pub fn list_subdirs(dir: &Path) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)?
        .take(MAX_COMPLETION_ENTRIES)
        .flatten()
    {
        let is_dir = entry
            .file_type()
            .is_ok_and(|t| t.is_dir() || (t.is_symlink() && entry.path().is_dir()));
        if is_dir {
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(index: usize, path: &str, size: u64) -> PreviewFile {
        PreviewFile {
            index,
            components: path.split('/').map(str::to_string).collect(),
            path: path.to_string(),
            size,
        }
    }

    fn prepared() -> PreparedTorrent {
        PreparedTorrent {
            bytes: vec![1, 2, 3],
            initial_peers: Vec::new(),
            stripped_udp: 0,
        }
    }

    fn preview(files: Vec<PreviewFile>) -> TorrentPreview {
        TorrentPreview {
            name: "Show".to_string(),
            files,
            preselected: None,
            subfolder: Some(PathBuf::from("Show")),
            prepared: prepared(),
        }
    }

    /// Opens in the tree, since the files sit in folders.
    fn tree_picker() -> Picker {
        Picker::new(
            preview(vec![
                file(0, "Season 1/e01.mkv", 300),
                file(1, "Season 1/e02.mkv", 100),
                file(2, "Season 2/e01.mkv", 500),
                // Index 3 was a padding file the engine left out.
                file(4, "notes.txt", 1),
            ]),
            "/dl",
            Vec::new(),
        )
    }

    /// The same files in the flat list, where most rules are easiest to see.
    fn picker() -> Picker {
        let mut p = tree_picker();
        assert_eq!(p.view, View::Tree);
        p.toggle_view();
        p.top();
        p
    }

    fn tree_labels(p: &Picker) -> Vec<String> {
        p.tree_rows()
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), p.tree.node(r.node).name))
            .collect()
    }

    fn cursor_label(p: &Picker) -> String {
        let row = p.table_state.selected().expect("a cursor");
        tree_labels(p)[row].trim().to_string()
    }

    #[test]
    fn opens_in_the_tree_with_top_level_folders_open() {
        let p = tree_picker();
        assert_eq!(
            tree_labels(&p),
            vec![
                "Season 1",
                "  e01.mkv",
                "  e02.mkv",
                "Season 2",
                "  e01.mkv",
                "notes.txt"
            ]
        );
        let flat = Picker::new(preview(vec![file(0, "a.iso", 1)]), "/dl", Vec::new());
        assert_eq!(flat.view, View::Flat, "nothing to fold");
    }

    #[test]
    fn space_on_a_folder_takes_the_whole_folder_and_then_releases_it() {
        let mut p = tree_picker();
        assert_eq!(cursor_label(&p), "Season 1");
        p.toggle_current();
        assert_eq!(p.only_files(), vec![2, 4], "all of season 1 off");
        p.move_by(1);
        p.toggle_current(); // e01 back on
        p.move_by(-1);
        assert_eq!(p.check(p.tree_rows()[0].node), Check::Some);
        p.toggle_current(); // part-selected folder: select it all
        assert_eq!(p.only_files(), vec![0, 1, 2, 4]);
    }

    #[test]
    fn folder_keys_open_close_and_step_out() {
        let mut p = tree_picker();
        p.collapse_current(); // close Season 1
        assert_eq!(
            tree_labels(&p),
            vec!["Season 1", "Season 2", "  e01.mkv", "notes.txt"]
        );
        p.expand_current();
        p.move_by(2); // Season 1/e02.mkv
        assert_eq!(cursor_label(&p), "e02.mkv");
        p.collapse_current(); // on a file: up to its folder
        assert_eq!(cursor_label(&p), "Season 1");
    }

    #[test]
    fn a_filter_in_the_tree_shows_matches_and_folder_space_takes_only_those() {
        let mut p = tree_picker();
        p.filter_push_str("e01");
        assert_eq!(
            tree_labels(&p),
            vec!["Season 1", "  e01.mkv", "Season 2", "  e01.mkv"]
        );
        // Space on Season 1 acts on the e01 shown, not on the hidden e02.
        p.top();
        p.toggle_current();
        assert_eq!(p.only_files(), vec![1, 2, 4]);
        // And the bulk keys act on the matches here too.
        p.set_visible(false);
        assert_eq!(p.only_files(), vec![1, 4]);
    }

    #[test]
    fn switching_views_keeps_the_file_under_the_cursor() {
        let mut p = picker();
        p.move_by(2); // Season 2/e01.mkv in the flat list
        p.toggle_view();
        assert_eq!(p.view, View::Tree);
        assert_eq!(cursor_label(&p), "e01.mkv");
        let row = p.table_state.selected().unwrap();
        assert_eq!(p.tree.node(p.tree_rows()[row].node).file, Some(2));
        p.toggle_view();
        assert_eq!(p.current(), Some(2));
    }

    #[test]
    fn switching_to_the_tree_opens_the_way_to_a_file_in_a_closed_folder() {
        let mut p = tree_picker();
        p.move_by(3); // Season 2
        p.collapse_current();
        p.toggle_view(); // flat; cursor on Season 2's first file
        assert_eq!(p.current(), Some(2));
        p.toggle_view();
        assert_eq!(
            cursor_label(&p),
            "e01.mkv",
            "Season 2 was reopened to show it"
        );
    }

    #[test]
    fn up_and_down_step_through_recent_folders_and_back() {
        let mut p = Picker::new(
            preview(vec![file(0, "a.iso", 1)]),
            "/dl",
            vec!["/srv/tv".to_string(), "~/films".to_string()],
        );
        p.folder_set("/my/draft");
        p.recent_older();
        assert_eq!(p.folder.as_str(), "/srv/tv");
        assert_eq!(p.recent_position(), Some((1, 2)));
        p.recent_older();
        assert_eq!(p.folder.as_str(), "~/films");
        p.recent_older(); // no older one
        assert_eq!(p.folder.as_str(), "~/films");
        p.recent_newer();
        p.recent_newer();
        assert_eq!(p.folder.as_str(), "/my/draft", "back to what was typed");
        assert_eq!(p.recent_position(), None);
        p.recent_older();
        p.folder_push('x'); // editing it makes it the user's own
        assert_eq!(p.recent_position(), None);
        assert_eq!(p.folder.as_str(), "/srv/tvx");
    }

    #[test]
    fn confirm_reports_the_folder_as_typed() {
        let mut p = picker();
        p.folder_set(" ~/tv ");
        let done = p.confirm().unwrap();
        assert_eq!(done.folder_typed, "~/tv");
        assert!(done.output_folder.ends_with("tv/Show"));
    }

    fn visible_paths(p: &Picker) -> Vec<&str> {
        p.visible()
            .iter()
            .map(|&i| p.files[i].path.as_str())
            .collect()
    }

    #[test]
    fn opens_with_everything_selected_in_torrent_order() {
        let p = picker();
        assert_eq!(p.selected_count(), 4);
        assert_eq!(p.selected_bytes(), 901);
        assert_eq!(p.total_bytes(), 901);
        assert_eq!(visible_paths(&p)[0], "Season 1/e01.mkv");
        assert_eq!(p.current(), Some(0));
        assert_eq!(p.folder.as_str(), "/dl");
    }

    #[test]
    fn a_magnet_preselection_is_the_starting_point_and_what_r_restores() {
        let mut pv = preview(vec![file(0, "a", 1), file(1, "b", 1), file(5, "c", 1)]);
        pv.preselected = Some(vec![5]);
        let mut p = Picker::new(pv, "/dl", Vec::new());
        assert_eq!(p.only_files(), vec![5]);
        p.set_visible(true);
        assert_eq!(p.only_files(), vec![0, 1, 5]);
        p.restore();
        assert_eq!(p.only_files(), vec![5]);
    }

    #[test]
    fn selection_sends_librqbit_indices_not_positions() {
        let mut p = picker();
        p.bottom();
        p.toggle_current(); // notes.txt, librqbit index 4
        assert_eq!(p.only_files(), vec![0, 1, 2]);
        p.toggle_current();
        assert_eq!(p.only_files(), vec![0, 1, 2, 4]);
    }

    #[test]
    fn bulk_keys_act_on_the_filtered_files_only() {
        let mut p = picker();
        p.filter_push_str("season 1");
        assert_eq!(
            visible_paths(&p),
            vec!["Season 1/e01.mkv", "Season 1/e02.mkv"]
        );
        p.set_visible(false);
        assert_eq!(p.only_files(), vec![2, 4]);
        p.invert_visible();
        assert_eq!(p.only_files(), vec![0, 1, 2, 4]);
        p.filter_clear();
        p.set_visible(false);
        assert_eq!(p.only_files(), Vec::<usize>::new());
    }

    #[test]
    fn the_filter_is_exact_when_it_can_be_and_fuzzy_when_it_must() {
        let mut p = picker();
        p.filter_push_str("season 2");
        assert_eq!(
            visible_paths(&p),
            vec!["Season 2/e01.mkv"],
            "no e02 from season 1"
        );
        assert!(!p.fuzzy);
        p.filter_clear();
        // Nothing contains "s1e02", so it falls back to fuzzy, and says so.
        p.filter_push_str("s1e02");
        assert_eq!(visible_paths(&p), vec!["Season 1/e02.mkv"]);
        assert!(p.fuzzy);
        p.filter_clear();
        assert!(!p.fuzzy);
    }

    #[test]
    fn a_filter_keeps_the_chosen_sort() {
        let mut p = picker();
        p.cycle_sort(); // size
        p.filter_push_str("mkv");
        assert_eq!(
            visible_paths(&p),
            vec!["Season 2/e01.mkv", "Season 1/e01.mkv", "Season 1/e02.mkv"]
        );
    }

    #[test]
    fn the_cursor_stays_on_its_file_while_the_filter_changes() {
        let mut p = picker();
        p.move_by(2); // Season 2/e01.mkv
        assert_eq!(p.current(), Some(2));
        p.filter_push_str("e01");
        assert_eq!(
            p.current(),
            Some(2),
            "still visible, so still under the cursor"
        );
        assert_eq!(p.table_state.selected(), Some(1));
        p.filter_clear();
        p.filter_push_str("notes");
        assert_eq!(
            p.current(),
            Some(3),
            "filtered out, so the first match takes it"
        );
    }

    #[test]
    fn sort_cycles_through_size_and_name() {
        let mut p = picker();
        p.cycle_sort();
        assert_eq!(p.sort, PickerSort::Size);
        assert_eq!(visible_paths(&p)[0], "Season 2/e01.mkv");
        p.cycle_sort();
        assert_eq!(p.sort, PickerSort::Name);
        assert_eq!(visible_paths(&p)[3], "Season 2/e01.mkv");
        assert_eq!(visible_paths(&p)[0], "notes.txt");
        p.cycle_sort();
        assert_eq!(p.sort, PickerSort::TorrentOrder);
    }

    #[test]
    fn navigation_clamps_and_pages() {
        let mut p = picker();
        p.move_by(-5);
        assert_eq!(p.table_state.selected(), Some(0));
        p.page_size = 2;
        p.page(1);
        assert_eq!(p.table_state.selected(), Some(2));
        p.page(1);
        assert_eq!(p.table_state.selected(), Some(3));
        p.top();
        assert_eq!(p.table_state.selected(), Some(0));
        p.bottom();
        assert_eq!(p.table_state.selected(), Some(3));
    }

    #[test]
    fn an_empty_filter_result_has_no_cursor_and_toggles_nothing() {
        let mut p = picker();
        p.filter_push_str("zzz");
        assert!(p.visible().is_empty());
        assert_eq!(p.table_state.selected(), None);
        p.toggle_current();
        p.move_by(1);
        assert_eq!(p.selected_count(), 4);
    }

    #[test]
    fn folders_resolve_tilde_relative_and_absolute() {
        let home = expand_tilde("~");
        assert_eq!(
            resolve_folder("~/films", "/dl"),
            Ok(Path::new(&home).join("films"))
        );
        assert_eq!(
            resolve_folder("films", "/dl"),
            Ok(PathBuf::from("/dl/films"))
        );
        assert_eq!(
            resolve_folder(" /srv/x ", "/dl"),
            Ok(PathBuf::from("/srv/x"))
        );
        assert!(resolve_folder("   ", "/dl").is_err());
    }

    #[test]
    fn the_output_folder_keeps_the_torrents_own_subfolder() {
        let mut p = picker();
        p.folder_set("/srv/media");
        assert_eq!(p.output_folder(), Ok(PathBuf::from("/srv/media/Show")));

        let mut single = preview(vec![file(0, "one.iso", 9)]);
        single.subfolder = None;
        let mut p = Picker::new(single, "/dl", Vec::new());
        p.folder_set("/srv/media");
        assert_eq!(p.output_folder(), Ok(PathBuf::from("/srv/media")));
    }

    #[test]
    fn confirm_refuses_nothing_selected_and_a_file_as_folder() {
        let mut p = picker();
        p.set_visible(false);
        assert_eq!(p.confirm().unwrap_err(), NOTHING_SELECTED);

        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("plain.txt");
        std::fs::write(&file_path, b"x").unwrap();
        let mut p = picker();
        p.folder_set(file_path.to_str().unwrap());
        assert!(p.confirm().unwrap_err().contains("not a folder"));
        // Refusals keep the prepared torrent for the next attempt.
        p.folder_set(dir.path().to_str().unwrap());
        let done = p.confirm().expect("valid now");
        assert_eq!(done.only_files, vec![0, 1, 2, 4]);
        assert_eq!(done.output_folder, dir.path().join("Show"));
    }

    #[test]
    fn confirm_hands_the_prepared_torrent_over_once() {
        let mut p = picker();
        assert!(p.confirm().is_ok());
        assert!(p.confirm().is_err());
    }

    #[test]
    fn the_space_probe_waits_for_typing_to_settle_and_ignores_stale_answers() {
        let mut p = picker();
        let now = Instant::now();
        assert_eq!(p.probe_due(now), Some(PathBuf::from("/dl")), "due on open");
        assert_eq!(p.probe_due(now), None, "only once");

        p.folder_push('x');
        assert_eq!(p.probe_due(Instant::now()), None, "still typing");
        let later = Instant::now() + SPACE_PROBE_DELAY;
        assert_eq!(p.probe_due(later), Some(PathBuf::from("/dlx")));

        let stale = FolderProbe {
            folder: PathBuf::from("/dl"),
            free: Some(1),
            exists: true,
            not_a_dir: false,
        };
        assert!(!p.apply_probe(stale), "answer for a folder no longer typed");
        let fresh = FolderProbe {
            folder: PathBuf::from("/dlx"),
            free: Some(5),
            exists: false,
            not_a_dir: false,
        };
        assert!(p.apply_probe(fresh));
        assert_eq!(p.current_probe().and_then(|x| x.free), Some(5));
        p.folder_pop();
        assert!(p.current_probe().is_none(), "the reading belongs to /dlx");
    }

    #[test]
    fn probe_folder_measures_the_nearest_existing_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let probe = probe_folder(dir.path().join("new/deeper"));
        assert!(!probe.exists);
        assert!(!probe.not_a_dir);
        assert!(probe.free.is_some());
    }

    #[test]
    fn a_file_anywhere_in_the_path_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, b"x").unwrap();
        let probe = probe_folder(file.join("tv"));
        assert!(probe.not_a_dir, "a file above the folder");

        let mut p = picker();
        p.folder_set(&file.join("tv").to_string_lossy());
        let err = p.confirm().unwrap_err();
        assert!(err.contains("notes.txt"), "{err}");
        assert!(p.prepared.is_some(), "nothing taken on a refusal");
    }

    #[test]
    fn a_file_where_the_torrent_subfolder_goes_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Show"), b"x").unwrap();
        let mut p = picker();
        p.folder_set(&dir.path().to_string_lossy());
        assert!(p.confirm().unwrap_err().contains("Show"));
    }

    #[test]
    fn a_relative_download_dir_is_made_absolute_once() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(absolute_dir("dl"), cwd.join("dl").to_string_lossy());
        // Already absolute on every platform (`/srv/dl` is not on Windows).
        let already = cwd.join("srv").to_string_lossy().into_owned();
        assert_eq!(absolute_dir(&already), already);
        let mut preview = preview(vec![file(0, "a.iso", 1)]);
        preview.subfolder = None;
        let p = Picker::new(preview, &absolute_dir("dl"), Vec::new());
        assert_eq!(p.output_folder().unwrap(), cwd.join("dl"));
    }

    fn dirs(names: &'static [&'static str]) -> impl Fn(&Path) -> io::Result<Vec<String>> {
        move |_| Ok(names.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn completion_finishes_a_unique_match_with_a_separator() {
        let sep = std::path::MAIN_SEPARATOR;
        let done = complete_dir("/srv/me", "/dl", dirs(&["media", "backups"]));
        assert_eq!(done.input, Some(format!("/srv/media{sep}")));
        assert!(done.candidates.is_empty());
    }

    #[test]
    fn completion_extends_to_the_common_prefix_and_lists_candidates() {
        let done = complete_dir("/srv/m", "/dl", dirs(&["movies", "movies-4k", "music"]));
        assert_eq!(done.input, None, "m is already the common prefix");
        assert_eq!(done.candidates, vec!["movies", "movies-4k", "music"]);

        let done = complete_dir("/srv/mo", "/dl", dirs(&["movies", "movies-4k"]));
        assert_eq!(done.input, Some("/srv/movies".to_string()));
    }

    #[test]
    fn completion_keeps_what_was_typed_and_resolves_where_it_points() {
        let seen = std::cell::RefCell::new(Vec::new());
        let list = |dir: &Path| {
            seen.borrow_mut().push(dir.to_path_buf());
            Ok(vec!["Downloads".to_string()])
        };
        let done = complete_dir("~/Dow", "/dl", list);
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(done.input, Some(format!("~/Downloads{sep}")));
        assert_eq!(seen.borrow()[0], PathBuf::from(expand_tilde("~/")));

        // No separator at all: a name inside the download dir.
        let seen = std::cell::RefCell::new(Vec::new());
        let list = |dir: &Path| {
            seen.borrow_mut().push(dir.to_path_buf());
            Ok(vec!["films".to_string()])
        };
        complete_dir("fi", "/dl", list);
        assert_eq!(seen.borrow()[0], PathBuf::from("/dl"));
    }

    #[test]
    fn completion_hides_dot_folders_until_asked() {
        let done = complete_dir("/h/", "/dl", dirs(&[".cache", "code"]));
        assert_eq!(
            done.input,
            Some(format!("/h/code{}", std::path::MAIN_SEPARATOR))
        );
        let done = complete_dir("/h/.", "/dl", dirs(&[".cache", "code"]));
        assert_eq!(
            done.input,
            Some(format!("/h/.cache{}", std::path::MAIN_SEPARATOR))
        );
    }

    #[test]
    fn completion_reports_no_match_and_unreadable_folders() {
        assert!(complete_dir("/srv/zz", "/dl", dirs(&["media"])).no_match);
        let unreadable = |_: &Path| -> io::Result<Vec<String>> {
            Err(io::Error::new(io::ErrorKind::NotFound, "gone"))
        };
        assert!(complete_dir("/nope/x", "/dl", unreadable).no_match);
    }

    #[test]
    fn a_bare_tilde_completes_to_home() {
        let done = complete_dir("~", "/dl", dirs(&[]));
        assert_eq!(done.input, Some(format!("~{}", std::path::MAIN_SEPARATOR)));
    }

    #[test]
    fn list_subdirs_reports_directories_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("file.txt"), b"x").unwrap();
        assert_eq!(list_subdirs(dir.path()).unwrap(), vec!["sub"]);
    }

    #[test]
    fn tab_in_the_picker_sets_the_folder_and_shows_candidates() {
        let mut p = picker();
        p.folder_set("/srv/m");
        p.complete_folder(dirs(&["movies", "music"]));
        assert_eq!(p.folder.as_str(), "/srv/m");
        assert_eq!(p.message.as_deref(), Some("movies  music"));
        p.folder_push('o');
        assert_eq!(p.message, None, "typing clears the candidates");
        p.complete_folder(dirs(&["movies", "music"]));
        assert_eq!(
            p.folder.as_str(),
            format!("/srv/movies{}", std::path::MAIN_SEPARATOR)
        );
    }
}
