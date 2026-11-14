//! The `gateway-private.json` file: the proofs a running gateway mints at
//! each start for Runvil's own programs.
//!
//! This file is deliberately not part of the discovery contract.
//! `gateway.json` is what any client reads to attach, and its key is the one
//! an operator hands to other tools. The proofs here are for Runvil's own
//! programs only, so they live beside it, in a file no public contract
//! names. Like the discovery file it is written atomically and owner-only,
//! and withdrawn on shutdown.
//!
//! A proof is never a credential on its own. A caller presents it in its
//! own header on top of its ordinary authentication. Both files carry the
//! same `pid` and `epoch`; a reader pairs them only when those agree, so a
//! proof from an earlier boot is never sent to a later one.

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::atomic::write_atomic_owner_only;
use crate::error::SidecarError;
use crate::file::GatewayDiscoveryFile;
use crate::paths::gateway_private_file_path;

/// The on-disk proofs of one gateway boot.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayPrivateFile {
    /// The gateway process id, equal to the discovery file's.
    pub pid: u32,
    /// The gateway's boot instant in unix seconds, equal to the discovery
    /// file's.
    pub epoch: u64,
    /// Proves the caller is Runvil's own client: required to use a model
    /// served on a user's ChatGPT plan.
    pub plan_proof: String,
    /// Proves the caller may manage what holds the plan's credential.
    pub owner_proof: String,
}

impl fmt::Debug for GatewayPrivateFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GatewayPrivateFile")
            .field("pid", &self.pid)
            .field("epoch", &self.epoch)
            .field("plan_proof", &"[REDACTED]")
            .field("owner_proof", &"[REDACTED]")
            .finish()
    }
}

impl GatewayPrivateFile {
    /// Whether these proofs belong to the boot `discovery` names.
    #[must_use]
    pub fn names_boot_of(&self, discovery: &GatewayDiscoveryFile) -> bool {
        self.pid == discovery.pid && self.epoch == discovery.epoch
    }

    /// The reason the file fails validation, or `None` when it is valid.
    #[must_use]
    pub fn validation_error(&self) -> Option<&'static str> {
        if self.pid == 0 {
            return Some("pid must not be 0");
        }
        for proof in [&self.plan_proof, &self.owner_proof] {
            if proof.is_empty() {
                return Some("a proof must not be empty");
            }
            if !proof.bytes().all(|byte| byte.is_ascii_graphic()) {
                return Some("a proof must be printable ASCII with no spaces");
            }
        }
        if self.plan_proof == self.owner_proof {
            return Some("the two proofs must differ");
        }
        None
    }

    /// Reads and validates the private file in `run_dir`, returning `None`
    /// when no file exists.
    ///
    /// # Errors
    /// Returns [`SidecarError::Read`] when the file exists but cannot be
    /// read, [`SidecarError::Parse`] when it is not valid JSON, and
    /// [`SidecarError::Invalid`] when it fails validation.
    pub fn read(run_dir: &Path) -> Result<Option<GatewayPrivateFile>, SidecarError> {
        let path = gateway_private_file_path(run_dir);
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(SidecarError::Read {
                    path,
                    source: error,
                });
            }
        };
        let file: GatewayPrivateFile =
            serde_json::from_str(&raw).map_err(|source| SidecarError::Parse {
                path: path.clone(),
                source: source.into(),
            })?;
        if let Some(reason) = file.validation_error() {
            return Err(SidecarError::Invalid {
                path,
                reason: reason.to_owned(),
            });
        }
        Ok(Some(file))
    }

    /// Writes the private file into `run_dir`, creating the directory when
    /// missing. The write is atomic and the file lands owner-only.
    ///
    /// # Errors
    /// Returns [`SidecarError::Invalid`] when the file fails validation,
    /// [`SidecarError::CreateDir`] when the run directory cannot be
    /// created, [`SidecarError::Serialize`] when the file cannot be
    /// serialized, and [`SidecarError::Write`] when the atomic write fails.
    pub fn write_to(&self, run_dir: &Path) -> Result<(), SidecarError> {
        let path = gateway_private_file_path(run_dir);
        if let Some(reason) = self.validation_error() {
            return Err(SidecarError::Invalid {
                path,
                reason: reason.to_owned(),
            });
        }
        fs::create_dir_all(run_dir).map_err(|source| SidecarError::CreateDir {
            path: run_dir.to_owned(),
            source,
        })?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|source| SidecarError::Serialize {
            source: source.into(),
        })?;
        write_atomic_owner_only(&path, &bytes)
            .map_err(|source| SidecarError::Write { path, source })
    }
}

/// Removes the private file in `run_dir` when it still belongs to process
/// `pid`, so a shutting-down gateway withdraws its proofs but never deletes
/// a replacement's. A missing, unreadable or foreign file is left alone.
///
/// # Errors
/// Returns [`SidecarError::Remove`] when the file is this process's own but
/// cannot be removed.
pub fn remove_private_if_mine(run_dir: &Path, pid: u32) -> Result<(), SidecarError> {
    let path = gateway_private_file_path(run_dir);
    let owned = fs::read_to_string(&path).is_ok_and(|raw| {
        serde_json::from_str::<GatewayPrivateFile>(&raw).is_ok_and(|file| file.pid == pid)
    });
    if !owned {
        return Ok(());
    }
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SidecarError::Remove {
            path,
            source: error,
        }),
    }
}

#[cfg(test)]
#[path = "private-tests.rs"]
mod tests;
