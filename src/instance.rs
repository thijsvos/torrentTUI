//! Session ownership and the control channel between processes.
//!
//! Exactly one process may own the librqbit session at a time. That is not a
//! nicety. librqbit's JSON persistence guards `session.json` with an
//! *in-process* `RwLock` only, and writes it through a **fixed** temp path
//! (`session.json.tmp`), so two writers interleave into one file. Its `.bitv`
//! fastresume files are worse: a detached flusher task rewrites the whole
//! snapshot at offset 0, and *deletes the file* if a write fails. Two
//! overlapping processes therefore lose torrents and force re-hashes. Because
//! `pick_listen_port` probes for a free port, the second instance starts
//! happily instead of failing — nothing but this module prevents any of it.
//!
//! # Where the lock lives
//!
//! In the **session directory**, not the config directory. librqbit's default
//! persistence folder is `ProjectDirs::from("com", "rqbit", "session")` — a
//! namespace shared with upstream `rqbit` and every other librqbit app, and
//! derived from `XDG_DATA_HOME` while our config dir follows
//! `XDG_CONFIG_HOME`. A lock in the config directory would guard a different
//! directory than the one it is protecting, and `XDG_CONFIG_HOME=… torrenttui`
//! would walk straight past it. [`crate::engine::torrent`] therefore passes an
//! explicit persistence folder and the lock sits inside it, so the lock and the
//! state it protects are always the same directory.
//!
//! # Two files, and why they are two
//!
//! - `session.lock` — **always empty, never read**. Ownership is an advisory
//!   whole-file lock taken with `std::fs::File::try_lock` (stable since Rust
//!   1.89; the crate's MSRV is 1.95). The kernel releases it when the owner
//!   dies, including on `SIGKILL` and on a `panic = "abort"` abort, so a crash
//!   never leaves a lock needing manual clearing.
//! - `daemon.json` — **never locked**, purely advisory. Who owns the session,
//!   how, since when, and with which privacy posture.
//!
//! They are separate because Windows byte-range locks are **mandatory**:
//! `LockFileEx` over the whole file denies other processes read *and* write to
//! that range, so metadata stored inside the lock file would be unreadable on
//! Windows exactly when it is needed. Unix `flock` is advisory and would allow
//! it, which is precisely the trap — CI runs the suite on windows-latest.
//!
//! The rule that follows: **every decision comes from the lock, every message
//! comes from `daemon.json`**, and the record is always treated as possibly
//! stale, truncated, or written by another version.
//!
//! # The control channel
//!
//! Requests are files in `control/`, written to a `.tmp` sibling and renamed
//! into place so a reader never sees a partial write — the same atomic-write
//! idiom [`crate::config::Config::save`] uses. The owner polls, acts, deletes.
//!
//! Why not librqbit's embedded HTTP API, which is already running? Mounting it
//! read-write does not add one route, it adds nine — including
//! `POST /torrents` whose `output_folder` is a free-form string (an
//! arbitrary-directory write primitive) and `POST /torrents/{id}/delete`
//! (deletes the user's files). Worse, the API password is 96 random bits
//! generated per run and deliberately never written to disk; letting a second
//! process use it means persisting that credential, turning an ephemeral
//! in-memory token into an on-disk grant for file deletion. And a magnet added
//! over `POST /torrents` would reach the session without passing through
//! [`crate::engine::torrent::strip_udp_trackers`], so a hand-off under proxy
//! lockdown would announce over `udp://` — exactly what the README promises it
//! does not do. Requests here become `EngineCommand::AddTorrent`, so there
//! stays exactly one filtering path.
//!
//! This module knows nothing about librqbit or ratatui: it is filesystem plus
//! serde, so every branch below is unit-testable against a `tempfile::TempDir`.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Ownership token. Always zero bytes; see the module docs.
const LOCK_FILE: &str = "session.lock";

/// Advisory description of the current owner. Never locked.
const RECORD_FILE: &str = "daemon.json";

/// Control-request directory.
const CONTROL_DIR: &str = "control";

/// Written by a detached child to say "I started". See [`announce_handoff`].
const HANDOFF_FILE: &str = "handoff.json";

/// Environment variable carrying the hand-off token to the child. An env var
/// rather than a flag: it is an implementation detail between two copies of the
/// same binary, not part of the CLI anyone should type.
pub const HANDOFF_ENV: &str = "TORRENTTUI_HANDOFF";

/// Request file asking a headless owner to shut down.
const STOP_REQUEST: &str = "stop";

/// Prefix and suffix for a hand-off request; the body is a magnet link.
const ADD_PREFIX: &str = "add-";
const ADD_SUFFIX: &str = ".magnet";

/// Cap on a control request's size. Mirrors `MAX_MAGNET_FILE_SIZE` in
/// [`crate::engine::watch`] so the two magnet-ingest paths agree.
const MAX_REQUEST_BYTES: u64 = 64 * 1024;

/// Cap on a hand-off magnet's length, mirroring the add dialog's
/// `MAX_INPUT_CHARS`. `validate_magnet` has no length cap of its own, and the
/// hand-off path bypasses the input widget that would otherwise apply one.
const MAX_MAGNET_CHARS: usize = 4096;

/// Record schema version. Bumped when a field's meaning changes, so a newer
/// record is reported as unrecognised rather than misparsed.
const RECORD_SCHEMA: u32 = 1;

/// A heartbeat older than this means the writer is gone. Generous next to the
/// 5 s write interval so a paused laptop or a busy box is not declared dead.
const HEARTBEAT_STALE_SECS: u64 = 30;

/// Whether a process with this id is still alive.
///
/// The heartbeat alone cannot tell "the lock is lying" from "the owner was just
/// `SIGKILL`ed": both leave a free lock next to a fresh record. Only the second
/// is common, and treating it as a live session made every hard kill block the
/// next launch for the whole staleness window.
///
/// On unix `kill(pid, 0)` sends no signal and only reports reachability, so it
/// answers exactly that question. Windows has no equally cheap equivalent
/// without pulling in a system-bindings crate, so there the heartbeat stands on
/// its own — the cost is a bounded wait after a hard kill, never a lost
/// session.
fn pid_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        if pid == 0 {
            return false;
        }
        // Sends no signal; only reports reachability. ESRCH means no such
        // process; EPERM means it exists but belongs to someone else, which
        // still counts as alive.
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// How the owning process is running. Decides whether a `stop` request is
/// honoured — a foreground window must never be closed by a background
/// request — and whether a second launch may take the session over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// A terminal UI is attached.
    Tui,
    /// The engine is running with no terminal.
    Headless,
}

impl Mode {
    /// Word used in user-facing messages and the log's per-run marker.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Tui => "window",
            Mode::Headless => "background session",
        }
    }
}

