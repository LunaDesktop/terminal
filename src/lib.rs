//! # luna-terminal
//!
//! Emulator terminala dla Luny, zbudowany wyłącznie na libLunie (bez GTK/Qt/VTE).
//!
//! Podział:
//! - [`term`] — model terminala: siatka znaków, scrollback, parser VT100/xterm,
//!   kolory SGR, tryb alternatywny i odpowiedzi DSR/DA,
//! - [`pty`] — pseudoterminal: `posix_openpt` + `fork`/`exec`, wątek czytający,
//! - [`ui`] — układ i rysowanie (font monospace, paleta ANSI, kursor, zakładki),
//! - [`prefs`] — ustawienia terminala (paleta, stopień pisma, kursor),
//! - [`app`] — pętla aplikacji, mapowanie klawiatury na sekwencje terminala.

pub mod app;
pub mod prefs;
pub mod pty;
pub mod term;
pub mod ui;

pub use app::run;
