//! The owner-only record of one ChatGPT sign-in, on disk.
//!
//! One file, `chatgpt-plan.json`, inside the `0700` `chatgpt-plan/`
//! subdirectory of the Gateway's profile directory, so a `0775` profile
//! directory under `umask 0002` works while keeping the record, temp files,
//! and lock private to this user. It holds `ext_agent_host_id`, the
//! registration (`client_id` and `sub`), and the closed [`Session`] lifecycle
//! ([`Session::SignedOut`], [`Session::Active`], [`Session::Refreshing`],
//! [`Session::NeedsAuth`]). The access token is never written: one refresh
//! after a restart mints a fresh one in memory.
//!
//! [`Store::open`] refuses symlinks on both the profile directory and
//! `chatgpt-plan/`, requires `chatgpt-plan/` to belong to this user with no
//! group or other access (`mode & 0o077 == 0`), holds an exclusive lock on
//! `chatgpt-plan.lock` for the life of [`Store`], and reaps crashed temp files
//! only under that lock. [`Store::load`] verifies a regular owner-only file of
//! this user before reading from the opened handle. Every write creates an
//! exclusive `0600` temp file, syncs it, renames it over the record, and syncs
//! the directory. Unverifiable or unparsable records fail closed.
//!
//! ## Windows DACL design (not yet enabled)
//!
//! [`Store::open`] returns [`StoreError::Unsupported`] on Windows until native
//! owner-only DACLs land (never `icacls`, which races creation, leaves
//! explicit ACEs for other local accounts intact, and depends on locale
//! strings):
//! 1. Cache the current user SID via `OpenProcessToken`, `GetTokenInformation`
//!    (`TokenUser`), and `ConvertSidToStringSidW`.
//! 2. Open `<profile_dir>` and `<profile_dir>\chatgpt-plan` with `CreateFileW`
//!    (`FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT`), reject
//!    `FILE_ATTRIBUTE_REPARSE_POINT`, and replace the directory DACL via
//!    `ConvertStringSecurityDescriptorToSecurityDescriptorW` +
//!    `SetSecurityInfo` (`DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION`)
//!    using `D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;<user-sid>)`.
//! 3. Lock `chatgpt-plan.lock` via `LockFileEx`
//!    (`LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY`).
//! 4. Create temp files born owner-only via `CreateFileW` (`CREATE_NEW`,
//!    `dwShareMode = 0`, `SECURITY_ATTRIBUTES` with
//!    `D:P(A;;FA;;;SY)(A;;FA;;;<user-sid>)`), `FlushFileBuffers`, and
//!    `MoveFileExW` (`MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`).
//! 5. On load, open with `FILE_FLAG_OPEN_REPARSE_POINT`, reject reparse
//!    points, and check `GetSecurityInfo` + `EqualSid` (`SE_DACL_PROTECTED`
//!    and ACE SIDs restricted to `TokenUser` / `WinLocalSystemSid`, plus
//!    `WinBuiltinAdministratorsSid` when elevated).
//!
//! Reference: OpenAI's "Registration and sign-in", section "5. Store
//! credentials in a local file",
//! <https://developers.openai.com/siwc/token-sharing-open-source/sign-in>.

