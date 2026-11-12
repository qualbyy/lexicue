//! Adapter over `portable-pty`. No other module may name that crate.
//!
//! On Linux the child is placed in a new session with a controlling terminal so
//! job control, `Ctrl+C`, and `SIGWINCH` behave like a normal terminal.

use super::{PtyBackend, PtyError, SpawnSpec};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub struct PortablePty {
    master: Option<Box<dyn MasterPty + Send>>,
    child: Option<Box<dyn Child + Send + Sync>>,
    writer: Option<Box<dyn Write + Send>>,
    rx: Option<Receiver<Vec<u8>>>,
    reader_thread: Option<JoinHandle<()>>,
    shell_pgid: Option<i32>,
    /// Real cell pixel size; `9x18` until the UI reports the measured value.
    cell_w: u16,
    cell_h: u16,
}

impl PortablePty {
    pub fn new() -> Self {
        Self {
            master: None,
            child: None,
            writer: None,
            rx: None,
            reader_thread: None,
            shell_pgid: None,
            cell_w: 9,
            cell_h: 18,
        }
    }

    /// `SIGTERM`, short grace period, then `SIGKILL`. Called on window close.
    pub fn shutdown(&mut self) -> Result<(), PtyError> {
        let pty_pgid = self.shell_pgid.take();
        // Closing writer/master triggers SIGHUP for the controlling session on Unix.
        drop(self.writer.take());
        drop(self.master.take());

        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + Duration::from_millis(120);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(15));
                    }
                    Ok(None) | Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
            // The leader being gone does not mean its jobs are: `bash -c 'sleep 100 &'`
            // leaves them holding the pgid, so a group kill reaps exactly those.
            #[cfg(unix)]
            if let Some(pgid) = pty_pgid {
                kill_group(pgid, libc::SIGKILL);
            }
        }

        self.rx = None;
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
        Ok(())
    }
}

impl Default for PortablePty {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PortablePty {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// Signals the whole process group led by `pgid`. The child is a session leader
/// (`portable-pty` does `setsid`), so `-pgid` reaches every descendant it forked.
#[cfg(unix)]
fn kill_group(pgid: i32, sig: i32) {
    if pgid <= 0 {
        return;
    }
    // SAFETY: `kill` only reads its two integer arguments and has no memory effects;
    // a dead/absent group just yields `ESRCH`, which we deliberately ignore.
    unsafe {
        libc::kill(-pgid, sig);
    }
}

impl PtyBackend for PortablePty {
    fn spawn(&mut self, spec: SpawnSpec) -> Result<(), PtyError> {
        if self.child.is_some() {
            let _ = self.shutdown();
        }

        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: spec.rows.max(1),
                cols: spec.cols.max(1),
                pixel_width: spec.cols.max(1).saturating_mul(self.cell_w),
                pixel_height: spec.rows.max(1).saturating_mul(self.cell_h),
            })
            .map_err(|e| PtyError::Spawn(e.to_string()))?;

        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        cmd.cwd(&spec.cwd);
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| PtyError::Spawn(e.to_string()))?;
        drop(pair.slave);

        let shell_pgid = child.process_id().map(|pid| pid as i32);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| PtyError::Spawn(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| PtyError::Spawn(e.to_string()))?;

        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(256);
        let handle = std::thread::Builder::new()
            .name("lexicue-pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx.send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                    }
                }
            })
            .map_err(|e| PtyError::Spawn(e.to_string()))?;

        self.master = Some(pair.master);
        self.child = Some(child);
        self.writer = Some(writer);
        self.rx = Some(rx);
        self.reader_thread = Some(handle);
        self.shell_pgid = shell_pgid;
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), PtyError> {
        let writer = self.writer.as_mut().ok_or(PtyError::Closed)?;
        writer
            .write_all(bytes)
            .map_err(|e| PtyError::Io(e.to_string()))?;
        writer.flush().map_err(|e| PtyError::Io(e.to_string()))
    }

    fn set_cell_pixels(&mut self, width: u32, height: u32) {
        self.cell_w = u16::try_from(width).unwrap_or(u16::MAX).max(1);
        self.cell_h = u16::try_from(height).unwrap_or(u16::MAX).max(1);
    }

    fn resize(&mut self, cols: u16, rows: u16) -> Result<(), PtyError> {
        let master = self.master.as_ref().ok_or(PtyError::Closed)?;
        master
            .resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: cols.max(1).saturating_mul(self.cell_w),
                pixel_height: rows.max(1).saturating_mul(self.cell_h),
            })
            .map_err(|e| PtyError::Io(e.to_string()))
    }

    fn drain(&mut self) -> Result<Vec<u8>, PtyError> {
        let Some(rx) = self.rx.as_ref() else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(chunk) => out.extend_from_slice(&chunk),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if disconnected {
            self.rx = None;
        }
        Ok(out)
    }

    fn is_alive(&mut self) -> bool {
        match self.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    fn kill(&mut self) -> Result<(), PtyError> {
        self.shutdown()
    }

    fn shell_signals(&self) -> super::guard::ShellSignals {
        #[cfg(unix)]
        if let Some(master) = self.master.as_ref() {
            let foreground_pgid = master.process_group_leader();
            let shell_pgid = self.shell_pgid;

            let mut echo = None;
            let mut raw = None;

            if let Some(termios) = master.get_termios() {
                let flags = format!("{:?}", termios.local_flags);
                let has_flag = |name: &str| {
                    flags
                        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                        .any(|tok| tok == name)
                };
                // Report canonical non-echo input (sudo/passwd/ssh passphrase);
                // interactive shells (zsh ZLE / bash readline) clear ICANON while editing.
                if has_flag("ICANON") && !has_flag("ECHO") {
                    echo = Some(false);
                    raw = Some(false);
                }
            }

            return super::guard::ShellSignals {
                echo,
                raw,
                foreground_pgid,
                shell_pgid,
            };
        }
        super::guard::ShellSignals::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::{vt::Vt100Screen, ScreenModel};

    #[test]
    fn spawns_reads_resizes_and_shuts_down_real_pty() {
        let mut pty = PortablePty::new();
        pty.spawn(SpawnSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "printf 'lexicue_pty_ok\\n'".into()],
            cwd: std::path::PathBuf::from("/tmp"),
            env: vec![("TERM".into(), "xterm-256color".into())],
            cols: 80,
            rows: 24,
        })
        .expect("pty spawn");

        pty.resize(100, 30).expect("pty resize");
        let mut screen = Vt100Screen::new(100, 30, 100);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut saw_marker = false;
        while Instant::now() < deadline {
            if let Ok(bytes) = pty.drain() {
                if !bytes.is_empty() {
                    screen.feed(&bytes);
                    let snap = screen.snapshot();
                    let text: String = snap.cells.iter().map(|c| c.ch).collect();
                    if text.contains("lexicue_pty_ok") {
                        saw_marker = true;
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        assert!(saw_marker, "expected PTY output marker");
        pty.shutdown().expect("pty shutdown");
    }
}
