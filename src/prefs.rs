//! Preferencje terminala: paleta, stopień pisma i kursor.
//!
//! Plik `$XDG_CONFIG_HOME/luna/terminal.conf` (albo `~/.config/luna/terminal.conf`)
//! ma prosty format `klucz=wartość` — można go edytować ręcznie. Kolory są
//! indeksami gotowych zestawów ([`BACKGROUNDS`], [`FOREGROUNDS`], [`CURSORS`]);
//! indeks `0` oznacza „kolor z motywu/presetu”.

use std::path::{Path, PathBuf};

use libluna_core::style::Appearance;
use libluna_core::{Color, Theme};

/// Domyślny i dopuszczalny stopień pisma.
pub const DEFAULT_FONT_SIZE: f64 = 14.0;
pub const MIN_FONT_SIZE: f64 = 8.0;
pub const MAX_FONT_SIZE: f64 = 32.0;

/// Motyw palety: jasny, ciemny albo zgodny z motywem środowiska.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scheme {
    #[default]
    System,
    Light,
    Dark,
}

impl Scheme {
    pub const ALL: [Scheme; 3] = [Scheme::System, Scheme::Light, Scheme::Dark];

    /// Etykieta do [`libluna_i18n::tr`].
    pub fn label(self) -> &'static str {
        match self {
            Scheme::System => "System",
            Scheme::Light => "Light",
            Scheme::Dark => "Dark",
        }
    }
}

/// Zestaw kolorów ANSI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preset {
    #[default]
    Luna,
    Solarized,
    Tango,
}

impl Preset {
    pub const ALL: [Preset; 3] = [Preset::Luna, Preset::Solarized, Preset::Tango];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Luna => "Luna",
            Preset::Solarized => "Solarized",
            Preset::Tango => "Tango",
        }
    }
}

/// Kształt kursora.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

impl CursorShape {
    pub const ALL: [CursorShape; 3] =
        [CursorShape::Block, CursorShape::Underline, CursorShape::Bar];

    pub fn label(self) -> &'static str {
        match self {
            CursorShape::Block => "Block",
            CursorShape::Underline => "Underline",
            CursorShape::Bar => "Bar",
        }
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgba8(r, g, b, 0xff)
}

/// Kolory tła do wyboru w Ustawieniach (`None` = tło presetuu).
pub const BACKGROUNDS: [Option<Color>; 6] = [
    None,
    Some(rgb(0x10, 0x12, 0x18)),
    Some(rgb(0x00, 0x2b, 0x36)),
    Some(rgb(0x1e, 0x1e, 0x2e)),
    Some(rgb(0x00, 0x00, 0x00)),
    Some(rgb(0xff, 0xff, 0xff)),
];

/// Nazwy kolorów tła (klucze i18n).
pub const BACKGROUND_NAMES: [&str; 6] =
    ["Default", "Midnight", "Solarized", "Nord", "Black", "White"];

/// Kolory tekstu do wyboru w Ustawieniach.
pub const FOREGROUNDS: [Option<Color>; 5] = [
    None,
    Some(rgb(0xff, 0xff, 0xff)),
    Some(rgb(0xd8, 0xde, 0xe9)),
    Some(rgb(0x00, 0x00, 0x00)),
    Some(rgb(0x65, 0x7b, 0x83)),
];

pub const FOREGROUND_NAMES: [&str; 5] = ["Default", "White", "Light gray", "Black", "Solarized"];

/// Kolory kursora do wyboru w Ustawieniach.
pub const CURSORS: [Option<Color>; 7] = [
    None,
    Some(rgb(0xff, 0xff, 0xff)),
    Some(rgb(0x50, 0xfa, 0x7b)),
    Some(rgb(0xff, 0xb8, 0x6c)),
    Some(rgb(0xff, 0x79, 0xc6)),
    Some(rgb(0x61, 0xaf, 0xef)),
    Some(rgb(0xc6, 0x78, 0xdd)),
];

pub const CURSOR_NAMES: [&str; 7] = [
    "Accent", "White", "Green", "Amber", "Pink", "Blue", "Purple",
];