/// How far along the owner is. The parent of a detach hand-off waits for
/// `Acquired` before it exits, which is what makes the hand-off verifiable
/// rather than hopeful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Owns the lock; has not built a session yet.
    Acquired,
    /// Engine running.
    Running,
    /// Startup failed. `error` carries the context chain. This is the only
    /// channel that reaches a user whose terminal is already gone.
    Failed,
}

/// The advisory description of whoever owns the session.
///
/// Every field is informational — used to write an accurate message, never to
/// decide whether the session is owned. Fields carry `#[serde(default)]` so a
/// record written by a different version still parses as far as it can.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonRecord {
    /// See [`RECORD_SCHEMA`]. A record from the future is displayed as
    /// unrecognised rather than trusted.
    #[serde(default)]
    pub schema: u32,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub pid: u32,
    pub mode: Mode,
    pub state: State,
    /// Random per-handoff token. The detaching parent waits for a record
    /// carrying *its* nonce, so a third process taking the lock in the same
    /// window cannot be mistaken for the child it spawned.
    #[serde(default)]
    pub nonce: String,
    #[serde(default)]
    pub started_at: u64,
    /// Rewritten every few seconds while alive. See [`Holder::Contested`].
    #[serde(default)]
    pub heartbeat: u64,
    #[serde(default)]
    pub download_dir: String,
    /// The privacy posture actually applied at session creation — not what the
    /// config file says now. `[privacy]` is applied once, so a daemon keeps the
    /// posture it started with even after the config is edited.
    #[serde(default)]
    pub privacy: Option<String>,
    /// Populated when `state` is [`State::Failed`].
    #[serde(default)]
    pub error: Option<String>,
}

impl DaemonRecord {
    pub fn new(mode: Mode, state: State, nonce: &str, download_dir: &str) -> Self {
        let now = unix_now();
        Self {
            schema: RECORD_SCHEMA,
            version: env!("CARGO_PKG_VERSION").to_string(),
            pid: std::process::id(),
            mode,
            state,
            nonce: nonce.to_string(),
            started_at: now,
            heartbeat: now,
            download_dir: download_dir.to_string(),
            privacy: None,
            error: None,
        }
    }

    /// Seconds this owner has been running, or `None` if the clock moved
    /// backwards (an NTP step, or a record copied between machines).
    pub fn uptime_secs(&self) -> Option<u64> {
        unix_now().checked_sub(self.started_at)
    }

    /// Whether the heartbeat still looks alive, as a pure function of the two
    /// timestamps so it can be tested without waiting.
    ///
    /// A future-dated heartbeat is clock skew, not life: trusting it would let
    /// one bad record wedge every future launch, so it reads as stale.
    pub fn heartbeat_fresh_at(&self, now: u64) -> bool {
        match now.checked_sub(self.heartbeat) {
            Some(age) => age <= HEARTBEAT_STALE_SECS,
            None => false,
        }
    }

    /// Whether this record came from a version whose layout we understand.
    pub fn recognised(&self) -> bool {
        self.schema <= RECORD_SCHEMA
    }
}

/// Seconds since the Unix epoch, saturating at 0 before it. The only failure
/// mode is a clock set before 1970, which costs an accurate uptime and nothing
/// else — not worth an error path.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A 128-bit hex token, built from the OS's own randomness via `File::open` on
/// the platform RNG where available and a time/pid mix otherwise. Used only to
/// distinguish "the child I spawned" from "some other process", never as a
/// secret.
pub fn nonce() -> String {
    // Kept per-platform rather than sharing a buffer across `cfg`s: a shared
    // `let mut bytes` is never mutated on Windows, which trips `unused_mut` —
    // and CI builds every platform with `-D warnings`.
    #[cfg(unix)]
    {
        let mut bytes = [0u8; 16];
        if File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .is_ok()
        {
            return bytes.iter().map(|b| format!("{:02x}", b)).collect();
        }
    }
    // Fallback: not cryptographic, and it does not need to be. Two processes
    // would have to start in the same nanosecond with the same pid to collide.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:032x}", nanos ^ ((std::process::id() as u128) << 96))
}

// ---------------------------------------------------------------------------
// Paths

/// The directory holding the librqbit session, its lock and its daemon record.
pub fn session_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("session")
}

pub fn lock_path(session_dir: &Path) -> PathBuf {
    session_dir.join(LOCK_FILE)
}

pub fn record_path(session_dir: &Path) -> PathBuf {
    session_dir.join(RECORD_FILE)
}

/// The daemon's own log. Deliberately *not* the shared `torrenttui.log`: that
/// file is rotated on every launch, and a rotation renames the inode a running
/// daemon still holds an append handle on — the daemon would keep writing into
/// `torrenttui.log.1` until a second rotation unlinked it, silently discarding
/// its output while it still consumed disk.
pub fn daemon_log_path(config_dir: &Path) -> PathBuf {
    config_dir.join("torrenttui-daemon.log")
}

// ---------------------------------------------------------------------------
// Ownership

/// Held for as long as this process owns the session. Dropping it closes the
/// file, which releases the lock.
///
/// Deliberately does **not** delete `session.lock` on drop. `unlink` removes a
/// *name*, not the inode a lock lives on: a process that unlinked on exit could
/// delete a lock file a newer process had already created and locked, after
/// which a third process would create a fresh file at the same path and lock it
/// successfully — two owners, which is the exact outcome this module exists to
/// prevent. A zero-byte file lingering forever costs nothing.
#[derive(Debug)]
pub struct SessionGuard {
    /// `Option` so [`SessionGuard::leak`] can move the handle out past `Drop`.
    file: Option<File>,
    record: PathBuf,
}

impl SessionGuard {
    /// Give up the guard **without releasing the lock**, letting the OS release
    /// it when the process exits.
    ///
    /// This is what makes the detach hand-off airtight, and it is not an
    /// optimisation — it is the correctness barrier. Under `#[tokio::main]` the
    /// guard is a local of the async block, so a plain drop releases the lock
    /// *before* the runtime tears down librqbit's spawned tasks. Those tasks
    /// hold their own `Arc<Session>` clones and outlive `run_engine`'s return,
    /// so they are still bound to the listen port and still able to flush
    /// `session.json` and `.bitv` files. A successor that acquired in that
    /// window would overlap with them — the exact clobber this module exists to
    /// prevent.
    ///
    /// Holding the lock for the process's whole lifetime means the child of a
    /// detach can simply block on it: the parent releases by dying, which is
    /// strictly after every socket is closed and every flush is done. The
    /// barrier is enforced by the kernel, not by our ordering assumptions.
    ///
    /// `mem::forget` on a `File` is safe and is the point, not a leak bug: the
    /// fd is reclaimed at process exit like every other fd.
    pub fn leak(mut self) {
        // Remove the record *before* the handle closes, so a successor cannot
        // acquire and write its own record only for ours to delete it.
        let _ = fs::remove_file(&self.record);
        if let Some(file) = self.file.take() {
            std::mem::forget(file);
        }
    }
}

