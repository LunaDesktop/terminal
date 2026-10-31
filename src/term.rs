//! Model terminala: siatka znaków, scrollback i parser VT100/xterm.
//!
//! Świadomie nie używamy `vte` ani `alacritty_terminal` — implementujemy
//! wystarczający podzbiór sekwencji, żeby działały powłoka, `ls --color`,
//! `less`, `top`, `nano` i typowe programy pełnoekranowe:
//!
//! - sterowanie kursorem (CUP/CUU/CUD/CUF/CUB/CHA/VPA),
//! - kasowanie (ED/EL/ECH), wstawianie i usuwanie znaków/linii (ICH/DCH/IL/DL),
//! - SGR: 16/256/truecolor, pogrubienie, podkreślenie, odwrócenie, przygaszenie,
//! - obszar przewijania (DECSTBM), tryb alternatywny (`?1049`), autowrap (`?7`),
//! - klawisze aplikacji (`?1`), widoczność kursora (`?25`), bracketed paste (`?2004`),
//! - OSC 0/2 (tytuł okna), DSR/DA (odpowiedzi wysyłane z powrotem do PTY).

use std::collections::VecDeque;

/// Maksymalna liczba linii historii.
pub const SCROLLBACK_LIMIT: usize = 2000;

/// Kolor znaku: domyślny z motywu, indeks palety 0–255 albo RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorSpec {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Cechy tekstu (SGR).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attrs(u8);

impl Attrs {
    pub const BOLD: u8 = 1 << 0;
    pub const DIM: u8 = 1 << 1;
    pub const ITALIC: u8 = 1 << 2;
    pub const UNDERLINE: u8 = 1 << 3;
    pub const REVERSE: u8 = 1 << 4;
    pub const HIDDEN: u8 = 1 << 5;

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }

    pub fn set(&mut self, flag: u8, on: bool) {
        if on {
            self.0 |= flag;
        } else {
            self.0 &= !flag;
        }
    }

    pub fn reset(&mut self) {
        self.0 = 0;
    }
}

/// Pojedyncza komórka siatki.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cell {
    /// Tekst komórki (może zawierać znaki łączące). Pusty = spacja.
    pub text: String,
    pub fg: ColorSpec,
    pub bg: ColorSpec,
    pub attrs: Attrs,
}

impl Cell {
    fn blank(bg: ColorSpec) -> Self {
        Self {
            text: String::new(),
            fg: ColorSpec::Default,
            bg,
            attrs: Attrs::default(),
        }
    }

    /// Czy komórka jest pusta (do grupowania tła).
    pub fn is_blank(&self) -> bool {
        self.text.is_empty()
    }
}

#[derive(Debug, Clone)]
struct Grid {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
    cx: usize,
    cy: usize,
    saved: (usize, usize),
    top: usize,
    bottom: usize,
    wrap_pending: bool,
    origin_mode: bool,
}

impl Grid {
    fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Self {
            cols,
            rows,
            cells: vec![Cell::default(); cols * rows],
            cx: 0,
            cy: 0,
            saved: (0, 0),
            top: 0,
            bottom: rows - 1,
            wrap_pending: false,
            origin_mode: false,
        }
    }

    fn row(&self, y: usize) -> &[Cell] {
        let start = (y.min(self.rows - 1)) * self.cols;
        &self.cells[start..start + self.cols]
    }

    fn cell_mut(&mut self, x: usize, y: usize) -> &mut Cell {
        let x = x.min(self.cols - 1);
        let y = y.min(self.rows - 1);
        &mut self.cells[y * self.cols + x]
    }

    fn clear_row(&mut self, y: usize, from: usize, to: usize, bg: ColorSpec) {
        let start = y * self.cols;
        for cell in &mut self.cells[start + from..start + to] {
            *cell = Cell::blank(bg);
        }
    }

    // (Usunięte kopiowanie wierszy — używamy `rotate_left/right` na plasterkach.)

    /// Przewija region w górę o `n` linii (bez historii).
    fn scroll_region_up(&mut self, n: usize, bg: ColorSpec) {
        let count = self.bottom - self.top + 1;
        let n = n.max(1).min(count);
        let start = self.top * self.cols;
        let end = (self.bottom + 1) * self.cols;
        self.cells[start..end].rotate_left(n * self.cols);
        for y in (self.bottom + 1 - n)..=self.bottom {
            self.clear_row(y, 0, self.cols, bg);
        }
        self.wrap_pending = false;
    }

    /// Przewija region w dół o `n` linii.
    fn scroll_region_down(&mut self, n: usize, bg: ColorSpec) {
        let count = self.bottom - self.top + 1;
        let n = n.max(1).min(count);
        let start = self.top * self.cols;
        let end = (self.bottom + 1) * self.cols;
        self.cells[start..end].rotate_right(n * self.cols);
        for y in self.top..self.top + n {
            self.clear_row(y, 0, self.cols, bg);
        }
        self.wrap_pending = false;
    }

    fn insert_cells(&mut self, x: usize, n: usize, bg: ColorSpec) {
        let n = n.min(self.cols - x);
        if n == 0 {
            return;
        }
        let row = self.cy * self.cols;
        self.cells[row + x..row + self.cols].rotate_right(n);
        for cell in &mut self.cells[row + x..row + x + n] {
            *cell = Cell::blank(bg);
        }
    }

    fn delete_cells(&mut self, x: usize, n: usize, bg: ColorSpec) {
        let n = n.min(self.cols - x);
        if n == 0 {
            return;
        }
        let row = self.cy * self.cols;
        self.cells[row + x..row + self.cols].rotate_left(n);
        for cell in &mut self.cells[row + self.cols - n..row + self.cols] {
            *cell = Cell::blank(bg);
        }
    }
}

#[derive(Debug)]
enum State {
    Ground,
    Escape,
    Csi(Vec<u8>),
    Osc(Vec<u8>, bool),
    Charset,
    Ignore(bool),
}

