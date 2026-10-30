//! The line editor behind every text box, and the add bar drawn with it.

use std::cell::Cell;
use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::completion::{Candidate, Completion};
use crate::theme::Theme;
use crate::ui::util::sanitize_display;

const PLACEHOLDER: &str = "magnet:?xt=urn:btih:... or /path/to/file.torrent";

/// Cap on the add-torrent buffer. Magnet links with a long tracker list run
/// 1–2 KB, so this leaves generous headroom while keeping an accidental
/// paste of the wrong clipboard buffer from growing the string unbounded.
const MAX_INPUT_CHARS: usize = 4096;

/// Undo steps a field keeps. Each is a copy of the text, so even a full
/// 4096-character field holds well under a megabyte of history.
const MAX_UNDO: usize = 100;

/// Rows the completion menu shows before it scrolls.
const MAX_MENU_ROWS: u16 = 8;

/// A single-line text field with a cursor. Every text box edits through one —
/// the add bar, the filter, search, palette and speed-limit bars, and the add
/// picker's filter and folder — so they all take the same keys; see
/// [`InputWidget::handle_key`].
pub struct InputWidget {
    buffer: String,
    /// Byte offset into `buffer`, always on a char boundary.
    cursor: usize,
    cap: usize,
    /// Which characters the field takes, typed or pasted.
    accept: fn(char) -> bool,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The last edit, so a run of typing or deleting undoes as one step.
    last_edit: Option<EditKind>,
    /// The first character on screen. Only the renderer knows the width, and
    /// it draws from a shared reference, hence the `Cell`.
    scroll: Cell<usize>,
    /// Tab-completion choices, while they are open.
    menu: Option<Menu>,
}

struct Snapshot {
    buffer: String,
    cursor: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    DeleteBack,
    DeleteForward,
    Complete,
    /// Never merged with its neighbours: a paste, a word or line deletion.
    Single,
}

/// What a key did to a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// The text changed.
    Changed,
    /// An editing key that left the text alone: a cursor move, or Backspace
    /// with nothing before the cursor.
    Unchanged,
    /// Not an editing key: the caller's to handle.
    Unhandled,
}

/// What the completion menu did with a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKey {
    /// The menu took it; the field may show a different choice now.
    Used,
    /// No menu was open, or the key closed it: handle the key as usual.
    Pass,
}

/// Completion choices for the text before the cursor.
pub struct Menu {
    /// The text before the cursor when the menu opened, for `Esc`.
    typed: String,
    /// What goes before every choice: the path up to the segment completed.
    head: String,
    pub items: Vec<Candidate>,
    /// The highlighted choice; `None` until the first Tab or arrow.
    pub selected: Option<usize>,
}

enum Action {
    Insert(char),
    Backspace,
    Delete,
    DeleteWordBack,
    DeleteWordForward,
    DeleteToStart,
    DeleteToEnd,
    Undo,
    Redo,
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
}

/// The editing keys. Readline's chords are the core because every terminal
/// delivers them; the modified arrows, Ctrl+Backspace, Ctrl+Delete and
/// Ctrl+Shift+Z are extras that only some terminals report.
fn action(key: KeyEvent) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    Some(match key.code {
        // Neither modifier, or both: Windows reports AltGr as Ctrl+Alt, and
        // AltGr is how many keyboard layouts type `\`, `@` or `{`.
        KeyCode::Char(c) if ctrl == alt => Action::Insert(c),
        KeyCode::Char(c) if ctrl => match c {
            'a' => Action::Home,
            'e' => Action::End,
            // ^H is what some terminals send for Backspace itself.
            'h' => Action::Backspace,
            'w' => Action::DeleteWordBack,
            'u' => Action::DeleteToStart,
            'k' => Action::DeleteToEnd,
            'z' if shift => Action::Redo,
            'z' => Action::Undo,
            'Z' | 'y' => Action::Redo,
            _ => return None,
        },
        KeyCode::Char(c) => match c {
            'b' => Action::WordLeft,
            'f' => Action::WordRight,
            'd' => Action::DeleteWordForward,
            _ => return None,
        },
        KeyCode::Backspace if ctrl || alt => Action::DeleteWordBack,
        KeyCode::Backspace => Action::Backspace,
        KeyCode::Delete if ctrl || alt => Action::DeleteWordForward,
        KeyCode::Delete => Action::Delete,
        KeyCode::Left if ctrl || alt => Action::WordLeft,
        KeyCode::Left => Action::Left,
        KeyCode::Right if ctrl || alt => Action::WordRight,
        KeyCode::Right => Action::Right,
        KeyCode::Home => Action::Home,
        KeyCode::End => Action::End,
        _ => return None,
    })
}

/// Letters and digits, with their combining marks; everything else — `/`,
/// `.`, `-`, spaces — separates words, so a word step in a path moves one
/// folder name at a time.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || is_mark(c)
}

/// A zero-width character, drawn on the character before it: a combining
/// accent, a variation selector, a joiner.
fn is_mark(c: char) -> bool {
    c.width() == Some(0)
}

impl Default for InputWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl InputWidget {
    pub fn new() -> Self {
        Self::with_cap(MAX_INPUT_CHARS)
    }

    /// A field holding at most `cap` characters.
    pub fn with_cap(cap: usize) -> Self {
        Self::with_filter(cap, accepts)
    }

