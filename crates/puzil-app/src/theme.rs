//! Colours. The palette follows the window appearance (light or dark).

use gpui_kit::*;

/// Room kept clear at the top of a screen for the window's traffic lights. Zero off macOS,
/// where the desktop draws a title bar above the window instead.
pub const TRAFFIC_LIGHTS: f32 = if cfg!(target_os = "macos") { 38. } else { 0. };
/// Where the traffic lights sit: centred in the 48px header strip.
pub const TRAFFIC_LIGHTS_AT: (f32, f32) = (16., 17.);
/// Left inset that clears the traffic lights (macOS 26 draws them about 70px wide).
pub const TRAFFIC_LIGHTS_WIDTH: f32 = if cfg!(target_os = "macos") { 86. } else { 16. };

/// Makes an element drag the window (and zoom it on double-click), standing in for the
/// title bar we don't have.
pub fn drag_region<E: InteractiveElement>(e: E) -> E {
    e.on_mouse_down(MouseButton::Left, |ev, window, _| {
        if ev.click_count == 2 {
            window.titlebar_double_click();
        } else {
            window.start_window_move();
        }
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub rail: Hsla,
    /// The account panel at the foot of the sidebar.
    pub panel: Hsla,
    /// Icons and other interactive glyphs at rest.
    pub icon: Hsla,
    /// Floating surfaces: menus, the hover toolbar, autocomplete.
    pub popover: Hsla,
    pub sidebar: Hsla,
    pub bg: Hsla,
    pub bg_hover: Hsla,
    pub bg_selected: Hsla,
    pub input_bg: Hsla,
    /// The message composer's box and its outline.
    pub composer_bg: Hsla,
    pub composer_border: Hsla,
    pub border: Hsla,
    /// Day dividers in the message list.
    pub divider: Hsla,
    pub fg: Hsla,
    pub fg_strong: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub link: Hsla,
    pub accent: Hsla,
    pub unread_badge: Hsla,
    pub mention_badge: Hsla,
    pub mention_bg: Hsla,
    pub mention_fg: Hsla,
    pub mention_me_bg: Hsla,
    pub mention_me_fg: Hsla,
    pub selection: Hsla,
    /// Background of a passage someone reacted to (Overlay inline reactions).
    pub passage: Hsla,
    pub code_bg: Hsla,
    pub quote_bar: Hsla,
    pub danger: Hsla,
    pub new_marker: Hsla,
    pub reaction_bg: Hsla,
    pub reaction_mine_bg: Hsla,
    pub reaction_mine_border: Hsla,
    /// Pygments token colours: keyword, string, comment, number, name.function, other.
    pub code_kw: Hsla,
    pub code_str: Hsla,
    pub code_comment: Hsla,
    pub code_num: Hsla,
    pub code_fn: Hsla,
}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn ca(hex: u32, alpha: f32) -> Hsla {
    let mut h: Hsla = rgb(hex).into();
    h.a = alpha;
    h
}

thread_local! {
    static CHOICE: std::cell::RefCell<(Palette, crate::settings::Mode)> = std::cell::RefCell::new((Palette::Neutral, crate::settings::Mode::System));
}

/// The built-in themes, each with a light and a dark mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    /// Discord's greys.
    Neutral,
    /// Warm beige and brown.
    Earth,
    /// Plum and ember, like the app icon.
    Aubergine,
    /// Deep sea blues and teal.
    Ocean,
    /// Fun: coral, peach and pink.
    Sunset,
    /// Fun: ultraviolet and cyan.
    Neon,
}

/// The few colours a theme is made of; the rest follow from them.
struct Spec {
    dark: bool,
    rail: u32,
    sidebar: u32,
    bg: u32,
    hover: u32,
    selected: u32,
    panel: u32,
    border: u32,
    fg: u32,
    fg_strong: u32,
    muted: u32,
    faint: u32,
    accent: u32,
    link: u32,
    mention: u32,
    code_bg: u32,
}

impl Spec {
    fn theme(&self) -> Theme {
        let base = if self.dark { Theme::dark() } else { Theme::light() };
        let accent = c(self.accent);
        Theme {
            dark: self.dark,
            rail: c(self.rail),
            panel: c(self.panel),
            icon: c(self.muted),
            popover: c(self.panel),
            sidebar: c(self.sidebar),
            bg: c(self.bg),
            bg_hover: c(self.hover),
            bg_selected: c(self.selected),
            input_bg: c(self.hover),
            composer_bg: c(self.panel),
            composer_border: c(self.border),
            border: c(self.border),
            divider: c(self.border),
            fg: c(self.fg),
            fg_strong: c(self.fg_strong),
            muted: c(self.muted),
            faint: c(self.faint),
            link: c(self.link),
            accent,
            unread_badge: c(self.faint),
            mention_badge: c(self.mention),
            mention_bg: accent.opacity(if self.dark { 0.28 } else { 0.16 }),
            mention_fg: if self.dark { c(self.link) } else { accent },
            selection: accent.opacity(0.3),
            passage: ca(0xffd166, if self.dark { 0.3 } else { 0.4 }),
            code_bg: c(self.code_bg),
            quote_bar: c(self.border),
            danger: c(self.mention),
            new_marker: c(self.mention),
            reaction_bg: c(self.hover),
            reaction_mine_bg: accent.opacity(0.18),
            reaction_mine_border: accent,
            ..base
        }
    }
}

impl Palette {
    pub const ALL: [Palette; 6] = [Palette::Neutral, Palette::Earth, Palette::Aubergine, Palette::Ocean, Palette::Sunset, Palette::Neon];

    pub fn name(self) -> &'static str {
        match self {
            Self::Neutral => "neutral",
            Self::Earth => "earth",
            Self::Aubergine => "aubergine",
            Self::Ocean => "ocean",
            Self::Sunset => "sunset",
            Self::Neon => "ultraviolet",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Neutral => "Neutral",
            Self::Earth => "Earth",
            Self::Aubergine => "Aubergine",
            Self::Ocean => "Ocean",
            Self::Sunset => "Sunset",
            Self::Neon => "Ultraviolet",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "neutral" => Some(Self::Neutral),
            "earth" => Some(Self::Earth),
            "aubergine" => Some(Self::Aubergine),
            "ocean" => Some(Self::Ocean),
            "sunset" => Some(Self::Sunset),
            "ultraviolet" | "neon" => Some(Self::Neon),
            _ => None,
        }
    }
}

