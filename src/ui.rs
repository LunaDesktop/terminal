//! Rysowanie terminala: pasek tytułu, zakładki, siatka znaków, paleta ANSI,
//! zaznaczenie i kursor; a także okno Ustawień.
//!
//! Font jest **monospace** (platforma tworzy `TextEngine::mono()`), więc każda
//! komórka ma stałą szerokość. Tekst rysujemy całymi seriami o tym samym stylu —
//! to znacznie mniej wywołań niż rysowanie znaku po znaku.

use libluna_core::style::weight;
use libluna_core::{Color, IconTheme, Insets, Point, Rect, Size, Theme};
use libluna_render::{Scene, TextEngine};
use libluna_widgets::{
    icon, Button, ControlsSide, HeaderBar, IconName, Menu, PaintCtx, PreferencesDialog, Tab,
    TabBar, ViewSwitcher, WindowControl,
};

use crate::prefs::{CursorShape, Prefs, Preset, Scheme};
use crate::term::{Attrs, Cell, ColorSpec, Terminal};

/// Bok przycisku na pasku tytułu (zakładki, „+”, Ustawienia).
const HEADER_BTN: f64 = 28.0;
const HEADER_GAP: f64 = 4.0;

/// Geometria okna i siatki.
#[derive(Debug, Clone)]
pub struct Layout {
    pub header: Rect,
    /// Obszar paska bez kontrolek okna (zakładki, przyciski).
    pub content: Rect,
    /// Pasek zakładek (zerowy przy jednej zakładce).
    pub tabs: Rect,
    pub plus: Rect,
    pub settings: Rect,
    pub area: Rect,
    pub inner: Rect,
    pub cols: usize,
    pub rows: usize,
    pub cell_w: f64,
    pub cell_h: f64,
    pub origin: Point,
}

/// Liczy układ dla danego rozmiaru okna, stopnia pisma i liczby zakładek.
pub fn layout(
    size: Size,
    theme: &Theme,
    text: &TextEngine,
    font_size: f64,
    tab_count: usize,
) -> Layout {
    let header = Rect::from_xywh(0.0, 0.0, size.width, theme.metrics.headerbar.height);
    let content = HeaderBar::new()
        .controls(ControlsSide::Right)
        .controls_layout(header, theme)
        .content;
    let by = header.center().y - HEADER_BTN / 2.0;
    let settings = Rect::from_xywh(content.right() - HEADER_BTN, by, HEADER_BTN, HEADER_BTN);
    let plus = Rect::from_xywh(
        settings.x() - HEADER_BTN - HEADER_GAP,
        by,
        HEADER_BTN,
        HEADER_BTN,
    );
    let tab_h = theme.metrics.tabs.height.min(header.height());
    let tabs = if tab_count > 1 {
        Rect::from_xywh(
            content.x(),
            header.center().y - tab_h / 2.0,
            (plus.x() - HEADER_GAP - content.x()).max(0.0),
            tab_h,
        )
    } else {
        Rect::ZERO
    };
    let area = Rect::from_xywh(
        0.0,
        header.bottom(),
        size.width,
        (size.height - header.height()).max(0.0),
    );
    let inner = area.deflate(Insets::splat(8.0));
    let metrics = text.measure("M", font_size, weight::NORMAL);
    let cell_w = metrics.width.max(1.0);
    let cell_h = (font_size * 1.35).max(metrics.height).ceil();
    let cols = ((inner.width() / cell_w).floor() as usize).max(1);
    let rows = ((inner.height() / cell_h).floor() as usize).max(1);
    Layout {
        header,
        content,
        tabs,
        plus,
        settings,
        area,
        inner,
        cols,
        rows,
        cell_w,
        cell_h,
        origin: Point::new(inner.x(), inner.y()),
    }
}

/// Paleta kolorów terminala (16 ANSI + 256 + RGB).
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub fg: Color,
    pub bg: Color,
    pub cursor: Color,
    /// Tło zaznaczenia (półprzezroczyste, rysowane pod tekstem).
    pub selection: Color,
    ansi: [Color; 16],
}

impl Palette {
    pub fn new(theme: &Theme, prefs: &Prefs) -> Self {
        let dark = prefs.dark(theme);
        let (mut fg, mut bg, mut cursor, ansi) = match prefs.preset {
            Preset::Luna => (
                theme.colors.text,
                theme.colors.view_bg,
                theme.colors.accent,
                luna_ansi(dark),
            ),
            Preset::Solarized => solarized(dark),
            Preset::Tango => tango(dark),
        };
        let explicit_background = prefs.background_color();
        if let Some(color) = explicit_background {
            bg = color;
        }
        if let Some(color) = prefs.foreground_color() {
            fg = color;
        } else if explicit_background.is_some() {
            // Ręcznie wybrane tło, ale tekst nadal „Default”: dobierz kontrast
            // zamiast brać kolor tekstu z motywu (np. białe na białym).
            fg = contrast_text(bg);
        }
        if let Some(color) = prefs.cursor_color() {
            cursor = color;
        }
        Self {
            fg,
            bg,
            cursor,
            selection: theme.colors.accent.scale_alpha(0.38),
            ansi,
        }
    }