impl Drop for SessionGuard {
    /// Only reached in tests and on error paths — `main` always calls
    /// [`SessionGuard::leak`], and `panic = "abort"` means `Drop` never runs on
    /// a panic anyway. Process death releases the lock either way.
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.record);
    }
}

/// Who owns the session right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    /// Nobody. The lock file may still exist — a crashed process leaves it
    /// behind — but the kernel dropped its lock.
    Free,
    /// Owned. The record is `None` when it is missing, truncated, or from an
    /// unrecognised schema: still owned, we just cannot describe it.
    Held(Option<DaemonRecord>),
    /// The lock says free, but a heartbeat written seconds ago says otherwise.
    ///
    /// Advisory locking is not universally reliable — `flock` is node-local on
    /// NFS mounted `local_lock=flock`, returns `ENOTSUP` on some macOS SMB
    /// mounts, and has been unreliable on Docker Desktop's FUSE bind mounts.
    /// The heartbeat is the only cross-platform second opinion, so when the two
    /// disagree we believe the heartbeat and refuse.
    Contested(DaemonRecord),
    /// The lock could not be evaluated at all. Fail closed: an I/O error tells
    /// us nothing, and guessing "free" invites a second owner.
    Unknown(String),
}

impl Holder {
    fn describe(&self) -> String {
        match self {
            Holder::Free => "TorrentTUI".to_string(),
            Holder::Held(Some(r)) => format!("TorrentTUI (pid {})", r.pid),
            Holder::Held(None) => "TorrentTUI (unreadable daemon record)".to_string(),
            Holder::Contested(r) => format!("TorrentTUI (pid {})", r.pid),
            Holder::Unknown(_) => "TorrentTUI".to_string(),
        }
    }

    /// The owner's record, when there is one to show.
    pub fn record(&self) -> Option<&DaemonRecord> {
        match self {
            Holder::Held(Some(r)) | Holder::Contested(r) => Some(r),
            _ => None,
        }
    }
}

/// Try to take ownership of the session in `session_dir`.
///
/// Creates the directory if needed. On contention the record is read
/// best-effort; an unreadable record yields `Held(None)` rather than an error,
/// because a session we cannot describe is still one we must not join.
pub fn acquire(session_dir: &Path) -> Result<Result<SessionGuard, Holder>> {
    fs::create_dir_all(session_dir)
        .with_context(|| format!("creating session directory {}", session_dir.display()))?;
    let path = lock_path(session_dir);

    // `create(true)` not `create_new(true)`: a benign concurrent create must not
    // look like a failure. `truncate(false)` because the file's *contents* are
    // never used and truncating would be a pointless write.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening lock file {}", path.display()))?;

    match file.try_lock() {
        Ok(()) => {
            // The lock is ours, but it may be lying — see `Holder::Contested`.
            if let Some(record) = read_record(session_dir) {
                if record.heartbeat_fresh_at(unix_now()) && pid_is_alive(record.pid) {
                    let _ = file.unlock();
                    return Ok(Err(Holder::Contested(record)));
                }
            }
            Ok(Ok(SessionGuard {
                file: Some(file),
                record: record_path(session_dir),
            }))
        }
        Err(TryLockError::WouldBlock) => Ok(Err(Holder::Held(read_record(session_dir)))),
        Err(TryLockError::Error(e)) => Ok(Err(Holder::Unknown(e.to_string()))),
    }
}

/// Who owns the session, without disturbing them.
///
/// Used by `--status`, `--stop` and the detach hand-off — paths that must never
/// take the lock even briefly. Probing must never become owning, so the lock is
/// released the instant it is acquired.
pub fn probe(session_dir: &Path) -> Holder {
    let path = lock_path(session_dir);
    let Ok(file) = OpenOptions::new()
        .read(true)
        .write(true)
        .create(false)
        .truncate(false)
        .open(&path)
    else {
        // No lock file at all: nobody has ever owned a session here.
        return Holder::Free;
    };

    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            match read_record(session_dir) {
                Some(r) if r.heartbeat_fresh_at(unix_now()) && pid_is_alive(r.pid) => {
                    Holder::Contested(r)
                }
                _ => Holder::Free,
            }
        }
        Err(TryLockError::WouldBlock) => Holder::Held(read_record(session_dir)),
        Err(TryLockError::Error(e)) => Holder::Unknown(e.to_string()),
    }
}

/// Read the advisory record. Any failure — missing, truncated, or a schema we
/// do not recognise — collapses to `None`, which callers render as "a session
/// is running" without claiming to know more.
pub fn read_record(session_dir: &Path) -> Option<DaemonRecord> {
    let body = fs::read_to_string(record_path(session_dir)).ok()?;
    let record: DaemonRecord = serde_json::from_str(&body).ok()?;
    record.recognised().then_some(record)
}

/// Write the record atomically: tmp + rename, so a reader polling for a state
/// change never observes a half-written record. Mirrors
/// [`crate::config::Config::save`].
pub fn write_record(session_dir: &Path, record: &DaemonRecord) -> Result<()> {
    fs::create_dir_all(session_dir)?;
    let final_path = record_path(session_dir);
    let tmp_path = final_path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(record)?;
    fs::write(&tmp_path, &body).with_context(|| format!("writing {}", tmp_path.display()))?;
    fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("renaming into {}", final_path.display()))?;
    Ok(())
}