impl Theme {
    /// The palette for the window's appearance. `PUZIL_THEME=dark|light` overrides it (for
    /// desktops that report no preference).
    /// The theme for the window's appearance, under the chosen palette and mode (Settings;
    /// the workspace keeps them current for its server).
    pub fn for_appearance(a: WindowAppearance) -> Self {
        let (palette, mode) = CHOICE.with(|c| *c.borrow());
        let dark = match mode {
            crate::settings::Mode::Dark => true,
            crate::settings::Mode::Light => false,
            crate::settings::Mode::System => matches!(a, WindowAppearance::Dark | WindowAppearance::VibrantDark),
        };
        Self::of(palette, dark)
    }

    /// Sets the palette and mode `for_appearance` uses.
    pub fn choose(palette: Palette, mode: crate::settings::Mode) {
        CHOICE.with(|c| *c.borrow_mut() = (palette, mode));
    }

    pub fn of(p: Palette, dark: bool) -> Self {
        match (p, dark) {
            (Palette::Neutral, false) => Self::light(),
            (Palette::Neutral, true) => Self::dark(),
            (Palette::Earth, false) => Self::earth_light(),
            (Palette::Earth, true) => Self::earth_dark(),
            (Palette::Aubergine, d) => Self::aubergine(d),
            (Palette::Ocean, d) => Self::ocean(d),
            (Palette::Sunset, d) => Self::sunset(d),
            (Palette::Neon, d) => Self::neon(d),
        }
    }

