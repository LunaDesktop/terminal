//! Pseudoterminal (PTY): `posix_openpt` + `fork`/`exec` + wątek czytający.
//!
//! Nie używamy `openpty` z `-lutil` — `posix_openpt`/`grantpt`/`unlockpt` są
//! w libc. Dziecko dostaje własną sesję (`setsid`), kontrolujący terminal
//! (`TIOCSCTTY`) i standardowe wejście/wyjście na slave. Rodzic trzyma master,
//! zapisuje do niego klawisze i czyta wyjście w osobnym wątku.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use libluna_platform::WakeHandle;

/// Zdarzenie z PTY.
#[derive(Debug)]
pub enum PtyEvent {
    /// Bajty wyjścia programu (do podania w [`crate::term::Terminal::feed`]).
    Data(Vec<u8>),
    /// Proces zakończył się; wartość to kod wyjścia (−1, gdy nieznany).
    Exited(i32),
}

/// Uruchomiony terminal podprocesu.
pub struct Pty {
    master: Option<OwnedFd>,
    child: libc::pid_t,
    rx: Receiver<PtyEvent>,
    reader: Option<JoinHandle<()>>,
}

impl Pty {
    /// Uruchamia powłokę użytkownika (`$SHELL`, domyślnie `/bin/sh`).
    ///
    /// `LUNA_TERMINAL_CMD` pozwala uruchomić dowolne polecenie przez `/bin/sh -c`
    /// (przydatne w testach i do diagnostyki).
    pub fn spawn(cols: u16, rows: u16, wake: Option<WakeHandle>) -> io::Result<Self> {
        if let Ok(cmd) = std::env::var("LUNA_TERMINAL_CMD") {
            if !cmd.trim().is_empty() {
                return Self::spawn_command("/bin/sh", &["-c", &cmd], cols, rows, wake);
            }
        }
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| s.starts_with('/') && std::path::Path::new(s).is_file())
            .unwrap_or_else(|| "/bin/sh".to_owned());
        Self::spawn_command(&shell, &[], cols, rows, wake)
    }

    /// Uruchamia jawnie podane polecenie.
    pub fn spawn_command(
        program: &str,
        args: &[&str],
        cols: u16,
        rows: u16,
        wake: Option<WakeHandle>,
    ) -> io::Result<Self> {
        let (master, slave, slave_name) = open_pty()?;

        // `execve` nie przeszukuje `PATH` — bez wcześniejszego rozwiązania nazwy
        // `luna-terminal -e htop` kończyło się kodem 127 i okno natychmiast znikało.
        let resolved = resolve_program(program)?;

        // Wszystko, czego dziecko potrzebuje po `fork`, przygotowujemy wcześniej.
        let prog = CString::new(resolved.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL w nazwie programu"))?;
        let mut argv_owned = Vec::with_capacity(args.len() + 1);
        argv_owned.push(
            CString::new(
                std::path::Path::new(program)
                    .file_name()
                    .map(|name| name.as_bytes().to_vec())
                    .unwrap_or_else(|| program.as_bytes().to_vec()),
            )
            .unwrap_or_else(|_| CString::new("sh").unwrap()),
        );
        for arg in args {
            argv_owned
                .push(CString::new(arg.as_bytes()).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "NUL w argumencie")
                })?);
        }
        let mut argv: Vec<*const libc::c_char> =
            argv_owned.iter().map(|arg| arg.as_ptr()).collect();
        argv.push(std::ptr::null());

        let env = build_env();
        let mut envp: Vec<*const libc::c_char> = env.iter().map(|item| item.as_ptr()).collect();
        envp.push(std::ptr::null());

        // Katalog roboczy powłoki: ten, z którego uruchomiono `luna-terminal`
        // (np. „Otwórz terminal tutaj” w Plikach ustawia `current_dir`), a gdy go
        // brak — katalog domowy. Bez tego terminal zawsze startował w `$HOME`.
        let cwd = std::env::current_dir()
            .ok()
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.as_os_str().as_bytes().to_vec())
            .or_else(|| std::env::var_os("HOME").map(|home| home.as_bytes().to_vec()))
            .filter(|bytes| bytes.len() < 4096)
            .and_then(|bytes| CString::new(bytes).ok())
            .unwrap_or_else(|| CString::new("/").unwrap());

        let master_raw = master.as_raw_fd();
        let slave_raw = slave.as_raw_fd();

        // `fork` jest bezpieczny, dopóki dziecko używa wyłącznie wywołań systemowych
        // i kończy `execve` — nie dotykamy alokatora ani biblioteki standardowej.
        let child = unsafe { libc::fork() };
        if child < 0 {
            return Err(io::Error::last_os_error());
        }
        if child == 0 {
            unsafe {
                // Bez własnej sesji i kontrolującego terminala Ctrl+C mógłby trafić
                // do grupy procesów sesji (Selene/luna-shell) i wylogować użytkownika.
                // Lepiej zakończyć dziecko niż dopuścić do takiego routingu.
                if libc::setsid() < 0 || libc::ioctl(slave_raw, libc::TIOCSCTTY as _, 0) < 0 {
                    libc::_exit(127);
                }
                if libc::dup2(slave_raw, 0) < 0
                    || libc::dup2(slave_raw, 1) < 0
                    || libc::dup2(slave_raw, 2) < 0
                {
                    libc::_exit(127);
                }
                if slave_raw > 2 {
                    libc::close(slave_raw);
                }
                libc::close(master_raw);
                libc::chdir(cwd.as_ptr());

                let mut mask: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut mask);
                libc::sigprocmask(libc::SIG_SETMASK, &mask, std::ptr::null_mut());
                for signal in [
                    libc::SIGHUP,
                    libc::SIGINT,
                    libc::SIGQUIT,
                    libc::SIGTERM,
                    libc::SIGCHLD,
                    libc::SIGPIPE,
                    libc::SIGWINCH,
                ] {
                    libc::signal(signal, libc::SIG_DFL);
                }

                libc::execve(prog.as_ptr(), argv.as_ptr(), envp.as_ptr());
                libc::_exit(127);
            }
        }

        // Rodzic zamyka slave i ustawia rozmiar okna.
        drop(slave);
        let _ = slave_name;
        set_winsize(master_raw, cols, rows);

        let (tx, rx) = mpsc::channel();
        let reader_fd = unsafe { libc::dup(master_raw) };
        if reader_fd < 0 {
            unsafe {
                libc::kill(child, libc::SIGHUP);
            }
            return Err(io::Error::last_os_error());
        }
        let reader = thread::Builder::new()
            .name("luna-terminal-pty".to_owned())
            .spawn(move || {
                // Bezpieczne: `reader_fd` jest świeżym duplikatem i tylko ten wątek go zamyka.
                let fd = unsafe { OwnedFd::from_raw_fd(reader_fd) };
                read_loop(fd, child, tx, wake);
            })?;

        Ok(Self {
            master: Some(master),
            child,
            rx,
            reader: Some(reader),
        })
    }

    fn master_fd(&self) -> libc::c_int {
        self.master.as_ref().map(|fd| fd.as_raw_fd()).unwrap_or(-1)
    }

    /// Odbiera zdarzenie bez blokowania.
    pub fn try_recv(&self) -> Option<PtyEvent> {
        self.rx.try_recv().ok()
    }

    /// Zapisuje bajty do PTY (klawisze, odpowiedzi DSR/DA).
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let fd = self.master_fd();
        if fd < 0 {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        let mut written = 0;
        while written < bytes.len() {
            let n = unsafe {
                libc::write(
                    fd,
                    bytes[written..].as_ptr() as *const libc::c_void,
                    bytes.len() - written,
                )
            };
            if n > 0 {
                written += n as usize;
            } else if n == 0 {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "PTY: zero bajtów"));
            } else {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
        }
        Ok(())
    }

    /// Zmienia rozmiar terminala (kompozytor wysyła programowi `SIGWINCH`).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let fd = self.master_fd();
        if fd >= 0 {
            set_winsize(fd, cols, rows);
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if self.child > 0 {
            unsafe {
                // Cała grupa procesów sesji (powłoka + zadania w tle).
                libc::kill(-self.child, libc::SIGHUP);
                libc::kill(self.child, libc::SIGTERM);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            unsafe {
                libc::kill(self.child, libc::SIGKILL);
            }
        }
        // Zamknięcie mastera (i śmierć dziecka) odblokowuje wątek czytający.
        self.master.take();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if self.child > 0 {
            let mut status = 0;
            unsafe {
                libc::waitpid(self.child, &mut status, 0);
            }
        }
    }
}

