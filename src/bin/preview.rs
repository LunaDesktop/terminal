//! Headless-podgląd terminala do dokumentacji (bez PTY i kompozytora).
//!
//! Uruchom: `cargo run -p luna-terminal --bin preview`
//! → `docs/images/luna-terminal.png`.

use std::error::Error;

use libluna_core::{Color, Size, Theme};
use libluna_render::{Scene, SoftwareRenderer, TextEngine};
use luna_terminal::prefs::Prefs;
use luna_terminal::term::Terminal;
use luna_terminal::ui::{self, SelectionView, ViewState};

const W: u32 = 900;
const H: u32 = 600;
const FONT: f64 = 14.0;

const SAMPLE: &str = "\
\x1b[1;32mpatryk@luna\x1b[0m:\x1b[1;34m~/projekty/luna\x1b[0m$ ls --color=auto\r\n\
\x1b[1;34mapps\x1b[0m  \x1b[1;34mcrates\x1b[0m  \x1b[1;34mde\x1b[0m  docs  scripts  Cargo.toml\r\n\
\x1b[1;32mpatryk@luna\x1b[0m:\x1b[1;34m~/projekty/luna\x1b[0m$ cargo test -p luna-terminal\r\n\
\x1b[1;32m   Compiling\x1b[0m luna-terminal v0.0.1\r\n\
\x1b[1;32m    Finished\x1b[0m test profile in 0.42s\r\n\
\x1b[1;32m     Running\x1b[0m unittests src/lib.rs\r\n\
running 14 tests\r\n\
test term::tests::writes_text_and_wraps ... \x1b[32mok\x1b[0m\r\n\
test term::tests::sgr_colors_and_attrs ... \x1b[32mok\x1b[0m\r\n\
test pty::tests::runs_command_and_reads_output ... \x1b[32mok\x1b[0m\r\n\
test app::tests::maps_control_and_special_keys ... \x1b[32mok\x1b[0m\r\n\
\r\n\
test result: \x1b[1;32mok\x1b[0m. 14 passed; 0 failed\r\n\
\x1b[1;32mpatryk@luna\x1b[0m:\x1b[1;34m~/projekty/luna\x1b[0m$ \x1b[38;2;255;180;80mecho\x1b[0m \"terminal na libLunie\"\r\n\
\x1b[1;38;2;255;180;80mterminal na libLunie\x1b[0m\r\n\
\x1b[1;32mpatryk@luna\x1b[0m:\x1b[1;34m~/projekty/luna\x1b[0m$ ";

fn main() -> Result<(), Box<dyn Error>> {
    let theme = Theme::dark();
    let text = TextEngine::mono();
    let size = Size::new(W as f64, H as f64);
    let prefs = Prefs::default();
    let tabs = vec!["luna".to_owned(), "testy".to_owned(), "htop".to_owned()];
    let lay = ui::layout(size, &theme, &text, FONT, tabs.len());

    let mut term = Terminal::new(lay.cols, lay.rows);
    term.feed(SAMPLE.as_bytes());

    let title = "patryk@luna: ~/projekty/luna";
    let view = ViewState {
        term: &term,
        title,
        tabs: &tabs,
        active_tab: 0,
        tab_hover: None,
        tab_scroll: 0.0,
        // Zaznaczona ścieżka w pierwszym wierszu — pokazuje nową funkcję kopiowania.
        selection: Some(SelectionView {
            start: (0, 12),
            end: (0, 26),
        }),
        hover: None,
        plus_hover: false,
        settings_hover: false,
        font_size: FONT,
        prefs: &prefs,
    };

    let mut scene = Scene::new();
    ui::paint(&mut scene, &theme, &text, size, &view);

    let mut renderer = SoftwareRenderer::new_mono(W, H);
    renderer.draw(&scene, 1.0, Color::from_rgba8(0x10, 0x12, 0x18, 0xff));
    let png = renderer.encode_png().ok_or("failed to encode PNG")?;
    std::fs::write("docs/images/luna-terminal.png", png)?;
    println!("saved docs/images/luna-terminal.png ({W}x{H})");

    // Ten sam kadr z otwartym oknem Ustawień (dokumentacja kolorystyki).
    ui::paint_settings(&mut scene, &theme, &text, size, &prefs, None);
    renderer.draw(&scene, 1.0, Color::from_rgba8(0x10, 0x12, 0x18, 0xff));
    let png = renderer.encode_png().ok_or("failed to encode PNG")?;
    std::fs::write("docs/images/luna-terminal-settings.png", png)?;
    println!("saved docs/images/luna-terminal-settings.png ({W}x{H})");
    Ok(())
}
