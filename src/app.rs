//! Pętla aplikacji terminala: PTY, wejście, zakładki, schowek, tytuł okna i rysowanie.

use std::error::Error;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use libluna_core::{
    ButtonState, Event, KeyEvent, Point, PointerButton, Rect, Size, Theme, ThemeSettings,
    WindowEvent,
};
use libluna_i18n::{set_locale, Locale};
use libluna_platform::{run as run_app, AppConfig, Frame, WakeHandle};
use libluna_render::TextEngine;
use libluna_widgets::{
    ControlsSide, HeaderBar, Menu, MenuEntry, Tab as WidgetTab, TabBar, WindowControl,
};

use crate::prefs::{CursorShape, Prefs, Preset, Scheme};
use crate::pty::{Pty, PtyEvent};
use crate::term::{Cell, Terminal};
use crate::ui;
use crate::ui::SettingsHit;

/// Odstęp między kliknięciami traktowanymi jako podwójne/potrójne.
const MULTI_CLICK: Duration = Duration::from_millis(400);

/// Jedna zakładka terminala: emulator, proces i termin zamknięcia po wyjściu.
struct Pane {
    term: Terminal,
    pty: Option<Pty>,
    close_at: Option<Instant>,
}

/// Zaznaczenie w buforze absolutnym (wiersz liczony od najstarszej linii historii).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Selection {
    anchor: (usize, usize),
    head: (usize, usize),
}

impl Selection {
    fn normalized(self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    fn is_empty(self) -> bool {
        self.anchor == self.head
    }
}

/// Akcja menu kontekstowego (PPM).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContextAction {
    Cut,
    Copy,
    Paste,
    SelectAll,
    NewTab,
    CloseTab,
    Settings,
}

/// Otwarte menu kontekstowe: widget, jego prostokąt i akcje wierszy.
struct ContextMenu {
    menu: Menu,
    panel: Rect,
    actions: Vec<Option<ContextAction>>,
}

/// Stan aplikacji.
pub struct App {
    panes: Vec<Pane>,
    active: usize,
    /// Polecenie z `-e` / pierwszego argumentu; `None` = powłoka użytkownika.
    command: Option<(String, Vec<String>)>,
    started: bool,
    prefs: Prefs,
    hover: Option<WindowControl>,
    tab_hover: Option<usize>,
    hover_plus: bool,
    hover_settings: bool,
    tab_scroll: f64,
    pointer: Option<Point>,
    selection: Option<Selection>,
    dragging: bool,
    last_press: Option<(Instant, (usize, usize), u8)>,
    settings_open: bool,
    context_menu: Option<ContextMenu>,
    reported_title: Option<String>,
    hold_open: bool,
    pending_close: bool,
    pending_minimize: bool,
    pending_maximize: bool,
    pending_move: bool,
}

impl App {
    pub fn new(command: Option<(String, Vec<String>)>, prefs: Prefs) -> Self {
        Self {
            panes: Vec::new(),
            active: 0,
            command,
            started: false,
            prefs,
            hover: None,
            tab_hover: None,
            hover_plus: false,
            hover_settings: false,
            tab_scroll: 0.0,
            pointer: None,
            selection: None,
            dragging: false,
            last_press: None,
            settings_open: false,
            context_menu: None,
            reported_title: None,
            hold_open: std::env::var("LUNA_TERMINAL_HOLD")
                .map(|v| !v.is_empty() && v != "0")
                .unwrap_or(false),
            pending_close: false,
            pending_minimize: false,
            pending_maximize: false,
            pending_move: false,
        }
    }

    fn pane(&self) -> &Pane {
        &self.panes[self.active.min(self.panes.len().saturating_sub(1))]
    }

    fn pane_mut(&mut self) -> &mut Pane {
        let index = self.active.min(self.panes.len().saturating_sub(1));
        &mut self.panes[index]
    }

    fn titles(&self) -> Vec<String> {
        self.panes
            .iter()
            .map(|pane| {
                if pane.term.title.is_empty() {
                    libluna_i18n::tr("Terminal")
                } else {
                    pane.term.title.clone()
                }
            })
            .collect()
    }

    fn active_title(&self) -> String {
        self.titles()
            .get(self.active)
            .cloned()
            .unwrap_or_else(|| libluna_i18n::tr("Terminal"))
    }

    fn start(&mut self, frame: &mut Frame, theme: &Theme) {
        if self.started {
            return;
        }
        self.started = true;
        let lay = ui::layout(frame.size, theme, frame.text, self.prefs.font_size, 1);
        let wake = Some(frame.waker());
        self.panes.clear();
        self.active = 0;
        let command = self.command.clone();
        self.open_pane(lay.cols, lay.rows, wake, command);
    }

    fn sync_layout(&mut self, frame: &Frame, theme: &Theme) {
        let lay = ui::layout(
            frame.size,
            theme,
            frame.text,
            self.prefs.font_size,
            self.panes.len(),
        );
        for pane in &mut self.panes {
            if (pane.term.cols(), pane.term.rows()) != (lay.cols, lay.rows) {
                pane.term.resize(lay.cols, lay.rows);
                if let Some(pty) = &mut pane.pty {
                    pty.resize(
                        lay.cols.min(u16::MAX as usize) as u16,
                        lay.rows.min(u16::MAX as usize) as u16,
                    );
                }
            }
        }
    }