    /// A field holding at most `cap` characters, of those `accept` allows.
    pub fn with_filter(cap: usize, accept: fn(char) -> bool) -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            cap,
            accept,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
            scroll: Cell::new(0),
            menu: None,
        }
    }

    pub fn value(&self) -> String {
        self.buffer.clone()
    }

    pub fn as_str(&self) -> &str {
        &self.buffer
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// The cursor, in characters from the start.
    pub fn cursor(&self) -> usize {
        self.buffer[..self.cursor].chars().count()
    }

    pub fn before_cursor(&self) -> &str {
        &self.buffer[..self.cursor]
    }

    /// Replace the contents, through the same filter and cap as typing, with
    /// the cursor at the end. Not an edit but a fresh start, the way opening
    /// a field is: the undo history goes with the old text.
    pub fn set(&mut self, s: &str) {
        self.buffer = self.filtered(s, self.cap);
        self.cursor = self.buffer.len();
        self.undo.clear();
        self.redo.clear();
        self.last_edit = None;
        self.scroll.set(0);
        self.menu = None;
    }

    pub fn clear(&mut self) {
        self.set("");
    }

    /// Type one character at the cursor.
    pub fn insert(&mut self, c: char) -> bool {
        if !(self.accept)(c) || self.buffer.chars().count() >= self.cap {
            return false;
        }
        // Each word typed is its own undo step.
        if is_word(c)
            && !self
                .before_cursor()
                .chars()
                .next_back()
                .is_some_and(is_word)
        {
            self.last_edit = None;
        }
        self.record(EditKind::Insert);
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        true
    }

    /// Insert a whole string at the cursor (a bracketed paste) as one undo
    /// step. Same filter and cap as typing, but the length is counted once:
    /// going through [`InputWidget::insert`] would recount the buffer per
    /// character — O(paste × cap) on the event loop, the freeze class the cap
    /// exists to prevent.
    pub fn insert_str(&mut self, s: &str) -> bool {
        let room = self.cap.saturating_sub(self.buffer.chars().count());
        let text = self.filtered(s, room);
        if text.is_empty() {
            return false;
        }
        self.record(EditKind::Single);
        self.buffer.insert_str(self.cursor, &text);
        self.cursor += text.len();
        true
    }

    /// Apply an editing key. Anything else comes back
    /// [`Edit::Unhandled`] without touching the text — which is also what
    /// keeps an unbound Ctrl or Alt chord from typing its letter.
    pub fn handle_key(&mut self, key: KeyEvent) -> Edit {
        let Some(action) = action(key) else {
            return Edit::Unhandled;
        };
        self.menu = None;
        let at = self.cursor;
        let changed = match action {
            Action::Insert(c) => self.insert(c),
            Action::Backspace => self.delete(self.prev_char(at)..at, EditKind::DeleteBack),
            Action::Delete => self.delete(at..self.next_char(at), EditKind::DeleteForward),
            Action::DeleteWordBack => self.delete(self.word_left(at)..at, EditKind::Single),
            Action::DeleteWordForward => self.delete(at..self.word_right(at), EditKind::Single),
            Action::DeleteToStart => self.delete(0..at, EditKind::Single),
            Action::DeleteToEnd => self.delete(at..self.buffer.len(), EditKind::Single),
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::Left => self.move_to(self.prev_char(at)),
            Action::Right => self.move_to(self.next_char(at)),
            Action::WordLeft => self.move_to(self.word_left(at)),
            Action::WordRight => self.move_to(self.word_right(at)),
            Action::Home => self.move_to(0),
            Action::End => self.move_to(self.buffer.len()),
        };
        if changed {
            Edit::Changed
        } else {
            Edit::Unchanged
        }
    }

    // -- completion ------------------------------------------------------

    /// Take a Tab completion of the text before the cursor (see
    /// [`crate::completion::complete_path`]): the text moves on as far as it
    /// got, and when there is more than one way to go on, the menu opens.
    pub fn complete(&mut self, done: Completion) {
        if let Some(before) = &done.input {
            self.replace_before_cursor(before);
        }
        if done.candidates.len() > 1 {
            self.menu = Some(Menu {
                typed: self.before_cursor().to_string(),
                head: done.head,
                items: done.candidates,
                selected: None,
            });
        }
    }

    pub fn menu(&self) -> Option<&Menu> {
        self.menu.as_ref()
    }

    /// A key while the completion menu is open. Tab and `↓` highlight the next
    /// choice, Shift+Tab and `↑` the previous one, and the field shows the
    /// highlighted choice in place; Enter keeps it and `Esc` goes back to what
    /// was typed. Any other key closes the menu with the field as it stands
    /// and comes back [`MenuKey::Pass`], so typing carries straight on from
    /// the choice.
    pub fn menu_key(&mut self, key: KeyEvent) -> MenuKey {
        let Some(menu) = self.menu.as_mut() else {
            return MenuKey::Pass;
        };
        let n = menu.items.len();
        let next = match (key.code, menu.selected) {
            (KeyCode::Tab | KeyCode::Down, None) => Some(0),
            (KeyCode::Tab | KeyCode::Down, Some(i)) => Some((i + 1) % n),
            (KeyCode::BackTab | KeyCode::Up, None) => Some(n - 1),
            (KeyCode::BackTab | KeyCode::Up, Some(i)) => Some((i + n - 1) % n),
            _ => None,
        };
        if let Some(i) = next {
            menu.selected = Some(i);
            let text = format!("{}{}", menu.head, menu.items[i].text());
            self.replace_before_cursor(&text);
            return MenuKey::Used;
        }
        let typed = std::mem::take(&mut menu.typed);
        self.menu = None;
        self.last_edit = None;
        match key.code {
            KeyCode::Esc => {
                self.replace_before_cursor(&typed);
                self.last_edit = None;
                MenuKey::Used
            }
            KeyCode::Enter => MenuKey::Used,
            _ => MenuKey::Pass,
        }
    }

    // -- drawing ---------------------------------------------------------

    /// The field as spans fitting `width` cells, scrolled so the cursor is on
    /// screen, with a `…` on each side that is cut off. `cursor` is the
    /// cursor's style while the field has the keys; `None` draws none.
    pub fn spans(&self, width: usize, text: Style, cursor: Option<Style>) -> Vec<Span<'static>> {
        if width == 0 {
            return Vec::new();
        }
        let chars: Vec<char> = self.buffer.chars().collect();
        let widths: Vec<usize> = chars.iter().map(|c| c.width().unwrap_or(0)).collect();
        let cells = |r: Range<usize>| widths[r].iter().sum::<usize>();
        let len = chars.len();
        let at = self.cursor();
        // The cursor covers its character and the combining marks after it.
        let at_end = if at < len {
            at + 1 + widths[at + 1..].iter().take_while(|&&w| w == 0).count()
        } else {
            len
        };
        let char_cells = widths.get(at).copied().unwrap_or(0);
        // At the end, or on a lone zero-width mark, the cursor is a block of
        // its own.
        let cursor_cells = char_cells.max(1);
        let block = cursor_cells - char_cells;

        let mut start = self.scroll.get().min(at);
        // Scroll right until the cursor fits, keeping a cell for each `…`.
        while start < at
            && usize::from(start > 0) + cells(start..at) + cursor_cells + usize::from(at_end < len)
                > width
        {
            start += 1;
        }
        // And back left while everything from there to the end fits.
        while start > 0 && usize::from(start > 1) + cells(start - 1..len) + block <= width {
            start -= 1;
        }
        self.scroll.set(start);

        // Then the rest whole if it fits, or as much as fits beside a `…`.
        let mut used = usize::from(start > 0) + cells(start..at) + cursor_cells;
        let mut end = len;
        if used + cells(at_end..len) > width {
            end = at_end;
            // Strictly less: the last cell is the `…`.
            while end < len && used + widths[end] < width {
                used += widths[end];
                end += 1;
            }
        }

        let collect = |r: Range<usize>| chars[r].iter().collect::<String>();
        let mut spans = Vec::new();
        if start > 0 {
            spans.push(Span::styled("\u{2026}", text));
        }
        spans.push(Span::styled(collect(start..at), text));
        match cursor {
            Some(style) if block > 0 => {
                spans.push(Span::styled("\u{2588}", style));
                spans.push(Span::styled(collect(at..at_end), text));
            }
            Some(style) => spans.push(Span::styled(
                collect(at..at_end),
                style.add_modifier(Modifier::REVERSED),
            )),
            None => spans.push(Span::styled(collect(at..at_end), text)),
        }
        spans.push(Span::styled(collect(at_end..end), text));
        if end < len {
            spans.push(Span::styled("\u{2026}", text));
        }
        spans
    }

    // -- internals -------------------------------------------------------

    fn filtered(&self, s: &str, room: usize) -> String {
        let accept = self.accept;
        s.chars().filter(|&c| accept(c)).take(room).collect()
    }

    /// Push an undo step for an edit of `kind`, unless it continues the last
    /// one.
    fn record(&mut self, kind: EditKind) {
        if kind == EditKind::Single || self.last_edit != Some(kind) {
            if self.undo.len() == MAX_UNDO {
                self.undo.remove(0);
            }
            self.undo.push(self.snapshot());
        }
        self.redo.clear();
        self.last_edit = Some(kind);
        if kind != EditKind::Complete {
            self.menu = None;
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            buffer: self.buffer.clone(),
            cursor: self.cursor,
        }
    }

    fn restore(&mut self, snapshot: Snapshot) -> Snapshot {
        let now = self.snapshot();
        self.buffer = snapshot.buffer;
        self.cursor = snapshot.cursor;
        self.last_edit = None;
        now
    }

    fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo.pop() else {
            return false;
        };
        let now = self.restore(snapshot);
        self.redo.push(now);
        true
    }

    fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo.pop() else {
            return false;
        };
        let now = self.restore(snapshot);
        self.undo.push(now);
        true
    }

    fn delete(&mut self, range: Range<usize>, kind: EditKind) -> bool {
        if range.is_empty() {
            return false;
        }
        self.record(kind);
        self.buffer.replace_range(range.clone(), "");
        self.cursor = range.start;
        true
    }

    fn move_to(&mut self, at: usize) -> bool {
        self.cursor = at;
        // Typing somewhere else starts a new undo step.
        self.last_edit = None;
        false
    }

    /// Swap the text before the cursor, as a completion does: one undo step
    /// however many choices the menu goes through.
    fn replace_before_cursor(&mut self, before: &str) {
        if before == self.before_cursor() {
            return;
        }
        let after = self.buffer[self.cursor..].chars().count();
        let before = self.filtered(before, self.cap.saturating_sub(after));
        self.record(EditKind::Complete);
        self.buffer.replace_range(..self.cursor, &before);
        self.cursor = before.len();
    }

    /// Where the character before `at` starts, combining marks and all: the
    /// cursor steps over a character as it is drawn, never onto a mark.
    fn prev_char(&self, at: usize) -> usize {
        let mut pos = at;
        for (i, c) in self.buffer[..at].char_indices().rev() {
            pos = i;
            if !is_mark(c) {
                break;
            }
        }
        pos
    }

    /// Where the character at `at` ends, with the combining marks after it.
    fn next_char(&self, at: usize) -> usize {
        let mut chars = self.buffer[at..].char_indices();
        let Some((_, first)) = chars.next() else {
            return at;
        };
        let mut end = at + first.len_utf8();
        for (i, c) in chars {
            if !is_mark(c) {
                break;
            }
            end = at + i + c.len_utf8();
        }
        end
    }

    /// The start of the word before `at`, past any separators in between.
    fn word_left(&self, at: usize) -> usize {
        let mut pos = at;
        let mut in_word = false;
        for (i, c) in self.buffer[..at].char_indices().rev() {
            if is_word(c) {
                in_word = true;
            } else if in_word {
                break;
            }
            pos = i;
        }
        pos
    }

    /// The end of the word after `at`, past any separators in between.
    fn word_right(&self, at: usize) -> usize {
        let mut pos = at;
        let mut in_word = false;
        for c in self.buffer[at..].chars() {
            if is_word(c) {
                in_word = true;
            } else if in_word {
                break;
            }
            pos += c.len_utf8();
        }
        pos
    }
}

