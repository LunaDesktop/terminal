# Luna Terminal

The Luna terminal — PTY, VT100/xterm, ANSI colours and a monospace font. Built on libLuna, without VTE.

> **Languages:** [Polski](README.pl.md) · [English](README.en.md) · [Deutsch](README.de.md)

## Part of the Luna project

This repository is part of the [LunaDesktop](https://github.com/LunaDesktop) organisation and the **Luna** project — a desktop written in Rust, without GTK, Qt or GObject.

## Building

```bash
git clone https://github.com/LunaDesktop/terminal.git
cd terminal
cargo build --release
```

Requirements: Linux, Rust 1.80+, Wayland (for windowed applications).

## Running

```bash
cargo run --release
```

## Documentation

- Project page: <https://luna.paganlinux.eu/>
- Developer docs: <https://luna.paganlinux.eu/en/docs/>
- Polish docs: <https://luna.paganlinux.eu/docs/>
- API index: <https://luna.paganlinux.eu/en/docs/api.html>

## License

The code is licensed under **GPL-3.0-or-later**. The full text is in [LICENSE](LICENSE).
