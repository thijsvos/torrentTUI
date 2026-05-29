/// Char-aware truncation. Returns `s` unchanged if its character count is at
/// most `max_len`; otherwise returns the first `max_len - 3` chars followed by
/// "...". Slicing strings by byte index (the previous implementation) panics
/// when the cut lands in the middle of a multi-byte UTF-8 character.
pub fn truncate(s: &str, max_len: usize) -> String {
    if s.chars().count() > max_len {
        let prefix: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{prefix}...")
    } else {
        s.to_string()
    }
}

/// Strip control characters (except tab) and HTML-escape the result so the
/// string is safe to render in the TUI and to embed in Linux desktop
/// notifications (libnotify treats body text as Pango markup). Torrent names
/// come from attacker-controlled metadata, so this runs at the engine boundary.
pub fn sanitize_display(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_control() && c != '\t' {
            continue;
        }
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
        assert!(out.chars().count() <= 8);
    }

    #[test]
    fn truncate_cjk_does_not_panic() {
        let name = "日本語のトレントテスト";
        let out = truncate(name, 6);
        assert_eq!(out.chars().count(), 6);
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
    fn sanitize_html_escape() {
        let s = "<a href=\"x\">click</a> & more";
        assert_eq!(
            sanitize_display(s),
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