/// Preferencje terminala.
#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    pub scheme: Scheme,
    pub preset: Preset,
    pub font_size: f64,
    pub background: usize,
    pub foreground: usize,
    pub cursor: usize,
    pub cursor_shape: CursorShape,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            scheme: Scheme::System,
            preset: Preset::Luna,
            font_size: DEFAULT_FONT_SIZE,
            background: 0,
            foreground: 0,
            cursor: 0,
            cursor_shape: CursorShape::Block,
        }
    }
}

impl Prefs {
    /// Wczytuje preferencje użytkownika (brak pliku = wartości domyślne).
    pub fn load() -> Self {
        config_path()
            .filter(|p| p.exists())
            .map(|p| Self::load_from(&p))
            .unwrap_or_default()
    }

    /// Zapisuje preferencje; błędy dysku są ignorowane.
    pub fn save(&self) {
        if let Some(path) = config_path() {
            let _ = self.save_to(&path);
        }
    }

    /// Czy paleta ma być ciemna (uwzględnia motyw środowiska dla `System`).
    pub fn dark(&self, theme: &Theme) -> bool {
        match self.scheme {
            Scheme::Light => false,
            Scheme::Dark => true,
            Scheme::System => theme.appearance == Appearance::Dark,
        }
    }

    pub fn background_color(&self) -> Option<Color> {
        BACKGROUNDS[self.background.min(BACKGROUNDS.len() - 1)]
    }

    pub fn foreground_color(&self) -> Option<Color> {
        FOREGROUNDS[self.foreground.min(FOREGROUNDS.len() - 1)]
    }

    pub fn cursor_color(&self) -> Option<Color> {
        CURSORS[self.cursor.min(CURSORS.len() - 1)]
    }

    pub fn background_name(&self) -> &'static str {
        BACKGROUND_NAMES[self.background.min(BACKGROUND_NAMES.len() - 1)]
    }

    pub fn foreground_name(&self) -> &'static str {
        FOREGROUND_NAMES[self.foreground.min(FOREGROUND_NAMES.len() - 1)]
    }

    pub fn cursor_name(&self) -> &'static str {
        CURSOR_NAMES[self.cursor.min(CURSOR_NAMES.len() - 1)]
    }

    /// Następna wartość w cyklu wyboru koloru tła.
    pub fn cycle_background(&mut self) {
        self.background = (self.background + 1) % BACKGROUNDS.len();
    }

    pub fn cycle_foreground(&mut self) {
        self.foreground = (self.foreground + 1) % FOREGROUNDS.len();
    }

    pub fn cycle_cursor(&mut self) {
        self.cursor = (self.cursor + 1) % CURSORS.len();
    }

    /// Zmienia stopień pisma o `delta`, przycinając do [`MIN_FONT_SIZE`]..=[`MAX_FONT_SIZE`].
    pub fn zoom(&mut self, delta: f64) {
        self.font_size = (self.font_size + delta).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
    }

    fn load_from(path: &Path) -> Self {
        let mut prefs = Self::default();
        let Ok(text) = std::fs::read_to_string(path) else {
            return prefs;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "scheme" => {
                    prefs.scheme = match value {
                        "light" => Scheme::Light,
                        "dark" => Scheme::Dark,
                        _ => Scheme::System,
                    };
                }
                "preset" => {
                    prefs.preset = match value {
                        "solarized" => Preset::Solarized,
                        "tango" => Preset::Tango,
                        _ => Preset::Luna,
                    };
                }
                "font_size" => {
                    if let Ok(size) = value.parse::<f64>() {
                        prefs.font_size = size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
                    }
                }
                "background" => {
                    prefs.background = parse_index(value, BACKGROUNDS.len());
                }
                "foreground" => {
                    prefs.foreground = parse_index(value, FOREGROUNDS.len());
                }
                "cursor" => {
                    prefs.cursor = parse_index(value, CURSORS.len());
                }
                "cursor_shape" => {
                    prefs.cursor_shape = match value {
                        "underline" => CursorShape::Underline,
                        "bar" => CursorShape::Bar,
                        _ => CursorShape::Block,
                    };
                }
                _ => {}
            }
        }
        prefs
    }

    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = format!(
            "# luna-terminal\nscheme={}\npreset={}\nfont_size={}\nbackground={}\nforeground={}\ncursor={}\ncursor_shape={}\n",
            match self.scheme {
                Scheme::System => "system",
                Scheme::Light => "light",
                Scheme::Dark => "dark",
            },
            match self.preset {
                Preset::Luna => "luna",
                Preset::Solarized => "solarized",
                Preset::Tango => "tango",
            },
            self.font_size,
            self.background,
            self.foreground,
            self.cursor,
            match self.cursor_shape {
                CursorShape::Block => "block",
                CursorShape::Underline => "underline",
                CursorShape::Bar => "bar",
            }
        );
        // Zapis przez plik tymczasowy: przerwany zapis nie zostawia pustej konfiguracji.
        let tmp = path.with_extension("conf.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, path)
    }
}

