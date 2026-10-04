//! Qualification certificates. A passed report names one exact component set.
//! Any drift expires that pass. A report with a missing component hash is not
//! a pass, even when the caller labels it passed.

use std::path::Path;

use roundtable_protocol::{Hash256, QualificationStatus};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OsIdentity {
    pub name: String,
    pub version: String,
}

/// One absolute binary that a certificate pins. The allowlist is the
/// certificate itself; a brand name is not a substitute.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertifiedBinary {
    pub role: String,
    pub absolute_path: String,
    pub version: String,
    pub sha256: Hash256,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualificationKey {
    pub os: OsIdentity,
    pub binaries: Vec<CertifiedBinary>,
    pub image_digest: String,
    pub policy_hash: Hash256,
    pub tool_contract_hash: Hash256,
    pub core_hash: Hash256,
    pub adapter_version: String,
    pub isolator_version: String,
    pub plan_hash: Hash256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QualificationReport {
    pub key: QualificationKey,
    pub status: QualificationStatus,
    pub evidence_ref: String,
}

pub fn evaluate_certificate(
    key: &QualificationKey,
    report: Option<&QualificationReport>,
) -> QualificationStatus {
    let Some(report) = report else {
        return QualificationStatus::NotTested;
    };
    if report.status != QualificationStatus::Passed {
        return report.status;
    }
    if &report.key != key {
        return QualificationStatus::Expired;
    }
    if !certificate_complete(key) {
        return QualificationStatus::Failed;
    }
    QualificationStatus::Passed
}

fn certificate_complete(key: &QualificationKey) -> bool {
    if key.os.name.is_empty() || key.os.version.is_empty() || key.binaries.is_empty() {
        return false;
    }
    if key.adapter_version.is_empty() || key.isolator_version.is_empty() {
        return false;
    }
    if !image_digest_complete(&key.image_digest) {
        return false;
    }
    if is_zero(key.policy_hash)
        || is_zero(key.tool_contract_hash)
        || is_zero(key.core_hash)
        || is_zero(key.plan_hash)
    {
        return false;
    }
    key.binaries.iter().all(binary_complete)
}

fn binary_complete(binary: &CertifiedBinary) -> bool {
    !binary.role.is_empty()
        && !binary.version.is_empty()
        && Path::new(&binary.absolute_path).is_absolute()
        && !is_zero(binary.sha256)
}

fn image_digest_complete(digest: &str) -> bool {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_zero(hash: Hash256) -> bool {
    hash == Hash256::from_bytes([0; 32])
}