    fn resolve(&self, spec: ColorSpec, background: bool) -> Color {
        match spec {
            ColorSpec::Default => {
                if background {
                    self.bg
                } else {
                    self.fg
                }
            }
            ColorSpec::Indexed(index) if index < 16 => self.ansi[index as usize],
            ColorSpec::Indexed(index) if index < 232 => {
                let i = index - 16;
                let levels = [0u8, 95, 135, 175, 215, 255];
                let r = levels[(i / 36) as usize];
                let g = levels[((i / 6) % 6) as usize];
                let b = levels[(i % 6) as usize];
                rgb(r, g, b)
            }
            ColorSpec::Indexed(index) => {
                let level = 8u8.saturating_add((index - 232).saturating_mul(10));
                rgb(level, level, level)
            }
            ColorSpec::Rgb(r, g, b) => rgb(r, g, b),
        }
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgba8(r, g, b, 0xff)
}

/// Czytelny kolor tekstu dla podanego tła.
fn contrast_text(bg: Color) -> Color {
    let [r, g, b, _] = bg.to_rgba8();
    let luminance = 0.2126 * r as f64 + 0.7152 * g as f64 + 0.0722 * b as f64;
    if luminance > 140.0 {
        rgb(0x1b, 0x1f, 0x27)
    } else {
        rgb(0xff, 0xff, 0xff)
    }
}

/// Rysuje menu kontekstowe (PPM) nad zawartością okna.
pub fn paint_context_menu(
    scene: &mut Scene,
    theme: &Theme,
    text: &TextEngine,
    menu: &Menu,
    panel: Rect,
) {
    let mut ctx = PaintCtx::new(scene, theme, text);
    menu.paint(&mut ctx, panel);
}

fn luna_ansi(dark: bool) -> [Color; 16] {
    if dark {
        [
            rgb(0x5c, 0x63, 0x70),
            rgb(0xe0, 0x6c, 0x75),
            rgb(0x98, 0xc3, 0x79),
            rgb(0xe5, 0xc0, 0x7b),
            rgb(0x61, 0xaf, 0xef),
            rgb(0xc6, 0x78, 0xdd),
            rgb(0x56, 0xb6, 0xc2),
            rgb(0xdc, 0xdf, 0xe4),
            rgb(0x7f, 0x84, 0x8e),
            rgb(0xff, 0x7b, 0x86),
            rgb(0xb5, 0xe8, 0x90),
            rgb(0xff, 0xd6, 0x8a),
            rgb(0x7c, 0xc4, 0xff),
            rgb(0xe2, 0x9b, 0xff),
            rgb(0x74, 0xd5, 0xe0),
            rgb(0xff, 0xff, 0xff),
        ]
    } else {
        [
            rgb(0x38, 0x3a, 0x42),
            rgb(0xe4, 0x56, 0x49),
            rgb(0x50, 0xa1, 0x4f),
            rgb(0xc1, 0x84, 0x01),
            rgb(0x40, 0x78, 0xf2),
            rgb(0xa6, 0x26, 0xa4),
            rgb(0x01, 0x84, 0xbc),
            rgb(0xa0, 0xa1, 0xa7),
            rgb(0x69, 0x6c, 0x77),
            rgb(0xd1, 0x4a, 0x3d),
            rgb(0x3f, 0x8c, 0x3f),
            rgb(0xa8, 0x6f, 0x00),
            rgb(0x2f, 0x66, 0xd6),
            rgb(0x8b, 0x1f, 0x8b),
            rgb(0x00, 0x71, 0x9e),
            rgb(0x1b, 0x1f, 0x27),
        ]
    }
}

fn solarized(dark: bool) -> (Color, Color, Color, [Color; 16]) {
    if dark {
        (
            rgb(0x83, 0x94, 0x96),
            rgb(0x00, 0x2b, 0x36),
            rgb(0x93, 0xa1, 0xa1),
            [
                rgb(0x07, 0x36, 0x42),
                rgb(0xdc, 0x32, 0x2f),
                rgb(0x85, 0x99, 0x00),
                rgb(0xb5, 0x89, 0x00),
                rgb(0x26, 0x8b, 0xd2),
                rgb(0xd3, 0x36, 0x82),
                rgb(0x2a, 0xa1, 0x98),
                rgb(0x93, 0xa1, 0xa1),
                rgb(0x58, 0x6e, 0x75),
                rgb(0xcb, 0x4b, 0x16),
                rgb(0x85, 0x99, 0x00),
                rgb(0xb5, 0x89, 0x00),
                rgb(0x26, 0x8b, 0xd2),
                rgb(0x6c, 0x71, 0xc4),
                rgb(0x2a, 0xa1, 0x98),
                rgb(0xfd, 0xf6, 0xe3),
            ],
        )
    } else {
        (
            rgb(0x65, 0x7b, 0x83),
            rgb(0xfd, 0xf6, 0xe3),
            rgb(0x58, 0x6e, 0x75),
            [
                rgb(0x07, 0x36, 0x42),
                rgb(0xdc, 0x32, 0x2f),
                rgb(0x85, 0x99, 0x00),
                rgb(0xb5, 0x89, 0x00),
                rgb(0x26, 0x8b, 0xd2),
                rgb(0xd3, 0x36, 0x82),
                rgb(0x2a, 0xa1, 0x98),
                rgb(0xee, 0xe8, 0xd5),
                rgb(0x58, 0x6e, 0x75),
                rgb(0xcb, 0x4b, 0x16),
                rgb(0x85, 0x99, 0x00),
                rgb(0xb5, 0x89, 0x00),
                rgb(0x26, 0x8b, 0xd2),
                rgb(0x6c, 0x71, 0xc4),
                rgb(0x2a, 0xa1, 0x98),
                rgb(0x00, 0x2b, 0x36),
            ],
        )
    }
}

fn tango(dark: bool) -> (Color, Color, Color, [Color; 16]) {
    let mut ansi = [
        rgb(0x2e, 0x34, 0x36),
        rgb(0xcc, 0x00, 0x00),
        rgb(0x4e, 0x9a, 0x06),
        rgb(0xc4, 0xa0, 0x00),
        rgb(0x34, 0x65, 0xa4),
        rgb(0x75, 0x50, 0x7b),
        rgb(0x06, 0x98, 0x9a),
        rgb(0xd3, 0xd7, 0xcf),
        rgb(0x55, 0x57, 0x53),
        rgb(0xef, 0x29, 0x29),
        rgb(0x8a, 0xe2, 0x34),
        rgb(0xfc, 0xe9, 0x4f),
        rgb(0x72, 0x9f, 0xcf),
        rgb(0xad, 0x7f, 0xa8),
        rgb(0x34, 0xe2, 0xe2),
        rgb(0xee, 0xee, 0xec),
    ];
    if dark {
        (
            rgb(0xd3, 0xd7, 0xcf),
            rgb(0x2e, 0x34, 0x36),
            rgb(0xff, 0xff, 0xff),
            ansi,
        )
    } else {
        // Na jasnym tle jasne „białe” kolory ANSI byłyby nieczytelne.
        ansi[7] = rgb(0x55, 0x57, 0x53);
        ansi[15] = rgb(0x00, 0x00, 0x00);
        (
            rgb(0x2e, 0x34, 0x36),
            rgb(0xff, 0xff, 0xff),
            rgb(0x00, 0x00, 0x00),
            ansi,
        )
    }
}

/// Zaznaczenie w przestrzeni widoku (wiersze/kolumny ekranu, 0 = góra).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionView {
    pub start: (usize, usize),
    pub end: (usize, usize),
}