/// Reject control chars (e.g. \x1b from an OSC sequence, \x07 BEL, raw
/// newlines from a multi-line paste; tabs too, every field is single-line)
/// and bidi controls, which could make a path or link display as something
/// other than what it is.
pub fn accepts(c: char) -> bool {
    !c.is_control() && !crate::ui::util::is_bidi_control(c)
}

/// The add bar. `pick` is the `A` flavour, which goes on to the add picker
/// instead of adding straight away; `note` is a remark for one keypress, such
/// as a completion that found nothing.
pub fn render_input(
    f: &mut Frame,
    area: Rect,
    input: &InputWidget,
    pick: bool,
    note: Option<&str>,
    theme: Theme,
) {
    let title = if pick {
        " Add Torrent, choosing files next (magnet link or .torrent path) "
    } else {
        " Add Torrent (magnet link or .torrent path) "
    };
    let mut block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.primary));
    if let Some(note) = note {
        block = block.title_bottom(Span::styled(
            format!(" {note} "),
            Style::default().fg(theme.warning),
        ));
    }

    let cursor = Style::default().fg(theme.text);
    let line = if input.is_empty() {
        Line::from(vec![
            Span::styled("\u{2588}", cursor),
            Span::styled(PLACEHOLDER, Style::default().fg(theme.muted)),
        ])
    } else {
        let width = area.width.saturating_sub(2) as usize;
        Line::from(input.spans(width, Style::default(), Some(cursor)))
    };

    let widget = Paragraph::new(line).block(block);
    f.render_widget(widget, area);
}