    /// Otwiera nową zakładkę (nowy PTY) i ustawia ją jako aktywną.
    ///
    /// `command` dotyczy tylko pierwszej zakładki (z `-e`); kolejne uruchamiają
    /// powłokę użytkownika (albo `LUNA_TERMINAL_CMD` w testach).
    fn open_pane(
        &mut self,
        cols: usize,
        rows: usize,
        wake: Option<WakeHandle>,
        command: Option<(String, Vec<String>)>,
    ) {
        let cols16 = cols.min(u16::MAX as usize) as u16;
        let rows16 = rows.min(u16::MAX as usize) as u16;
        let mut term = Terminal::new(cols, rows);
        let result = match &command {
            Some((program, args)) => {
                let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                Pty::spawn_command(program, &refs, cols16, rows16, wake)
            }
            None => Pty::spawn(cols16, rows16, wake),
        };
        let pty = match result {
            Ok(pty) => Some(pty),
            Err(err) => {
                let message = format!(
                    "\x1b[1;31m{}\x1b[0m\r\n{err}\r\n",
                    libluna_i18n::tr("Luna Terminal: cannot start the pseudoterminal")
                );
                term.feed(message.as_bytes());
                None
            }
        };
        self.panes.push(Pane {
            term,
            pty,
            close_at: None,
        });
        self.active = self.panes.len() - 1;
        self.selection = None;
        self.dragging = false;
        self.last_press = None;
    }

    fn new_tab(&mut self, frame: &mut Frame) {
        let (cols, rows) = (self.pane().term.cols(), self.pane().term.rows());
        let wake = Some(frame.waker());
        self.open_pane(cols, rows, wake, None);
        self.tab_scroll = 0.0;
        frame.request_frame();
    }

    /// Zamyka zakładkę; ostatnia zakładka zamyka okno.
    fn close_tab(&mut self, index: usize) {
        if self.panes.len() <= 1 {
            self.pending_close = true;
            return;
        }
        if index >= self.panes.len() {
            return;
        }
        self.panes.remove(index);
        if index < self.active {
            self.active -= 1;
        } else if index == self.active {
            self.active = self.active.min(self.panes.len() - 1);
        }
        self.selection = None;
        self.dragging = false;
    }

    fn switch(&mut self, delta: isize) {
        let len = self.panes.len();
        if len <= 1 {
            return;
        }
        let len_i = len as isize;
        self.active = ((self.active as isize + delta).rem_euclid(len_i)) as usize;
        self.selection = None;
        self.dragging = false;
    }

    fn select_tab(&mut self, index: usize) {
        if index < self.panes.len() {
            self.active = index;
            self.selection = None;
            self.dragging = false;
        }
    }

    fn zoom(&mut self, delta: f64) {
        self.prefs.zoom(delta);
        self.prefs.save();
        self.selection = None;
    }

    fn drain(&mut self, frame: &mut Frame) {
        let mut close_due = Vec::new();
        let mut next_deadline: Option<Duration> = None;
        let hold = self.hold_open;
        for (index, pane) in self.panes.iter_mut().enumerate() {
            let events = pane
                .pty
                .as_ref()
                .map(|pty| {
                    let mut out = Vec::new();
                    while let Some(event) = pty.try_recv() {
                        out.push(event);
                    }
                    out
                })
                .unwrap_or_default();
            for event in events {
                match event {
                    PtyEvent::Data(bytes) => {
                        let responses = pane.term.feed(&bytes);
                        if !responses.is_empty() {
                            if let Some(pty) = &mut pane.pty {
                                let _ = pty.write(&responses);
                            }
                        }
                    }
                    PtyEvent::Exited(code) => {
                        if !hold && pane.close_at.is_none() {
                            let note = format!(
                                "\r\n\x1b[2m[{}: {code}]\x1b[0m\r\n",
                                libluna_i18n::tr("process exited")
                            );
                            pane.term.feed(note.as_bytes());
                            pane.close_at = Some(Instant::now() + Duration::from_millis(450));
                        }
                    }
                }
            }
            if let Some(deadline) = pane.close_at {
                let now = Instant::now();
                if now >= deadline {
                    close_due.push(index);
                } else {
                    let delay = deadline - now;
                    next_deadline = Some(next_deadline.map_or(delay, |d| d.min(delay)));
                }
            }
        }
        for index in close_due.into_iter().rev() {
            self.close_tab(index);
        }
        if let Some(delay) = next_deadline {
            frame.request_frame_after(delay);
        }
    }

    fn copy_selection(&mut self, frame: &mut Frame) {
        let Some(selection) = self.selection else {
            return;
        };
        let Some(pane) = self.panes.get(self.active) else {
            return;
        };
        let (start, end) = selection.normalized();
        let text = pane.term.text_between(start, end);
        if !text.is_empty() {
            frame.copy_to_clipboard(text);
        }
    }

    fn paste(&mut self, text: &str) {
        self.close_context_menu();
        if text.is_empty() {
            return;
        }
        let Some(pane) = self.panes.get_mut(self.active) else {
            return;
        };
        let bracketed = pane.term.bracketed_paste;
        let mut data = Vec::with_capacity(text.len() + 12);
        if bracketed {
            data.extend_from_slice(b"\x1b[200~");
        }
        data.extend_from_slice(text.as_bytes());
        if bracketed {
            data.extend_from_slice(b"\x1b[201~");
        }
        if let Some(pty) = &mut pane.pty {
            let _ = pty.write(&data);
        }
        pane.term.view_bottom();
        self.selection = None;
    }

    fn close_context_menu(&mut self) {
        self.context_menu = None;
    }

    fn open_context_menu(&mut self, text: &TextEngine, size: Size, theme: &Theme, at: Point) {
        let selected = self
            .selection
            .is_some_and(|selection| !selection.is_empty());
        let mut entries = Vec::new();
        let mut actions = Vec::new();

        let copy = MenuEntry::action(libluna_i18n::tr("Copy")).shortcut("Ctrl+Shift+C");
        entries.push(if selected { copy } else { copy.disabled() });
        actions.push(Some(ContextAction::Copy));

        let cut = MenuEntry::action(libluna_i18n::tr("Cut")).shortcut("Ctrl+Shift+X");
        entries.push(if selected { cut } else { cut.disabled() });
        actions.push(Some(ContextAction::Cut));

        entries.push(MenuEntry::action(libluna_i18n::tr("Paste")).shortcut("Ctrl+Shift+V"));
        actions.push(Some(ContextAction::Paste));
        entries.push(MenuEntry::separator());
        actions.push(None);
        entries.push(MenuEntry::action(libluna_i18n::tr("Select All")));
        actions.push(Some(ContextAction::SelectAll));
        entries.push(MenuEntry::separator());
        actions.push(None);
        entries.push(MenuEntry::action(libluna_i18n::tr("New Tab")).shortcut("Ctrl+Shift+T"));
        actions.push(Some(ContextAction::NewTab));
        if self.panes.len() > 1 {
            entries.push(MenuEntry::action(libluna_i18n::tr("Close Tab")).shortcut("Ctrl+Shift+W"));
            actions.push(Some(ContextAction::CloseTab));
        }
        entries.push(MenuEntry::action(libluna_i18n::tr("Settings")).shortcut("Ctrl+,"));
        actions.push(Some(ContextAction::Settings));

        let menu = Menu::new(entries);
        let width = menu.width(text, theme);
        let viewport = Rect::from_xywh(0.0, 0.0, size.width, size.height);
        let anchor = Rect::from_xywh(at.x, at.y, 1.0, 1.0);
        let panel = menu.place(anchor, width, viewport, theme);
        self.context_menu = Some(ContextMenu {
            menu,
            panel,
            actions,
        });
    }