/// Emulator jednego ekranu terminala.
pub struct Terminal {
    cols: usize,
    rows: usize,
    primary: Grid,
    alt: Option<Grid>,
    scrollback: VecDeque<Vec<Cell>>,
    /// Pozycja widoku: 0 = dół (bieżący ekran), >0 = linie historii.
    scroll_offset: usize,

    pub title: String,
    pub cursor_visible: bool,
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub bell: bool,

    insert_mode: bool,
    newline_mode: bool,
    autowrap: bool,

    fg: ColorSpec,
    bg: ColorSpec,
    attrs: Attrs,

    state: State,
    utf8_pending: Vec<u8>,
    response: Vec<u8>,
}

impl Terminal {
    pub fn new(cols: usize, rows: usize) -> Self {
        let primary = Grid::new(cols, rows);
        Self {
            cols: primary.cols,
            rows: primary.rows,
            primary,
            alt: None,
            scrollback: VecDeque::new(),
            scroll_offset: 0,
            title: String::new(),
            cursor_visible: true,
            application_cursor: false,
            bracketed_paste: false,
            bell: false,
            insert_mode: false,
            newline_mode: false,
            autowrap: true,
            fg: ColorSpec::Default,
            bg: ColorSpec::Default,
            attrs: Attrs::default(),
            state: State::Ground,
            utf8_pending: Vec::new(),
            response: Vec::new(),
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub fn max_scroll(&self) -> usize {
        self.scrollback.len()
    }

    pub fn is_alt(&self) -> bool {
        self.alt.is_some()
    }

    pub fn cursor(&self) -> (usize, usize) {
        let g = self.grid();
        (g.cx, g.cy)
    }

    pub fn cursor_is_visible(&self) -> bool {
        self.cursor_visible && self.scroll_offset == 0
    }

    fn grid(&self) -> &Grid {
        self.alt.as_ref().unwrap_or(&self.primary)
    }

    fn grid_mut(&mut self) -> &mut Grid {
        self.alt.as_mut().unwrap_or(&mut self.primary)
    }

    /// Wiersz w buforze absolutnym: `0` to najstarsza linia historii, dalej bieżący ekran.
    pub fn line(&self, index: usize) -> &[Cell] {
        let history = self.scrollback.len();
        if index < history {
            return self.scrollback.get(index).map(Vec::as_slice).unwrap_or(&[]);
        }
        let y = index - history;
        if y < self.grid().rows {
            self.grid().row(y)
        } else {
            &[]
        }
    }

    /// Liczba wierszy w buforze absolutnym (historia + bieżący ekran).
    pub fn line_count(&self) -> usize {
        self.scrollback.len() + self.grid().rows
    }

    /// Indeks pierwszego wiersza widoku w buforze absolutnym.
    pub fn view_start(&self) -> usize {
        self.scrollback.len().saturating_sub(self.scroll_offset)
    }

    /// Wiersz widoku (z uwzględnieniem historii) — `0` to góra ekranu.
    pub fn view_row(&self, screen_row: usize, offset: usize) -> &[Cell] {
        let start = self.scrollback.len().saturating_sub(offset);
        self.line(start + screen_row)
    }

    /// Tekst zaznaczenia między dwoma punktami bufora (włącznie).
    ///
    /// Wiersze są łączone `\n`, a końcowe spacje każdej linii są obcinane —
    /// tak jak w większości terminali. Punkty mogą być podane w dowolnej kolejności.
    pub fn text_between(&self, a: (usize, usize), b: (usize, usize)) -> String {
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let mut out = String::new();
        for row in start.0..=end.0 {
            if row > start.0 {
                out.push('\n');
            }
            let cells = self.line(row);
            let from = if row == start.0 {
                start.1.min(cells.len())
            } else {
                0
            };
            let to = if row == end.0 {
                end.1.saturating_add(1).min(cells.len())
            } else {
                cells.len()
            };
            let mut text = String::new();
            for cell in &cells[from.min(to)..to] {
                text.push_str(&cell.text);
            }
            out.push_str(text.trim_end());
        }
        out
    }

    /// Przewija widok w górę (starsze linie). W trybie alternatywnym historia nie jest
    /// dostępna — tak jak w xtermie.
    pub fn view_up(&mut self, lines: usize) {
        if self.alt.is_some() {
            return;
        }
        self.scroll_offset = (self.scroll_offset + lines).min(self.max_scroll());
    }

    /// Przewija widok w dół (nowsze linie).
    pub fn view_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    pub fn view_top(&mut self) {
        if self.alt.is_some() {
            return;
        }
        self.scroll_offset = self.max_scroll();
    }

    pub fn view_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    /// Zmiana rozmiaru siatki (bez reflow — jak w większości terminali).
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        resize_grid(&mut self.primary, cols, rows);
        if let Some(alt) = &mut self.alt {
            resize_grid(alt, cols, rows);
        }
        for row in &mut self.scrollback {
            row.resize(cols, Cell::default());
        }
        self.cols = cols;
        self.rows = rows;
        self.scroll_offset = self.scroll_offset.min(self.scrollback.len());
    }

    /// Przetwarza bajty z PTY. Zwraca odpowiedzi do wysłania z powrotem.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.response.clear();
        let mut input = Vec::with_capacity(self.utf8_pending.len() + bytes.len());
        input.append(&mut self.utf8_pending);
        input.extend_from_slice(bytes);

        let mut i = 0;
        while i < input.len() {
            let byte = input[i];
            // UTF-8 tylko w stanie Ground; w OSC/CSI bajty składamy bez zmian.
            if matches!(self.state, State::Ground) && byte >= 0x80 {
                let len = utf8_len(byte);
                if len == 0 {
                    self.put_char('\u{fffd}');
                    i += 1;
                    continue;
                }
                if i + len > input.len() {
                    // Niepełny znak — dokończymy przy następnym `feed`.
                    self.utf8_pending.extend_from_slice(&input[i..]);
                    i = input.len();
                    continue;
                }
                match std::str::from_utf8(&input[i..i + len]) {
                    Ok(text) => {
                        for ch in text.chars() {
                            self.put_char(ch);
                        }
                        i += len;
                    }
                    Err(_) => {
                        // Uszkodzona sekwencja: zjadamy tylko jeden bajt, aby nie
                        // połknąć następnych (np. ESC rozpoczynającego sekwencję
                        // sterującą, która przyszła w tym samym fragmencie PTY).
                        self.put_char('\u{fffd}');
                        i += 1;
                    }
                }
                continue;
            }

            i += 1;
            let state = std::mem::replace(&mut self.state, State::Ground);
            self.step(state, byte);
        }
        std::mem::take(&mut self.response)
    }