/// Poll [`acquire`] until it succeeds or `budget` elapses.
///
/// Used by the detached child, which starts while its parent still holds the
/// lock, and by a take-over waiting for a daemon to finish shutting down. The
/// wait is what turns "hope the other process is gone" into "the kernel says it
/// is gone".
pub fn acquire_waiting(
    session_dir: &Path,
    budget: std::time::Duration,
) -> Result<std::result::Result<SessionGuard, Holder>> {
    let deadline = std::time::Instant::now() + budget;
    loop {
        match acquire(session_dir)? {
            Ok(guard) => return Ok(Ok(guard)),
            Err(holder) => {
                if std::time::Instant::now() >= deadline {
                    return Ok(Err(holder));
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    }
}

/// Wait until nobody owns the session. Returns whether it came free in time.
///
/// This is the acknowledgement for `--stop` and for a take-over, and it is the
/// only signal that means what we need: because the lock is held for the
/// owner's whole process lifetime and released by the kernel at exit, seeing it
/// free is positive proof the owner is gone, its state flushed and its ports
/// released. A "goodbye" file or a pid check would prove neither.
pub fn wait_for_release(session_dir: &Path, budget: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if matches!(probe(session_dir), Holder::Free) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// How often the waits above re-check. Fast enough that a hand-off feels
/// instant, slow enough to be free.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

// ---------------------------------------------------------------------------
// The detach hand-off marker

/// Announce that this process started, for the parent that spawned it.
///
/// The parent cannot watch the *lock* to see the child arrive — it still holds
/// the lock itself, and must keep holding it until it exits so the two never
/// overlap on the same session files. So the child leaves a note instead, as
/// early as it can, and the parent waits for it before reporting success.
///
/// This proves the child ran its own code: the binary existed, was executable,
/// and got as far as reading its arguments. It does **not** prove the session
/// came up — for that, a failure is recorded in `daemon.json` once the child
/// owns the lock, and `--status` reports it.
pub fn announce_handoff(session_dir: &Path, nonce: &str) {
    let _ = fs::create_dir_all(session_dir);
    // Best effort throughout: failing to announce costs the parent an accurate
    // message, never the hand-off itself.
    let _ = fs::write(session_dir.join(HANDOFF_FILE), nonce.as_bytes());
}

/// Remove any previous marker, so a stale one cannot be mistaken for this
/// hand-off. Called by the parent immediately before spawning.
pub fn clear_handoff(session_dir: &Path) {
    let _ = fs::remove_file(session_dir.join(HANDOFF_FILE));
}

/// Wait for the child to announce itself with *our* token.
///
/// The token matters: a third process could take the lock in the same window,
/// and "something is running" is not the same claim as "the process I started
/// is running".
pub fn wait_for_handoff(session_dir: &Path, nonce: &str, budget: std::time::Duration) -> bool {
    let path = session_dir.join(HANDOFF_FILE);
    let deadline = std::time::Instant::now() + budget;
    loop {
        if fs::read_to_string(&path).is_ok_and(|body| body.trim() == nonce) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

// ---------------------------------------------------------------------------
// Spawning the background copy

/// Build the argv for the detached background process. Split from the spawn the
/// same way [`crate::player::build_command`] is, so it can be asserted without
/// starting a process.
///
/// `--download-dir` is forwarded already tilde-expanded: the child inherits no
/// shell, and `main` has resolved the override before this point.
pub fn build_headless_command(
    exe: &Path,
    download_dir: Option<&str>,
    nonce: &str,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--headless");
    if let Some(dir) = download_dir {
        cmd.arg("--download-dir").arg(dir);
    }
    cmd.env(HANDOFF_ENV, nonce);
    cmd
}

/// Start the background copy of ourselves, fully detached from this terminal.
///
/// Detaching is the difference between a feature and a bug report: a plain
/// `Command::spawn` leaves the child in the terminal's foreground process
/// group, so closing the window delivers `SIGHUP` and kills exactly the process
/// the user asked to keep running.
pub fn spawn_headless_child(download_dir: Option<&str>, nonce: &str) -> Result<u32> {
    let exe = std::env::current_exe().context("locating the torrenttui binary")?;
    let mut cmd = build_headless_command(&exe, download_dir, nonce);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // setpgid(0, 0): leave the terminal's foreground process group, so
        // neither the kernel's hangup nor the shell's job control reaches the
        // child when the window closes.
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS (0x8): no inherited console, so closing the parent's
        // console cannot kill it. CREATE_NEW_PROCESS_GROUP (0x200): no Ctrl+C
        // from the parent's group. Plain literals — `windows-sys` is not needed
        // for two constants, and `CommandExt` is std.
        cmd.creation_flags(0x0000_0008 | 0x0000_0200);
    }

    let child = cmd
        .spawn()
        .context("starting the background TorrentTUI process")?;
    let pid = child.id();
    // Fire-and-forget, like `player::spawn_player`. Unlike that case there is no
    // zombie to worry about: this process exits immediately afterwards, so init
    // adopts and reaps the child.
    drop(child);
    Ok(pid)
}

// ---------------------------------------------------------------------------
// Startup decision

/// The CLI inputs that steer startup. Extracted from `Cli` so the decision
/// below can be exercised without building a clap parse.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flags {
    pub headless: bool,
    pub stop: bool,
    pub status: bool,
    pub torrent_source: Option<String>,
    /// This process was spawned by a detaching parent (see [`HANDOFF_ENV`]).
    /// The parent still owns the session and releases it by exiting, so this
    /// child must *wait* for the lock rather than refuse it.
    pub handoff: bool,
}

/// What `main` should do, given the flags and who owns the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Startup {
    /// Take the lock and run the terminal UI.
    RunTui,
    /// Take the lock and run the engine with no terminal.
    RunHeadless,
    /// Ask the background session to stop, wait for it, then run the UI.
    TakeOver,
    /// Give this magnet to the running session and exit.
    HandOff(String),
    /// Ask the background session to stop, then exit.
    RequestStop,
    /// Print to stdout and exit 0.
    Report(String),
    /// Print to stderr and exit 1.
    Refuse(String),
}

/// The whole startup matrix, as one total function over flags × ownership.
///
/// This is where "the background process must be explicit" is actually
/// enforced, so it is deliberately pure: every branch below is a test case.
///
/// Order matters. `--status` neither fails nor mutates, so it wins. `--stop` is
/// next because it is about an existing session rather than this one. Only then
/// do the run paths apply.
pub fn decide(flags: &Flags, holder: &Holder) -> Startup {
    if flags.status {
        return Startup::Report(format_status(holder));
    }

    // An unevaluable lock is never a reason to start a second engine.
    if let Holder::Unknown(err) = holder {
        return Startup::Refuse(format!(
            "Could not determine whether TorrentTUI is already running: {}\n\
             Refusing to start rather than risk two sessions sharing one download state.",
            err
        ));
    }

    if flags.stop {
        return match holder {
            Holder::Free => Startup::Report("No background session is running.".to_string()),
            Holder::Held(Some(r)) | Holder::Contested(r) if r.mode == Mode::Tui => {
                Startup::Refuse(format!(
                    "A TorrentTUI window is running (pid {}) — quit it with `q`.",
                    r.pid
                ))
            }
            // Headless, or an owner we cannot describe: the request is advisory
            // and a window ignores it, so asking is safe either way.
            _ => Startup::RequestStop,
        };
    }

    if flags.headless {
        return match holder {
            Holder::Free => Startup::RunHeadless,
            // Spawned by a detach. The parent is still holding the lock — it
            // must, until it exits, so the two never overlap on the same
            // session files — and will drop it by dying. Refusing here is what
            // made detach a race the child could lose.
            _ if flags.handoff => Startup::RunHeadless,
            other => Startup::Refuse(already_running(other)),
        };
    }

    match holder {
        Holder::Free => Startup::RunTui,
        // A source given while something else owns the session belongs to that
        // session. Starting a second owner just to hold one magnet is exactly
        // the silent-corruption path this module exists to close.
        _ if flags.torrent_source.is_some() => match flags.torrent_source.clone() {
            Some(source) => Startup::HandOff(source),
            // Unreachable given the guard, but `decide` stays total rather than
            // relying on an unwrap this crate does not allow in production code.
            None => Startup::RunTui,
        },
        Holder::Held(Some(r)) if r.mode == Mode::Headless => Startup::TakeOver,
        other => Startup::Refuse(already_running(other)),
    }
}

/// Refusal text shared by every "someone else owns it" branch.
fn already_running(holder: &Holder) -> String {
    match holder {
        Holder::Contested(r) => format!(
            "{} is already running (pid {}), even though the lock file looks free.\n\
             That usually means this directory is on a filesystem where file locking \
             is unreliable (a network share, or some container mounts).",
            holder.describe(),
            r.pid
        ),
        other => format!("{} is already running.", other.describe()),
    }
}

/// The `--status` report, also used to describe what a take-over is taking
/// over.
pub fn format_status(holder: &Holder) -> String {
    match holder {
        Holder::Free => "No TorrentTUI session is running.".to_string(),
        Holder::Unknown(err) => format!("Could not read the session lock: {}", err),
        Holder::Held(None) => {
            "A TorrentTUI session is running, but its daemon record could not be read.".to_string()
        }
        Holder::Held(Some(r)) | Holder::Contested(r) => {
            let uptime = match r.uptime_secs() {
                Some(0) | None => "just started".to_string(),
                Some(secs) => format!("up {}", crate::ui::layout::format_eta(Some(secs))),
            };
            let mut out = format!(
                "TorrentTUI {} running as a {} (pid {}, {}).\nDownloading to {}",
                r.version,
                r.mode.label(),
                r.pid,
                uptime,
                r.download_dir
            );
            if let Some(privacy) = r.privacy.as_deref() {
                out.push_str(&format!("\nPrivacy (as applied at start): {}", privacy));
            }
            if r.state == State::Failed {
                out.push_str(&format!(
                    "\nThis session FAILED to start: {}",
                    r.error.as_deref().unwrap_or("no reason recorded")
                ));
            }
            out
        }
    }
}

// ---------------------------------------------------------------------------
// Control channel

/// A request left for the session owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Shut the session down. Honoured only by a headless owner.
    Stop,
    /// Add this magnet link.
    Add(String),
}

fn control_dir(session_dir: &Path) -> PathBuf {
    session_dir.join(CONTROL_DIR)
}

/// Create `control/` restricted to this user on unix.
///
/// It is a command channel: anything dropped in here can add a torrent to, or
/// stop, the running session. The default umask would leave it world-readable
/// and — if `config_dir()` ever fell back to the working directory — possibly
/// world-writable, so the mode is set explicitly rather than inherited.
fn ensure_control_dir(session_dir: &Path) -> Result<PathBuf> {
    let dir = control_dir(session_dir);
    if dir.is_dir() {
        return Ok(dir);
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&dir)
        .with_context(|| format!("creating control directory {}", dir.display()))?;
    Ok(dir)
}

/// Whether a source may be handed to a running session.
///
/// Magnets only, deliberately. A `.torrent` **path** would make the daemon read
/// a file chosen by whoever wrote the request, resolved against the daemon's
/// working directory rather than the sender's — a different trust boundary
/// wearing the same syntax. The caller refuses those with an explanation
/// instead.
pub fn handoff_source(source: &str) -> std::result::Result<String, String> {
    let source = source.trim();
    if !source.starts_with("magnet:?") {
        return Err(
            "Only magnet links can be handed to a running session. Stop it first with \
             `torrenttui --stop`, then add the .torrent file."
                .to_string(),
        );
    }
    if source.chars().count() > MAX_MAGNET_CHARS {
        return Err(format!(
            "Magnet link is too long ({} characters, limit {}).",
            source.chars().count(),
            MAX_MAGNET_CHARS
        ));
    }
    if source.chars().any(|c| c.is_control()) {
        return Err("Magnet link contains control characters.".to_string());
    }
    crate::ui::input::validate_magnet(source)?;
    Ok(source.to_string())
}

/// Write a request for the current owner to pick up.
///
/// Written to a `.tmp` sibling and renamed, so the owner's scan never sees a
/// half-written body: the scan matches exact names and the `add-*.magnet`
/// shape, and a `.tmp` matches neither.
pub fn send_request(session_dir: &Path, request: &Request) -> Result<()> {
    let dir = ensure_control_dir(session_dir)?;

    let (name, body) = match request {
        Request::Stop => (STOP_REQUEST.to_string(), String::new()),
        Request::Add(magnet) => (
            format!("{}{}{}", ADD_PREFIX, nonce(), ADD_SUFFIX),
            magnet.clone(),
        ),
    };

    let final_path = dir.join(&name);
    let tmp_path = dir.join(format!("{}.tmp", name));
    fs::write(&tmp_path, body.as_bytes())
        .with_context(|| format!("writing {}", tmp_path.display()))?;
    fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("renaming into {}", final_path.display()))?;
    Ok(())
}

/// Collect and remove every pending request.
///
/// Each entry is removed as it is read, so a request is acted on at most once
/// even if the owner is slow or crashes mid-add. Filenames come from `read_dir`
/// and are re-joined onto the control directory, so nothing a sender writes can
/// escape it; `symlink_metadata` keeps a symlink from redirecting the read.
pub fn take_requests(session_dir: &Path) -> Vec<Request> {
    let dir = control_dir(session_dir);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        // A rename that has not landed yet is not ours to touch.
        if name.ends_with(".tmp") {
            continue;
        }
        let path = entry.path();
        // Not `entry.file_type()`: that follows nothing, but be explicit —
        // anything that is not a regular file is refused unread.
        if !fs::symlink_metadata(&path)
            .map(|m| m.file_type().is_file())
            .unwrap_or(false)
        {
            continue;
        }

        if name == STOP_REQUEST {
            let _ = fs::remove_file(&path);
            out.push(Request::Stop);
            continue;
        }

        if name.starts_with(ADD_PREFIX) && name.ends_with(ADD_SUFFIX) {
            let body = read_capped(&path);
            // Remove before acting: a crash mid-add must not re-add on the next
            // poll.
            let _ = fs::remove_file(&path);
            if let Some(body) = body {
                if let Ok(magnet) = handoff_source(&body) {
                    out.push(Request::Add(magnet));
                }
            }
            continue;
        }

        // Unknown name: sweep it so the directory cannot grow without bound.
        let _ = fs::remove_file(&path);
    }
    out
}