/// Stan potrzebny do narysowania jednej klatki.
pub struct ViewState<'a> {
    pub term: &'a Terminal,
    pub title: &'a str,
    pub tabs: &'a [String],
    pub active_tab: usize,
    pub tab_hover: Option<usize>,
    pub tab_scroll: f64,
    pub selection: Option<SelectionView>,
    pub hover: Option<WindowControl>,
    pub plus_hover: bool,
    pub settings_hover: bool,
    pub font_size: f64,
    pub prefs: &'a Prefs,
}

/// Rysuje terminal. Zwraca policzony układ (żeby pętla mogła dopasować PTY).
pub fn paint(
    scene: &mut Scene,
    theme: &Theme,
    text: &TextEngine,
    size: Size,
    view: &ViewState<'_>,
) -> Layout {
    let lay = layout(size, theme, text, view.font_size, view.tabs.len());
    let mut ctx = PaintCtx::new(scene, theme, text);
    let palette = Palette::new(theme, view.prefs);

    ctx.scene.fill_rect(lay.area, palette.bg);
    HeaderBar::new()
        .controls(ControlsSide::Right)
        .paint(&mut ctx, lay.header, view.hover);

    if view.tabs.len() > 1 {
        tab_bar(view, lay.tabs, text, theme).paint(&mut ctx, lay.tabs);
    } else {
        let title = ellipsize(view.title, 52);
        ctx.label_center(
            lay.content.center().x,
            lay.header.center().y,
            &title,
            theme.typography.body.size,
            weight::BOLD,
            theme.colors.text,
        );
    }
    header_button(&mut ctx, lay.plus, IconName::Plus, view.plus_hover);
    header_button(
        &mut ctx,
        lay.settings,
        IconName::Settings,
        view.settings_hover,
    );

    ctx.scene.push_clip(lay.inner);
    let blank = Cell::default();
    for row in 0..lay.rows {
        let cells = view.term.view_row(row, view.term.scroll_offset());
        let cells: Vec<&Cell> = (0..lay.cols)
            .map(|x| cells.get(x).unwrap_or(&blank))
            .collect();
        let selection = view.selection.and_then(|sel| selection_for_row(sel, row));
        draw_row(
            &mut ctx,
            &palette,
            &cells,
            row,
            &lay,
            view.font_size,
            selection,
        );
    }

    // Pasek przewijania historii.
    if view.term.scroll_offset() > 0 {
        draw_history(&mut ctx, &palette, view.term, &lay);
    }

    // Kursor — tylko gdy widok jest na dole.
    if view.term.cursor_is_visible() {
        draw_cursor(&mut ctx, &palette, view.term, &lay, view);
    }

    ctx.scene.pop_clip();
    lay
}