    fn step(&mut self, state: State, byte: u8) {
        match state {
            State::Ground => self.ground(byte),
            State::Escape => self.escape(byte),
            State::Csi(mut buf) => {
                if byte == 0x1b {
                    self.state = State::Escape;
                } else if (0x40..=0x7e).contains(&byte) {
                    buf.push(byte);
                    self.csi(&buf);
                } else {
                    if buf.len() < 256 {
                        buf.push(byte);
                    }
                    self.state = State::Csi(buf);
                }
            }
            State::Osc(mut buf, saw_esc) => {
                if saw_esc {
                    if byte == b'\\' {
                        self.osc(&buf);
                    } else {
                        if buf.len() < 4096 {
                            buf.push(0x1b);
                            buf.push(byte);
                        }
                        self.state = State::Osc(buf, false);
                    }
                } else if byte == 0x07 {
                    self.osc(&buf);
                } else if byte == 0x1b {
                    self.state = State::Osc(buf, true);
                } else {
                    if buf.len() < 4096 {
                        buf.push(byte);
                    }
                    self.state = State::Osc(buf, false);
                }
            }
            State::Charset => {}
            State::Ignore(saw_esc) => {
                if saw_esc {
                    if byte == b'\\' {
                        return;
                    }
                    self.state = State::Ignore(false);
                } else if byte == 0x1b {
                    self.state = State::Ignore(true);
                } else if byte == 0x07 {
                    // BEL też kończy DCS/APC.
                } else {
                    self.state = State::Ignore(false);
                }
            }
        }
    }

    fn ground(&mut self, byte: u8) {
        match byte {
            0x1b => self.state = State::Escape,
            0x07 => self.bell = true,
            0x08 => {
                let g = self.grid_mut();
                g.wrap_pending = false;
                g.cx = g.cx.saturating_sub(1);
            }
            0x09 => self.tab_forward(1),
            0x0a..=0x0c => {
                self.line_feed();
                if self.newline_mode {
                    self.grid_mut().cx = 0;
                }
            }
            0x0d => {
                let g = self.grid_mut();
                g.cx = 0;
                g.wrap_pending = false;
            }
            0x0e | 0x0f | 0x00..=0x06 | 0x10..=0x1a | 0x1c..=0x1f | 0x7f => {}
            _ => self.put_char(byte as char),
        }
    }

    fn escape(&mut self, byte: u8) {
        match byte {
            b'[' => self.state = State::Csi(Vec::new()),
            b']' => self.state = State::Osc(Vec::new(), false),
            b'(' | b')' | b'*' | b'+' => self.state = State::Charset,
            b'7' => {
                let g = self.grid_mut();
                g.saved = (g.cx, g.cy);
            }
            b'8' => {
                let g = self.grid_mut();
                g.cx = g.saved.0.min(g.cols - 1);
                g.cy = g.saved.1.min(g.rows - 1);
            }
            b'D' => self.line_feed(),
            b'E' => {
                self.grid_mut().cx = 0;
                self.line_feed();
            }
            b'M' => self.reverse_index(),
            b'H' => self.set_tab_stop(),
            b'c' => self.reset(),
            b'P' | b'X' | b'^' | b'_' => self.state = State::Ignore(false),
            _ => {}
        }
    }

    fn osc(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let Some((code, value)) = text.split_once(';') else {
            return;
        };
        if code == "0" || code == "2" {
            let clean: String = value
                .chars()
                .filter(|c| !c.is_control())
                .take(160)
                .collect();
            self.title = clean.trim().to_owned();
        }
    }

    fn csi(&mut self, bytes: &[u8]) {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return;
        };
        let Some((final_index, final_byte)) = text.char_indices().next_back() else {
            return;
        };
        let body = &text[..final_index];
        let (private, body) = match body.strip_prefix('?') {
            Some(rest) => (Some(b'?'), rest),
            None => match body.strip_prefix('>') {
                Some(rest) => (Some(b'>'), rest),
                None => match body.strip_prefix('=') {
                    Some(rest) => (Some(b'='), rest),
                    None => match body.strip_prefix('<') {
                        Some(rest) => (Some(b'<'), rest),
                        None => match body.strip_prefix('!') {
                            Some(rest) => (Some(b'!'), rest),
                            None => (None, body),
                        },
                    },
                },
            },
        };
        let params: Vec<Option<usize>> = body
            .split(';')
            .map(|part| {
                let part = part.trim_start_matches(['<', '=', '!']);
                if part.is_empty() {
                    None
                } else {
                    part.parse().ok()
                }
            })
            .collect();
        let n = |index: usize| params.get(index).copied().flatten();