/// The completion menu, dropped down from `field` (the area the field is drawn
/// in): above it when there is more room there, else below, kept inside
/// `bounds` and starting at column `x` where it fits.
pub fn render_menu(f: &mut Frame, field: Rect, bounds: Rect, x: u16, menu: &Menu, theme: Theme) {
    let labels: Vec<String> = menu
        .items
        .iter()
        .map(|c| sanitize_display(&c.text()))
        .collect();
    let title = match menu.selected {
        Some(i) => format!(" {}/{} ", i + 1, menu.items.len()),
        None => format!(" {} matches ", menu.items.len()),
    };
    let widest = labels.iter().map(|l| l.width()).max().unwrap_or(0);
    // Borders, and a space either side of the name.
    let width = (widest.max(title.width()) + 4).min(bounds.width as usize) as u16;
    let rows = (menu.items.len() as u16).min(MAX_MENU_ROWS) + 2;
    let above = field.y.saturating_sub(bounds.y);
    let below = bounds.bottom().saturating_sub(field.bottom());
    let (y, height) = if above >= rows || above >= below {
        (field.y - rows.min(above), rows.min(above))
    } else {
        (field.bottom(), rows.min(below))
    };
    if height < 3 || width < 5 {
        return;
    }
    let x = x.clamp(bounds.x, bounds.right() - width);
    let area = Rect {
        x,
        y,
        width,
        height,
    };
    let items: Vec<ListItem> = menu
        .items
        .iter()
        .zip(labels)
        .map(|(c, label)| {
            let ink = if c.is_dir { theme.primary } else { theme.text };
            ListItem::new(Span::styled(format!(" {label} "), Style::default().fg(ink)))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.primary)),
        )
        .highlight_style(
            Style::default()
                .bg(theme.selection_bg)
                .add_modifier(Modifier::BOLD),
        );
    let mut state = ListState::default().with_selected(menu.selected);
    f.render_widget(Clear, area);
    f.render_stateful_widget(list, area, &mut state);
}

/// Pre-flight check on what the user typed, so an obvious mistake produces a
/// status-bar message instead of a round trip to the engine. Dispatches on the
/// literal `.torrent` suffix: anything ending in it is checked for existence
/// only — not readability, size or content — and everything else is validated
/// as a magnet link. `Ok` therefore means "worth handing to the engine", not
/// "will succeed"; the engine re-checks size and parses for real.
///
/// Expects an already tilde-expanded path; both callers run `expand_tilde`
/// first.
pub fn validate_torrent_source(input: &str) -> Result<(), String> {
    let input = input.trim();

    if input.ends_with(".torrent") {
        let path = std::path::Path::new(input);
        if path.exists() {
            return Ok(());
        } else {
            return Err(format!("File not found: {}", input));
        }
    }

    validate_magnet(input)
}