#[cfg(unix)]
use std::fs::{self, OpenOptions};
use std::io;
#[cfg(unix)]
use std::io::{Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

#[path = "store-record.rs"]
mod record;

pub use self::record::{Credentials, Record, Revocation, Secret, Session, Status};

/// The private subdirectory created inside the Gateway's profile directory.
pub const STORE_DIR: &str = "chatgpt-plan";

/// The record's file name inside [`STORE_DIR`].
pub const RECORD_FILE: &str = "chatgpt-plan.json";

/// The exclusive lock file inside [`STORE_DIR`].
pub const LOCK_FILE: &str = "chatgpt-plan.lock";

/// Owner-only mode (`rw-------`) for ancillary files in [`STORE_DIR`].
#[cfg(unix)]
const OWNER_FILE_MODE: u32 = 0o600;

/// What every temp file of a write starts and ends with.
#[cfg(unix)]
const TEMP_PREFIX: &str = ".chatgpt-plan.json.";
#[cfg(unix)]
const TEMP_SUFFIX: &str = ".tmp";

/// Why the store could not be opened, or its record read or written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The file exists but does not parse, or has an unknown version.
    #[error("the ChatGPT sign-in record is unreadable; sign in again after resetting it")]
    Corrupt,
    /// The store directory is not a private directory of this user.
    #[error("the ChatGPT sign-in directory is not private to this user: {0}")]
    UnsafeDirectory(&'static str),
    /// A file in the store is not a regular owner-only file of this user.
    #[error("the ChatGPT sign-in file is not private to this user: {0}")]
    UnsafeFile(&'static str),
    /// Another process already holds the store's exclusive lock.
    #[error("another process is already using this ChatGPT sign-in store")]
    Locked,
    /// This platform cannot yet store the record owner-only.
    #[error("ChatGPT sign-in is not yet supported on this platform")]
    Unsupported,
    /// A file-system operation failed while opening, reading, writing,
    /// syncing, or removing the store.
    #[error("the ChatGPT sign-in store failed while trying to {operation}")]
    Io {
        /// Which store phase failed.
        operation: &'static str,
        /// Underlying I/O error kept for internal diagnostics; not emitted in
        /// the Display message.
        #[source]
        source: io::Error,
    },
}

fn io_op(operation: &'static str) -> impl FnOnce(io::Error) -> StoreError {
    move |source| StoreError::Io { operation, source }
}

/// The record's private directory, exclusive lock, and read and write rules.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    uid: u32,
    #[cfg(unix)]
    _lock: fs::File,
}

impl Store {
    /// Opens the private `chatgpt-plan/` store under `profile_dir`, creating
    /// it with mode `0700` when absent and holding its exclusive lock.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the directory or lock file is unsafe,
    /// already locked, unsupported on this platform, or fails I/O.
    pub fn open(profile_dir: &Path) -> Result<Store, StoreError> {
        open_store(profile_dir)
    }

    fn path(&self) -> PathBuf {
        self.dir.join(RECORD_FILE)
    }

    /// Reads the record. `Ok(None)` means no sign-in has ever been stored.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the directory or record is unsafe, corrupt,
    /// unsupported on this platform, or fails I/O.
    pub fn load(&self) -> Result<Option<Record>, StoreError> {
        let Some(bytes) = read_private(&self.dir, &self.path(), self.uid)? else {
            return Ok(None);
        };
        let record: Record = serde_json::from_slice(&bytes).map_err(|_| StoreError::Corrupt)?;
        if record.version != Record::VERSION || !record.is_consistent() {
            return Err(StoreError::Corrupt);
        }
        Ok(Some(record))
    }

    /// Replaces the record, owner-only and atomically.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the directory is unsafe, unsupported on
    /// this platform, or fails I/O. On error the previous record is unchanged.
    pub fn save(&self, record: &Record) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(record)
            .map_err(|error| io_op("serialize record")(error.into()))?;
        write_private(&self.dir, &self.path(), self.uid, &bytes)
    }

    /// Deletes the record: the complete local teardown. A missing record is
    /// not an error.
    ///
    /// # Errors
    /// Returns [`StoreError`] on unsupported platforms or when removal fails.
    pub fn remove(&self) -> Result<(), StoreError> {
        remove_private(&self.dir, &self.path())
    }
}

#[cfg(unix)]
fn open_store(profile_dir: &Path) -> Result<Store, StoreError> {
    if !fs::symlink_metadata(profile_dir)
        .map_err(io_op("inspect profile directory"))?
        .file_type()
        .is_dir()
    {
        return Err(StoreError::UnsafeDirectory("not a plain directory"));
    }

    let dir = profile_dir.join(STORE_DIR);
    match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(io_op("create store directory")(error)),
    }

    let dir_meta = fs::symlink_metadata(&dir).map_err(io_op("inspect store directory"))?;
    check_private_dir(&dir_meta, None)?;
    let open_meta = fs::File::open(&dir)
        .and_then(|handle| handle.metadata())
        .map_err(io_op("open store directory"))?;
    if !same_inode(&dir_meta, &open_meta) {
        return Err(StoreError::UnsafeDirectory("not a plain directory"));
    }
    check_private_dir(&open_meta, None)?;

    let (lock, lock_meta) = acquire_lock(&dir.join(LOCK_FILE))?;
    let uid = current_uid_in(&dir)?;
    check_private_dir(&open_meta, Some(uid))?;
    check_private_file(&lock_meta, Some(uid))?;

    // Reaping runs only after the exclusive lock is held, so an active temp
    // write from another process can never be mistaken for crash residue.
    reap_stale_temps(&dir);
    Ok(Store {
        dir,
        uid,
        _lock: lock,
    })
}

#[cfg(unix)]
fn acquire_lock(lock_path: &Path) -> Result<(fs::File, fs::Metadata), StoreError> {
    match fs::symlink_metadata(lock_path) {
        Ok(meta) => check_private_file(&meta, None)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_op("inspect lock file")(error)),
    }

    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(OWNER_FILE_MODE)
        .open(lock_path)
        .map_err(io_op("open lock file"))?;
    let fstat = lock.metadata().map_err(io_op("inspect open lock file"))?;
    let lstat = fs::symlink_metadata(lock_path).map_err(io_op("inspect lock file"))?;
    if !same_inode(&fstat, &lstat) {
        return Err(StoreError::UnsafeFile("not a regular file"));
    }
    check_private_file(&fstat, None)?;
    match lock.try_lock() {
        Ok(()) => Ok((lock, fstat)),
        Err(fs::TryLockError::WouldBlock) => Err(StoreError::Locked),
        Err(fs::TryLockError::Error(error)) => Err(io_op("lock store")(error)),
    }
}