fn read_loop(
    fd: OwnedFd,
    child: libc::pid_t,
    tx: mpsc::Sender<PtyEvent>,
    wake: Option<WakeHandle>,
) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = unsafe {
            libc::read(
                fd.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
        if n > 0 {
            let _ = tx.send(PtyEvent::Data(buf[..n as usize].to_vec()));
            if let Some(wake) = &wake {
                wake.wake();
            }
        } else if n == 0 {
            break;
        } else {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
    }
    let mut status = 0;
    let code = unsafe {
        if libc::waitpid(child, &mut status, 0) > 0 && libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else {
            -1
        }
    };
    let _ = tx.send(PtyEvent::Exited(code));
    if let Some(wake) = &wake {
        wake.wake();
    }
}

/// Tworzy parę PTY: (master, slave, nazwa slave).
/// Rozwiązuje nazwę programu tak jak `execvp`: ścieżka bezwzględna/względna
/// przechodzi bez zmian, a goła nazwa jest szukana w katalogach `PATH`.
///
/// Musi się to dziać **przed** `fork` — w dziecku wolno używać wyłącznie
/// wywołań bezpiecznych dla sygnałów, a `std::fs` takie nie jest.
fn resolve_program(program: &str) -> io::Result<String> {
    if program.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pusta nazwa programu",
        ));
    }
    if program.contains('/') {
        return Ok(program.to_owned());
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(program);
            if is_executable(&candidate) {
                return Ok(candidate.to_string_lossy().into_owned());
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("nie znaleziono programu „{program}” w PATH"),
    ))
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn open_pty() -> io::Result<(OwnedFd, OwnedFd, String)> {
    let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    if master < 0 {
        return Err(io::Error::last_os_error());
    }
    let master = unsafe { OwnedFd::from_raw_fd(master) };

    if unsafe { libc::grantpt(master.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::unlockpt(master.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }

    let mut name = [0 as libc::c_char; 256];
    let rc = unsafe { libc::ptsname_r(master.as_raw_fd(), name.as_mut_ptr(), name.len()) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    let slave_name = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    let slave_path = CString::new(slave_name.clone())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    let slave = unsafe { libc::open(slave_path.as_ptr(), libc::O_RDWR | libc::O_NOCTTY) };
    if slave < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((master, unsafe { OwnedFd::from_raw_fd(slave) }, slave_name))
}

fn set_winsize(fd: libc::c_int, cols: u16, rows: u16) {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        libc::ioctl(fd, libc::TIOCSWINSZ, &size);
    }
}

/// Środowisko dziecka: dziedziczone + ustawienia terminala (bez `setenv` po `fork`).
fn build_env() -> Vec<CString> {
    let mut items: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for (key, value) in std::env::vars_os() {
        let key = key.as_bytes().to_vec();
        if key == b"TERM" || key == b"COLORTERM" || key == b"LUNA_TERMINAL" {
            continue;
        }
        if !key.contains(&0) && !value.as_bytes().contains(&0) {
            items.push((key, value.as_bytes().to_vec()));
        }
    }
    items.push((b"TERM".to_vec(), b"xterm-256color".to_vec()));
    items.push((b"COLORTERM".to_vec(), b"truecolor".to_vec()));
    items.push((b"LUNA_TERMINAL".to_vec(), b"1".to_vec()));
    items
        .into_iter()
        .filter_map(|(key, value)| {
            let mut item = key;
            item.push(b'=');
            item.extend_from_slice(&value);
            CString::new(item).ok()
        })
        .collect()
}

#[cfg(test)]
fn proc_ids(pid: libc::pid_t) -> Option<(libc::pid_t, libc::pid_t)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `comm` może zawierać spacje i nawiasy — pola liczymy po ostatnim ')'.
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_whitespace();
    let _state = fields.next()?;
    let _ppid = fields.next()?;
    let pgrp = fields.next()?.parse().ok()?;
    let sid = fields.next()?.parse().ok()?;
    Some((pgrp, sid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn runs_command_and_reads_output() {
        let mut pty = Pty::spawn_command(
            "/bin/sh",
            &["-c", "printf 'witaj\\n'; exit 3"],
            80,
            24,
            None,
        )
        .expect("PTY");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        let mut exit = None;
        while Instant::now() < deadline && exit.is_none() {
            match pty.try_recv() {
                Some(PtyEvent::Data(bytes)) => output.extend_from_slice(&bytes),
                Some(PtyEvent::Exited(code)) => exit = Some(code),
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        assert_eq!(String::from_utf8_lossy(&output).trim(), "witaj");
        assert_eq!(exit, Some(3));
        // Zapis do PTY nie może panikować po zakończeniu procesu.
        let _ = pty.write(b"echo test\n");
    }

    #[test]
    fn resolves_bare_command_from_path() {
        let resolved = resolve_program("sh").expect("sh w PATH");
        assert!(
            resolved.starts_with('/'),
            "oczekiwano ścieżki, jest {resolved}"
        );
        assert!(std::path::Path::new(&resolved).is_file());
        // Ścieżka jawna nie może być przepisywana.
        assert_eq!(resolve_program("/bin/sh").unwrap(), "/bin/sh");
        assert!(resolve_program("luna-terminal-nie-ma-takiego").is_err());
    }

    #[test]
    fn runs_bare_command_found_in_path() {
        let pty = Pty::spawn_command("sh", &["-c", "printf 'z-path'; exit 0"], 80, 24, None)
            .expect("PTY");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        let mut exit = None;
        while Instant::now() < deadline && exit.is_none() {
            match pty.try_recv() {
                Some(PtyEvent::Data(bytes)) => output.extend_from_slice(&bytes),
                Some(PtyEvent::Exited(code)) => exit = Some(code),
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        assert_eq!(String::from_utf8_lossy(&output).trim(), "z-path");
        assert_eq!(exit, Some(0));
    }

    #[test]
    fn child_gets_its_own_terminal_session() {
        let pty = Pty::spawn_command("/bin/sh", &["-c", "sleep 5"], 80, 24, None).expect("PTY");
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut ids = None;
        while Instant::now() < deadline {
            if let Some(parsed) = proc_ids(pty.child) {
                ids = Some(parsed);
                // `fork` wraca przed `setsid` — czekamy, aż dziecko zdąży zmienić grupę.
                if parsed == (pty.child, pty.child) {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let (pgrp, sid) = ids.expect("odczyt /proc/<pid>/stat");
        assert_eq!(pgrp, pty.child, "dziecko musi być liderem własnej grupy");
        assert_eq!(sid, pty.child, "dziecko musi mieć własną sesję");
    }

    #[test]
    fn ctrl_c_reaches_pty_child_not_session() {
        let mut pty = Pty::spawn_command(
            "/bin/sh",
            &["-c", "trap 'echo GOTINT; exit 0' INT; sleep 5"],
            80,
            24,
            None,
        )
        .expect("PTY");
        // Daj powłoce chwilę na ustawienie trap i wejście w sleep.
        std::thread::sleep(Duration::from_millis(150));
        pty.write(b"\x03").expect("zapis ^C do PTY");

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut output = Vec::new();
        while Instant::now() < deadline {
            match pty.try_recv() {
                Some(PtyEvent::Data(bytes)) => output.extend_from_slice(&bytes),
                Some(PtyEvent::Exited(_)) => break,
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        assert!(
            String::from_utf8_lossy(&output).contains("GOTINT"),
            "Ctrl+C nie dotarło do dziecka PTY: {:?}",
            String::from_utf8_lossy(&output)
        );
    }

    #[test]
    fn resizes_without_error() {
        let mut pty =
            Pty::spawn_command("/bin/sh", &["-c", "sleep 0.2"], 40, 12, None).expect("PTY");
        pty.resize(120, 40);
        std::thread::sleep(Duration::from_millis(300));
    }
}