    fn select_all(&mut self) {
        let Some(pane) = self.panes.get(self.active) else {
            return;
        };
        let rows = pane.term.line_count();
        if rows == 0 {
            return;
        }
        let last = rows - 1;
        let cols = pane.term.line(last).len();
        self.selection = Some(Selection {
            anchor: (0, 0),
            head: (last, cols.saturating_sub(1)),
        });
        self.dragging = false;
    }

    fn context_action(&mut self, frame: &mut Frame, action: ContextAction) {
        match action {
            ContextAction::Copy => self.copy_selection(frame),
            ContextAction::Cut => {
                self.copy_selection(frame);
                self.selection = None;
            }
            ContextAction::Paste => frame.paste_from_clipboard(),
            ContextAction::SelectAll => self.select_all(),
            ContextAction::NewTab => self.new_tab(frame),
            ContextAction::CloseTab => self.close_tab(self.active),
            ContextAction::Settings => {
                self.settings_open = true;
                self.selection = None;
            }
        }
        frame.request_frame();
    }

    fn key(&mut self, frame: &mut Frame, ke: &KeyEvent, lay: &ui::Layout) {
        let ctrl = ke.modifiers.ctrl;
        let shift = ke.modifiers.shift;

        if self.context_menu.is_some() {
            if ke.keysym == 0xff1b {
                self.close_context_menu();
                frame.request_frame();
                return;
            }
            self.close_context_menu();
        }

        if self.settings_open {
            if ke.keysym == 0xff1b {
                self.settings_open = false;
                self.prefs.save();
                frame.request_frame();
            }
            return;
        }

        // Same modyfikatory nie są „wpisywaniem”: Ctrl+Shift+C dociera jako trzy
        // osobne zdarzenia (Ctrl, Shift, C), a czyszczenie zaznaczenia na pierwszym
        // z nich kasowało tekst, zanim skrót kopiowania zdążył go odczytać.
        if is_modifier_keysym(ke.keysym) {
            return;
        }

        if ctrl && shift {
            match ke.keysym {
                0x43 | 0x63 => {
                    self.copy_selection(frame);
                    return;
                }
                0x56 | 0x76 => {
                    frame.paste_from_clipboard();
                    return;
                }
                0x54 | 0x74 => {
                    self.new_tab(frame);
                    return;
                }
                0x57 | 0x77 => {
                    self.close_tab(self.active);
                    return;
                }
                0x4e | 0x6e => {
                    open_window();
                    return;
                }
                _ => {}
            }
        }

        if ctrl {
            match ke.keysym {
                0xff09 => {
                    self.switch(if shift { -1 } else { 1 });
                    return;
                }
                0xff55 => {
                    self.switch(-1);
                    return;
                }
                0xff56 => {
                    self.switch(1);
                    return;
                }
                0x2b | 0x3d => {
                    self.zoom(1.0);
                    return;
                }
                0x2d => {
                    self.zoom(-1.0);
                    return;
                }
                0x30 => {
                    self.zoom(crate::prefs::DEFAULT_FONT_SIZE - self.prefs.font_size);
                    return;
                }
                0x2c => {
                    self.settings_open = true;
                    frame.request_frame();
                    return;
                }
                _ => {}
            }
        }

        if ke.modifiers.alt {
            if let Some(index) = alt_tab_index(ke.keysym) {
                self.select_tab(index);
                return;
            }
        }

        // Shift+PageUp/PageDown/Home/End — historia (nie idzie do powłoki).
        if shift && !ctrl {
            match ke.keysym {
                0xff55 => {
                    self.pane_mut()
                        .term
                        .view_up(lay.rows.saturating_sub(1).max(1));
                    return;
                }
                0xff56 => {
                    self.pane_mut()
                        .term
                        .view_down(lay.rows.saturating_sub(1).max(1));
                    return;
                }
                0xff50 => {
                    self.pane_mut().term.view_top();
                    return;
                }
                0xff57 => {
                    self.pane_mut().term.view_bottom();
                    return;
                }
                _ => {}
            }
        }

        // Każde wpisanie klawisza wraca do bieżącego ekranu i czyści zaznaczenie.
        let application_cursor = self.pane().term.application_cursor;
        if let Some(bytes) = encode_key(ke, application_cursor) {
            let pane = self.pane_mut();
            pane.term.view_bottom();
            if let Some(pty) = &mut pane.pty {
                let _ = pty.write(&bytes);
            }
        }
        self.selection = None;
        self.dragging = false;
    }