fn header_button(ctx: &mut PaintCtx, rect: Rect, name: IconName, hover: bool) {
    if hover {
        ctx.scene
            .fill_rounded_rect(rect, 6.0, ctx.theme.colors.shade);
    }
    // Pasek tytułu to zestaw symbolicznych kontrolek — wszystkie muszą być
    // jednokolorowe jak „×”/„−”/„□”. Wymuszamy styl monochromatyczny, bo inaczej
    // „Ustawienia” brały pastelową (lub zewnętrzną) ikonę z zestawu i odstawały
    // od reszty paska.
    icon::paint_icon_themed(
        ctx,
        name,
        rect.deflate(Insets::splat(6.0)),
        ctx.theme.colors.text,
        IconTheme::Monochrome,
    );
}

fn tab_bar(view: &ViewState<'_>, rect: Rect, text: &TextEngine, theme: &Theme) -> TabBar {
    let tabs: Vec<Tab> = view
        .tabs
        .iter()
        .map(|title| Tab::new(title.clone()).closable(true))
        .collect();
    let mut bar = TabBar::with_tabs(tabs).expand(false);
    bar.set_selected(view.active_tab);
    bar.set_hovered(view.tab_hover);
    bar.set_scroll(view.tab_scroll, rect, text, theme);
    bar
}

