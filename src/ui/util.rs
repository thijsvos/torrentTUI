/// Truncate to `max_len` *terminal columns*, appending "..." when it does not
/// fit. Measures display width rather than `char` count: a CJK or emoji name
/// occupies two columns per character, so counting chars let a 45-char name
/// take 90 columns and push the neighbouring table columns off screen. Names
/// come from torrent metadata, so that was attacker-controlled.
///
/// For `max_len` under 3 the result is just "...", which is *wider* than
/// `max_len` — callers laying out fixed-width columns must not go that narrow.
pub fn truncate(s: &str, max_len: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    // Fast path: byte length is an upper bound on display width, so if it
    // already fits we skip the per-character pass entirely.
    if s.len() <= max_len {
        return s.to_string();
    }
    if s.width() <= max_len {
        return s.to_string();
    }
    let budget = max_len.saturating_sub(3);
    let mut out = String::new();
    let mut used = 0usize;
    for c in s.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push_str("...");
    out
}

/// Pad `s` to exactly `width` terminal columns, truncating first if needed.
/// `format!("{:<45}")` pads by `char` count, which is wrong for the same reason
/// [`truncate`] is.
pub fn pad_to_width(s: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let t = truncate(s, width);
    let w = t.width();
    let mut out = t;
    for _ in w..width {
        out.push(' ');
    }
    out
}

/// Strip control characters (except tab) and Unicode bidi controls so the
/// string is safe to render in the TUI. Torrent names come from
/// attacker-controlled metadata — and indexer search results are the same
/// threat class — so this runs at those boundaries.
///
/// Bidi controls need stripping separately because `char::is_control` covers
/// only the Cc category: U+202E (right-to-left override) and friends are Cf
/// and sail through it, letting a name like `gpj.exe\u{202E}iva.` render
/// visually reversed as an extension-spoofing trick.
///
/// Deliberately does *not* escape markup. It used to, for the benefit of one
/// consumer — the Linux libnotify body — and every other consumer paid for it:
/// an ordinary release name like `Rock & Roll` rendered in the table as
/// `Rock &amp; Roll`. Markup escaping now lives in [`escape_markup`], called
/// only at the notification site.
pub fn sanitize_display(s: &str) -> String {
    s.chars()
        .filter(|c| (!c.is_control() || *c == '\t') && !is_bidi_control(*c))
        .collect()
}

/// Unicode bidirectional/format characters that visually reorder text:
/// embeddings/overrides (U+202A-202E), isolates (U+2066-2069) and the LTR/RTL
/// marks (U+200E/200F). Crate-visible so input paths that render their buffer
/// raw (the search query bar and titles) can reject these at entry instead of
/// sanitizing at every render site.
pub(crate) fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}'
    )
}

/// Escape the three characters Pango treats as markup. Call this on top of
/// [`sanitize_display`] when the string is going into a Linux desktop
/// notification body, and nowhere else — the terminal is not a markup renderer.
// Only the Linux/Windows notification path calls this; macOS plays a sound
// instead, so the function is genuinely unused there.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn escape_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            other => out.push(other),
        }
    }
    out
}

/// True when `name`'s extension matches a common streamable media format
/// (the kinds a desktop media player will start playing from a partial file
/// via HTTP range requests). Case-insensitive on the extension only.
pub fn is_streamable_media(name: &str) -> bool {
    let ext = match name.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => ext,
        _ => return false,
    };
    let lower = ext.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        // Video
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "mpg" | "mpeg" | "m4v" | "ogv" | "ts"
        // Audio
        | "mp3" | "flac" | "ogg" | "opus" | "m4a" | "wav" | "aac"
    )
}