    fn aubergine(dark: bool) -> Self {
        if dark {
            Spec { dark, rail: 0x150d1b, sidebar: 0x1a1121, bg: 0x211528, hover: 0x2a1c33, selected: 0x3d2748, panel: 0x2b1d35, border: 0x3a2845,
                fg: 0xeee2ee, fg_strong: 0xfbf3fa, muted: 0xb8a3bc, faint: 0x8f7a95, accent: 0xe8956b, link: 0xf2a880, mention: 0xf2706a, code_bg: 0x1a1121 }.theme()
        } else {
            Spec { dark, rail: 0xeee4ef, sidebar: 0xf3ebf3, bg: 0xfbf7fb, hover: 0xf3ebf3, selected: 0xe2d1e4, panel: 0xffffff, border: 0xe2d4e3,
                fg: 0x34233a, fg_strong: 0x1d1021, muted: 0x6c5572, faint: 0x8f7a95, accent: 0x7a3b80, link: 0xb4532a, mention: 0xd2443c, code_bg: 0xf3ebf3 }.theme()
        }
    }

    fn ocean(dark: bool) -> Self {
        if dark {
            Spec { dark, rail: 0x0a141c, sidebar: 0x0e1a24, bg: 0x12212d, hover: 0x182a38, selected: 0x1f3a4c, panel: 0x1a2c3a, border: 0x253b4b,
                fg: 0xdbe7ef, fg_strong: 0xf2f8fb, muted: 0x93aabb, faint: 0x6d8596, accent: 0x3fb7c7, link: 0x6dd0dd, mention: 0xf07a6a, code_bg: 0x0e1a24 }.theme()
        } else {
            Spec { dark, rail: 0xe3edf2, sidebar: 0xeaf2f6, bg: 0xf7fbfc, hover: 0xedf4f7, selected: 0xcfe2eb, panel: 0xffffff, border: 0xd6e4eb,
                fg: 0x1f3442, fg_strong: 0x0c1c27, muted: 0x4f6878, faint: 0x7790a0, accent: 0x0f7c8f, link: 0x0b6f9e, mention: 0xd24a3c, code_bg: 0xedf4f7 }.theme()
        }
    }

    fn sunset(dark: bool) -> Self {
        if dark {
            Spec { dark, rail: 0x1d0f17, sidebar: 0x24121c, bg: 0x2c1622, hover: 0x371d2b, selected: 0x55283d, panel: 0x3a1f2e, border: 0x4b2a3b,
                fg: 0xfbe6e3, fg_strong: 0xfff5f2, muted: 0xd9a9a8, faint: 0xa97d82, accent: 0xff7a6b, link: 0xffb38a, mention: 0xffcc4d, code_bg: 0x24121c }.theme()
        } else {
            Spec { dark, rail: 0xffe6dc, sidebar: 0xffeee6, bg: 0xfff8f4, hover: 0xffefe8, selected: 0xffd0c1, panel: 0xffffff, border: 0xf6d6ca,
                fg: 0x4a2430, fg_strong: 0x2c0f1a, muted: 0x8a5a62, faint: 0xb08690, accent: 0xe8505b, link: 0xd9480f, mention: 0xc81d6b, code_bg: 0xffefe8 }.theme()
        }
    }

    fn neon(dark: bool) -> Self {
        if dark {
            Spec { dark, rail: 0x0b0716, sidebar: 0x100a1f, bg: 0x140d27, hover: 0x1d1436, selected: 0x2f1f5a, panel: 0x1d1438, border: 0x2d2152,
                fg: 0xe7e2ff, fg_strong: 0xf7f5ff, muted: 0xa79fd1, faint: 0x7a71a8, accent: 0xa855f7, link: 0x22d3ee, mention: 0xff4fa3, code_bg: 0x100a1f }.theme()
        } else {
            Spec { dark, rail: 0xece6ff, sidebar: 0xf2eeff, bg: 0xfbfaff, hover: 0xf2eeff, selected: 0xdcd0ff, panel: 0xffffff, border: 0xe0d8fb,
                fg: 0x2a2150, fg_strong: 0x150e33, muted: 0x5f5590, faint: 0x8a80b8, accent: 0x7c3aed, link: 0x0891b2, mention: 0xdb2777, code_bg: 0xf2eeff }.theme()
        }
    }

