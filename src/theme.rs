//! Built-in colour themes.
//!
//! A dozen well-known dark colour schemes, bundled as ready-to-use presets.
//! Each entry carries a packed `0xRRGGBB` palette — background, text, the
//! semantic states, a muted ink and the scheme's own selection colour — which
//! [`Palette::theme`] maps onto the semantic slots below.
//!
//! The set and the first colour mapping came from
//! [OpenCode](https://github.com/anomalyco/opencode)'s MIT-licensed themes
//! (`packages/ui/src/theme/themes/*.json`). Each preset was then re-mapped
//! onto its scheme's upstream palette so a status keeps the meaning it has
//! under `system`: cyan-ish chrome, a magenta-ish upload accent, blue-ish
//! Downloading, green Seeding, yellow Paused, red errors. The tests below
//! enforce that no two status colours blur together and that every preset
//! stays readable on its own background.
//!
//! `system` reproduces TorrentTUI's original ANSI colours exactly, so it is the
//! default and a first launch is unchanged.
//!
//! A [`Theme`] is a flat set of semantic slots. Renderers never name a raw
//! `Color` themselves, so switching themes re-colours every widget at once.

use std::sync::LazyLock;

use ratatui::style::Color;

/// Semantic colour slots shared by every widget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    /// Borders, prompts, the active tab, the info hash — the UI's accent.
    pub primary: Color,
    /// Secondary accent: the upload speed, the "fetching metadata" state.
    pub accent: Color,
    /// Download speed, seeding, healthy state.
    pub success: Color,
    /// Informational messages, paused, capped.
    pub warning: Color,
    /// Errors, stalled/blocked, destructive actions.
    pub error: Color,
    /// Downloading state.
    pub info: Color,
    /// Primary text.
    pub text: Color,
    /// Secondary text: status-bar hints, the ratio.
    pub text_dim: Color,
    /// Labels, borders, placeholders — the quietest ink.
    pub muted: Color,
    /// Background of a marked row.
    pub bg_element: Color,
    /// Background of the highlighted (cursor) row in every table and list.
    /// Its own slot rather than `muted`: `muted` is an ink tuned to be quiet
    /// on the background, and as a background it left row text unreadable.
    pub selection_bg: Color,
    /// Progress-column ramp: `<25`, `25-50`, `50-75`, `75-100`, `100`.
    pub progress: [Color; 5],
}

impl Theme {
    /// The default theme: TorrentTUI's original ANSI palette, byte for byte.
    /// Kept hand-written (not derived from the presets) so the default look is
    /// exactly what the app showed before theming.
    pub const fn system() -> Theme {
        Theme {
            primary: Color::Cyan,
            accent: Color::Magenta,
            success: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,
            info: Color::Blue,
            text: Color::White,
            text_dim: Color::Gray,
            muted: Color::DarkGray,
            bg_element: Color::Indexed(236),
            selection_bg: Color::DarkGray,
            progress: [
                Color::Red,
                Color::Rgb(255, 165, 0),
                Color::Yellow,
                Color::LightGreen,
                Color::Green,
            ],
        }
    }