    fn handle_events(
        &mut self,
        frame: &mut Frame,
        lay: &ui::Layout,
        theme: &Theme,
        text: &TextEngine,
    ) {
        let events = frame.events.to_vec();
        for event in &events {
            match event {
                Event::PointerMotion { position, .. } => {
                    self.pointer = Some(*position);
                    if let Some(menu) = &mut self.context_menu {
                        menu.menu.hover_at(menu.panel, Some(*position), theme);
                        frame.request_frame();
                        continue;
                    }
                    self.update_hover(lay, theme, text, *position);
                    if self.dragging {
                        if let Some(cell) = self.cell_at(lay, *position, true) {
                            let abs = self.absolute(cell);
                            if let Some(selection) = &mut self.selection {
                                selection.head = abs;
                            }
                        }
                    }
                }
                Event::PointerLeave => {
                    self.close_context_menu();
                    self.pointer = None;
                    self.hover = None;
                    self.tab_hover = None;
                    self.hover_plus = false;
                    self.hover_settings = false;
                }
                Event::PointerButton(button) => {
                    self.pointer_button(frame, lay, theme, text, button)
                }
                Event::Scroll(scroll) => self.scroll(lay, theme, text, scroll),
                Event::Key(key) if key.state == ButtonState::Pressed => self.key(frame, key, lay),
                Event::Paste(text) => self.paste(text),
                // Upuszczenie plików w oknie terminala wstawia ich ścieżki
                // (w cudzysłowach, gotowe do użycia w powłoce).
                Event::Drop { paths, .. } => {
                    let text = shell_paths(&paths);
                    self.paste(&text);
                }
                Event::Window(WindowEvent::Resize(_)) => self.close_context_menu(),
                _ => {}
            }
        }
    }

    fn update_hover(&mut self, lay: &ui::Layout, theme: &Theme, text: &TextEngine, p: Point) {
        if lay.header.contains(p) {
            self.hover = HeaderBar::new()
                .controls(ControlsSide::Right)
                .control_at(lay.header, theme, p);
            self.hover_plus = lay.plus.contains(p);
            self.hover_settings = lay.settings.contains(p);
            self.tab_hover = if self.panes.len() > 1 {
                self.tab_bar(lay.tabs, text, theme)
                    .index_at(lay.tabs, text, theme, p)
            } else {
                None
            };
        } else {
            self.hover = None;
            self.hover_plus = false;
            self.hover_settings = false;
            self.tab_hover = None;
        }
    }

    fn pointer_button(
        &mut self,
        frame: &mut Frame,
        lay: &ui::Layout,
        theme: &Theme,
        text: &TextEngine,
        button: &libluna_core::PointerEvent,
    ) {
        let p = button.position;

        // Otwarte menu kontekstowe: lewy/środkowy klik wybiera pozycję albo zamyka
        // menu (klik poza nim nie trafia już do terminala). PPM tylko przełącza menu.
        if self.context_menu.is_some() && button.button != PointerButton::Right {
            let action = self.context_menu.as_ref().and_then(|menu| {
                menu.menu
                    .index_at(menu.panel, p, theme)
                    .and_then(|index| menu.actions.get(index).copied().flatten())
            });
            self.close_context_menu();
            if let Some(action) = action {
                self.context_action(frame, action);
            }
            return;
        }

        match (button.button, button.state) {
            (PointerButton::Left, ButtonState::Pressed) => {
                if self.settings_open {
                    if let Some(hit) = ui::settings_hit(frame.size, theme, p) {
                        self.apply_settings(frame, hit);
                    }
                    return;
                }
                if lay.header.contains(p) {
                    let header = HeaderBar::new().controls(ControlsSide::Right);
                    if let Some(control) = header.control_at(lay.header, theme, p) {
                        match control {
                            WindowControl::Minimize => self.pending_minimize = true,
                            WindowControl::Maximize => self.pending_maximize = true,
                            WindowControl::Close => self.pending_close = true,
                        }
                        return;
                    }
                    if lay.settings.contains(p) {
                        self.settings_open = true;
                        self.selection = None;
                        return;
                    }
                    if lay.plus.contains(p) {
                        self.new_tab(frame);
                        return;
                    }
                    if self.panes.len() > 1 {
                        let bar = self.tab_bar(lay.tabs, text, theme);
                        if let Some(index) = bar.close_at(lay.tabs, text, theme, p) {
                            self.close_tab(index);
                            return;
                        }
                        if let Some(index) = bar.index_at(lay.tabs, text, theme, p) {
                            self.select_tab(index);
                            return;
                        }
                    }
                    self.pending_move = true;
                    return;
                }
                if let Some(cell) = self.cell_at(lay, p, false) {
                    self.begin_selection(cell, button.modifiers.shift);
                }
            }
            (PointerButton::Left, ButtonState::Released) => {
                self.dragging = false;
                if self.selection.is_some_and(Selection::is_empty) {
                    self.selection = None;
                }
            }
            (PointerButton::Middle, ButtonState::Pressed) => {
                if !self.settings_open && lay.area.contains(p) {
                    frame.paste_from_clipboard();
                }
            }
            (PointerButton::Right, ButtonState::Pressed) => {
                if self.settings_open {
                    return;
                }
                if self.context_menu.is_some() {
                    self.close_context_menu();
                } else {
                    self.dragging = false;
                    self.open_context_menu(text, frame.size, theme, p);
                }
                frame.request_frame();
            }
            _ => {}
        }
    }

    fn scroll(
        &mut self,
        lay: &ui::Layout,
        theme: &Theme,
        text: &TextEngine,
        scroll: &libluna_core::ScrollEvent,
    ) {
        if self.context_menu.is_some() {
            self.close_context_menu();
            return;
        }
        if self.settings_open {
            return;
        }
        if self.panes.len() > 1 && lay.tabs.contains(scroll.position) {
            let mut bar = self.tab_bar(lay.tabs, text, theme);
            if bar.on_scroll(scroll.dy * 40.0, lay.tabs, text, theme) {
                self.tab_scroll = bar.scroll();
            }
            return;
        }
        if lay.area.contains(scroll.position) {
            let lines = (scroll.dy.abs().ceil() as usize).max(1) * 3;
            let pane = self.pane_mut();
            if scroll.dy > 0.0 {
                pane.term.view_down(lines);
            } else {
                pane.term.view_up(lines);
            }
        }
    }