        if private == Some(b'>') {
            // Secondary DA — nie odpowiadamy (klient może pytać o wersję xterm).
            return;
        }
        if private == Some(b'?') {
            match final_byte {
                'h' | 'l' => {
                    let on = final_byte == 'h';
                    for param in params.iter().flatten() {
                        self.private_mode(*param, on);
                    }
                }
                _ => {}
            }
            return;
        }
        // Kitty keyboard protocol (`CSI = ... u`), SGR mouse (`CSI < ... M/m`)
        // i inne sekwencje prywatne nie są poleceniami kursora. W szczególności
        // `ESC [ = 5 u` nie może przywracać zapisanego kursora — fish wysyła to
        // na starcie i powodowało przeskok kursora w losowe miejsce.
        if matches!(private, Some(b'=') | Some(b'<') | Some(b'!')) {
            return;
        }

        match final_byte {
            'A' => self.move_cursor(0, -(n(0).unwrap_or(1).max(1) as isize)),
            'B' => self.move_cursor(0, n(0).unwrap_or(1).max(1) as isize),
            'C' => self.move_cursor(n(0).unwrap_or(1).max(1) as isize, 0),
            'D' => self.move_cursor(-(n(0).unwrap_or(1).max(1) as isize), 0),
            'E' => {
                let count = n(0).unwrap_or(1).max(1);
                self.grid_mut().cx = 0;
                for _ in 0..count {
                    self.line_feed();
                }
            }
            'F' => {
                let count = n(0).unwrap_or(1).max(1);
                self.grid_mut().cx = 0;
                for _ in 0..count {
                    self.line_up();
                }
            }
            'G' => self.grid_mut().cx = n(0).unwrap_or(1).saturating_sub(1).min(self.cols - 1),
            'd' => self.set_row(n(0).unwrap_or(1).saturating_sub(1)),
            'H' | 'f' => {
                let row = n(0).unwrap_or(1).max(1) - 1;
                let col = n(1).unwrap_or(1).max(1) - 1;
                self.set_cursor(col, row);
            }
            'J' => self.erase_display(n(0).unwrap_or(0)),
            'K' => self.erase_line(n(0).unwrap_or(0)),
            'L' => self.insert_lines(n(0).unwrap_or(1).max(1)),
            'M' => self.delete_lines(n(0).unwrap_or(1).max(1)),
            'P' => {
                let count = n(0).unwrap_or(1).max(1);
                let x = self.grid().cx;
                let bg = self.bg;
                self.grid_mut().delete_cells(x, count, bg);
            }
            '@' => {
                let count = n(0).unwrap_or(1).max(1);
                let x = self.grid().cx;
                let bg = self.bg;
                self.grid_mut().insert_cells(x, count, bg);
            }
            'X' => {
                let count = n(0).unwrap_or(1).max(1);
                let (x, y) = (self.grid().cx, self.grid().cy);
                let to = (x + count).min(self.cols);
                let bg = self.bg;
                self.grid_mut().clear_row(y, x, to, bg);
            }
            'S' => self.scroll_up(n(0).unwrap_or(1).max(1)),
            'T' => self.scroll_down(n(0).unwrap_or(1).max(1)),
            'r' => {
                let top = n(0).unwrap_or(1).max(1) - 1;
                let bottom = n(1).unwrap_or(self.rows).clamp(1, self.rows) - 1;
                if top < bottom {
                    let g = self.grid_mut();
                    g.top = top;
                    g.bottom = bottom;
                    g.cx = 0;
                    g.cy = if g.origin_mode { top } else { 0 };
                }
            }
            's' => {
                let g = self.grid_mut();
                g.saved = (g.cx, g.cy);
            }
            'u' => {
                let g = self.grid_mut();
                g.cx = g.saved.0.min(g.cols - 1);
                g.cy = g.saved.1.min(g.rows - 1);
            }
            'm' => self.sgr(&params),
            'h' | 'l' => {
                let on = final_byte == 'h';
                for param in params.iter().flatten() {
                    match *param {
                        4 => self.insert_mode = on,
                        20 => self.newline_mode = on,
                        _ => {}
                    }
                }
            }
            'n' => match n(0).unwrap_or(0) {
                5 => self.response.extend_from_slice(b"\x1b[0n"),
                6 => {
                    let (x, y) = (self.grid().cx + 1, self.grid().cy + 1);
                    self.response
                        .extend_from_slice(format!("\x1b[{y};{x}R").as_bytes());
                }
                _ => {}
            },
            'c' => self.response.extend_from_slice(b"\x1b[?1;2c"),
            _ => {}
        }
    }

    fn private_mode(&mut self, mode: usize, on: bool) {
        match mode {
            1 => self.application_cursor = on,
            6 => {
                let g = self.grid_mut();
                g.origin_mode = on;
                g.cx = 0;
                g.cy = if on { g.top } else { 0 };
            }
            7 => self.autowrap = on,
            25 => self.cursor_visible = on,
            47 | 1047 => {
                if on {
                    self.enter_alt(false);
                } else {
                    self.leave_alt(false);
                }
            }
            1049 => {
                if on {
                    self.enter_alt(true);
                } else {
                    self.leave_alt(true);
                }
            }
            2004 => self.bracketed_paste = on,
            _ => {}
        }
    }

    fn enter_alt(&mut self, save_cursor: bool) {
        if self.alt.is_some() {
            return;
        }
        if save_cursor {
            let g = &mut self.primary;
            g.saved = (g.cx, g.cy);
        }
        let mut alt = Grid::new(self.cols, self.rows);
        if !save_cursor {
            alt.cx = self.primary.cx;
            alt.cy = self.primary.cy;
        }
        self.alt = Some(alt);
        self.scroll_offset = 0;
    }

    fn leave_alt(&mut self, restore_cursor: bool) {
        if self.alt.take().is_some() && restore_cursor {
            let g = &mut self.primary;
            g.cx = g.saved.0.min(g.cols - 1);
            g.cy = g.saved.1.min(g.rows - 1);
        }
        self.scroll_offset = 0;
    }

    fn set_cursor(&mut self, x: usize, y: usize) {
        let g = self.grid_mut();
        let min_y = if g.origin_mode { g.top } else { 0 };
        let max_y = if g.origin_mode { g.bottom } else { g.rows - 1 };
        g.cx = x.min(g.cols - 1);
        g.cy = (y + min_y).clamp(min_y, max_y);
        g.wrap_pending = false;
    }

    fn set_row(&mut self, row: usize) {
        let g = self.grid_mut();
        let min_y = if g.origin_mode { g.top } else { 0 };
        let max_y = if g.origin_mode { g.bottom } else { g.rows - 1 };
        g.cy = (row + min_y).clamp(min_y, max_y);
        g.wrap_pending = false;
    }

    fn move_cursor(&mut self, dx: isize, dy: isize) {
        let g = self.grid_mut();
        let new_x = (g.cx as isize + dx).clamp(0, g.cols as isize - 1) as usize;
        let new_y = (g.cy as isize + dy).clamp(0, g.rows as isize - 1) as usize;
        g.cx = new_x;
        g.cy = new_y;
        g.wrap_pending = false;
    }

    fn line_up(&mut self) {
        let g = self.grid_mut();
        if g.cy > 0 {
            g.cy -= 1;
        }
        g.wrap_pending = false;
    }

    fn line_feed(&mut self) {
        let (cy, bottom) = {
            let g = self.grid();
            (g.cy, g.bottom)
        };
        if cy == bottom {
            self.scroll_up(1);
        } else if cy + 1 < self.rows {
            self.grid_mut().cy += 1;
        }
        self.grid_mut().wrap_pending = false;
    }

    fn reverse_index(&mut self) {
        let (cy, top) = {
            let g = self.grid();
            (g.cy, g.top)
        };
        if cy == top {
            self.scroll_down(1);
        } else if cy > 0 {
            self.grid_mut().cy -= 1;
        }
        self.grid_mut().wrap_pending = false;
    }

    fn scroll_up(&mut self, n: usize) {
        let n = n.max(1);
        let bg = self.bg;
        if self.alt.is_none() {
            let g = &mut self.primary;
            if g.top == 0 && g.bottom + 1 == self.rows {
                let before = self.scrollback.len();
                for _ in 0..n {
                    let row: Vec<Cell> = g.cells.drain(0..self.cols).collect();
                    g.cells
                        .extend(std::iter::repeat_with(Cell::default).take(self.cols));
                    self.scrollback.push_back(row);
                }
                let overflow = self.scrollback.len().saturating_sub(SCROLLBACK_LIMIT);
                self.scrollback.drain(0..overflow);
                // Gdy użytkownik ogląda historię, nowe linie nie mogą „przesunąć”
                // obrazu pod kursorem — korygujemy offset o zmianę długości historii.
                let after = self.scrollback.len();
                if self.scroll_offset > 0 {
                    if after > before {
                        self.scroll_offset = (self.scroll_offset + (after - before)).min(after);
                    } else {
                        self.scroll_offset = self.scroll_offset.saturating_sub(before - after);
                    }
                }
            } else {
                g.scroll_region_up(n, bg);
            }
        } else {
            self.grid_mut().scroll_region_up(n, bg);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let bg = self.bg;
        self.grid_mut().scroll_region_down(n, bg);
    }

    fn insert_lines(&mut self, n: usize) {
        let (cy, bottom, cols) = {
            let g = self.grid();
            (g.cy, g.bottom, g.cols)
        };
        if cy < self.grid().top || cy > bottom {
            return;
        }
        let n = n.min(bottom - cy + 1);
        let bg = self.bg;
        let start = cy * cols;
        let end = (bottom + 1) * cols;
        self.grid_mut().cells[start..end].rotate_right(n * cols);
        for y in cy..cy + n {
            self.grid_mut().clear_row(y, 0, cols, bg);
        }
    }

    fn delete_lines(&mut self, n: usize) {
        let (cy, bottom, cols) = {
            let g = self.grid();
            (g.cy, g.bottom, g.cols)
        };
        if cy < self.grid().top || cy > bottom {
            return;
        }
        let n = n.min(bottom - cy + 1);
        let bg = self.bg;
        let start = cy * cols;
        let end = (bottom + 1) * cols;
        self.grid_mut().cells[start..end].rotate_left(n * cols);
        for y in (bottom + 1 - n)..=bottom {
            self.grid_mut().clear_row(y, 0, cols, bg);
        }
    }

    fn erase_display(&mut self, mode: usize) {
        let (cx, cy, cols) = {
            let g = self.grid();
            (g.cx, g.cy, g.cols)
        };
        let bg = self.bg;
        match mode {
            0 => {
                self.grid_mut().clear_row(cy, cx, cols, bg);
                for y in cy + 1..self.rows {
                    self.grid_mut().clear_row(y, 0, cols, bg);
                }
            }
            1 => {
                for y in 0..cy {
                    self.grid_mut().clear_row(y, 0, cols, bg);
                }
                self.grid_mut().clear_row(cy, 0, cx + 1, bg);
            }
            2 => {
                for y in 0..self.rows {
                    self.grid_mut().clear_row(y, 0, cols, bg);
                }
            }
            3 => self.scrollback.clear(),
            _ => {}
        }
        self.grid_mut().wrap_pending = false;
    }

    fn erase_line(&mut self, mode: usize) {
        let (cx, cy, cols) = {
            let g = self.grid();
            (g.cx, g.cy, g.cols)
        };
        let (from, to) = match mode {
            0 => (cx, cols),
            1 => (0, cx + 1),
            2 => (0, cols),
            _ => return,
        };
        let bg = self.bg;
        self.grid_mut().clear_row(cy, from, to, bg);
        self.grid_mut().wrap_pending = false;
    }

    fn sgr(&mut self, params: &[Option<usize>]) {
        if params.is_empty() {
            self.reset_attrs();
            return;
        }
        let mut i = 0;
        while i < params.len() {
            let code = params[i].unwrap_or(0);
            match code {
                0 => self.reset_attrs(),
                1 => self.attrs.set(Attrs::BOLD, true),
                2 => self.attrs.set(Attrs::DIM, true),
                3 => self.attrs.set(Attrs::ITALIC, true),
                4 => self.attrs.set(Attrs::UNDERLINE, true),
                7 => self.attrs.set(Attrs::REVERSE, true),
                8 => self.attrs.set(Attrs::HIDDEN, true),
                22 => {
                    self.attrs.set(Attrs::BOLD, false);
                    self.attrs.set(Attrs::DIM, false);
                }
                23 => self.attrs.set(Attrs::ITALIC, false),
                24 => self.attrs.set(Attrs::UNDERLINE, false),
                27 => self.attrs.set(Attrs::REVERSE, false),
                28 => self.attrs.set(Attrs::HIDDEN, false),
                30..=37 => self.fg = ColorSpec::Indexed((code - 30) as u8),
                39 => self.fg = ColorSpec::Default,
                40..=47 => self.bg = ColorSpec::Indexed((code - 40) as u8),
                49 => self.bg = ColorSpec::Default,
                90..=97 => self.fg = ColorSpec::Indexed((code - 90 + 8) as u8),
                100..=107 => self.bg = ColorSpec::Indexed((code - 100 + 8) as u8),
                38 | 48 => {
                    let (color, used) = extended_color(params, i);
                    if code == 38 {
                        self.fg = color;
                    } else {
                        self.bg = color;
                    }
                    i += used;
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn reset_attrs(&mut self) {
        self.fg = ColorSpec::Default;
        self.bg = ColorSpec::Default;
        self.attrs.reset();
    }

    fn reset(&mut self) {
        self.primary = Grid::new(self.cols, self.rows);
        self.alt = None;
        self.scrollback.clear();
        self.scroll_offset = 0;
        self.title.clear();
        self.cursor_visible = true;
        self.application_cursor = false;
        self.bracketed_paste = false;
        self.insert_mode = false;
        self.newline_mode = false;
        self.autowrap = true;
        self.reset_attrs();
        self.state = State::Ground;
        self.utf8_pending.clear();
    }

    fn set_tab_stop(&mut self) {
        // Tabulatory co 8 kolumn — własne stopnie nie są potrzebne w praktyce.
    }

    fn tab_forward(&mut self, n: usize) {
        for _ in 0..n {
            let g = self.grid_mut();
            let next = ((g.cx / 8) + 1) * 8;
            g.cx = next.min(g.cols - 1);
            g.wrap_pending = false;
        }
    }

    fn put_char(&mut self, ch: char) {
        let width = char_width(ch);
        if width == 0 {
            // Znak łączący — doklej do poprzedniej komórki.
            let g = self.grid_mut();
            let index = if g.cx > 0 {
                g.cy * g.cols + g.cx - 1
            } else if g.cy > 0 {
                (g.cy - 1) * g.cols + g.cols - 1
            } else {
                0
            };
            g.cells[index].text.push(ch);
            return;
        }

        if self.grid().wrap_pending {
            if self.autowrap {
                self.grid_mut().cx = 0;
                self.line_feed();
            }
            self.grid_mut().wrap_pending = false;
        }

        let mut cx = self.grid().cx;
        if width == 2 && cx + 1 >= self.cols {
            if self.autowrap {
                self.grid_mut().cx = 0;
                self.line_feed();
                cx = 0;
            } else {
                cx = self.cols.saturating_sub(2);
            }
        }

        let (fg, bg, attrs, insert) = (self.fg, self.bg, self.attrs, self.insert_mode);
        if insert {
            self.grid_mut().insert_cells(cx, width, bg);
        }
        {
            let g = self.grid_mut();
            let cy = g.cy;
            let cell = g.cell_mut(cx, cy);
            cell.text.clear();
            cell.text.push(ch);
            cell.fg = fg;
            cell.bg = bg;
            cell.attrs = attrs;
        }
        if width == 2 && cx + 1 < self.cols {
            let g = self.grid_mut();
            let cy = g.cy;
            let cell = g.cell_mut(cx + 1, cy);
            *cell = Cell {
                text: String::new(),
                fg,
                bg,
                attrs,
            };
        }

        let cols = self.cols;
        let autowrap = self.autowrap;
        let g = self.grid_mut();
        // `width` poza `cx` nie może przekroczyć ostatniej kolumny.
        let next = cx + width;
        if next >= cols {
            g.cx = cols - 1;
            g.wrap_pending = autowrap;
        } else {
            g.cx = next;
        }
    }
}

fn resize_grid(grid: &mut Grid, cols: usize, rows: usize) {
    let mut cells = vec![Cell::default(); cols * rows];
    let copy_cols = cols.min(grid.cols);
    let copy_rows = rows.min(grid.rows);
    for y in 0..copy_rows {
        let src = y * grid.cols;
        let dst = y * cols;
        cells[dst..dst + copy_cols].clone_from_slice(&grid.cells[src..src + copy_cols]);
    }
    grid.cols = cols;
    grid.rows = rows;
    grid.cells = cells;
    grid.cx = grid.cx.min(cols - 1);
    grid.cy = grid.cy.min(rows - 1);
    grid.top = 0;
    grid.bottom = rows - 1;
    grid.wrap_pending = false;
}

/// Kolor rozszerzony SGR (`38;5;n`, `38;2;r;g;b`). Zwraca kolor i liczbę
/// dodatkowych parametrów.
fn extended_color(params: &[Option<usize>], start: usize) -> (ColorSpec, usize) {
    match params.get(start + 1).copied().flatten() {
        Some(5) => {
            let index = params
                .get(start + 2)
                .copied()
                .flatten()
                .unwrap_or(0)
                .min(255) as u8;
            (ColorSpec::Indexed(index), 2)
        }
        Some(2) => {
            let r = params.get(start + 2).copied().flatten().unwrap_or(0) as u8;
            let g = params.get(start + 3).copied().flatten().unwrap_or(0) as u8;
            let b = params.get(start + 4).copied().flatten().unwrap_or(0) as u8;
            (ColorSpec::Rgb(r, g, b), 4)
        }
        _ => (ColorSpec::Default, 0),
    }
}

/// Długość sekwencji UTF-8 na podstawie bajtu wiodącego (0 = nieprawidłowy).
fn utf8_len(byte: u8) -> usize {
    match byte {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 0,
    }
}

/// Przybliżona szerokość znaku w kolumnach (0 = łączący, 2 = szeroki).
pub fn char_width(ch: char) -> usize {
    let cp = ch as u32;
    if cp == 0 {
        return 0;
    }
    // Znaki łączące i selektory wariantów.
    if (0x0300..=0x036f).contains(&cp)
        || (0x0483..=0x0489).contains(&cp)
        || (0x0591..=0x05bd).contains(&cp)
        || (0x0610..=0x061a).contains(&cp)
        || (0x064b..=0x065f).contains(&cp)
        || (0x0e31..=0x0e3a).contains(&cp)
        || (0x1ab0..=0x1aff).contains(&cp)
        || (0x1dc0..=0x1dff).contains(&cp)
        || (0x20d0..=0x20ff).contains(&cp)
        || (0xfe00..=0xfe0f).contains(&cp)
        || (0xfe20..=0xfe2f).contains(&cp)
        || (0xe0100..=0xe01ef).contains(&cp)
    {
        return 0;
    }
    // Szerokie (CJK, Hangul, pełnoekranowe formy, emoji).
    if (0x1100..=0x115f).contains(&cp)
        || (0x2e80..=0xa4cf).contains(&cp)
        || (0xac00..=0xd7a3).contains(&cp)
        || (0xf900..=0xfaff).contains(&cp)
        || (0xfe10..=0xfe19).contains(&cp)
        || (0xfe30..=0xfe6f).contains(&cp)
        || (0xff00..=0xff60).contains(&cp)
        || (0xffe0..=0xffe6).contains(&cp)
        || (0x1f300..=0x1f64f).contains(&cp)
        || (0x1f900..=0x1f9ff).contains(&cp)
        || (0x20000..=0x3fffd).contains(&cp)
    {
        return 2;
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term() -> Terminal {
        Terminal::new(10, 4)
    }

    fn line(term: &Terminal, y: usize) -> String {
        term.view_row(y, 0)
            .iter()
            .map(|cell| {
                if cell.text.is_empty() {
                    ' '
                } else {
                    cell.text.chars().next().unwrap_or(' ')
                }
            })
            .collect()
    }

    #[test]
    fn writes_text_and_wraps() {
        let mut t = term();
        t.feed(b"abcde");
        assert_eq!(line(&t, 0), "abcde     ");
        t.feed(b"fghij");
        assert_eq!(line(&t, 0), "abcdefghij");
        assert_eq!(t.cursor(), (9, 0));
        t.feed(b"K");
        assert_eq!(line(&t, 1), "K         ");
        assert_eq!(t.cursor(), (1, 1));
    }

    #[test]
    fn newline_and_scrollback() {
        let mut t = Terminal::new(5, 2);
        t.feed(b"a\r\nb\r\nc\r\n");
        assert_eq!(t.scrollback_len(), 2);
        // Ekran pokazuje dwie ostatnie linie.
        assert_eq!(t.view_row(0, 0)[0].text, "c");
        assert_eq!(t.view_row(1, 0)[0].text, "");
        t.view_top();
        assert_eq!(t.view_row(0, t.scroll_offset())[0].text, "a");
        t.view_bottom();
        assert_eq!(t.scroll_offset(), 0);
    }

    #[test]
    fn sgr_colors_and_attrs() {
        let mut t = term();
        t.feed(b"\x1b[1;31mA\x1b[0mB\x1b[38;5;196mC\x1b[38;2;1;2;3mD");
        let row = t.view_row(0, 0);
        assert_eq!(row[0].text, "A");
        assert!(row[0].attrs.has(Attrs::BOLD));
        assert_eq!(row[0].fg, ColorSpec::Indexed(1));
        assert_eq!(row[1].fg, ColorSpec::Default);
        assert!(!row[1].attrs.has(Attrs::BOLD));
        assert_eq!(row[2].fg, ColorSpec::Indexed(196));
        assert_eq!(row[3].fg, ColorSpec::Rgb(1, 2, 3));
    }

    #[test]
    fn cursor_moves_and_erases() {
        let mut t = term();
        t.feed(b"hello");
        t.feed(b"\x1b[1;1H\x1b[K");
        assert_eq!(line(&t, 0), "          ");
        t.feed(b"\x1b[2;3HX");
        assert_eq!(t.view_row(1, 0)[2].text, "X");
        t.feed(b"\x1b[2J");
        assert_eq!(line(&t, 1), "          ");
    }

    #[test]
    fn scroll_region_keeps_top_line() {
        let mut t = Terminal::new(4, 4);
        t.feed(b"1\r\n2\r\n3\r\n4");
        t.feed(b"\x1b[2;3r\x1b[3;1H\nX");
        // Wiersz 1 jest poza regionem i zostaje.
        assert_eq!(line(&t, 0), "1   ");
        assert_eq!(line(&t, 1), "3   ");
    }

    #[test]
    fn alt_screen_restores_content() {
        let mut t = term();
        t.feed(b"main");
        t.feed(b"\x1b[?1049h");
        t.feed(b"\x1b[2Jalt");
        assert_eq!(t.view_row(0, 0)[0].text, "a");
        t.feed(b"\x1b[?1049l");
        assert_eq!(t.view_row(0, 0)[0].text, "m");
    }

    #[test]
    fn osc_sets_title_and_dsr_responds() {
        let mut t = term();
        let out = t.feed(b"\x1b]2;Moja powloka\x07");
        assert!(out.is_empty());
        assert_eq!(t.title, "Moja powloka");

        t.feed(b"\x1b[3;4H");
        let out = t.feed(b"\x1b[6n");
        assert_eq!(out, b"\x1b[3;4R");
        assert_eq!(t.feed(b"\x1b[c"), b"\x1b[?1;2c");
    }

    #[test]
    fn tab_and_backspace() {
        let mut t = term();
        t.feed(b"a\tb");
        assert_eq!(t.view_row(0, 0)[8].text, "b");
        t.feed(b"\x08\x08X");
        assert_eq!(t.view_row(0, 0)[7].text, "X");
    }

    #[test]
    fn resize_keeps_content_and_clamps_cursor() {
        let mut t = Terminal::new(4, 2);
        t.feed(b"abcd\r\nef");
        t.resize(6, 3);
        assert_eq!(t.cols(), 6);
        assert_eq!(t.rows(), 3);
        assert_eq!(t.view_row(0, 0)[0].text, "a");
        assert_eq!(t.view_row(1, 0)[1].text, "f");
        t.resize(2, 1);
        assert_eq!(t.cols(), 2);
        assert_eq!(t.cursor().0, 1);
    }

    #[test]
    fn history_view_stays_anchored_when_output_arrives() {
        let mut t = Terminal::new(5, 2);
        t.feed(b"a\r\nb\r\nc\r\n");
        t.view_top();
        let before = t.view_row(0, t.scroll_offset())[0].text.clone();
        assert!(t.scroll_offset() > 0);
        t.feed(b"d\r\n");
        assert_eq!(t.view_row(0, t.scroll_offset())[0].text, before);
    }

    #[test]
    fn wide_char_occupies_two_columns() {
        let mut t = term();
        t.feed("界".as_bytes());
        assert_eq!(t.cursor(), (2, 0));
        assert_eq!(char_width('界'), 2);
        assert_eq!(char_width('\u{301}'), 0);
    }

    #[test]
    fn insert_and_delete_chars() {
        let mut t = term();
        t.feed(b"abcde\x1b[1;2H\x1b[2@XY\x1b[1;1H\x1b[3P");
        // „X” i „Y” wstawione w kolumnie 1, potem usunięte 3 znaki od początku.
        assert_eq!(line(&t, 0), "bcde      ");
    }

    #[test]
    fn copies_trimmed_selection_text() {
        let mut t = Terminal::new(20, 4);
        t.feed(b"hello world\r\nsecond line\r\n");
        let start = t.view_start();
        assert_eq!(start, 0);
        assert_eq!(t.text_between((0, 0), (0, 4)), "hello");
        assert_eq!(t.text_between((0, 6), (1, 5)), "world\nsecond");
        // Odwrócone punkty działają tak samo.
        assert_eq!(t.text_between((1, 11), (0, 0)), "hello world\nsecond line");
        // Kolumny poza linią są przycinane, a końcowe spacje obcinane.
        assert_eq!(t.text_between((0, 5), (1, 99)), " world\nsecond line");
    }

    #[test]
    fn kitty_keyboard_flags_do_not_restore_cursor() {
        let mut t = Terminal::new(20, 4);
        t.feed(b"\x1b[3;7H\x1b[s\x1b[1;1H");
        t.feed(b"\x1b[u");
        assert_eq!(t.cursor(), (6, 2));
        // `CSI = 5 u` to kitty keyboard protocol, nie restore cursor.
        t.feed(b"\x1b[1;1H\x1b[=5u");
        assert_eq!(t.cursor(), (0, 0));
    }

    #[test]
    fn fish_prompt_redraw_ignores_kitty_keyboard_flags() {
        let mut t = Terminal::new(80, 24);
        // Kursor zapisany gdzie indziej — błędne `CSI = 5 u` byłoby widoczne.
        t.feed(b"\x1b[10;20H\x1b[s\x1b[1;1H");
        t.feed(b"\x1b[?2004h\x1b[?2031h\x1b[=5u");
        t.feed(b"\x1b]133;A;click_events=1\x1b\\");
        t.feed(b"\x1b[92mpatryks\x1b[m@\x1b[mpatryk-tobefilledbyoem\x1b[m \x1b[32m~/luna\x1b[m (main)\x1b[m> ");
        t.feed(b"\x1b]133;B\x1b\\\x1b[K\r\x1b[46C");
        assert_eq!(t.cursor(), (46, 0));
        assert_eq!(
            line(&t, 0).trim_end(),
            "patryks@patryk-tobefilledbyoem ~/luna (main)>"
        );
    }

    #[test]
    fn incomplete_utf8_does_not_swallow_following_escape() {
        let mut t = Terminal::new(20, 4);
        // „⏎” (U+23CE) = E2 8F 8E. Pierwszy fragment kończy się w połowie.
        t.feed(&[0xE2, 0x8F]);
        // Drugi fragment kończy znak i zawiera ESC [ K. ESC musi zostać wykonane,
        // a nie wpisane do siatki jako literały "[K".
        t.feed(&[0x8E, 0x1b, b'[', b'K']);
        assert_eq!(line(&t, 0).trim_end(), "⏎");
        assert!(!line(&t, 0).contains('['));
        assert!(!line(&t, 0).contains('K'));
        assert_eq!(t.cursor(), (1, 0));
    }

    #[test]
    fn absolute_lines_include_scrollback() {
        let mut t = Terminal::new(5, 2);
        t.feed(b"a\r\nb\r\nc\r\n");
        assert_eq!(t.scrollback_len(), 2);
        assert_eq!(t.line_count(), 4);
        assert_eq!(t.line(0)[0].text, "a");
        assert_eq!(t.line(t.line_count() - 2)[0].text, "c");
        t.view_top();
        assert_eq!(t.view_start(), 0);
        assert_eq!(t.view_row(0, t.scroll_offset())[0].text, "a");
    }
}