    /// The progress-column colour for a completion percentage. Thresholds match
    /// the pre-theming gradient, and `system` reproduces it exactly.
    pub fn progress_color(&self, percent: f64) -> Color {
        if percent >= 100.0 {
            self.progress[4]
        } else if percent >= 75.0 {
            self.progress[3]
        } else if percent >= 50.0 {
            self.progress[2]
        } else if percent >= 25.0 {
            self.progress[1]
        } else {
            self.progress[0]
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::system()
    }
}

/// Expand a packed `0xRRGGBB` literal into a ratatui colour.
const fn rgb(hex: u32) -> Color {
    Color::Rgb(
        ((hex >> 16) & 0xFF) as u8,
        ((hex >> 8) & 0xFF) as u8,
        (hex & 0xFF) as u8,
    )
}

/// One channel of [`mix`].
const fn mix_channel(a: u32, b: u32, t: u32, shift: u32) -> u32 {
    let av = (a >> shift) & 0xFF;
    let bv = (b >> shift) & 0xFF;
    (av * (100 - t) + bv * t + 50) / 100
}

/// Blend two packed colours, `t` percent of the way from `a` to `b`, rounded.
/// Integer maths so it can run in a `const` context.
const fn mix(a: u32, b: u32, t: u32) -> u32 {
    (mix_channel(a, b, t, 16) << 16) | (mix_channel(a, b, t, 8) << 8) | mix_channel(a, b, t, 0)
}

/// A preset's palette, packed as
/// `[background, text, primary, accent, success, warning, error, info, muted, selection]`.
struct Palette {
    id: &'static str,
    name: &'static str,
    hex: [u32; 10],
}

impl Palette {
    /// Derive the semantic slots. Most come straight from the palette; one
    /// blend darkens the marked-row background, one builds `text_dim`, and
    /// two build the progress ramp, so every widget is covered without a
    /// per-theme override table.
    const fn theme(&self) -> Theme {
        let [neutral, ink, primary, accent, success, warning, error, info, comment, selection] =
            self.hex;
        Theme {
            primary: rgb(primary),
            accent: rgb(accent),
            success: rgb(success),
            warning: rgb(warning),
            error: rgb(error),
            info: rgb(info),
            text: rgb(ink),
            text_dim: rgb(mix(ink, comment, 50)),
            muted: rgb(comment),
            bg_element: rgb(mix(neutral, ink, 6)),
            selection_bg: rgb(selection),
            progress: [
                rgb(error),
                rgb(mix(error, warning, 50)),
                rgb(warning),
                rgb(mix(warning, success, 50)),
                rgb(success),
            ],
        }
    }
}

/// The built-in palettes, one line per theme — `rustfmt` is opted out to keep
/// them that way.
#[rustfmt::skip]
const PALETTES: &[Palette] = &[
    Palette { id: "catppuccin", name: "Catppuccin Mocha", hex: [0x1e1e2e, 0xcdd6f4, 0x89dceb, 0xcba6f7, 0xa6e3a1, 0xf9e2af, 0xf38ba8, 0x89b4fa, 0x7f849c, 0x45475a] },
    Palette { id: "dracula", name: "Dracula", hex: [0x282a36, 0xf8f8f2, 0x8be9fd, 0xff79c6, 0x50fa7b, 0xf1fa8c, 0xff5555, 0xbd93f9, 0x6272a4, 0x44475a] },
    Palette { id: "everforest", name: "Everforest", hex: [0x2d353b, 0xd3c6aa, 0x83c092, 0xd699b6, 0xa7c080, 0xdbbc7f, 0xe67e80, 0x7fbbb3, 0x859289, 0x475258] },
    Palette { id: "github", name: "GitHub Dark", hex: [0x0d1117, 0xc9d1d9, 0x39c5cf, 0xbc8cff, 0x3fb950, 0xd29922, 0xf85149, 0x58a6ff, 0x8b949e, 0x264f78] },
    Palette { id: "gruvbox", name: "Gruvbox Dark", hex: [0x282828, 0xebdbb2, 0x8ec07c, 0xd3869b, 0xb8bb26, 0xfabd2f, 0xfb4934, 0x83a598, 0x928374, 0x504945] },
    Palette { id: "kanagawa", name: "Kanagawa", hex: [0x1f1f28, 0xdcd7ba, 0x7fb4ca, 0xd27e99, 0x98bb6c, 0xe6c384, 0xff5d62, 0x7e9cd8, 0x727169, 0x2d4f67] },
    Palette { id: "monokai", name: "Monokai", hex: [0x272822, 0xf8f8f2, 0x66d9ef, 0xae81ff, 0xa6e22e, 0xe6db74, 0xf92672, 0x66d9ef, 0x75715e, 0x49483e] },
    Palette { id: "nord", name: "Nord", hex: [0x2e3440, 0xe5e9f0, 0x88c0d0, 0xb48ead, 0xa3be8c, 0xebcb8b, 0xbf616a, 0x81a1c1, 0x7b88a1, 0x434c5e] },
    Palette { id: "one-dark", name: "One Dark", hex: [0x282c34, 0xabb2bf, 0x56b6c2, 0xc678dd, 0x98c379, 0xe5c07b, 0xe06c75, 0x61afef, 0x7f848e, 0x3e4451] },
    Palette { id: "rosepine", name: "Rose Pine", hex: [0x191724, 0xe0def4, 0xebbcba, 0xc4a7e7, 0x31748f, 0xf6c177, 0xeb6f92, 0x9ccfd8, 0x6e6a86, 0x403d52] },
    Palette { id: "solarized", name: "Solarized Dark", hex: [0x002b36, 0x93a1a1, 0x2aa198, 0xd33682, 0x859900, 0xb58900, 0xdc322f, 0x268bd2, 0x657b83, 0x073642] },
    Palette { id: "tokyonight", name: "Tokyo Night", hex: [0x1a1b26, 0xc0caf5, 0x7dcfff, 0xbb9af7, 0x9ece6a, 0xe0af68, 0xf7768e, 0x7aa2f7, 0x737aa2, 0x283457] },
];

/// One selectable theme: a stable `id` (written to `config.toml`), a display
/// `name`, and its palette.
pub struct ThemeEntry {
    pub id: &'static str,
    pub name: &'static str,
    pub theme: Theme,
}

/// The theme registry: `system` first (the default), then the built-in
/// palettes. Built once on first use.
pub static THEMES: LazyLock<Vec<ThemeEntry>> = LazyLock::new(|| {
    let mut themes = Vec::with_capacity(PALETTES.len() + 1);
    themes.push(ThemeEntry {
        id: "system",
        name: "System",
        theme: Theme::system(),
    });
    themes.extend(PALETTES.iter().map(|p| ThemeEntry {
        id: p.id,
        name: p.name,
        theme: p.theme(),
    }));
    themes
});

/// Resolve a theme id — typically a `[ui] theme` value — to its entry. An
/// unknown id falls back to `system`, so a stale or mistyped config value can
/// never stop the app from starting.
pub fn by_name(name: &str) -> &'static ThemeEntry {
    THEMES.iter().find(|e| e.id == name).unwrap_or(&THEMES[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Linear-light sRGB channels of a preset colour.
    fn linear(c: Color) -> [f64; 3] {
        let Color::Rgb(r, g, b) = c else {
            panic!("presets are 24-bit, got {c:?}");
        };
        [r, g, b].map(|v| {
            let v = f64::from(v) / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        })
    }

    /// WCAG relative luminance.
    fn luminance(c: Color) -> f64 {
        let [r, g, b] = linear(c);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    /// WCAG contrast ratio, 1.0 (identical) to 21.0 (black on white).
    fn contrast(a: Color, b: Color) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// CIE76 colour difference in L*a*b* (D65). Around 2 is just noticeable;
    /// below ~20 two status colours read as the same hue at a glance.
    fn delta_e(a: Color, b: Color) -> f64 {
        fn lab(c: Color) -> [f64; 3] {
            let [r, g, b] = linear(c);
            let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
            let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
            let f = |t: f64| {
                if t > 0.008856 {
                    t.cbrt()
                } else {
                    7.787 * t + 16.0 / 116.0
                }
            };
            let (fx, fy, fz) = (f(x), f(y), f(z));
            [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
        }
        let (a, b) = (lab(a), lab(b));
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    }

    /// Every preset with its own background, which the theme itself does not
    /// carry (the UI never paints it).
    fn presets() -> impl Iterator<Item = (&'static str, Color, Theme)> {
        PALETTES.iter().map(|p| (p.id, rgb(p.hex[0]), p.theme()))
    }

    #[test]
    fn theme_ids_are_unique_and_non_empty() {
        for (i, a) in THEMES.iter().enumerate() {
            assert!(!a.id.is_empty(), "empty id");
            assert!(!a.name.is_empty(), "empty name for {}", a.id);
            assert!(
                !THEMES[..i].iter().any(|b| b.id == a.id),
                "duplicate id {}",
                a.id
            );
        }
    }

    #[test]
    fn the_registry_has_system_first_then_the_built_in_palettes() {
        assert_eq!(THEMES[0].id, "system");
        assert_eq!(THEMES.len(), PALETTES.len() + 1);
        assert!(THEMES.iter().any(|e| e.id == "gruvbox"));
    }

    #[test]
    fn unknown_name_falls_back_to_system() {
        assert_eq!(by_name("system").id, "system");
        assert_eq!(by_name("no-such-theme").id, "system");
        assert_eq!(by_name("").id, "system");
    }

    #[test]
    fn a_built_in_palette_differs_from_the_system_default() {
        // Guards against an expanded table collapsing to black, i.e. every
        // preset ending up identical to `system`.
        assert_ne!(by_name("gruvbox").theme, Theme::system());
    }

    #[test]
    fn system_progress_ramp_matches_the_original_gradient() {
        let t = Theme::system();
        assert_eq!(t.progress_color(0.0), Color::Red);
        assert_eq!(t.progress_color(24.9), Color::Red);
        assert_eq!(t.progress_color(25.0), Color::Rgb(255, 165, 0));
        assert_eq!(t.progress_color(49.9), Color::Rgb(255, 165, 0));
        assert_eq!(t.progress_color(50.0), Color::Yellow);
        assert_eq!(t.progress_color(74.9), Color::Yellow);
        assert_eq!(t.progress_color(75.0), Color::LightGreen);
        assert_eq!(t.progress_color(99.9), Color::LightGreen);
        assert_eq!(t.progress_color(100.0), Color::Green);
    }

    #[test]
    fn system_highlights_rows_the_way_the_app_always_did() {
        // Before theming every table highlighted its cursor row with a
        // DarkGray background; `system` must keep that exact look.
        assert_eq!(Theme::system().selection_bg, Color::DarkGray);
    }

    #[test]
    fn every_preset_keeps_its_status_colours_apart() {
        // Downloading, Seeding, Paused, Error and Fetching metadata (plus the
        // upload speed) are told apart by these five slots. Two that match —
        // or merely look alike — turn a status into a lookalike of another:
        // an upload speed in the error red reads as a failure.
        for (id, _, t) in presets() {
            let slots = [
                ("accent", t.accent),
                ("success", t.success),
                ("warning", t.warning),
                ("error", t.error),
                ("info", t.info),
            ];
            for (i, (a, ca)) in slots.iter().enumerate() {
                for (b, cb) in &slots[i + 1..] {
                    let d = delta_e(*ca, *cb);
                    assert!(d >= 20.0, "{id}: {a} and {b} look alike (ΔE {d:.1})");
                }
            }
        }
    }

    #[test]
    fn every_preset_is_readable_on_its_own_background() {
        for (id, bg, t) in presets() {
            let text = contrast(t.text, bg);
            assert!(text >= 4.5, "{id}: text on background is {text:.2}:1");
            // 3:1 is the WCAG floor for UI components and bold text, which is
            // what status words, speeds and borders are.
            for (name, c) in [
                ("primary", t.primary),
                ("accent", t.accent),
                ("success", t.success),
                ("warning", t.warning),
                ("error", t.error),
                ("info", t.info),
                ("muted", t.muted),
            ] {
                let r = contrast(c, bg);
                assert!(r >= 3.0, "{id}: {name} on background is {r:.2}:1");
            }
        }
    }

    #[test]
    fn the_selection_bar_is_readable_in_every_preset() {
        for (id, bg, t) in presets() {
            // Row text sits on the bar at full size.
            let text = contrast(t.text, t.selection_bg);
            assert!(
                text >= 4.5,
                "{id}: text on the selection bar is {text:.2}:1"
            );
            // Coloured cells (status, speeds) keep their hue on the bar and
            // must not vanish into it.
            for (name, c) in [
                ("accent", t.accent),
                ("success", t.success),
                ("warning", t.warning),
                ("error", t.error),
                ("info", t.info),
            ] {
                let r = contrast(c, t.selection_bg);
                assert!(r >= 2.0, "{id}: {name} on the selection bar is {r:.2}:1");
            }
            // The bar itself must stand out from the rows around it. It also
            // carries a `▶` marker and bold text, so colour is not the only cue.
            let bar = contrast(t.selection_bg, bg);
            assert!(
                bar >= 1.1,
                "{id}: selection bar is {bar:.2}:1 against the background"
            );
        }
    }
}