    fn apply_settings(&mut self, frame: &mut Frame, hit: SettingsHit) {
        match hit {
            SettingsHit::Close => {
                self.settings_open = false;
                self.prefs.save();
            }
            SettingsHit::Scheme(index) => {
                let scheme = Scheme::ALL[index.min(Scheme::ALL.len() - 1)];
                self.prefs.scheme = scheme;
                if scheme == Scheme::System {
                    // „System” = podążaj za motywem; ręczne nadpisania kolorów
                    // wracają do wartości domyślnych.
                    self.prefs.background = 0;
                    self.prefs.foreground = 0;
                }
                self.prefs.save();
            }
            SettingsHit::Preset(index) => {
                self.prefs.preset = Preset::ALL[index.min(Preset::ALL.len() - 1)];
                self.prefs.save();
            }
            SettingsHit::FontSmaller => {
                self.prefs.zoom(-1.0);
                self.prefs.save();
            }
            SettingsHit::FontLarger => {
                self.prefs.zoom(1.0);
                self.prefs.save();
            }
            SettingsHit::CursorShape(index) => {
                self.prefs.cursor_shape = CursorShape::ALL[index.min(CursorShape::ALL.len() - 1)];
                self.prefs.save();
            }
            SettingsHit::Background => {
                self.prefs.cycle_background();
                self.prefs.save();
            }
            SettingsHit::Foreground => {
                self.prefs.cycle_foreground();
                self.prefs.save();
            }
            SettingsHit::Cursor => {
                self.prefs.cycle_cursor();
                self.prefs.save();
            }
        }
        frame.request_frame();
    }

    fn apply_pending(&mut self, frame: &mut Frame) {
        if self.pending_close {
            frame.close();
            self.pending_close = false;
        }
        if self.pending_minimize {
            frame.minimize();
            self.pending_minimize = false;
        }
        if self.pending_maximize {
            frame.toggle_maximize();
            self.pending_maximize = false;
        }
        if self.pending_move {
            frame.begin_move();
            self.pending_move = false;
        }
    }

    fn tab_bar(&self, rect: libluna_core::Rect, text: &TextEngine, theme: &Theme) -> TabBar {
        let tabs: Vec<WidgetTab> = self
            .titles()
            .into_iter()
            .map(|title| WidgetTab::new(title).closable(true))
            .collect();
        let mut bar = TabBar::with_tabs(tabs).expand(false);
        bar.set_selected(self.active);
        bar.set_hovered(self.tab_hover);
        bar.set_scroll(self.tab_scroll, rect, text, theme);
        bar
    }

    /// Przewija pasek tak, żeby aktywna zakładka była widoczna.
    fn ensure_active_tab(&mut self, lay: &ui::Layout, theme: &Theme, text: &TextEngine) {
        if self.panes.len() < 2 {
            return;
        }
        let mut bar = self.tab_bar(lay.tabs, text, theme);
        bar.scroll_into_view(self.active, lay.tabs, text, theme);
        self.tab_scroll = bar.scroll();
    }

    /// Zamienia punkt w oknie na (wiersz, kolumna) widoku.
    fn cell_at(&self, lay: &ui::Layout, p: Point, clamp: bool) -> Option<(usize, usize)> {
        let (x, y) = if clamp {
            (
                p.x.clamp(lay.inner.x(), lay.inner.right() - 0.5),
                p.y.clamp(lay.inner.y(), lay.inner.bottom() - 0.5),
            )
        } else if lay.inner.contains(p) {
            (p.x, p.y)
        } else {
            return None;
        };
        let col = (((x - lay.origin.x) / lay.cell_w).floor() as isize)
            .clamp(0, lay.cols as isize - 1) as usize;
        let row = (((y - lay.origin.y) / lay.cell_h).floor() as isize)
            .clamp(0, lay.rows as isize - 1) as usize;
        Some((row, col))
    }

    /// Punkt widoku → punkt bufora absolutnego.
    fn absolute(&self, (row, col): (usize, usize)) -> (usize, usize) {
        (self.pane().term.view_start() + row, col)
    }

    fn begin_selection(&mut self, cell: (usize, usize), shift: bool) {
        let abs = self.absolute(cell);
        if shift {
            if let Some(selection) = &mut self.selection {
                selection.head = abs;
                self.dragging = true;
                return;
            }
        }
        let now = Instant::now();
        let mut count = 1;
        if let Some((then, position, previous)) = self.last_press {
            if now.duration_since(then) < MULTI_CLICK && position == abs {
                count = previous % 3 + 1;
            }
        }
        self.last_press = Some((now, abs, count));
        let selection = {
            let term = &self.pane().term;
            match count {
                2 => word_selection(term, abs),
                3 => line_selection(term, abs),
                _ => Selection {
                    anchor: abs,
                    head: abs,
                },
            }
        };
        self.selection = Some(selection);
        self.dragging = true;
    }

    /// Zaznaczenie w przestrzeni widoku (przycięte do ekranu).
    fn selection_view(&self, lay: &ui::Layout) -> Option<ui::SelectionView> {
        let selection = self.selection?;
        let term = &self.pane().term;
        let (start, end) = selection.normalized();
        let first = term.view_start();
        let last = first + lay.rows;
        if end.0 < first || start.0 >= last {
            return None;
        }
        let start_row = start.0.saturating_sub(first);
        let end_row = end.0.saturating_sub(first).min(lay.rows.saturating_sub(1));
        let start_col = if start.0 < first {
            0
        } else {
            start.1.min(lay.cols.saturating_sub(1))
        };
        let end_col = if end.0 >= last {
            lay.cols.saturating_sub(1)
        } else {
            end.1.min(lay.cols.saturating_sub(1))
        };
        Some(ui::SelectionView {
            start: (start_row, start_col),
            end: (end_row, end_col),
        })
    }
}

fn alt_tab_index(keysym: u32) -> Option<usize> {
    match keysym {
        0x31..=0x39 => Some((keysym - 0x31) as usize),
        _ => None,
    }
}