    /// Earth, light: converted from an OKLCH palette.
    pub fn earth_light() -> Self {
        Self {
            rail: c(0xf4e7d5),
            sidebar: c(0xf2e5d3),
            panel: c(0xfffaf2),
            popover: c(0xfffaf2),
            bg: c(0xf8f0e3),
            bg_hover: c(0xf4eadb),
            bg_selected: c(0xd7bfa3),
            input_bg: c(0xf0e4d4),
            composer_bg: c(0xfffaf2),
            composer_border: c(0xd8c4ad),
            border: c(0xddcbb6),
            divider: c(0xd8c4ad),
            fg: c(0x382722),
            fg_strong: c(0x33231f),
            muted: c(0x705b50),
            faint: c(0x796359),
            icon: c(0x776155),
            link: c(0x4e2f2a),
            accent: c(0x4e2f2a),
            unread_badge: c(0x776155),
            mention_badge: c(0xa44735),
            mention_fg: c(0x4e2f2a),
            mention_me_fg: c(0x71450f),
            code_bg: c(0xf4eadb),
            quote_bar: c(0xcbb399),
            danger: c(0xa44735),
            new_marker: c(0xa44735),
            reaction_bg: c(0xead9c4),
            reaction_mine_border: c(0x4e2f2a),
            mention_bg: ca(0xead8ce, 1.0),
            mention_me_bg: ca(0xf5e7c8, 1.0),
            selection: ca(0xffde8b, 0.6),
            passage: ca(0xffde8b, 0.45),
            reaction_mine_bg: ca(0xead8ce, 1.0),
            ..Self::light()
        }
    }

    /// Earth, dark.
    pub fn earth_dark() -> Self {
        Self {
            rail: c(0x1c1614),
            sidebar: c(0x1d1714),
            panel: c(0x302620),
            popover: c(0x302620),
            bg: c(0x211a17),
            bg_hover: c(0x29201c),
            bg_selected: c(0x49372e),
            input_bg: c(0x322821),
            composer_bg: c(0x302620),
            composer_border: c(0x4d3d34),
            border: c(0x4a3a31),
            divider: c(0x4d3d34),
            fg: c(0xf1e5d5),
            fg_strong: c(0xf0e4d4),
            muted: c(0xb9a99a),
            faint: c(0xa18e7f),
            icon: c(0xad9b8c),
            link: c(0xd09a82),
            accent: c(0xc58f78),
            unread_badge: c(0xad9b8c),
            mention_badge: c(0xef8873),
            mention_fg: c(0xe7b49d),
            mention_me_fg: c(0xefbd75),
            code_bg: c(0x1c1715),
            quote_bar: c(0x665246),
            danger: c(0xef8873),
            new_marker: c(0xef8873),
            reaction_bg: c(0x3b2f28),
            reaction_mine_border: c(0xc58f78),
            mention_bg: ca(0x412d27, 1.0),
            mention_me_bg: ca(0x41321f, 1.0),
            selection: ca(0x8c6c1f, 0.6),
            passage: ca(0x8c6c1f, 0.45),
            reaction_mine_bg: ca(0x412d27, 1.0),
            ..Self::dark()
        }
    }

    pub fn dark() -> Self {
        Self {
            dark: true,
            rail: c(0x1e1f22),
            panel: c(0x232428),
            icon: c(0xb5bac1),
            popover: c(0x111214),
            sidebar: c(0x2b2d31),
            bg: c(0x313338),
            bg_hover: c(0x2e3035),
            bg_selected: c(0x404249),
            input_bg: c(0x383a40),
            composer_bg: c(0x383a40),
            composer_border: c(0x3f4147),
            border: c(0x1f2023),
            divider: c(0x3f4147),
            fg: c(0xdbdee1),
            fg_strong: c(0xf2f3f5),
            muted: c(0x949ba4),
            faint: c(0x6d6f78),
            link: c(0x00a8fc),
            accent: c(0x5865f2),
            unread_badge: c(0x80848e),
            mention_badge: c(0xf23f43),
            mention_bg: hsla(0.64, 0.6, 0.6, 0.25),
            mention_fg: c(0xc9cdfb),
            mention_me_bg: hsla(0.12, 0.9, 0.5, 0.25),
            mention_me_fg: c(0xf0c674),
            selection: hsla(0.6, 0.8, 0.55, 0.35),
            passage: hsla(0.13, 0.9, 0.5, 0.2),
            code_bg: c(0x2b2d31),
            quote_bar: c(0x4e5058),
            danger: c(0xf23f43),
            new_marker: c(0xf23f43),
            reaction_bg: c(0x2b2d31),
            reaction_mine_bg: hsla(0.64, 0.6, 0.6, 0.15),
            reaction_mine_border: c(0x5865f2),
            code_kw: c(0xc678dd),
            code_str: c(0x98c379),
            code_comment: c(0x7f848e),
            code_num: c(0xd19a66),
            code_fn: c(0x61afef),
        }
    }