/// Centre a popup of `percent_x` x `percent_y` inside `r`.
///
/// Widens to `u32` before multiplying: the naive `r.width * percent_x / 100`
/// overflows `u16` past about 1090 columns, which panics in debug and wraps in
/// release. Saturating the multiply is not enough either — it pins at
/// `u16::MAX` and yields a popup far too small.
pub fn centered_rect(
    percent_x: u16,
    percent_y: u16,
    r: ratatui::layout::Rect,
) -> ratatui::layout::Rect {
    let popup_width = (r.width as u32 * percent_x as u32 / 100) as u16;
    let popup_height = (r.height as u32 * percent_y as u32 / 100) as u16;
    let x = r.x + (r.width.saturating_sub(popup_width)) / 2;
    let y = r.y + (r.height.saturating_sub(popup_height)) / 2;
    ratatui::layout::Rect::new(x, y, popup_width, popup_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_unchanged() {
        assert_eq!(truncate("abc", 10), "abc");
    }

    #[test]
    fn truncate_long_ascii() {
        assert_eq!(truncate("abcdefghij", 7), "abcd...");
    }

    #[test]
    fn truncate_emoji_does_not_panic() {
        // 4-byte emoji chars must not be split mid-byte.
        let name = "abc\u{1F389}def\u{1F389}ghi\u{1F389}jkl";
        let out = truncate(name, 8);
        assert!(out.ends_with("..."));
        // Emoji are two columns wide, so the budget is in columns, not chars.
        assert!(unicode_width::UnicodeWidthStr::width(out.as_str()) <= 8);
    }

    #[test]
    fn truncate_cjk_fits_the_column_budget() {
        // Was `chars().count() == 6`, which is the measure that caused the bug:
        // six CJK chars occupy twelve columns. The budget is columns.
        let name = "日本語のトレントテスト";
        let out = truncate(name, 6);
        assert!(
            unicode_width::UnicodeWidthStr::width(out.as_str()) <= 6,
            "{out:?} exceeds 6 columns"
        );
        assert!(out.ends_with("..."));
    }

    #[test]
    fn truncate_latin_accented() {
        let out = truncate("Película_grande_de_película_film", 12);
        assert_eq!(out.chars().count(), 12);
        assert!(out.ends_with("..."));
    }

    #[test]
    fn sanitize_strips_control_chars() {
        let s = "hello\x07\x1b[31mworld\r\n";
        let cleaned = sanitize_display(s);
        assert_eq!(cleaned, "hello[31mworld");
    }

    #[test]
    fn truncate_measures_terminal_columns_not_chars() {
        // Ten CJK chars are twenty columns wide, so a 12-column budget must
        // cut them — counting chars would have let all ten through.
        let cjk =
            "\u{4f60}\u{597d}\u{4f60}\u{597d}\u{4f60}\u{597d}\u{4f60}\u{597d}\u{4f60}\u{597d}";
        let out = truncate(cjk, 12);
        assert!(
            unicode_width::UnicodeWidthStr::width(out.as_str()) <= 12,
            "{out:?} is wider than 12 columns"
        );
        assert!(out.ends_with("..."));
        // ASCII behaviour is unchanged.
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("abcdefghij", 8), "abcde...");
    }

    #[test]
    fn pad_to_width_pads_by_columns() {
        use unicode_width::UnicodeWidthStr;
        assert_eq!(pad_to_width("ab", 5).width(), 5);
        assert_eq!(pad_to_width("\u{4f60}\u{597d}", 6).width(), 6);
        assert_eq!(pad_to_width("abcdefghij", 6).width(), 6);
    }

    #[test]
    fn sanitize_display_leaves_markup_characters_alone() {
        // The terminal is not a markup renderer. Escaping here turned an
        // ordinary release name into `Rock &amp; Roll` in the table.
        assert_eq!(sanitize_display("Rock & Roll (2010)"), "Rock & Roll (2010)");
        assert_eq!(sanitize_display("Sherlock <2010>"), "Sherlock <2010>");
        // ...but control characters are still stripped.
        assert_eq!(sanitize_display("a\u{7}b\u{1b}c"), "abc");
    }

    #[test]
    fn centered_rect_survives_a_very_wide_terminal() {
        use ratatui::layout::Rect;
        // `r.width * percent_x` overflows u16 past ~1090 columns.
        let r = Rect::new(0, 0, 4000, 2000);
        let popup = centered_rect(60, 80, r);
        assert_eq!(popup.width, 2400);
        assert_eq!(popup.height, 1600);
        assert!(popup.x + popup.width <= r.width);
    }

    #[test]
    fn escape_markup_escapes_pango_specials() {
        let s = "<a href=\"x\">click</a> & more";
        assert_eq!(
            escape_markup(s),
            "&lt;a href=\"x\"&gt;click&lt;/a&gt; &amp; more"
        );
    }

    #[test]
    fn sanitize_keeps_unicode() {
        assert_eq!(sanitize_display("Película\u{1F389}"), "Película\u{1F389}");
    }

    #[test]
    fn sanitize_keeps_tab() {
        assert_eq!(sanitize_display("a\tb"), "a\tb");
    }

    #[test]
    fn sanitize_strips_bidi_controls() {
        // U+202E reverses everything after it visually; is_control() does not
        // catch it (category Cf, not Cc).
        assert_eq!(sanitize_display("gpj.exe\u{202E}iva."), "gpj.exeiva.");
        assert_eq!(sanitize_display("a\u{202A}b\u{202B}c"), "abc");
        assert_eq!(sanitize_display("a\u{2066}b\u{2069}c"), "abc");
        assert_eq!(sanitize_display("a\u{200E}b\u{200F}c"), "abc");
        // Ordinary RTL text is untouched — only the invisible controls go.
        assert_eq!(sanitize_display("مرحبا"), "مرحبا");
    }

    #[test]
    fn streamable_video_extensions() {
        for ext in ["mp4", "mkv", "webm", "MOV", "Mp4", "AVI", "m4v"] {
            let name = format!("clip.{ext}");
            assert!(is_streamable_media(&name), "expected streamable: {ext}");
        }
    }

    #[test]
    fn streamable_audio_extensions() {
        for ext in ["mp3", "FLAC", "ogg", "opus", "m4a", "wav"] {
            let name = format!("song.{ext}");
            assert!(is_streamable_media(&name), "expected streamable: {ext}");
        }
    }

    #[test]
    fn not_streamable_extensions() {
        for name in [
            "archive.iso",
            "readme.txt",
            "image.png",
            "doc.pdf",
            "noext",
            "trailing.",
            ".hidden",
        ] {
            assert!(
                !is_streamable_media(name),
                "expected not streamable: {name}"
            );
        }
    }

    #[test]
    fn streamable_handles_multiple_dots() {
        // Only the trailing extension counts.
        assert!(is_streamable_media("Show.S01E02.1080p.mkv"));
        assert!(!is_streamable_media("Show.S01E02.1080p.txt"));
    }
}