/// Zaznacza słowo (ciąg znaków bez spacji) wokół punktu.
fn word_selection(term: &Terminal, (row, col): (usize, usize)) -> Selection {
    let cells = term.line(row);
    if cells.is_empty() {
        return Selection {
            anchor: (row, col),
            head: (row, col),
        };
    }
    let col = col.min(cells.len() - 1);
    let is_word = |cell: &Cell| cell.text.chars().any(|c| !c.is_whitespace());
    if !is_word(&cells[col]) {
        return Selection {
            anchor: (row, col),
            head: (row, col),
        };
    }
    let mut start = col;
    while start > 0 && is_word(&cells[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < cells.len() && is_word(&cells[end + 1]) {
        end += 1;
    }
    Selection {
        anchor: (row, start),
        head: (row, end),
    }
}

/// Zaznacza cały wiersz.
fn line_selection(term: &Terminal, (row, _): (usize, usize)) -> Selection {
    Selection {
        anchor: (row, 0),
        head: (row, term.line(row).len().saturating_sub(1)),
    }
}

/// Uruchamia terminal.
pub fn run() -> Result<(), Box<dyn Error>> {
    let (command, help) = parse_args();
    if help {
        print_help();
        return Ok(());
    }

    set_locale(Locale::from_env());
    libluna_i18n::load_catalogs_from_dirs();

    let mut prefs = Prefs::load();
    if let Some(size) = std::env::var("LUNA_TERMINAL_FONT_SIZE")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|size| (crate::prefs::MIN_FONT_SIZE..=crate::prefs::MAX_FONT_SIZE).contains(size))
    {
        prefs.font_size = size;
    }
    let system_dark = std::env::var("LUNA_SYSTEM_DARK")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false);

    let config = AppConfig::new("Terminal")
        .app_id("eu.luna.Terminal")
        .initial_size(900, 600)
        .settings(ThemeSettings::load())
        .system_dark(system_dark)
        // Escape jest zwykłym klawiszem terminala (np. w `vim`), nie zamyka okna.
        .escape_to_exit(false)
        .monospace(true);

    let mut app = App::new(command, prefs);
    run_app(config, move |frame: &mut Frame| {
        let theme = frame.settings().resolve_themed(frame.system_dark());

        app.start(frame, &theme);
        app.sync_layout(frame, &theme);
        app.drain(frame);
        let lay = ui::layout(
            frame.size,
            &theme,
            frame.text,
            app.prefs.font_size,
            app.panes.len(),
        );
        let text = frame.text;
        app.handle_events(frame, &lay, &theme, text);
        app.apply_pending(frame);
        app.sync_layout(frame, &theme);

        let desired_title = app.active_title();
        if app.reported_title.as_deref() != Some(desired_title.as_str()) {
            frame.set_title(&desired_title);
            app.reported_title = Some(desired_title.clone());
        }

        let lay = ui::layout(
            frame.size,
            &theme,
            frame.text,
            app.prefs.font_size,
            app.panes.len(),
        );
        app.ensure_active_tab(&lay, &theme, text);
        let titles = app.titles();
        let view = ui::ViewState {
            term: &app.pane().term,
            title: &desired_title,
            tabs: &titles,
            active_tab: app.active,
            tab_hover: app.tab_hover,
            tab_scroll: app.tab_scroll,
            selection: app.selection_view(&lay),
            hover: app.hover,
            plus_hover: app.hover_plus,
            settings_hover: app.hover_settings,
            font_size: app.prefs.font_size,
            prefs: &app.prefs,
        };
        ui::paint(frame.scene, &theme, frame.text, frame.size, &view);
        if app.settings_open {
            ui::paint_settings(
                frame.scene,
                &theme,
                frame.text,
                frame.size,
                &app.prefs,
                app.pointer,
            );
        }
        if let Some(menu) = &app.context_menu {
            ui::paint_context_menu(frame.scene, &theme, frame.text, &menu.menu, menu.panel);
        }
    })
}

/// Otwiera kolejne okno terminala (bez czekania na jego zakończenie).
fn open_window() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = Command::new(exe)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

fn parse_args() -> (Option<(String, Vec<String>)>, bool) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut command = None;
    let mut help = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                help = true;
                break;
            }
            "-e" | "--" => {
                if let Some(program) = args.get(i + 1) {
                    command = Some((program.clone(), args[i + 2..].to_vec()));
                }
                break;
            }
            value if !value.starts_with('-') => {
                command = Some((value.to_owned(), args[i + 1..].to_vec()));
                break;
            }
            _ => i += 1,
        }
    }
    (command, help)
}

fn print_help() {
    println!(
        "luna-terminal — terminal Luny (libLuna)\n\n\
         Użycie: luna-terminal [polecenie [argumenty…]]\n  \
           luna-terminal                 uruchamia $SHELL (domyślnie /bin/sh)\n  \
           luna-terminal -e htop         uruchamia podane polecenie\n\n\
         Skróty:\n  \
           Ctrl+Shift+C / Ctrl+Shift+V   kopiuj / wklej\n  \
           Ctrl+Shift+T / Ctrl+Shift+W   nowa / zamknij zakładkę\n  \
           Ctrl+Shift+N                  nowe okno\n  \
           Ctrl+Tab, Ctrl+PageUp/Down    przełączanie zakładek\n  \
           Alt+1…9                       wybór zakładki\n  \
           Ctrl+,                        ustawienia (kolory, pismo)\n  \
           Ctrl++ / Ctrl+- / Ctrl+0      stopień pisma\n  \
           Shift+PageUp / Shift+PageDown historia\n  \
           Shift+Home / Shift+End        początek / koniec historii\n\n\
         Zmienne:\n  \
           LUNA_TERMINAL_FONT_SIZE   stopień pisma (8–32, domyślnie 14)\n  \
           LUNA_TERMINAL_CMD         polecenie zamiast powłoki (przez sh -c)\n  \
           LUNA_TERMINAL_HOLD=1      nie zamykaj okna po zakończeniu procesu\n"
    );
}