#[cfg(unix)]
fn current_uid_in(dir: &Path) -> Result<u32, StoreError> {
    let suffix = crate::oauth::random_token(12).ok_or_else(|| {
        io_op("generate ownership probe name")(io::Error::other("no system randomness"))
    })?;
    let probe = dir.join(format!(".chatgpt-plan.probe.{suffix}"));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(OWNER_FILE_MODE)
        .open(&probe)
        .map_err(io_op("create ownership probe file"))?;
    let uid = file
        .metadata()
        .map(|m| m.uid())
        .map_err(io_op("inspect ownership probe file"));
    let _ = fs::remove_file(&probe);
    uid
}

#[cfg(unix)]
fn check_private_dir(meta: &fs::Metadata, uid: Option<u32>) -> Result<(), StoreError> {
    if !meta.file_type().is_dir() {
        return Err(StoreError::UnsafeDirectory("not a plain directory"));
    }
    if uid.is_some_and(|expected| meta.uid() != expected) {
        return Err(StoreError::UnsafeDirectory("owned by another user"));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(StoreError::UnsafeDirectory("accessible by group or others"));
    }
    Ok(())
}

#[cfg(unix)]
fn verify_private_dir(dir: &Path, uid: u32) -> Result<(), StoreError> {
    let meta = fs::symlink_metadata(dir).map_err(io_op("inspect store directory"))?;
    check_private_dir(&meta, Some(uid))
}

#[cfg(unix)]
fn check_private_file(meta: &fs::Metadata, uid: Option<u32>) -> Result<(), StoreError> {
    if !meta.file_type().is_file() {
        return Err(StoreError::UnsafeFile("not a regular file"));
    }
    if uid.is_some_and(|expected| meta.uid() != expected) {
        return Err(StoreError::UnsafeFile("owned by another user"));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(StoreError::UnsafeFile("accessible by group or others"));
    }
    Ok(())
}

#[cfg(unix)]
fn read_private(dir: &Path, target: &Path, uid: u32) -> Result<Option<Vec<u8>>, StoreError> {
    verify_private_dir(dir, uid)?;
    reap_stale_temps(dir);

    let lstat = match fs::symlink_metadata(target) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_op("inspect record file")(error)),
    };
    check_private_file(&lstat, Some(uid))?;

    let mut file = fs::File::open(target).map_err(io_op("open record file"))?;
    let fstat = file.metadata().map_err(io_op("inspect open record file"))?;
    if !same_inode(&lstat, &fstat) {
        return Err(StoreError::UnsafeFile("not a regular file"));
    }
    check_private_file(&fstat, Some(uid))?;

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(io_op("read record file"))?;
    Ok(Some(bytes))
}

/// Writes `bytes` to `target` in `dir` through a fresh owner-only temp file,
/// then syncs `dir` so the rename is durable.
#[cfg(unix)]
fn write_private(dir: &Path, target: &Path, uid: u32, bytes: &[u8]) -> Result<(), StoreError> {
    verify_private_dir(dir, uid)?;
    reap_stale_temps(dir);
    let suffix = crate::oauth::random_token(12).ok_or_else(|| {
        io_op("generate temp record name")(io::Error::other("no system randomness"))
    })?;
    let temp = dir.join(format!("{TEMP_PREFIX}{suffix}{TEMP_SUFFIX}"));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(io_op("create temp record file"))?;
        file.write_all(bytes)
            .map_err(io_op("write temp record file"))?;
        file.sync_all().map_err(io_op("sync temp record file"))?;
        fs::rename(&temp, target).map_err(io_op("rename record file"))?;
        fs::File::open(dir)
            .and_then(|dir_file| dir_file.sync_all())
            .map_err(io_op("sync store directory"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(unix)]
fn remove_private(dir: &Path, target: &Path) -> Result<(), StoreError> {
    reap_stale_temps(dir);
    match fs::remove_file(target) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            Err(io_op("remove record file")(error))
        }
        _ => Ok(()),
    }
}

#[cfg(unix)]
fn same_inode(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Removes temp files a crashed write left behind. Each may hold a refresh
/// token; callers must hold the store's exclusive lock first.
#[cfg(unix)]
fn reap_stale_temps(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(TEMP_PREFIX) && name.ends_with(TEMP_SUFFIX) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// No owner-only files yet on this platform: refuse rather than read or write
/// a token another account might access.
#[cfg(not(unix))]
fn open_store(_profile_dir: &Path) -> Result<Store, StoreError> {
    Err(StoreError::Unsupported)
}

#[cfg(not(unix))]
fn read_private(_dir: &Path, _target: &Path, _uid: u32) -> Result<Option<Vec<u8>>, StoreError> {
    Err(StoreError::Unsupported)
}

#[cfg(not(unix))]
fn write_private(_dir: &Path, _target: &Path, _uid: u32, _bytes: &[u8]) -> Result<(), StoreError> {
    Err(StoreError::Unsupported)
}

#[cfg(not(unix))]
fn remove_private(_dir: &Path, _target: &Path) -> Result<(), StoreError> {
    Err(StoreError::Unsupported)
}

#[cfg(test)]
#[path = "store-tests.rs"]
mod tests;