pub fn validate_magnet(link: &str) -> Result<(), String> {
    let link = link.trim();
    if !link.starts_with("magnet:?") {
        return Err("Must be a magnet link (magnet:?) or .torrent file path".to_string());
    }
    if !link.contains("xt=urn:btih:") {
        return Err("Must contain 'xt=urn:btih:'".to_string());
    }

    if let Some(pos) = link.find("xt=urn:btih:") {
        let after = &link[pos + 12..];
        let hash = after.split('&').next().unwrap_or("");
        if hash.len() == 40 {
            if !hash.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err("Invalid hex info hash".to_string());
            }
        } else if hash.len() == 32 {
            if !hash.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err("Invalid base32 info hash".to_string());
            }
        } else {
            return Err(format!(
                "Info hash must be 40 hex or 32 base32 chars, got {}",
                hash.len()
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn alt(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
    }

    fn typed(s: &str) -> InputWidget {
        let mut w = InputWidget::new();
        for c in s.chars() {
            w.handle_key(key(KeyCode::Char(c)));
        }
        w
    }

    /// The text with the cursor drawn as `|`.
    fn shown(w: &InputWidget) -> String {
        let (before, after) = w.as_str().split_at(w.before_cursor().len());
        format!("{before}|{after}")
    }

    fn press(w: &mut InputWidget, keys: &[KeyEvent]) {
        for &k in keys {
            w.handle_key(k);
        }
    }

    #[test]
    fn input_buffer_starts_empty() {
        let w = InputWidget::new();
        assert_eq!(w.value(), "");
        assert_eq!(w.cursor(), 0);
    }

    #[test]
    fn typing_and_pasting_are_capped() {
        let mut w = InputWidget::new();
        w.insert_str(&"m".repeat(MAX_INPUT_CHARS + 100));
        assert_eq!(w.value().chars().count(), MAX_INPUT_CHARS);
        // Still capped one char at a time as well, wherever the cursor is.
        assert!(!w.insert('x'));
        w.handle_key(key(KeyCode::Home));
        assert!(!w.insert('x'));
        assert!(!w.insert_str("xyz"));
        assert_eq!(w.value().chars().count(), MAX_INPUT_CHARS);
    }

    #[test]
    fn input_drops_control_chars() {
        let mut w = InputWidget::new();
        for c in ['a', '\x1b', '\x07', '\n', '\t', 'b'] {
            w.insert(c);
        }
        assert_eq!(w.value(), "ab");
    }

    #[test]
    fn input_paste_filters_controls() {
        let mut w = InputWidget::new();
        w.insert_str("magnet:?\x1b[31mxt=urn:btih:0123456789abcdef0123456789abcdef01234567\n");
        // ANSI escape, the literal '[31m' bytes (printable), and the trailing
        // newline are filtered: \x1b and \n are control chars; '[31m' is
        // printable so it stays. The hash payload itself is unchanged.
        assert!(w.value().starts_with("magnet:?[31mxt=urn:btih:"));
        assert!(!w.value().contains('\n'));
        assert!(!w.value().contains('\x1b'));
    }

    #[test]
    fn a_digits_only_field_takes_digits_only() {
        let mut w = InputWidget::with_filter(3, |c| c.is_ascii_digit());
        w.insert_str("1a2b3c4");
        assert_eq!(w.value(), "123");
    }

    #[test]
    fn typing_goes_in_at_the_cursor() {
        let mut w = typed("/srv/moves");
        press(&mut w, &[key(KeyCode::Left), key(KeyCode::Left)]);
        w.handle_key(key(KeyCode::Char('i')));
        assert_eq!(shown(&w), "/srv/movi|es");
        w.insert_str("XY");
        assert_eq!(shown(&w), "/srv/moviXY|es");
    }

    #[test]
    fn backspace_and_delete_work_either_side_of_the_cursor() {
        let mut w = typed("abcd");
        press(&mut w, &[key(KeyCode::Left), key(KeyCode::Left)]);
        assert_eq!(w.handle_key(key(KeyCode::Backspace)), Edit::Changed);
        assert_eq!(shown(&w), "a|cd");
        assert_eq!(w.handle_key(key(KeyCode::Delete)), Edit::Changed);
        assert_eq!(shown(&w), "a|d");

        press(&mut w, &[key(KeyCode::Home)]);
        assert_eq!(w.handle_key(key(KeyCode::Backspace)), Edit::Unchanged);
        press(&mut w, &[key(KeyCode::End)]);
        assert_eq!(w.handle_key(key(KeyCode::Delete)), Edit::Unchanged);
        assert_eq!(w.value(), "ad");
    }

    #[test]
    fn ctrl_h_is_backspace() {
        let mut w = typed("ab");
        w.handle_key(ctrl(KeyCode::Char('h')));
        assert_eq!(shown(&w), "a|");
    }

    #[test]
    fn home_end_and_their_readline_chords() {
        let mut w = typed("hello");
        assert_eq!(w.handle_key(key(KeyCode::Home)), Edit::Unchanged);
        assert_eq!(w.cursor(), 0);
        w.handle_key(key(KeyCode::End));
        assert_eq!(w.cursor(), 5);
        w.handle_key(ctrl(KeyCode::Char('a')));
        assert_eq!(w.cursor(), 0);
        w.handle_key(ctrl(KeyCode::Char('e')));
        assert_eq!(w.cursor(), 5);
        // Arrows stop at the ends.
        w.handle_key(key(KeyCode::Right));
        assert_eq!(w.cursor(), 5);
        w.handle_key(key(KeyCode::Home));
        w.handle_key(key(KeyCode::Left));
        assert_eq!(w.cursor(), 0);
    }

    #[test]
    fn word_steps_move_one_folder_name_at_a_time() {
        let mut w = typed("/home/me/Downloads/");
        for k in [
            ctrl(KeyCode::Left),
            alt(KeyCode::Left),
            alt(KeyCode::Char('b')),
        ] {
            w.handle_key(key(KeyCode::End));
            w.handle_key(k);
            assert_eq!(shown(&w), "/home/me/|Downloads/");
            w.handle_key(k);
            assert_eq!(shown(&w), "/home/|me/Downloads/");
        }
        for k in [
            ctrl(KeyCode::Right),
            alt(KeyCode::Right),
            alt(KeyCode::Char('f')),
        ] {
            w.handle_key(key(KeyCode::Home));
            w.handle_key(k);
            assert_eq!(shown(&w), "/home|/me/Downloads/");
            w.handle_key(k);
            assert_eq!(shown(&w), "/home/me|/Downloads/");
        }
    }

    #[test]
    fn word_deletion_takes_a_folder_name_back_or_forward() {
        for k in [
            ctrl(KeyCode::Char('w')),
            alt(KeyCode::Backspace),
            ctrl(KeyCode::Backspace),
        ] {
            let mut w = typed("/home/me/Downloads/");
            assert_eq!(w.handle_key(k), Edit::Changed);
            assert_eq!(shown(&w), "/home/me/|");
            w.handle_key(k);
            assert_eq!(shown(&w), "/home/|");
        }
        for k in [alt(KeyCode::Char('d')), ctrl(KeyCode::Delete)] {
            let mut w = typed("/home/me");
            w.handle_key(key(KeyCode::Home));
            w.handle_key(k);
            assert_eq!(shown(&w), "|/me");
        }
    }

    #[test]
    fn ctrl_u_and_ctrl_k_cut_to_either_end() {
        let mut w = typed("magnet:?xt");
        press(&mut w, &[key(KeyCode::Left), key(KeyCode::Left)]);
        w.handle_key(ctrl(KeyCode::Char('k')));
        assert_eq!(shown(&w), "magnet:?|");
        w.handle_key(key(KeyCode::Left));
        w.handle_key(ctrl(KeyCode::Char('u')));
        assert_eq!(shown(&w), "|?");
    }

    #[test]
    fn unbound_chords_type_nothing() {
        // The bug this replaced: the add bar and the search box ignored the
        // modifiers, so Ctrl+X typed an `x`.
        let mut w = typed("ab");
        for k in [
            ctrl(KeyCode::Char('x')),
            alt(KeyCode::Char('x')),
            ctrl(KeyCode::Char(' ')),
            key(KeyCode::Tab),
            key(KeyCode::F(5)),
        ] {
            assert_eq!(w.handle_key(k), Edit::Unhandled, "{k:?}");
        }
        assert_eq!(w.value(), "ab");
    }

    #[test]
    fn altgr_still_types() {
        let mut w = InputWidget::new();
        let altgr = KeyEvent::new(
            KeyCode::Char('\\'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        assert_eq!(w.handle_key(altgr), Edit::Changed);
        let shifted = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT);
        w.handle_key(shifted);
        assert_eq!(w.value(), "\\A");
    }

    #[test]
    fn undo_takes_back_a_word_at_a_time_and_redo_brings_it_back() {
        let mut w = typed("hello world");
        w.handle_key(ctrl(KeyCode::Char('z')));
        assert_eq!(w.value(), "hello ");
        w.handle_key(ctrl(KeyCode::Char('z')));
        assert_eq!(w.value(), "");
        assert_eq!(w.handle_key(ctrl(KeyCode::Char('z'))), Edit::Unchanged);

        w.handle_key(ctrl(KeyCode::Char('y')));
        assert_eq!(w.value(), "hello ");
        w.handle_key(KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(shown(&w), "hello world|");
        assert_eq!(w.handle_key(ctrl(KeyCode::Char('Z'))), Edit::Unchanged);
    }

    #[test]
    fn a_run_of_deletes_is_one_step_and_a_move_ends_it() {
        let mut w = typed("abcdef");
        press(
            &mut w,
            &[
                key(KeyCode::Backspace),
                key(KeyCode::Backspace),
                key(KeyCode::Left),
                key(KeyCode::Backspace),
            ],
        );
        assert_eq!(w.value(), "abd");
        w.handle_key(ctrl(KeyCode::Char('z')));
        assert_eq!(shown(&w), "abc|d");
        w.handle_key(ctrl(KeyCode::Char('z')));
        assert_eq!(shown(&w), "abcdef|");
    }

    #[test]
    fn a_paste_and_a_cut_are_one_step_each() {
        let mut w = typed("x");
        w.insert_str("/a/b/c");
        w.handle_key(ctrl(KeyCode::Char('w')));
        assert_eq!(w.value(), "x/a/b/");
        w.handle_key(ctrl(KeyCode::Char('z')));
        assert_eq!(w.value(), "x/a/b/c");
        w.handle_key(ctrl(KeyCode::Char('z')));
        assert_eq!(w.value(), "x");
    }

    #[test]
    fn a_new_edit_drops_the_redo_steps() {
        let mut w = typed("ab");
        w.handle_key(ctrl(KeyCode::Char('z')));
        w.handle_key(key(KeyCode::Char('c')));
        assert_eq!(w.handle_key(ctrl(KeyCode::Char('y'))), Edit::Unchanged);
        assert_eq!(w.value(), "c");
    }

    #[test]
    fn the_history_is_bounded_and_set_starts_it_over() {
        let mut w = InputWidget::new();
        for _ in 0..(MAX_UNDO + 20) {
            w.insert_str("ab");
        }
        assert_eq!(w.undo.len(), MAX_UNDO);
        w.set("fresh");
        assert_eq!(shown(&w), "fresh|");
        assert_eq!(w.handle_key(ctrl(KeyCode::Char('z'))), Edit::Unchanged);
    }

    #[test]
    fn multibyte_characters_are_stepped_over_whole() {
        let mut w = typed("café→東京");
        press(&mut w, &[key(KeyCode::Left), key(KeyCode::Left)]);
        assert_eq!(shown(&w), "café→|東京");
        w.handle_key(key(KeyCode::Backspace));
        assert_eq!(shown(&w), "café|東京");
        w.handle_key(key(KeyCode::Delete));
        assert_eq!(shown(&w), "café|京");
        w.handle_key(ctrl(KeyCode::Char('w')));
        assert_eq!(shown(&w), "|京");
        assert_eq!(w.cursor(), 0);
    }

    // -- completion ------------------------------------------------------

    fn several(head: &str, names: &[&str]) -> Completion {
        Completion {
            input: None,
            head: head.to_string(),
            candidates: names.iter().map(|n| Candidate::dir(n)).collect(),
            no_match: false,
        }
    }

    #[test]
    fn a_completion_replaces_only_what_is_before_the_cursor() {
        let mut w = typed("/srv/mo/tv");
        press(
            &mut w,
            &[key(KeyCode::Left), key(KeyCode::Left), key(KeyCode::Left)],
        );
        w.complete(Completion {
            input: Some("/srv/movies".to_string()),
            head: "/srv/".to_string(),
            ..Default::default()
        });
        assert_eq!(shown(&w), "/srv/movies|/tv");
        assert!(w.menu().is_none());
        w.handle_key(ctrl(KeyCode::Char('z')));
        assert_eq!(shown(&w), "/srv/mo|/tv");
    }

    #[test]
    fn several_choices_open_a_menu_that_tab_walks_through() {
        let sep = std::path::MAIN_SEPARATOR;
        let mut w = typed("/srv/m");
        w.complete(several("/srv/", &["movies", "music"]));
        assert_eq!(w.menu().unwrap().selected, None, "nothing picked yet");
        assert_eq!(w.value(), "/srv/m");

        assert_eq!(w.menu_key(key(KeyCode::Tab)), MenuKey::Used);
        assert_eq!(w.value(), format!("/srv/movies{sep}"));
        w.menu_key(key(KeyCode::Down));
        assert_eq!(w.value(), format!("/srv/music{sep}"));
        w.menu_key(key(KeyCode::Tab));
        assert_eq!(w.value(), format!("/srv/movies{sep}"), "wraps around");
        w.menu_key(key(KeyCode::BackTab));
        assert_eq!(w.value(), format!("/srv/music{sep}"));
        w.menu_key(key(KeyCode::Up));
        assert_eq!(w.menu().unwrap().selected, Some(0));

        assert_eq!(w.menu_key(key(KeyCode::Enter)), MenuKey::Used);
        assert!(w.menu().is_none());
        assert_eq!(shown(&w), format!("/srv/movies{sep}|"));
        // However far the menu went, it undoes in one step.
        w.handle_key(ctrl(KeyCode::Char('z')));
        assert_eq!(w.value(), "/srv/m");
    }

    #[test]
    fn shift_tab_starts_from_the_last_choice() {
        let mut w = typed("x");
        w.complete(several("", &["xa", "xb", "xc"]));
        w.menu_key(key(KeyCode::BackTab));
        assert_eq!(w.menu().unwrap().selected, Some(2));
    }

    #[test]
    fn esc_in_the_menu_goes_back_to_what_was_typed() {
        let mut w = typed("/srv/m");
        w.complete(several("/srv/", &["movies", "music"]));
        w.menu_key(key(KeyCode::Tab));
        assert_eq!(w.menu_key(key(KeyCode::Esc)), MenuKey::Used);
        assert_eq!(shown(&w), "/srv/m|");
        assert!(w.menu().is_none());
    }

    #[test]
    fn any_other_key_closes_the_menu_and_carries_on() {
        let sep = std::path::MAIN_SEPARATOR;
        let mut w = typed("/srv/m");
        w.complete(several("/srv/", &["movies", "music"]));
        w.menu_key(key(KeyCode::Tab));
        let k = key(KeyCode::Char('x'));
        assert_eq!(w.menu_key(k), MenuKey::Pass);
        assert!(w.menu().is_none());
        w.handle_key(k);
        assert_eq!(w.value(), format!("/srv/movies{sep}x"));
        // The keys go back to their usual jobs.
        assert_eq!(w.menu_key(key(KeyCode::Tab)), MenuKey::Pass);
    }

    #[test]
    fn editing_closes_the_menu() {
        let mut w = typed("/srv/m");
        w.complete(several("/srv/", &["movies", "music"]));
        w.insert_str("u");
        assert!(w.menu().is_none());
        w.complete(several("/srv/", &["mua", "mub"]));
        w.set("other");
        assert!(w.menu().is_none());
    }

    // -- drawing ---------------------------------------------------------

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    const CURSOR: Option<Style> = Some(Style::new());

    #[test]
    fn a_short_field_shows_whole_with_the_cursor_at_the_end() {
        let w = typed("abc");
        assert_eq!(text(&w.spans(10, Style::default(), CURSOR)), "abc\u{2588}");
        assert_eq!(text(&w.spans(10, Style::default(), None)), "abc");
        assert!(w.spans(0, Style::default(), CURSOR).is_empty());
    }

    #[test]
    fn the_cursor_shows_on_the_character_it_is_on() {
        let mut w = typed("abc");
        w.handle_key(key(KeyCode::Left));
        let spans = w.spans(10, Style::default(), CURSOR);
        assert_eq!(text(&spans), "abc");
        let on = spans.iter().find(|s| s.content == "c").unwrap();
        assert!(on.style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn a_long_field_scrolls_to_keep_the_cursor_in_view() {
        let mut w = typed("0123456789abcdefghij");
        // 8 cells: `…`, six characters, the cursor block.
        assert_eq!(
            text(&w.spans(8, Style::default(), CURSOR)),
            "\u{2026}efghij\u{2588}"
        );

        // Moving left inside the view keeps it, only taking the cell the
        // end-of-text cursor no longer needs.
        press(&mut w, &[key(KeyCode::Left), key(KeyCode::Left)]);
        assert_eq!(
            text(&w.spans(8, Style::default(), CURSOR)),
            "\u{2026}defghij"
        );

        // Off the left edge it follows, with a `…` on both cut sides.
        press(&mut w, &[ctrl(KeyCode::Char('a'))]);
        assert_eq!(
            text(&w.spans(8, Style::default(), CURSOR)),
            "0123456\u{2026}"
        );
        for _ in 0..10 {
            w.handle_key(key(KeyCode::Right));
        }
        let shown = text(&w.spans(8, Style::default(), CURSOR));
        assert!(
            shown.contains('a'),
            "the cursor's character is on screen: {shown}"
        );
        assert!(shown.starts_with('\u{2026}') && shown.ends_with('\u{2026}'));
        assert_eq!(shown.chars().count(), 8);
    }

    #[test]
    fn a_shrunk_field_scrolls_back() {
        let mut w = typed("0123456789abcdefghij");
        w.spans(8, Style::default(), CURSOR);
        w.handle_key(ctrl(KeyCode::Char('u')));
        w.insert_str("abc");
        assert_eq!(text(&w.spans(8, Style::default(), CURSOR)), "abc\u{2588}");
    }

    #[test]
    fn wide_characters_count_two_cells() {
        let w = typed("東京東京東京");
        let shown = text(&w.spans(6, Style::default(), CURSOR));
        // `…` (1) + 東京 (4) + cursor (1).
        assert_eq!(shown, "\u{2026}東京\u{2588}");
        assert!(UnicodeWidthStr::width(shown.as_str()) <= 6);
    }

    #[test]
    fn a_combining_mark_moves_and_deletes_with_its_character() {
        // `e` + U+0301 is one `é` on screen.
        let mut w = typed("cafe\u{301}s");
        w.handle_key(key(KeyCode::Left));
        w.handle_key(key(KeyCode::Left));
        assert_eq!(shown(&w), "caf|e\u{301}s");
        w.handle_key(key(KeyCode::Right));
        assert_eq!(shown(&w), "cafe\u{301}|s");
        w.handle_key(key(KeyCode::Backspace));
        assert_eq!(shown(&w), "caf|s");
        // And it is part of the word.
        let mut w = typed("/cafe\u{301}");
        w.handle_key(ctrl(KeyCode::Char('w')));
        assert_eq!(w.value(), "/");
    }

    #[test]
    fn the_cursor_draws_a_character_with_its_marks() {
        let mut w = typed("ae\u{301}b");
        w.handle_key(key(KeyCode::Left));
        w.handle_key(key(KeyCode::Left));
        let spans = w.spans(10, Style::default(), CURSOR);
        let on: Vec<_> = spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
            .collect();
        assert_eq!(on.len(), 1);
        assert_eq!(on[0].content, "e\u{301}");
    }

    #[test]
    fn trailing_marks_need_no_cut_marker() {
        // The case the random test found: zero-width marks at the end fit in
        // no cells, so they must not make room for a `…` that overflows.
        let mut w = typed("东éé-é.东\u{301}   \u{301}\u{301}");
        w.handle_key(key(KeyCode::Left));
        w.handle_key(key(KeyCode::Left));
        let drawn = text(&w.spans(9, Style::default(), CURSOR));
        assert!(UnicodeWidthStr::width(drawn.as_str()) <= 9, "{drawn:?}");
    }

    /// Random keys, pastes, completions and widths: nothing panics, the
    /// cursor stays on a character boundary, and the drawn field fits its
    /// width with the cursor on screen. Seeded, so a failure reproduces.
    #[test]
    fn random_editing_keeps_every_invariant() {
        let mut seed: u64 = 0x5eed_1020;
        let mut next = |n: usize| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) as usize % n
        };
        let typing = [
            'a', 'Z', '7', '/', ' ', '.', '-', 'é', '東', '\u{301}', '\x1b',
        ];
        let keys = [
            key(KeyCode::Left),
            key(KeyCode::Right),
            key(KeyCode::Home),
            key(KeyCode::End),
            key(KeyCode::Backspace),
            key(KeyCode::Delete),
            ctrl(KeyCode::Left),
            ctrl(KeyCode::Right),
            ctrl(KeyCode::Char('w')),
            ctrl(KeyCode::Char('u')),
            ctrl(KeyCode::Char('k')),
            ctrl(KeyCode::Char('z')),
            ctrl(KeyCode::Char('y')),
            alt(KeyCode::Char('d')),
            key(KeyCode::Tab),
            key(KeyCode::Down),
            key(KeyCode::Up),
            key(KeyCode::Esc),
            key(KeyCode::Enter),
        ];
        for round in 0..200 {
            let mut w = InputWidget::with_cap(1 + next(40));
            for _ in 0..300 {
                match next(10) {
                    0..=3 => {
                        w.handle_key(key(KeyCode::Char(typing[next(typing.len())])));
                    }
                    4 => {
                        let s: String = (0..next(12)).map(|_| typing[next(typing.len())]).collect();
                        w.insert_str(&s);
                    }
                    5 => w.complete(several(
                        w.before_cursor(),
                        &["x", "東y", "zz"][..1 + next(3)],
                    )),
                    _ => {
                        let k = keys[next(keys.len())];
                        if w.menu_key(k) == MenuKey::Pass {
                            w.handle_key(k);
                        }
                    }
                }
                assert!(w.as_str().is_char_boundary(w.before_cursor().len()));
                assert!(w.as_str().chars().count() <= w.cap, "round {round}");
                let width = 4 + next(30);
                let spans = w.spans(width, Style::default(), CURSOR);
                let drawn = text(&spans);
                assert!(
                    UnicodeWidthStr::width(drawn.as_str()) <= width,
                    "round {round}: {drawn:?} is wider than {width} (text {:?}, cursor {}, scroll {})",
                    w.as_str(),
                    w.cursor(),
                    w.scroll.get()
                );
                let on_screen = spans.iter().any(|s| {
                    s.content == "\u{2588}" || s.style.add_modifier.contains(Modifier::REVERSED)
                });
                assert!(on_screen, "round {round}: no cursor in {drawn:?}");
            }
        }
    }

    #[test]
    fn valid_hex_magnet() {
        let link = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567";
        assert!(validate_magnet(link).is_ok());
    }

    #[test]
    fn valid_hex_magnet_with_params() {
        let link = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=test";
        assert!(validate_magnet(link).is_ok());
    }

    #[test]
    fn valid_base32_magnet() {
        let link = "magnet:?xt=urn:btih:ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
        assert!(validate_magnet(link).is_ok());
    }

    #[test]
    fn missing_magnet_prefix() {
        assert!(validate_magnet("http://example.com").is_err());
    }

    #[test]
    fn missing_btih() {
        assert!(validate_magnet("magnet:?dn=test").is_err());
    }

    #[test]
    fn wrong_hash_length() {
        let link = "magnet:?xt=urn:btih:0123456789";
        let err = validate_magnet(link).unwrap_err();
        assert!(err.contains("40 hex or 32 base32"));
    }

    #[test]
    fn invalid_hex_chars() {
        let link = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef0123456g";
        assert!(validate_magnet(link).is_err());
    }

    #[test]
    fn whitespace_trimmed() {
        let link = "  magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567  ";
        assert!(validate_magnet(link).is_ok());
    }

    #[test]
    fn torrent_file_not_found() {
        assert!(validate_torrent_source("/nonexistent/path.torrent").is_err());
    }
}