/// Czy to klawisz modyfikujący (Shift/Ctrl/Alt/Super, blokady)?
///
/// Wayland wysyła je jak zwykłe klawisze — nie produkują tekstu, więc nie mogą
/// kończyć zaznaczenia ani trafiać do PTY.
fn is_modifier_keysym(keysym: u32) -> bool {
    matches!(
        keysym,
        0xffe1..=0xffe6   // Shift, Control, Caps_Lock, Shift_Lock
            | 0xffe7..=0xffee // Meta, Alt, Super, Hyper
            | 0xfe03 | 0xfe11 // ISO_Level3_Shift / ISO_Level5_Shift
            | 0xff7e | 0xff7f // Mode_switch / Num_Lock
    )
}

/// Zamienia zdarzenie klawiatury na bajty dla PTY (xterm-256color).
fn encode_key(ke: &KeyEvent, application_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = ke.modifiers.ctrl;
    let alt = ke.modifiers.alt;
    let shift = ke.modifiers.shift;

    if ctrl {
        if let Some(byte) = control_byte(ke.keysym) {
            let mut out = Vec::with_capacity(2);
            if alt {
                out.push(0x1b);
            }
            out.push(byte);
            return Some(out);
        }
    }

    let sequence: Option<&[u8]> = match ke.keysym {
        0xff0d | 0xff8d => Some(b"\r"),
        0xff08 => Some(b"\x7f"),
        0xff09 if shift => Some(b"\x1b[Z"),
        0xff09 => Some(b"\t"),
        0xff1b => Some(b"\x1b"),
        0xff51 => Some(if application_cursor {
            b"\x1bOD"
        } else {
            b"\x1b[D"
        }),
        0xff52 => Some(if application_cursor {
            b"\x1bOA"
        } else {
            b"\x1b[A"
        }),
        0xff53 => Some(if application_cursor {
            b"\x1bOC"
        } else {
            b"\x1b[C"
        }),
        0xff54 => Some(if application_cursor {
            b"\x1bOB"
        } else {
            b"\x1b[B"
        }),
        0xff50 => Some(if application_cursor {
            b"\x1bOH"
        } else {
            b"\x1b[H"
        }),
        0xff57 => Some(if application_cursor {
            b"\x1bOF"
        } else {
            b"\x1b[F"
        }),
        0xff63 => Some(b"\x1b[2~"),
        0xffff => Some(b"\x1b[3~"),
        0xff55 => Some(b"\x1b[5~"),
        0xff56 => Some(b"\x1b[6~"),
        // F1–F4: SS3; F5–F12: CSI.
        0xffbe => Some(b"\x1bOP"),
        0xffbf => Some(b"\x1bOQ"),
        0xffc0 => Some(b"\x1bOR"),
        0xffc1 => Some(b"\x1bOS"),
        0xffc2 => Some(b"\x1b[15~"),
        0xffc3 => Some(b"\x1b[17~"),
        0xffc4 => Some(b"\x1b[18~"),
        0xffc5 => Some(b"\x1b[19~"),
        0xffc6 => Some(b"\x1b[20~"),
        0xffc7 => Some(b"\x1b[21~"),
        0xffc8 => Some(b"\x1b[23~"),
        0xffc9 => Some(b"\x1b[24~"),
        _ => None,
    };
    if let Some(sequence) = sequence {
        let mut out = Vec::with_capacity(sequence.len() + 1);
        if alt {
            out.push(0x1b);
        }
        out.extend_from_slice(sequence);
        return Some(out);
    }

    if !ctrl {
        if let Some(text) = &ke.text {
            if !text.is_empty() && !text.chars().all(char::is_control) {
                let mut out = Vec::with_capacity(text.len() + 1);
                if alt {
                    out.push(0x1b);
                }
                out.extend_from_slice(text.as_bytes());
                return Some(out);
            }
        }
        if (0x20..=0x7e).contains(&ke.keysym) {
            let mut out = Vec::with_capacity(2);
            if alt {
                out.push(0x1b);
            }
            out.push(ke.keysym as u8);
            return Some(out);
        }
    }
    None
}

fn control_byte(keysym: u32) -> Option<u8> {
    match keysym {
        0x61..=0x7a => Some((keysym - 0x60) as u8),
        0x40 => Some(0x00),
        0x5b => Some(0x1b),
        0x5c => Some(0x1c),
        0x5d => Some(0x1d),
        0x5e => Some(0x1e),
        0x5f => Some(0x1f),
        0x20 => Some(0x00),
        _ => None,
    }
}

