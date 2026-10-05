//! Persistent rollout gate. The on-disk default is disabled. A read failure
//! stays read-only and denies every admission; it does not rewrite the file.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use roundtable_protocol::{ErrorCode, MonoMs, QualificationStatus, RtResult};
use serde::{Deserialize, Serialize};

use super::qualification::QualificationKey;
use super::rt_error;

/// Fake, qualification, and product scopes do not convert into each other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionScope {
    Fake,
    Qualification {
        approval_id: String,
        expires_at: MonoMs,
        attempt_limit: u32,
        spend_limit: u64,
        recipient: String,
        fixture_hash: roundtable_protocol::Hash256,
    },
    Product {
        rollout_generation: u64,
    },
}

/// Always false. Same-variant equality is not a conversion either.
pub fn scopes_convert(_from: &ExecutionScope, _to: &ExecutionScope) -> bool {
    false
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmissionFacts {
    pub certificate: QualificationStatus,
    pub presented_key: QualificationKey,
    pub qualification_attempts_used: u32,
    pub qualification_spend_used: u64,
    pub fixture_hash: roundtable_protocol::Hash256,
    pub recipient: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPolicy {
    pub enabled: bool,
    pub generation: u64,
    pub allowed_qualification_keys: Vec<QualificationKey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatePermit {
    scope: ExecutionScope,
}

impl GatePermit {
    pub fn authorizes(&self, scope: &ExecutionScope) -> bool {
        match (&self.scope, scope) {
            (ExecutionScope::Fake, ExecutionScope::Fake) => true,
            (
                ExecutionScope::Qualification { .. } | ExecutionScope::Product { .. },
                ExecutionScope::Qualification { .. } | ExecutionScope::Product { .. },
            ) => self.scope == *scope,
            _ => false,
        }
    }
}

enum GateState {
    Open(ExecutionPolicy),
    /// The file could not be read. Admission stays denied and the bytes stay.
    ReadOnlyDenied,
}

pub struct ExecutionGate {
    data_dir: PathBuf,
    state: Mutex<GateState>,
}

impl ExecutionGate {
    pub fn open(data_dir: &Path) -> Self {
        let path = policy_path(data_dir);
        let state = match fs::read(&path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                GateState::Open(ExecutionPolicy::default())
            }
            Err(_) => GateState::ReadOnlyDenied,
            Ok(bytes) => match serde_json::from_slice::<ExecutionPolicy>(&bytes) {
                Ok(policy) => GateState::Open(policy),
                Err(_) => GateState::ReadOnlyDenied,
            },
        };
        Self {
            data_dir: data_dir.to_path_buf(),
            state: Mutex::new(state),
        }
    }

    pub fn enabled(&self) -> bool {
        match &*self.lock() {
            GateState::Open(policy) => policy.enabled,
            GateState::ReadOnlyDenied => false,
        }
    }

    pub fn is_read_only(&self) -> bool {
        matches!(&*self.lock(), GateState::ReadOnlyDenied)
    }

    pub fn store_policy(&self, policy: &ExecutionPolicy) -> RtResult<()> {
        if self.is_read_only() {
            return Err(rt_error(ErrorCode::InvalidState, "policy_read_only"));
        }
        let bytes = serde_json::to_vec(policy)
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "policy_encode"))?;
        atomic_write(&policy_path(&self.data_dir), &bytes)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "policy_write"))?;
        *self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = GateState::Open(policy.clone());
        Ok(())
    }

    pub fn check(
        &self,
        scope: &ExecutionScope,
        facts: &AdmissionFacts,
        now: MonoMs,
    ) -> RtResult<GatePermit> {
        let policy = {
            let guard = self.lock();
            match &*guard {
                GateState::ReadOnlyDenied => {
                    return Err(rt_error(
                        ErrorCode::CapabilityUnqualified,
                        "policy_unreadable",
                    ));
                }
                GateState::Open(policy) => policy.clone(),
            }
        };
        match scope {
            ExecutionScope::Fake => Ok(GatePermit {
                scope: ExecutionScope::Fake,
            }),
            ExecutionScope::Qualification {
                approval_id: _,
                expires_at,
                attempt_limit,
                spend_limit,
                recipient,
                fixture_hash,
            } => {
                if facts.certificate != QualificationStatus::Passed {
                    return Err(rt_error(
                        ErrorCode::CapabilityUnqualified,
                        "certificate_not_passed",
                    ));
                }
                if now.0 >= expires_at.0 {
                    return Err(rt_error(
                        ErrorCode::CapabilityUnqualified,
                        "qualification_expired",
                    ));
                }
                if facts.qualification_attempts_used >= *attempt_limit {
                    return Err(rt_error(ErrorCode::CapabilityUnqualified, "attempt_limit"));
                }
                if facts.qualification_spend_used > *spend_limit {
                    return Err(rt_error(ErrorCode::CapabilityUnqualified, "spend_limit"));
                }
                if &facts.recipient != recipient || &facts.fixture_hash != fixture_hash {
                    return Err(rt_error(
                        ErrorCode::CapabilityUnqualified,
                        "fixture_mismatch",
                    ));
                }
                Ok(GatePermit {
                    scope: scope.clone(),
                })
            }
            ExecutionScope::Product { rollout_generation } => {
                if !policy.enabled {
                    return Err(rt_error(
                        ErrorCode::CapabilityUnqualified,
                        "rollout_disabled",
                    ));
                }
                if policy.generation != *rollout_generation {
                    return Err(rt_error(
                        ErrorCode::CapabilityUnqualified,
                        "generation_mismatch",
                    ));
                }
                if !policy
                    .allowed_qualification_keys
                    .iter()
                    .any(|key| key == &facts.presented_key)
                {
                    return Err(rt_error(
                        ErrorCode::CapabilityUnqualified,
                        "key_not_allowed",
                    ));
                }
                if facts.certificate != QualificationStatus::Passed {
                    return Err(rt_error(
                        ErrorCode::CapabilityUnqualified,
                        "certificate_not_passed",
                    ));
                }
                Ok(GatePermit {
                    scope: scope.clone(),
                })
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

fn policy_path(data_dir: &Path) -> PathBuf {
    data_dir.join("roundtable").join("execution-policy.json")
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "policy path has no parent",
        )
    })?;
    fs::create_dir_all(parent)?;
    let mut tmp_name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "policy file name"))?
        .to_os_string();
    tmp_name.push(".tmp");
    let tmp = parent.join(tmp_name);
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    replace_file(&tmp, path)?;
    fsync_dir(parent)?;
    Ok(())
}

fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            fs::remove_file(to)?;
            fs::rename(from, to)
        }
        Err(err) => Err(err),
    }
}

pub(crate) fn fsync_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        // Read-only directory handles return ACCESS_DENIED from FlushFileBuffers.
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        file.sync_all()?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let file = std::fs::File::open(path)?;
        file.sync_all()?;
        Ok(())
    }
}