/// Read a request body, refusing anything over [`MAX_REQUEST_BYTES`] before the
/// read rather than after.
fn read_capped(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_REQUEST_BYTES {
        return None;
    }
    let mut body = String::new();
    file.read_to_string(&mut body).ok()?;
    Some(body)
}

/// Delete every pending request. Called by an owner at startup so a `stop` left
/// by a previous run cannot shut the new one down immediately.
pub fn clear_requests(session_dir: &Path) {
    let dir = control_dir(session_dir);
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let _ = fs::remove_file(entry.path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn record(mode: Mode) -> DaemonRecord {
        DaemonRecord::new(mode, State::Running, "n0", "/dl")
    }

    fn held(mode: Mode) -> Holder {
        Holder::Held(Some(record(mode)))
    }

    // -- ownership ----------------------------------------------------------

    #[test]
    fn acquire_takes_a_free_lock() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        assert!(acquire(&sd).unwrap().is_ok());
        assert!(lock_path(&sd).exists());
    }

    #[test]
    fn the_lock_file_stays_empty() {
        // Metadata must never live in the lock file: Windows byte-range locks
        // are mandatory, so a locked file cannot be read by anyone else.
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        let _guard = acquire(&sd).unwrap().expect("acquired");
        assert_eq!(fs::metadata(lock_path(&sd)).unwrap().len(), 0);
    }

    #[test]
    fn second_acquire_is_refused_and_reports_the_holder() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        let _first = acquire(&sd).unwrap().expect("first acquires");
        write_record(&sd, &record(Mode::Headless)).unwrap();

        // Locks belong to the open file description, so a second acquire
        // conflicts even from the same process — which is exactly what the
        // detach hand-off relies on.
        match acquire(&sd).unwrap() {
            Err(Holder::Held(Some(r))) => {
                assert_eq!(r.mode, Mode::Headless);
                assert_eq!(r.pid, std::process::id());
            }
            other => panic!("expected Held(Some(_)), got {:?}", other),
        }
    }

    #[test]
    fn dropping_the_guard_releases_the_lock() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        let first = acquire(&sd).unwrap().expect("acquired");
        assert!(matches!(probe(&sd), Holder::Held(_)));
        drop(first);
        assert_eq!(probe(&sd), Holder::Free);
        assert!(acquire(&sd).unwrap().is_ok());
    }

    #[test]
    fn a_lock_file_left_by_a_crash_is_not_a_held_session() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        fs::create_dir_all(&sd).unwrap();
        fs::write(lock_path(&sd), b"").unwrap();
        // The kernel released the dead process's lock; a stale record must not
        // resurrect it either.
        let mut stale = record(Mode::Headless);
        stale.heartbeat = unix_now() - (HEARTBEAT_STALE_SECS + 5);
        write_record(&sd, &stale).unwrap();

        assert_eq!(probe(&sd), Holder::Free);
        assert!(acquire(&sd).unwrap().is_ok());
    }

    #[test]
    fn a_fresh_heartbeat_beats_a_free_looking_lock() {
        // The failure mode on filesystems where advisory locking is unreliable:
        // we get the lock even though someone else is plainly alive.
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        fs::create_dir_all(&sd).unwrap();
        fs::write(lock_path(&sd), b"").unwrap();
        write_record(&sd, &record(Mode::Headless)).unwrap();

        assert!(matches!(probe(&sd), Holder::Contested(_)));
        match acquire(&sd).unwrap() {
            Err(Holder::Contested(r)) => assert_eq!(r.pid, std::process::id()),
            other => panic!("expected Contested, got {:?}", other),
        }
        // And having refused, we must not have kept the lock.
        assert!(matches!(probe(&sd), Holder::Contested(_)));
    }

    #[test]
    fn a_dead_owner_does_not_contest_a_free_lock() {
        // A session killed with SIGKILL leaves a fresh heartbeat behind. Before
        // the liveness check that blocked the next launch for the whole
        // staleness window, which is the common case, not the rare one.
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        fs::create_dir_all(&sd).unwrap();
        fs::write(lock_path(&sd), b"").unwrap();
        let mut dead = record(Mode::Headless);
        // pid 0 is never a live user process on any platform we ship.
        dead.pid = 0;
        write_record(&sd, &dead).unwrap();

        // Asserted per platform because the guarantee genuinely differs, and a
        // test that hid that would claim coverage it does not have.
        #[cfg(unix)]
        {
            // `kill(pid, 0)` reports the owner is gone, so the free lock is
            // believed immediately.
            assert_eq!(probe(&sd), Holder::Free);
            assert!(acquire(&sd).unwrap().is_ok());
        }
        #[cfg(not(unix))]
        {
            // No cheap liveness check there, so the heartbeat stands alone and
            // a hard-killed session reads as live until it goes stale. The cost
            // is a bounded wait, never a lost session.
            assert!(matches!(probe(&sd), Holder::Contested(_)));
            let mut stale = dead;
            stale.heartbeat = unix_now() - (HEARTBEAT_STALE_SECS + 5);
            write_record(&sd, &stale).unwrap();
            assert_eq!(probe(&sd), Holder::Free);
            assert!(acquire(&sd).unwrap().is_ok());
        }
    }

    #[test]
    fn probing_does_not_take_ownership() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        fs::create_dir_all(&sd).unwrap();
        fs::write(lock_path(&sd), b"").unwrap();
        assert_eq!(probe(&sd), Holder::Free);
        // If the probe had kept the lock, this would fail.
        assert!(acquire(&sd).unwrap().is_ok());
    }

    #[test]
    fn probe_on_a_never_used_directory_is_free() {
        let dir = TempDir::new().unwrap();
        assert_eq!(probe(&session_dir(dir.path())), Holder::Free);
    }

    // -- the daemon record --------------------------------------------------

    #[test]
    fn record_round_trips() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        let mut r = record(Mode::Headless);
        r.privacy = Some("proxy+blocklist".to_string());
        write_record(&sd, &r).unwrap();
        assert_eq!(read_record(&sd), Some(r));
    }

    #[test]
    fn a_garbage_record_reads_as_none_not_an_error() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        fs::create_dir_all(&sd).unwrap();
        fs::write(record_path(&sd), b"{not json").unwrap();
        assert_eq!(read_record(&sd), None);
    }

    #[test]
    fn a_record_from_a_newer_schema_is_not_trusted() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        let mut r = record(Mode::Headless);
        r.schema = RECORD_SCHEMA + 1;
        write_record(&sd, &r).unwrap();
        assert_eq!(read_record(&sd), None);
    }

    #[test]
    fn a_record_missing_optional_fields_still_parses() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        fs::create_dir_all(&sd).unwrap();
        fs::write(
            record_path(&sd),
            br#"{"mode":"headless","state":"running"}"#,
        )
        .unwrap();
        let r = read_record(&sd).expect("parses with defaults");
        assert_eq!(r.mode, Mode::Headless);
        assert_eq!(r.pid, 0);
        assert!(r.privacy.is_none());
    }

    #[test]
    fn heartbeat_freshness_is_a_pure_function() {
        let mut r = record(Mode::Headless);
        r.heartbeat = 1_000;
        assert!(r.heartbeat_fresh_at(1_000));
        assert!(r.heartbeat_fresh_at(1_000 + HEARTBEAT_STALE_SECS));
        assert!(!r.heartbeat_fresh_at(1_000 + HEARTBEAT_STALE_SECS + 1));
        // A future-dated heartbeat is clock skew, not life. Trusting it would
        // let one bad record wedge every future launch.
        assert!(!r.heartbeat_fresh_at(999));
    }

    // -- the startup matrix -------------------------------------------------

    #[test]
    fn a_free_session_runs_normally() {
        assert_eq!(decide(&Flags::default(), &Holder::Free), Startup::RunTui);
        let headless = Flags {
            headless: true,
            ..Default::default()
        };
        assert_eq!(decide(&headless, &Holder::Free), Startup::RunHeadless);
    }

    #[test]
    fn a_second_window_is_refused() {
        // The whole point: today this starts a second engine that silently
        // clobbers the first one's session state.
        match decide(&Flags::default(), &held(Mode::Tui)) {
            Startup::Refuse(msg) => assert!(msg.contains("already running"), "{msg}"),
            other => panic!("expected Refuse, got {:?}", other),
        }
    }

    #[test]
    fn a_headless_owner_is_taken_over() {
        assert_eq!(
            decide(&Flags::default(), &held(Mode::Headless)),
            Startup::TakeOver
        );
    }

    #[test]
    fn an_undescribable_owner_is_refused_not_taken_over() {
        // We cannot tell a daemon from a window, so we must not stop it.
        match decide(&Flags::default(), &Holder::Held(None)) {
            Startup::Refuse(_) => {}
            other => panic!("expected Refuse, got {:?}", other),
        }
    }

    #[test]
    fn a_source_goes_to_the_running_session_whoever_owns_it() {
        let flags = Flags {
            torrent_source: Some("magnet:?xt=urn:btih:aa".to_string()),
            ..Default::default()
        };
        for holder in [held(Mode::Tui), held(Mode::Headless), Holder::Held(None)] {
            assert_eq!(
                decide(&flags, &holder),
                Startup::HandOff("magnet:?xt=urn:btih:aa".to_string()),
                "{holder:?}"
            );
        }
        // With nothing running it is just a normal launch.
        assert_eq!(decide(&flags, &Holder::Free), Startup::RunTui);
    }

    #[test]
    fn stop_targets_only_a_background_session() {
        let flags = Flags {
            stop: true,
            ..Default::default()
        };
        assert_eq!(decide(&flags, &held(Mode::Headless)), Startup::RequestStop);
        // A window must never be closed by a background request.
        match decide(&flags, &held(Mode::Tui)) {
            Startup::Refuse(msg) => assert!(msg.contains("quit it with `q`"), "{msg}"),
            other => panic!("expected Refuse, got {:?}", other),
        }
        match decide(&flags, &Holder::Free) {
            Startup::Report(msg) => assert!(msg.contains("No background session"), "{msg}"),
            other => panic!("expected Report, got {:?}", other),
        }
    }

    #[test]
    fn a_handoff_child_waits_for_the_lock_instead_of_refusing() {
        // The regression this guards: a detaching parent holds the lock until
        // it exits, so its child always sees the session as owned. Refusing
        // there kills the background session the user just asked for, and only
        // *sometimes* — whichever of the two won the race.
        let flags = Flags {
            headless: true,
            handoff: true,
            ..Default::default()
        };
        for holder in [held(Mode::Tui), held(Mode::Headless), Holder::Held(None)] {
            assert_eq!(decide(&flags, &holder), Startup::RunHeadless, "{holder:?}");
        }
    }

    #[test]
    fn headless_never_joins_an_existing_session() {
        let flags = Flags {
            headless: true,
            ..Default::default()
        };
        for holder in [held(Mode::Tui), held(Mode::Headless), Holder::Held(None)] {
            assert!(
                matches!(decide(&flags, &holder), Startup::Refuse(_)),
                "{holder:?}"
            );
        }
    }

    #[test]
    fn status_always_reports_and_never_acts() {
        let flags = Flags {
            status: true,
            // Even alongside inputs that would otherwise do something.
            stop: true,
            headless: true,
            handoff: true,
            torrent_source: Some("magnet:?xt=urn:btih:aa".to_string()),
        };
        for holder in [Holder::Free, held(Mode::Tui), held(Mode::Headless)] {
            assert!(
                matches!(decide(&flags, &holder), Startup::Report(_)),
                "{holder:?}"
            );
        }
    }

    #[test]
    fn an_unevaluable_lock_fails_closed() {
        let holder = Holder::Unknown("permission denied".to_string());
        for flags in [
            Flags::default(),
            Flags {
                headless: true,
                ..Default::default()
            },
            Flags {
                stop: true,
                ..Default::default()
            },
        ] {
            match decide(&flags, &holder) {
                Startup::Refuse(msg) => assert!(msg.contains("Refusing to start"), "{msg}"),
                other => panic!("expected Refuse, got {:?}", other),
            }
        }
        // ...except status, which only ever reports.
        let status = Flags {
            status: true,
            ..Default::default()
        };
        assert!(matches!(decide(&status, &holder), Startup::Report(_)));
    }

    #[test]
    fn a_contested_lock_explains_itself() {
        match decide(
            &Flags::default(),
            &Holder::Contested(record(Mode::Headless)),
        ) {
            Startup::Refuse(msg) => assert!(msg.contains("locking is unreliable"), "{msg}"),
            other => panic!("expected Refuse, got {:?}", other),
        }
    }

    #[test]
    fn status_text_covers_every_holder() {
        assert!(format_status(&Holder::Free).contains("No TorrentTUI session"));
        assert!(format_status(&Holder::Held(None)).contains("could not be read"));
        assert!(format_status(&Holder::Unknown("x".into())).contains("Could not read"));

        let mut r = record(Mode::Headless);
        r.privacy = Some("proxy+wg0".to_string());
        let text = format_status(&Holder::Held(Some(r.clone())));
        assert!(text.contains("background session"), "{text}");
        assert!(text.contains("proxy+wg0"), "{text}");

        r.state = State::Failed;
        r.error = Some("error starting TCP listener".to_string());
        let text = format_status(&Holder::Held(Some(r)));
        assert!(text.contains("FAILED to start"), "{text}");
        assert!(text.contains("TCP listener"), "{text}");
    }

    // -- the control channel ------------------------------------------------

    #[test]
    fn stop_request_round_trips_and_is_consumed() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        send_request(&sd, &Request::Stop).unwrap();
        assert_eq!(take_requests(&sd), vec![Request::Stop]);
        assert!(take_requests(&sd).is_empty());
    }

    #[test]
    fn add_requests_round_trip_and_accumulate() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        let a = format!("magnet:?xt=urn:btih:{}", "a".repeat(40));
        let b = format!("magnet:?xt=urn:btih:{}", "b".repeat(40));
        send_request(&sd, &Request::Add(a.clone())).unwrap();
        send_request(&sd, &Request::Add(b.clone())).unwrap();

        let mut got: Vec<String> = take_requests(&sd)
            .into_iter()
            .filter_map(|r| match r {
                Request::Add(s) => Some(s),
                Request::Stop => None,
            })
            .collect();
        got.sort();
        let mut want = vec![a, b];
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn a_half_written_request_is_invisible_until_renamed() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        let control = control_dir(&sd);
        fs::create_dir_all(&control).unwrap();
        // What `send_request` looks like mid-flight.
        fs::write(
            control.join("add-abc.magnet.tmp"),
            b"magnet:?xt=urn:btih:aa",
        )
        .unwrap();
        assert!(take_requests(&sd).is_empty());
        // And it is not swept away either — it is not ours yet.
        assert!(control.join("add-abc.magnet.tmp").exists());
    }

    #[test]
    fn an_oversized_request_is_dropped_unread() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        let control = control_dir(&sd);
        fs::create_dir_all(&control).unwrap();
        fs::write(
            control.join("add-big.magnet"),
            "x".repeat((MAX_REQUEST_BYTES + 1) as usize),
        )
        .unwrap();

        assert!(take_requests(&sd).is_empty());
        assert!(!control.join("add-big.magnet").exists());
    }

    #[test]
    fn a_request_that_is_not_a_valid_magnet_is_discarded() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        let control = control_dir(&sd);
        fs::create_dir_all(&control).unwrap();
        for body in ["", "   ", "/etc/passwd", "./local.torrent", "magnet:?dn=x"] {
            fs::write(control.join("add-x.magnet"), body).unwrap();
            assert!(take_requests(&sd).is_empty(), "accepted {body:?}");
        }
    }

    #[test]
    fn unknown_entries_are_swept() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        let control = control_dir(&sd);
        fs::create_dir_all(&control).unwrap();
        fs::write(control.join("nonsense"), b"hello").unwrap();
        assert!(take_requests(&sd).is_empty());
        assert!(!control.join("nonsense").exists());
    }

    #[test]
    fn clear_requests_drops_a_stop_left_by_a_previous_run() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        send_request(&sd, &Request::Stop).unwrap();
        clear_requests(&sd);
        assert!(take_requests(&sd).is_empty());
    }

    #[test]
    fn take_requests_on_a_missing_directory_is_empty_not_an_error() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        assert!(take_requests(&sd).is_empty());
        clear_requests(&sd);
    }

    // -- hand-off validation ------------------------------------------------

    #[test]
    fn handoff_accepts_only_magnets() {
        let ok = format!("magnet:?xt=urn:btih:{}", "a".repeat(40));
        assert_eq!(handoff_source(&format!("  {ok}  ")), Ok(ok.clone()));

        // A .torrent path would make the daemon read a file chosen by the
        // sender, resolved against the daemon's cwd, not the sender's.
        let err = handoff_source("./x.torrent").unwrap_err();
        assert!(err.contains("Only magnet links"), "{err}");
        assert!(handoff_source("").is_err());
    }

    #[test]
    fn handoff_rejects_oversized_and_control_characters() {
        let long = format!("magnet:?xt=urn:btih:{}", "a".repeat(MAX_MAGNET_CHARS));
        assert!(handoff_source(&long).unwrap_err().contains("too long"));

        let sneaky = format!("magnet:?xt=urn:btih:{}\u{7}x", "a".repeat(40));
        assert!(handoff_source(&sneaky)
            .unwrap_err()
            .contains("control characters"));
    }

    #[test]
    fn headless_command_carries_the_flag_and_the_resolved_download_dir() {
        let cmd = build_headless_command(Path::new("/usr/local/bin/torrenttui"), Some("/dl"), "n1");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            cmd.get_program().to_string_lossy(),
            "/usr/local/bin/torrenttui"
        );
        assert_eq!(args, vec!["--headless", "--download-dir", "/dl"]);

        // No override: the child reads the same config we did.
        let cmd = build_headless_command(Path::new("torrenttui"), None, "n1");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["--headless"]);
    }

    #[test]
    fn handoff_marker_is_matched_by_token_not_mere_presence() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        let instant = std::time::Duration::from_millis(0);

        assert!(!wait_for_handoff(&sd, "mine", instant));
        announce_handoff(&sd, "mine");
        assert!(wait_for_handoff(&sd, "mine", instant));
        // A third process taking the lock in the same window is not the child
        // we spawned, so its marker must not satisfy our wait.
        announce_handoff(&sd, "someone-else");
        assert!(!wait_for_handoff(&sd, "mine", instant));
    }

    #[test]
    fn clearing_the_marker_prevents_a_stale_one_from_confirming() {
        let dir = TempDir::new().unwrap();
        let sd = session_dir(dir.path());
        announce_handoff(&sd, "old");
        clear_handoff(&sd);
        assert!(!wait_for_handoff(
            &sd,
            "old",
            std::time::Duration::from_millis(0)
        ));
        // Clearing a marker that is not there is not an error.
        clear_handoff(&sd);
    }

    #[test]
    fn the_handoff_token_reaches_the_child_through_the_environment() {
        let cmd = build_headless_command(Path::new("torrenttui"), None, "tok123");
        let env: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(
            env.contains(&(HANDOFF_ENV.to_string(), Some("tok123".to_string()))),
            "{env:?}"
        );
    }

    // NOTE: nothing here can assert the *detach* flags. `process_group` and
    // `creation_flags` are write-only on `Command` — there is no getter — so
    // "the child survives closing the terminal" is only ever verified by
    // actually closing a terminal. It is a manual step in the PR checklist, not
    // something a green `cargo test` says anything about.

    #[test]
    fn nonces_differ() {
        assert_ne!(nonce(), nonce());
        assert_eq!(nonce().len(), 32);
    }
}