/// Ścieżki plików jako argumenty powłoki (z cudzysłowami, gdy trzeba).
fn shell_paths(paths: &[std::path::PathBuf]) -> String {
    paths
        .iter()
        .map(|path| {
            let text = path.display().to_string();
            let simple = !text.is_empty()
                && text
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || "/._-+@%:,".contains(ch));
            if simple {
                text
            } else {
                format!("'{}'", text.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::shell_paths;
    use std::path::PathBuf;

    #[test]
    fn dropped_paths_are_quoted_for_the_shell() {
        assert_eq!(
            shell_paths(&[PathBuf::from("/home/ula/plik.txt")]),
            "/home/ula/plik.txt"
        );
        assert_eq!(
            shell_paths(&[
                PathBuf::from("/home/ula/Moje pliki/a.txt"),
                PathBuf::from("/tmp/x'y"),
            ]),
            "'/home/ula/Moje pliki/a.txt' '/tmp/x'\\''y'"
        );
    }

    use super::*;
    use libluna_core::{Modifiers, Size};

    fn key(keysym: u32, text: Option<&str>) -> KeyEvent {
        KeyEvent {
            keysym,
            keycode: 0,
            text: text.map(str::to_owned),
            state: ButtonState::Pressed,
            modifiers: Modifiers::NONE,
            repeat: false,
        }
    }

    fn with_ctrl(mut ke: KeyEvent) -> KeyEvent {
        ke.modifiers.ctrl = true;
        ke
    }

    #[test]
    fn maps_control_and_special_keys() {
        assert_eq!(
            encode_key(&with_ctrl(key(0x63, Some("c"))), false),
            Some(vec![0x03])
        );
        assert_eq!(
            encode_key(&key(0xff52, None), false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_key(&key(0xff52, None), true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(encode_key(&key(0xff08, None), false), Some(vec![0x7f]));
        assert_eq!(encode_key(&key(0xff09, None), false), Some(vec![b'\t']));
    }

    #[test]
    fn maps_text_and_alt() {
        assert_eq!(
            encode_key(&key(0x61, Some("ą")), false),
            Some("ą".as_bytes().to_vec())
        );
        let mut alt = key(0x78, Some("x"));
        alt.modifiers.alt = true;
        assert_eq!(encode_key(&alt, false), Some(b"\x1bx".to_vec()));
    }

    #[test]
    fn modifier_keys_are_not_typing() {
        // Wayland wysyła same modyfikatory jako zwykłe klawisze — nie mogą
        // kończyć zaznaczenia (inaczej Ctrl+Shift+C gubiło tekst przed „C”).
        for keysym in [0xffe1, 0xffe2, 0xffe3, 0xffe4, 0xffe9, 0xffeb, 0xfe03] {
            assert!(is_modifier_keysym(keysym), "{keysym:#x}");
        }
        assert!(!is_modifier_keysym(0x43)); // C
        assert!(!is_modifier_keysym(0x63)); // c
    }

    #[test]
    fn maps_alt_tab_numbers() {
        assert_eq!(alt_tab_index(0x31), Some(0));
        assert_eq!(alt_tab_index(0x39), Some(8));
        assert_eq!(alt_tab_index(0x30), None);
        assert_eq!(alt_tab_index(0x61), None);
    }

    #[test]
    fn word_selection_extends_to_whitespace() {
        let mut term = Terminal::new(30, 2);
        term.feed(b"echo /tmp/plik.txt test");
        let row = term.line_count() - term.rows();
        let selection = word_selection(&term, (row, 9));
        assert_eq!(selection.anchor, (row, 5));
        assert_eq!(selection.head, (row, 17));
        assert_eq!(
            term.text_between(selection.anchor, selection.head),
            "/tmp/plik.txt"
        );
        let space = word_selection(&term, (row, 4));
        assert!(space.is_empty());
    }

    #[test]
    fn line_selection_covers_whole_row() {
        let mut term = Terminal::new(20, 2);
        term.feed(b"hello");
        let row = term.line_count() - term.rows();
        let selection = line_selection(&term, (row, 2));
        assert_eq!(selection.anchor, (row, 0));
        assert_eq!(selection.head, (row, 19));
        assert_eq!(term.text_between(selection.anchor, selection.head), "hello");
    }

    fn app_with_panes(count: usize) -> App {
        let mut app = App::new(None, Prefs::default());
        for _ in 0..count {
            app.panes.push(Pane {
                term: Terminal::new(20, 4),
                pty: None,
                close_at: None,
            });
        }
        app
    }

    #[test]
    fn switching_and_closing_tabs_keeps_active_valid() {
        let mut app = app_with_panes(3);
        app.switch(1);
        assert_eq!(app.active, 1);
        app.close_tab(0);
        assert_eq!(app.panes.len(), 2);
        assert_eq!(app.active, 0);
        app.close_tab(1);
        assert_eq!(app.panes.len(), 1);
        assert_eq!(app.active, 0);
        app.close_tab(0);
        assert_eq!(app.panes.len(), 1);
        assert!(app.pending_close);
    }

    #[test]
    fn selection_view_clips_to_viewport() {
        let theme = Theme::dark();
        let text = TextEngine::mono();
        let lay = ui::layout(Size::new(300.0, 160.0), &theme, &text, 14.0, 1);
        let mut app = app_with_panes(1);
        app.panes[0].term = Terminal::new(lay.cols, lay.rows);
        for line in 0..lay.rows + 5 {
            app.panes[0].term.feed(format!("{line}\r\n").as_bytes());
        }
        app.panes[0].term.view_bottom();
        let start = app.panes[0].term.view_start();
        app.selection = Some(Selection {
            anchor: (start + 1, 0),
            head: (start + 2, 2),
        });
        let view = app.selection_view(&lay).expect("selection in view");
        assert_eq!(view.start, (1, 0));
        assert_eq!(view.end, (2, 2));

        app.panes[0].term.view_top();
        assert!(app.selection_view(&lay).is_none());
    }

    #[test]
    fn context_menu_exposes_clipboard_actions() {
        let mut app = app_with_panes(1);
        let theme = Theme::dark();
        let text = TextEngine::mono();
        app.open_context_menu(
            &text,
            Size::new(400.0, 300.0),
            &theme,
            Point::new(20.0, 20.0),
        );
        let menu = app.context_menu.as_ref().expect("menu");
        let actions: Vec<ContextAction> = menu.actions.iter().flatten().copied().collect();
        assert!(actions.contains(&ContextAction::Copy));
        assert!(actions.contains(&ContextAction::Cut));
        assert!(actions.contains(&ContextAction::Paste));
        assert!(actions.contains(&ContextAction::SelectAll));
        assert!(actions.contains(&ContextAction::NewTab));
        assert!(actions.contains(&ContextAction::Settings));
        assert!(menu.panel.width() > 0.0);
    }

    #[test]
    fn select_all_covers_scrollback() {
        let mut app = app_with_panes(1);
        app.panes[0].term = Terminal::new(20, 3);
        app.panes[0].term.feed(b"one\r\ntwo\r\nthree\r\n");
        app.select_all();
        let selection = app.selection.expect("selection");
        assert_eq!(selection.anchor, (0, 0));
        let text = app.panes[0]
            .term
            .text_between(selection.anchor, selection.head);
        assert!(text.contains("one"));
        assert!(text.contains("three"));
    }

    #[test]
    fn parses_command_line() {
        // `parse_args` czyta `std::env::args`, więc sprawdzamy tylko kontrakt
        // `control_byte` i kodowanie — pełny CLI jest w `print_help`.
        assert_eq!(control_byte(0x61), Some(0x01));
        assert_eq!(control_byte(0x7a), Some(0x1a));
        assert_eq!(control_byte(0x5b), Some(0x1b));
        assert_eq!(control_byte(0x31), None);
    }
}