fn parse_index(value: &str, len: usize) -> usize {
    value
        .parse::<usize>()
        .map(|index| index.min(len - 1))
        .unwrap_or(0)
}

/// Ścieżka pliku konfiguracji.
fn config_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir).join("luna/terminal.conf"));
        }
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/luna/terminal.conf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_known_keys_and_clamps() {
        let dir = std::env::temp_dir().join("luna-terminal-prefs-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("terminal.conf");
        std::fs::write(
            &path,
            "# komentarz\nscheme=dark\npreset=solarized\nfont_size=99\nbackground=4\nforeground=99\ncursor=2\ncursor_shape=bar\nnieznane=1\n",
        )
        .unwrap();
        let prefs = Prefs::load_from(&path);
        assert_eq!(prefs.scheme, Scheme::Dark);
        assert_eq!(prefs.preset, Preset::Solarized);
        assert_eq!(prefs.font_size, MAX_FONT_SIZE);
        assert_eq!(prefs.background, 4);
        assert_eq!(prefs.foreground, FOREGROUNDS.len() - 1);
        assert_eq!(prefs.cursor, 2);
        assert_eq!(prefs.cursor_shape, CursorShape::Bar);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_values_fall_back_to_defaults_and_roundtrip() {
        let dir = std::env::temp_dir().join("luna-terminal-prefs-roundtrip");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("terminal.conf");
        std::fs::write(&path, "scheme=xx\npreset=xx\ncursor_shape=xx\n").unwrap();
        let mut prefs = Prefs::load_from(&path);
        assert_eq!(prefs.scheme, Scheme::System);
        assert_eq!(prefs.preset, Preset::Luna);
        assert_eq!(prefs.cursor_shape, CursorShape::Block);
        prefs.scheme = Scheme::Light;
        prefs.preset = Preset::Tango;
        prefs.font_size = 18.0;
        prefs.background = 3;
        prefs.foreground = 2;
        prefs.cursor = 5;
        prefs.cursor_shape = CursorShape::Underline;
        prefs.save_to(&path).unwrap();
        assert_eq!(Prefs::load_from(&path), prefs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cycling_and_zoom_stay_in_range() {
        let mut prefs = Prefs::default();
        for _ in 0..BACKGROUNDS.len() {
            prefs.cycle_background();
        }
        assert_eq!(prefs.background, 0);
        prefs.cursor = CURSORS.len() - 1;
        prefs.cycle_cursor();
        assert_eq!(prefs.cursor, 0);

        prefs.font_size = MAX_FONT_SIZE;
        prefs.zoom(4.0);
        assert_eq!(prefs.font_size, MAX_FONT_SIZE);
        prefs.zoom(-100.0);
        assert_eq!(prefs.font_size, MIN_FONT_SIZE);
    }

    #[test]
    fn scheme_resolves_against_theme() {
        let dark = Theme::dark();
        let light = Theme::light();
        let mut prefs = Prefs::default();
        assert!(prefs.dark(&dark));
        assert!(!prefs.dark(&light));
        prefs.scheme = Scheme::Light;
        assert!(!prefs.dark(&dark));
        prefs.scheme = Scheme::Dark;
        assert!(prefs.dark(&light));
    }
}