    /// Discord's light greys, sampled from its current light theme.
    pub fn light() -> Self {
        Self {
            dark: false,
            rail: c(0xf3f3f4),
            panel: c(0xffffff),
            icon: c(0x5c5d63),
            popover: c(0xffffff),
            sidebar: c(0xf3f3f4),
            bg: c(0xfbfbfb),
            bg_hover: c(0xf3f3f4),
            bg_selected: c(0xdedee0),
            input_bg: c(0xebebed),
            composer_bg: c(0xffffff),
            composer_border: c(0xe2e2e4),
            border: c(0xe2e2e4),
            divider: c(0xe9e9ea),
            fg: c(0x2e2f34),
            fg_strong: c(0x060607),
            muted: c(0x4f5058),
            faint: c(0x909193),
            link: c(0x006ce7),
            accent: c(0x5865f2),
            unread_badge: c(0x80848e),
            mention_badge: c(0xda373c),
            mention_bg: hsla(0.64, 0.6, 0.6, 0.18),
            mention_fg: c(0x3c45a5),
            mention_me_bg: hsla(0.12, 0.9, 0.5, 0.25),
            mention_me_fg: c(0x8a5a00),
            selection: hsla(0.6, 0.8, 0.55, 0.3),
            passage: hsla(0.13, 0.95, 0.5, 0.22),
            code_bg: c(0xf3f3f4),
            quote_bar: c(0xc4c9ce),
            danger: c(0xda373c),
            new_marker: c(0xda373c),
            reaction_bg: c(0xf0f0f1),
            reaction_mine_bg: hsla(0.64, 0.6, 0.6, 0.15),
            reaction_mine_border: c(0x5865f2),
            code_kw: c(0xa626a4),
            code_str: c(0x50a14f),
            code_comment: c(0xa0a1a7),
            code_num: c(0x986801),
            code_fn: c(0x4078f2),
        }
    }

    /// Matches gpui-component's inputs and text areas to this palette.
    pub fn apply_to_components(&self, cx: &mut App) {
        use gpui_kit::component::{Theme as C, ThemeMode};
        C::change(if self.dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
        let t = self.clone();
        C::update(cx, move |c| {
            c.font_family = crate::ui::rich::UI_FONT.into();
            c.colors.background = t.input_bg;
            c.colors.input = t.input_bg;
            c.colors.foreground = t.fg;
            c.colors.muted_foreground = t.faint;
            c.colors.border = t.border;
            c.colors.primary = t.accent;
            c.colors.primary_foreground = white();
            c.colors.ring = t.accent;
            c.colors.caret = t.fg_strong;
            c.colors.selection = t.selection;
            c.colors.popover = t.popover;
            c.colors.popover_foreground = t.fg;
        });
    }

    /// Colour for a Pygments token class (`k`, `kd`, `s2`, `c1`, `mi`, `nf`, …).
    pub fn code_color(&self, class: &str) -> Hsla {
        let mut ch = class.chars();
        match (ch.next(), ch.next()) {
            (Some('k'), _) | (Some('o'), Some('w')) => self.code_kw,
            (Some('s'), _) => self.code_str,
            (Some('c'), _) => self.code_comment,
            (Some('m'), _) => self.code_num,
            (Some('n'), Some('f')) | (Some('n'), Some('c')) => self.code_fn,
            _ => self.fg,
        }
    }

    /// A stable colour per user, for avatars and names.
    pub fn user_color(&self, user_id: u64) -> Hsla {
        let h = ((user_id.wrapping_mul(2654435761)) % 360) as f32 / 360.;
        hsla(h, 0.55, if self.dark { 0.62 } else { 0.42 }, 1.)
    }
}