/// Skraca napis do `max` znaków, dodając wielokropek.
fn ellipsize(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn selection_for_row(sel: SelectionView, row: usize) -> Option<(usize, usize)> {
    if row < sel.start.0 || row > sel.end.0 {
        return None;
    }
    let from = if row == sel.start.0 { sel.start.1 } else { 0 };
    let to = if row == sel.end.0 {
        sel.end.1.saturating_add(1)
    } else {
        usize::MAX
    };
    Some((from, to))
}

fn draw_row(
    ctx: &mut PaintCtx,
    palette: &Palette,
    cells: &[&Cell],
    row: usize,
    lay: &Layout,
    font_size: f64,
    selection: Option<(usize, usize)>,
) {
    let y = lay.origin.y + row as f64 * lay.cell_h;
    let mut x = 0;

    // Tła — grupujemy sąsiednie komórki o tym samym kolorze.
    while x < cells.len() {
        let bg = effective(cells[x], palette).0;
        let mut end = x + 1;
        while end < cells.len() && effective(cells[end], palette).0 == bg {
            end += 1;
        }
        if bg != palette.bg {
            ctx.scene.fill_rect(
                Rect::from_xywh(
                    lay.origin.x + x as f64 * lay.cell_w,
                    y,
                    (end - x) as f64 * lay.cell_w,
                    lay.cell_h,
                ),
                bg,
            );
        }
        x = end;
    }

    // Zaznaczenie leży pod tekstem, nad tłami komórek.
    if let Some((from, to)) = selection {
        let from = from.min(cells.len());
        let to = to.min(cells.len());
        if from < to {
            ctx.scene.fill_rect(
                Rect::from_xywh(
                    lay.origin.x + from as f64 * lay.cell_w,
                    y,
                    (to - from) as f64 * lay.cell_w,
                    lay.cell_h,
                ),
                palette.selection,
            );
        }
    }

    // Tekst — serie o identycznym stylu (puste komórki przerywają serię, co jest
    // też poprawne dla znaków szerokich, które zajmują dwie kolumny).
    let mut x = 0;
    while x < cells.len() {
        if cells[x].text.is_empty() {
            x += 1;
            continue;
        }
        let style = style_key(cells[x], palette);
        let mut end = x + 1;
        while end < cells.len()
            && !cells[end].text.is_empty()
            && style_key(cells[end], palette) == style
        {
            end += 1;
        }
        let mut line = String::new();
        for cell in &cells[x..end] {
            line.push_str(&cell.text);
        }
        let mut fg = style.1;
        let attrs = cells[x].attrs;
        if attrs.has(Attrs::HIDDEN) {
            fg = style.0;
        } else if attrs.has(Attrs::DIM) {
            fg = fg.scale_alpha(0.62);
        }
        let baseline = y + lay.cell_h * 0.80;
        let weight = if attrs.has(Attrs::BOLD) {
            weight::BOLD
        } else {
            weight::NORMAL
        };
        ctx.label(
            lay.origin.x + x as f64 * lay.cell_w,
            baseline,
            &line,
            font_size,
            weight,
            fg,
        );
        if attrs.has(Attrs::UNDERLINE) {
            ctx.scene.fill_rect(
                Rect::from_xywh(
                    lay.origin.x + x as f64 * lay.cell_w,
                    baseline + 2.0,
                    (end - x) as f64 * lay.cell_w,
                    1.0,
                ),
                fg,
            );
        }
        x = end;
    }
}

fn draw_cursor(
    ctx: &mut PaintCtx,
    palette: &Palette,
    term: &Terminal,
    lay: &Layout,
    view: &ViewState<'_>,
) {
    let (cx, cy) = term.cursor();
    if cx >= lay.cols || cy >= lay.rows {
        return;
    }
    let cursor_rect = Rect::from_xywh(
        lay.origin.x + cx as f64 * lay.cell_w,
        lay.origin.y + cy as f64 * lay.cell_h,
        lay.cell_w,
        lay.cell_h,
    );
    match view.prefs.cursor_shape {
        CursorShape::Block => {
            ctx.scene.fill_rect(cursor_rect, palette.cursor);
            if let Some(glyph) = term.view_row(cy, 0).get(cx) {
                if !glyph.text.is_empty() {
                    let baseline = cursor_rect.y() + lay.cell_h * 0.80;
                    let weight = if glyph.attrs.has(Attrs::BOLD) {
                        weight::BOLD
                    } else {
                        weight::NORMAL
                    };
                    ctx.label(
                        cursor_rect.x(),
                        baseline,
                        &glyph.text,
                        view.font_size,
                        weight,
                        palette.bg,
                    );
                }
            }
        }
        CursorShape::Underline => {
            ctx.scene.fill_rect(
                Rect::from_xywh(
                    cursor_rect.x(),
                    cursor_rect.bottom() - 2.0,
                    cursor_rect.width(),
                    2.0,
                ),
                palette.cursor,
            );
        }
        CursorShape::Bar => {
            ctx.scene.fill_rect(
                Rect::from_xywh(cursor_rect.x(), cursor_rect.y(), 2.0, cursor_rect.height()),
                palette.cursor,
            );
        }
    }
}

fn effective(cell: &Cell, palette: &Palette) -> (Color, Color) {
    let mut fg = palette.resolve(cell.fg, false);
    let mut bg = palette.resolve(cell.bg, true);
    if cell.attrs.has(Attrs::REVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    (bg, fg)
}

/// Kolor tła, kolor tekstu i cechy — pełny klucz serii tekstu.
fn style_key(cell: &Cell, palette: &Palette) -> (Color, Color, u8) {
    let (bg, fg) = effective(cell, palette);
    (bg, fg, cell.attrs.bits())
}

fn draw_history(ctx: &mut PaintCtx, palette: &Palette, term: &Terminal, lay: &Layout) {
    let track = Rect::from_xywh(
        lay.inner.right() - 3.0,
        lay.inner.y(),
        3.0,
        lay.inner.height(),
    );
    ctx.scene.fill_rect(track, palette.fg.scale_alpha(0.10));
    let history = term.scrollback_len();
    let thumb_h =
        (lay.rows as f64 / (history + lay.rows).max(1) as f64).clamp(0.05, 1.0) * track.height();
    let start = history.saturating_sub(term.scroll_offset());
    let frac = if history > 0 {
        start as f64 / history as f64
    } else {
        0.0
    };
    let top = track.y() + frac * (track.height() - thumb_h).max(0.0);
    ctx.scene.fill_rect(
        Rect::from_xywh(track.x(), top, track.width(), thumb_h.max(12.0)),
        palette.fg.scale_alpha(0.45),
    );
}

// --- Ustawienia -----------------------------------------------------------

const SETTINGS_ROWS: usize = 7;
const SETTINGS_ROW_H: f64 = 40.0;

/// Trafienie w oknie Ustawień.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsHit {
    Close,
    Scheme(usize),
    Preset(usize),
    FontSmaller,
    FontLarger,
    CursorShape(usize),
    Background,
    Foreground,
    Cursor,
}

struct SettingsLayout {
    rows: [Rect; SETTINGS_ROWS],
    scheme: Rect,
    preset: Rect,
    font_smaller: Rect,
    font_value: Rect,
    font_larger: Rect,
    cursor_shape: Rect,
    background: Rect,
    foreground: Rect,
    cursor: Rect,
    done: Rect,
}

fn settings_dialog() -> PreferencesDialog {
    PreferencesDialog::new(libluna_i18n::tr("Terminal Settings"), 432.0)
}

fn settings_layout(size: Size, theme: &Theme) -> SettingsLayout {
    let rects = settings_dialog().rects(size, theme);
    let pad = theme.spacing.lg;
    let inner = rects.content.deflate(Insets::new(pad, pad, pad, pad));
    let rows: [Rect; SETTINGS_ROWS] = std::array::from_fn(|i| {
        Rect::from_xywh(
            inner.x(),
            inner.y() + i as f64 * SETTINGS_ROW_H,
            inner.width(),
            SETTINGS_ROW_H,
        )
    });
    let seg_w = 232.0_f64.min(inner.width() * 0.55);
    let seg =
        |row: Rect| Rect::from_xywh(inner.right() - seg_w, row.center().y - 15.0, seg_w, 30.0);
    let btn = 28.0;
    let font_larger = Rect::from_xywh(
        inner.right() - btn,
        rows[2].center().y - btn / 2.0,
        btn,
        btn,
    );
    let font_value = Rect::from_xywh(font_larger.x() - 56.0, rows[2].y(), 48.0, SETTINGS_ROW_H);
    let font_smaller = Rect::from_xywh(
        font_value.x() - btn - 4.0,
        rows[2].center().y - btn / 2.0,
        btn,
        btn,
    );
    let color_area =
        |row: Rect| Rect::from_xywh(inner.right() - 220.0, row.y(), 220.0, row.height());
    let bh = theme.metrics.button.height;
    let done = Rect::from_xywh(inner.right() - 120.0, inner.bottom() - bh, 120.0, bh);
    SettingsLayout {
        rows,
        scheme: seg(rows[0]),
        preset: seg(rows[1]),
        font_smaller,
        font_value,
        font_larger,
        cursor_shape: seg(rows[3]),
        background: color_area(rows[4]),
        foreground: color_area(rows[5]),
        cursor: color_area(rows[6]),
        done,
    }
}

/// Rysuje okno Ustawień terminala.
pub fn paint_settings(
    scene: &mut Scene,
    theme: &Theme,
    text: &TextEngine,
    size: Size,
    prefs: &Prefs,
) {
    let mut ctx = PaintCtx::new(scene, theme, text);
    settings_dialog().paint(&mut ctx, size);
    let lay = settings_layout(size, &theme);
    let palette = Palette::new(theme, prefs);
    let body = theme.typography.body.size;
    let labels = [
        libluna_i18n::tr("Color scheme"),
        libluna_i18n::tr("Palette"),
        libluna_i18n::tr("Font size"),
        libluna_i18n::tr("Cursor shape"),
        libluna_i18n::tr("Background"),
        libluna_i18n::tr("Text color"),
        libluna_i18n::tr("Cursor color"),
    ];
    for (i, label) in labels.iter().enumerate() {
        ctx.label_left(
            lay.rows[i].x() + 12.0,
            lay.rows[i].center().y,
            label,
            body,
            weight::NORMAL,
            theme.colors.text,
        );
        if i > 0 {
            ctx.scene.fill_rect(
                Rect::from_xywh(lay.rows[i].x(), lay.rows[i].y(), lay.rows[i].width(), 1.0),
                theme.colors.border,
            );
        }
    }

    let mut scheme = ViewSwitcher::new(Scheme::ALL.iter().map(|s| libluna_i18n::tr(s.label())));
    scheme.set_selected(
        Scheme::ALL
            .iter()
            .position(|s| *s == prefs.scheme)
            .unwrap_or(0),
    );
    scheme.paint(&mut ctx, lay.scheme);

    let mut preset = ViewSwitcher::new(Preset::ALL.iter().map(|p| libluna_i18n::tr(p.label())));
    preset.set_selected(
        Preset::ALL
            .iter()
            .position(|p| *p == prefs.preset)
            .unwrap_or(0),
    );
    preset.paint(&mut ctx, lay.preset);

    Button::new("−").paint(&mut ctx, lay.font_smaller);
    Button::new("+").paint(&mut ctx, lay.font_larger);
    ctx.label_center(
        lay.font_value.center().x,
        lay.font_value.center().y,
        &format!("{:.0}", prefs.font_size),
        body,
        weight::NORMAL,
        theme.colors.text,
    );

    let mut shape = ViewSwitcher::new(CursorShape::ALL.iter().map(|s| libluna_i18n::tr(s.label())));
    shape.set_selected(
        CursorShape::ALL
            .iter()
            .position(|s| *s == prefs.cursor_shape)
            .unwrap_or(0),
    );
    shape.paint(&mut ctx, lay.cursor_shape);

    color_row(
        &mut ctx,
        lay.background,
        prefs.background_color().unwrap_or(palette.bg),
        libluna_i18n::tr(prefs.background_name()),
    );
    color_row(
        &mut ctx,
        lay.foreground,
        prefs.foreground_color().unwrap_or(palette.fg),
        libluna_i18n::tr(prefs.foreground_name()),
    );
    color_row(
        &mut ctx,
        lay.cursor,
        prefs.cursor_color().unwrap_or(palette.cursor),
        libluna_i18n::tr(prefs.cursor_name()),
    );

    Button::new(libluna_i18n::tr("Done")).paint(&mut ctx, lay.done);
}

fn color_row(ctx: &mut PaintCtx, area: Rect, color: Color, name: String) {
    let swatch = Rect::from_xywh(area.right() - 196.0, area.center().y - 9.0, 24.0, 18.0);
    ctx.scene.fill_rounded_rect(swatch, 4.0, color);
    ctx.scene
        .stroke_rounded_rect(swatch, 4.0, 1.0, ctx.theme.colors.border);
    ctx.label_right(
        area.right() - 8.0,
        area.center().y,
        &name,
        ctx.theme.typography.body.size,
        weight::NORMAL,
        ctx.theme.colors.text,
    );
}

/// Co trafił klik w oknie Ustawień (albo `None`).
pub fn settings_hit(size: Size, theme: &Theme, p: Point) -> Option<SettingsHit> {
    let lay = settings_layout(size, theme);
    let dialog = settings_dialog();
    if dialog.close_at(size, theme, p) || dialog.scrim_at(size, theme, p) || lay.done.contains(p) {
        return Some(SettingsHit::Close);
    }
    let scheme = ViewSwitcher::new(Scheme::ALL.iter().map(|s| libluna_i18n::tr(s.label())));
    if let Some(i) = scheme.index_at(lay.scheme, p) {
        return Some(SettingsHit::Scheme(i));
    }
    let preset = ViewSwitcher::new(Preset::ALL.iter().map(|s| libluna_i18n::tr(s.label())));
    if let Some(i) = preset.index_at(lay.preset, p) {
        return Some(SettingsHit::Preset(i));
    }
    if lay.font_smaller.contains(p) {
        return Some(SettingsHit::FontSmaller);
    }
    if lay.font_larger.contains(p) {
        return Some(SettingsHit::FontLarger);
    }
    let shape = ViewSwitcher::new(CursorShape::ALL.iter().map(|s| libluna_i18n::tr(s.label())));
    if let Some(i) = shape.index_at(lay.cursor_shape, p) {
        return Some(SettingsHit::CursorShape(i));
    }
    if lay.background.contains(p) {
        return Some(SettingsHit::Background);
    }
    if lay.foreground.contains(p) {
        return Some(SettingsHit::Foreground);
    }
    if lay.cursor.contains(p) {
        return Some(SettingsHit::Cursor);
    }
    None
}

/// Prostokąt okna Ustawień (dla testów i układu).
pub fn settings_panel(size: Size, theme: &Theme) -> Rect {
    settings_dialog().rects(size, theme).panel
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prefs::Prefs;
    use crate::term::Terminal;

    fn state<'a>(term: &'a Terminal, prefs: &'a Prefs, title: &'a str) -> ViewState<'a> {
        ViewState {
            term,
            title,
            tabs: &[],
            active_tab: 0,
            tab_hover: None,
            tab_scroll: 0.0,
            selection: None,
            hover: None,
            plus_hover: false,
            settings_hover: false,
            font_size: 14.0,
            prefs,
        }
    }

    #[test]
    fn layout_has_positive_grid() {
        let theme = Theme::dark();
        let text = TextEngine::mono();
        let lay = layout(Size::new(900.0, 600.0), &theme, &text, 14.0, 1);
        assert!(lay.cols >= 20);
        assert!(lay.rows >= 10);
        assert!(lay.origin.y >= lay.header.bottom());
        assert!(lay.origin.x >= lay.inner.x());
        assert!(lay.settings.right() <= lay.content.right());
        assert!(lay.plus.right() < lay.settings.x());
    }

    #[test]
    fn tabs_only_take_space_with_more_than_one() {
        let theme = Theme::dark();
        let text = TextEngine::mono();
        let one = layout(Size::new(900.0, 600.0), &theme, &text, 14.0, 1);
        let two = layout(Size::new(900.0, 600.0), &theme, &text, 14.0, 2);
        assert_eq!(one.tabs, Rect::ZERO);
        assert!(two.tabs.width() > 0.0);
        assert!(two.tabs.right() <= two.plus.x());
    }

    #[test]
    fn paints_all_sgr_styles_and_selection() {
        let theme = Theme::dark();
        let text = TextEngine::none();
        let mut term = Terminal::new(20, 4);
        term.feed(b"\x1b[1;31mczerwony\x1b[0m \x1b[7mreverse\x1b[0m\x1b[38;2;10;200;30mrgb");
        let prefs = Prefs::default();
        let mut scene = Scene::new();
        let size = Size::new(900.0, 600.0);
        let mut view = state(&term, &prefs, "Terminal");
        view.selection = Some(SelectionView {
            start: (0, 0),
            end: (0, 4),
        });
        let lay = paint(&mut scene, &theme, &text, size, &view);
        assert!(!scene.is_empty());
        assert!(lay.cols > 0);
    }

    #[test]
    fn presets_change_background_and_ansi() {
        let theme = Theme::dark();
        let mut prefs = Prefs {
            preset: Preset::Solarized,
            ..Prefs::default()
        };
        let solarized = Palette::new(&theme, &prefs);
        assert_eq!(solarized.bg.to_rgba8(), [0x00, 0x2b, 0x36, 0xff]);
        assert_eq!(solarized.fg.to_rgba8(), [0x83, 0x94, 0x96, 0xff]);
        prefs.preset = Preset::Tango;
        let tango = Palette::new(&theme, &prefs);
        assert_eq!(tango.bg.to_rgba8(), [0x2e, 0x34, 0x36, 0xff]);
        prefs.background = 4;
        let black = Palette::new(&theme, &prefs);
        assert_eq!(black.bg.to_rgba8(), [0x00, 0x00, 0x00, 0xff]);
    }

    #[test]
    fn explicit_light_background_gets_contrasting_text() {
        let theme = Theme::dark();
        let prefs = Prefs {
            background: 5, // White
            ..Prefs::default()
        };
        let palette = Palette::new(&theme, &prefs);
        let [r, g, b, _] = palette.fg.to_rgba8();
        assert!(r < 80 && g < 80 && b < 80, "oczekiwano ciemnego tekstu");
    }

    #[test]
    fn default_colors_follow_theme() {
        let theme = Theme::dark();
        let palette = Palette::new(&theme, &Prefs::default());
        assert_eq!(palette.bg, theme.colors.view_bg);
        assert_eq!(palette.fg, theme.colors.text);
    }

    #[test]
    fn selection_rows_are_inclusive() {
        let sel = SelectionView {
            start: (1, 3),
            end: (3, 2),
        };
        assert_eq!(selection_for_row(sel, 0), None);
        assert_eq!(selection_for_row(sel, 1), Some((3, usize::MAX)));
        assert_eq!(selection_for_row(sel, 2), Some((0, usize::MAX)));
        assert_eq!(selection_for_row(sel, 3), Some((0, 3)));
        assert_eq!(selection_for_row(sel, 4), None);
    }

    #[test]
    fn settings_dialog_hits_controls() {
        let theme = Theme::dark();
        let size = Size::new(900.0, 600.0);
        let lay = settings_layout(size, &theme);
        assert_eq!(
            settings_hit(size, &theme, lay.done.center()),
            Some(SettingsHit::Close)
        );
        assert_eq!(
            settings_hit(size, &theme, lay.font_larger.center()),
            Some(SettingsHit::FontLarger)
        );
        assert_eq!(
            settings_hit(size, &theme, lay.background.center()),
            Some(SettingsHit::Background)
        );
        assert_eq!(
            settings_hit(
                size,
                &theme,
                Point::new(lay.scheme.x() + 4.0, lay.scheme.center().y)
            ),
            Some(SettingsHit::Scheme(0))
        );
        assert_eq!(
            settings_hit(size, &theme, Point::new(1.0, 1.0)),
            Some(SettingsHit::Close)
        );
    }

    #[test]
    fn ellipsize_keeps_short_titles() {
        assert_eq!(ellipsize("Terminal", 20), "Terminal");
        let long = ellipsize("abcdefghijklmnopqrstuvwxyz", 10);
        assert_eq!(long.chars().count(), 10);
        assert!(long.ends_with('…'));
    }
}
