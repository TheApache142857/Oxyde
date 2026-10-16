//! Edit locks: while a window edits a document, a small file sits next to it
//! (`Bracket.oxyprt.lock` next to `Bracket.oxyprt`) saying who edits it, from which window.
//! Another window opening the same file is told, and opens it read-only or takes the file
//! over. The lock is removed when the document is closed or the window quits; one left
//! behind by a crash is recognized (its window no longer runs on this computer) and
//! replaced without asking.
//!
//! The lock is a plain JSON file: `{"user": …, "host": …, "pid": …, "since": …}`, with
//! `since` in seconds since 1970. It is advisory: nothing stops another program from
//! writing the document.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Who holds a lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockInfo {
    pub user: String,
    pub host: String,
    pub pid: u32,
    /// Seconds since 1970.
    pub since: u64,
}

impl LockInfo {
    /// This window.
    pub fn ours() -> Self {
        Self {
            user: std::env::var("USERNAME")
                .or_else(|_| std::env::var("USER"))
                .unwrap_or_else(|_| "someone".to_owned()),
            host: hostname(),
            pid: std::process::id(),
            since: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        }
    }

    /// Whether this window wrote it.
    pub fn is_ours(&self) -> bool {
        self.pid == std::process::id() && self.host == hostname()
    }

    /// Whether the window that wrote it has gone (same computer, process no longer
    /// running): a lock left behind by a crash.
    pub fn is_stale(&self) -> bool {
        self.host == hostname() && !self.is_ours() && !process_alive(self.pid)
    }

    /// "hugo on WORKSTATION, for 12 minutes".
    pub fn describe(&self) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let elapsed = now.saturating_sub(self.since);
        let age = if elapsed < 90 {
            "just now".to_owned()
        } else if elapsed < 2 * 3600 {
            format!("for {} minutes", elapsed / 60)
        } else if elapsed < 2 * 86_400 {
            format!("for {} hours", elapsed / 3600)
        } else {
            format!("for {} days", elapsed / 86_400)
        };
        format!("{} on {}, {age}", self.user, self.host)
    }
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|h| h.trim().to_owned())
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| "this computer".to_owned())
}

/// Whether a process with this id runs on this computer.
fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        match output {
            // One CSV row per matching process; "INFO: No tasks…" otherwise.
            Ok(out) => String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\"")),
            // Cannot tell: assume it runs, so nothing is taken over by mistake.
            Err(_) => true,
        }
    }
    #[cfg(not(windows))]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
}

/// The lock file of a document: its path with `.lock` appended.
pub fn lock_path(document: &Path) -> PathBuf {
    let mut s = document.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

/// The lock on a document, if one is there and readable.
pub fn read(document: &Path) -> Option<LockInfo> {
    let text = std::fs::read_to_string(lock_path(document)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Who edits `document` in another running window, if anyone: a file that must not be
/// written over from here.
pub fn held_elsewhere(document: &Path) -> Option<LockInfo> {
    read(document).filter(|lock| !lock.is_ours() && !lock.is_stale())
}

/// The locks this window holds.
#[derive(Debug, Default)]
pub struct Locks {
    held: HashSet<PathBuf>,
}

impl Locks {
    /// Takes the lock on `document` for this window; `Err` with the holder when another
    /// running window has it. A lock already held, or left behind by a crashed window, is
    /// (re)written. The lock is not taken if its file cannot be written (a read-only
    /// folder): editing goes on without one.
    pub fn acquire(&mut self, document: &Path) -> Result<(), LockInfo> {
        if self.held.contains(document) {
            return Ok(());
        }
        if let Some(existing) = read(document) {
            if !existing.is_ours() && !existing.is_stale() {
                return Err(existing);
            }
        }
        self.write(document);
        Ok(())
    }

    /// Takes the lock whoever holds it.
    pub fn force(&mut self, document: &Path) {
        self.write(document);
    }

    fn write(&mut self, document: &Path) {
        let ours = LockInfo::ours();
        if let Ok(text) = serde_json::to_string_pretty(&ours) {
            if std::fs::write(lock_path(document), text).is_ok() {
                self.held.insert(document.to_path_buf());
            }
        }
    }

    /// Releases every lock but those on `keep`.
    pub fn retain(&mut self, keep: &[PathBuf]) {
        let gone: Vec<PathBuf> = self
            .held
            .iter()
            .filter(|p| !keep.contains(p))
            .cloned()
            .collect();
        for path in gone {
            self.release(&path);
        }
    }

    pub fn release(&mut self, document: &Path) {
        if self.held.remove(document) {
            // Only our own lock: another window may have taken the file over meanwhile.
            if read(document).is_some_and(|l| l.is_ours()) {
                let _ = std::fs::remove_file(lock_path(document));
            }
        }
    }

    pub fn release_all(&mut self) {
        let all: Vec<PathBuf> = self.held.iter().cloned().collect();
        for path in all {
            self.release(&path);
        }
    }
}

impl Drop for Locks {
    fn drop(&mut self) {
        self.release_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_document(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oxyde-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, b"document").unwrap();
        path
    }

    #[test]
    fn a_lock_is_taken_kept_and_released() {
        let document = temp_document("a.oxyprt");
        let mut locks = Locks::default();
        assert!(locks.acquire(&document).is_ok());
        assert!(locks.held.contains(&document));
        assert!(lock_path(&document).exists());
        let info = read(&document).unwrap();
        assert!(info.is_ours());
        // Taking it again is fine; another `Locks` of this window sees it as ours too.
        assert!(locks.acquire(&document).is_ok());
        assert!(Locks::default().acquire(&document).is_ok());
        locks.retain(&[]);
        assert!(!lock_path(&document).exists());
        assert!(!locks.held.contains(&document));
    }

    #[test]
    fn another_windows_lock_is_reported_or_taken_over() {
        let document = temp_document("b.oxyprt");
        let other = LockInfo {
            user: "pat".to_owned(),
            host: "ELSEWHERE".to_owned(),
            pid: 4242,
            since: 0,
        };
        std::fs::write(
            lock_path(&document),
            serde_json::to_string(&other).unwrap(),
        )
        .unwrap();
        let mut locks = Locks::default();
        let holder = locks.acquire(&document).unwrap_err();
        assert_eq!(holder, other);
        assert!(holder.describe().starts_with("pat on ELSEWHERE, for"));
        assert!(!locks.held.contains(&document));
        locks.force(&document);
        assert!(read(&document).unwrap().is_ours());
        drop(locks);
        assert!(!lock_path(&document).exists());
    }

    #[test]
    fn a_lock_of_a_crashed_window_on_this_computer_is_replaced() {
        let document = temp_document("c.oxyprt");
        let stale = LockInfo {
            // No process has this id for long.
            pid: u32::MAX - 7,
            ..LockInfo::ours()
        };
        std::fs::write(
            lock_path(&document),
            serde_json::to_string(&stale).unwrap(),
        )
        .unwrap();
        let mut locks = Locks::default();
        assert!(locks.acquire(&document).is_ok());
        assert!(read(&document).unwrap().is_ours());
    }

    #[test]
    fn an_unreadable_lock_does_not_block() {
        let document = temp_document("d.oxyprt");
        std::fs::write(lock_path(&document), b"not json").unwrap();
        assert!(Locks::default().acquire(&document).is_ok());
    }
}
