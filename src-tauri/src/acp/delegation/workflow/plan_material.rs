//! Bounded Plan material parsing, selectors, identities, and change proofs.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::OnceLock;

use pulldown_cmark::{Event, HeadingLevel, MetadataBlockKind, Options, Parser, Tag, TagEnd};
use regex::Regex;
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

use super::types::{
    DocumentGateKind, ManifestNodeRole, NormalizedManifest, PlanChangeClassification,
    PlanLineageResetReason, PlanLocalizedChangeV2, TaskSpecificationIdentityV1, MAX_TASKS,
    PHASE_PLAN,
};
use super::validate::active_plan_material_task_indices;

pub const PLAN_MATERIAL_SCHEMA_V1: &str = "PlanMaterialSchemaV1";
pub const PLAN_LOCALIZED_CHANGE_SCHEMA_V2: &str = "PlanLocalizedChangeV2";
pub const PLAN_LOCALIZED_CHANGE_CLASSIFIER_VERSION: &str = "plan_localized_change_v2";
pub const MAX_PLAN_MATERIAL_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_PLAN_SECTION_BYTES: usize = 512 * 1024;

const FRONT_MATTER_KEY: &str = "plan.front_matter";
const GLOBAL_CONSTRAINTS_KEY: &str = "plan.global_constraints";
const GLOBAL_PREAMBLE_KEY: &str = "plan.global_preamble";
const POLICIES_FINGERPRINT_KEY: &str = "plan.policies_fingerprint";
const SHARED_KEYS: [&str; 4] = [
    FRONT_MATTER_KEY,
    GLOBAL_CONSTRAINTS_KEY,
    GLOBAL_PREAMBLE_KEY,
    POLICIES_FINGERPRINT_KEY,
];

/// One normalized literal source span in a parsed Plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanMaterialEntryV1 {
    normalized_body: String,
    body_sha256: String,
}

/// Bounded, normalized Plan material keyed by shared and `task.N` selectors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanMaterialSchemaV1 {
    schema: String,
    plan_sha256: String,
    referenced_task_indices: BTreeSet<u32>,
    materials: BTreeMap<String, PlanMaterialEntryV1>,
    #[serde(skip)]
    source_bytes: Vec<u8>,
}

pub type PlanMaterialMap = PlanMaterialSchemaV1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum MaterialSelectorKindV1 {
    All,
    Keys { keys: BTreeSet<String> },
}

/// Immutable material selector carrying provenance for one bound Plan map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MaterialSelectorV1 {
    #[serde(flatten)]
    kind: MaterialSelectorKindV1,
    #[serde(skip)]
    material_binding_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlanReviewerMaterialV1 {
    node_id: String,
    selector: MaterialSelectorV1,
    is_passing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthorizedLocalizedPlanChangeV1 {
    authorization_id: String,
    material_binding_sha256: String,
    reviewers: BTreeMap<String, PlanReviewerMaterialV1>,
}

/// Server-owned context retaining the full cohort and any localized authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanLocalizedChangeAuthorizationV1 {
    reviewer_cohort_node_ids: BTreeSet<String>,
    localized_change: Option<AuthorizedLocalizedPlanChangeV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanMaterialErrorKind {
    FileTooLarge,
    InvalidUtf8,
    InvalidTaskHeading,
    InvalidReferencedTask(u32),
    DuplicateReference(u32),
    DuplicateTask(u32),
    MissingTask(u32),
    TooManyTasks,
    SectionTooLarge(String),
    MissingReviewerNode(String),
    InvalidReviewerNode(String),
    AmbiguousReviewerIdentity,
    MissingRouteNode(String),
    InvalidRouteNode(String),
    ManifestTaskSetMismatch,
    SelectorKeyMissing(String),
    InvalidPoliciesMaterial,
    InvalidMaterialMap,
    MissingPlanGate,
    AmbiguousPlanGate,
    ReviewerCohortMismatch,
    InvalidAuthorization,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct PlanMaterialError {
    kind: PlanMaterialErrorKind,
    message: String,
}

impl PlanMaterialError {
    fn new(kind: PlanMaterialErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> PlanMaterialErrorKind {
        self.kind.clone()
    }

    pub const fn code(&self) -> &'static str {
        match self.kind {
            PlanMaterialErrorKind::DuplicateTask(_) => "duplicate_task",
            _ => "completion_plan_material_invalid",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanMaterialChangeInputV1 {
    Parsed(BoundPlanMaterialMap),
    Invalid {
        plan_sha256: String,
        error_kind: PlanMaterialErrorKind,
    },
}

/// Plan material whose policy fingerprint was derived from a validated manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundPlanMaterialMap {
    material: PlanMaterialMap,
    binding_sha256: String,
}

/// Validated estimated-publication comparison consumed by durable store transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanPublicationMaterialDecisionV1 {
    prior_material: BoundPlanMaterialMap,
    current_material: BoundPlanMaterialMap,
    changed_keys: BTreeSet<String>,
    selector_sets_changed: bool,
}

impl PlanMaterialChangeInputV1 {
    pub fn parsed(material: BoundPlanMaterialMap) -> Self {
        Self::Parsed(material)
    }

    pub fn invalid(plan_sha256: impl Into<String>, error_kind: PlanMaterialErrorKind) -> Self {
        Self::Invalid {
            plan_sha256: plan_sha256.into(),
            error_kind,
        }
    }

    pub fn material(&self) -> Option<&BoundPlanMaterialMap> {
        match self {
            Self::Parsed(material) => Some(material),
            Self::Invalid { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanMaterialGoldenProjectionV1 {
    schema: String,
    materials: BTreeMap<String, PlanMaterialEntryV1>,
    task_identities: BTreeMap<String, TaskIdentityGoldenProjectionV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TaskIdentityGoldenProjectionV1 {
    identity: TaskSpecificationIdentityV1,
    identity_sha256: String,
}

#[derive(Debug)]
struct HeadingSpan {
    level: u8,
    line_start: usize,
    body_start: usize,
    is_atx: bool,
    text: String,
}

#[derive(Debug)]
struct ActiveHeading {
    level: u8,
    range: Range<usize>,
    text: String,
}

#[derive(Serialize)]
struct ReferencedTaskPolicyMaterial<'a> {
    referenced_task_indices: &'a BTreeSet<u32>,
}

/// Parse all literal Task sections plus the four shared Plan material keys.
pub fn parse_plan_material(
    bytes: &[u8],
    referenced_task_indices: &[u32],
) -> Result<PlanMaterialMap, PlanMaterialError> {
    if bytes.len() > MAX_PLAN_MATERIAL_BYTES {
        return Err(PlanMaterialError::new(
            PlanMaterialErrorKind::FileTooLarge,
            format!(
                "Plan bytes {} exceed {MAX_PLAN_MATERIAL_BYTES}",
                bytes.len()
            ),
        ));
    }

    let decoded = std::str::from_utf8(bytes).map_err(|_| {
        PlanMaterialError::new(
            PlanMaterialErrorKind::InvalidUtf8,
            "Plan material is not valid UTF-8",
        )
    })?;
    let source = normalize_source_for_parsing(decoded);
    let referenced_task_indices = validate_referenced_tasks(referenced_task_indices)?;
    let (headings, front_matter) = collect_headings_and_front_matter(&source);

    let mut tasks = Vec::new();
    let mut seen_tasks = BTreeSet::new();
    for heading in &headings {
        if !matches!(heading.level, 2 | 3) || !heading.is_atx {
            continue;
        }
        let Some(task_index) = task_heading_index(&heading.text)? else {
            continue;
        };
        if !seen_tasks.insert(task_index) {
            return Err(PlanMaterialError::new(
                PlanMaterialErrorKind::DuplicateTask(task_index),
                format!("duplicate Task {task_index} heading"),
            ));
        }
        tasks.push((task_index, heading));
    }

    if tasks.len() > MAX_TASKS {
        return Err(PlanMaterialError::new(
            PlanMaterialErrorKind::TooManyTasks,
            format!("Plan has {} Tasks; maximum is {MAX_TASKS}", tasks.len()),
        ));
    }
    for task_index in &referenced_task_indices {
        if !seen_tasks.contains(task_index) {
            return Err(PlanMaterialError::new(
                PlanMaterialErrorKind::MissingTask(*task_index),
                format!("referenced Task {task_index} is missing"),
            ));
        }
    }

    let mut materials = BTreeMap::new();
    insert_material(
        &mut materials,
        FRONT_MATTER_KEY,
        front_matter
            .as_ref()
            .map(|range| &source[range.clone()])
            .unwrap_or(""),
    )?;
    let first_task_start = tasks
        .first()
        .map(|(_, heading)| heading.line_start)
        .unwrap_or(source.len());
    insert_material(
        &mut materials,
        GLOBAL_PREAMBLE_KEY,
        &source[..first_task_start],
    )?;

    let global_constraints = headings
        .iter()
        .enumerate()
        .find(|(_, heading)| global_constraints_regex().is_match(&heading.text))
        .map(|(index, heading)| {
            let end = headings[index + 1..]
                .iter()
                .find(|candidate| candidate.level <= heading.level)
                .map(|candidate| candidate.line_start)
                .unwrap_or(source.len());
            &source[heading.body_start..end]
        })
        .unwrap_or("");
    insert_material(&mut materials, GLOBAL_CONSTRAINTS_KEY, global_constraints)?;

    let policy_bytes = serde_json::to_vec(&ReferencedTaskPolicyMaterial {
        referenced_task_indices: &referenced_task_indices,
    })
    .map_err(|error| {
        PlanMaterialError::new(
            PlanMaterialErrorKind::InvalidPoliciesMaterial,
            format!("serialize referenced Task policies: {error}"),
        )
    })?;
    insert_material_bytes(&mut materials, POLICIES_FINGERPRINT_KEY, &policy_bytes)?;

    for (position, (task_index, heading)) in tasks.iter().enumerate() {
        let end = tasks[position + 1..]
            .iter()
            .find(|(_, candidate)| candidate.level <= heading.level)
            .map(|(_, candidate)| candidate.line_start)
            .unwrap_or(source.len());
        insert_material(
            &mut materials,
            &format!("task.{task_index}"),
            &source[heading.body_start..end],
        )?;
    }

    Ok(PlanMaterialMap {
        schema: PLAN_MATERIAL_SCHEMA_V1.into(),
        plan_sha256: sha256_prefixed(bytes),
        referenced_task_indices,
        materials,
        source_bytes: bytes.to_vec(),
    })
}

/// Validate parsed material and bind its policy fingerprint to manifest authority.
pub fn bind_plan_material(
    manifest: &NormalizedManifest,
    material: &PlanMaterialMap,
) -> Result<BoundPlanMaterialMap, PlanMaterialError> {
    validate_material_map(material)?;
    if manifest_task_indices(manifest)? != material.referenced_task_indices {
        return Err(PlanMaterialError::new(
            PlanMaterialErrorKind::ManifestTaskSetMismatch,
            "manifest task policies do not match parsed Plan references",
        ));
    }
    let bytes = canonical_policy_material(manifest)?;
    let mut bound = material.clone();
    bound.replace_policies_material(&bytes)?;
    validate_material_map(&bound)?;
    let binding_sha256 = material_binding_digest(&bound)?;
    Ok(BoundPlanMaterialMap {
        material: bound,
        binding_sha256,
    })
}

/// Build a complete, validated estimated-publication material decision.
pub fn plan_publication_material_decision(
    prior_manifest: &NormalizedManifest,
    prior: &PlanMaterialMap,
    current_manifest: &NormalizedManifest,
    current: &PlanMaterialMap,
) -> Result<PlanPublicationMaterialDecisionV1, PlanMaterialError> {
    let prior_material = bind_plan_material(prior_manifest, prior)?;
    let current_material = bind_plan_material(current_manifest, current)?;
    let prior_selectors = derive_plan_reviewer_selectors(prior_manifest, &prior_material)?;
    let current_selectors = derive_plan_reviewer_selectors(current_manifest, &current_material)?;
    let selector_sets_changed =
        selector_kinds(&prior_selectors) != selector_kinds(&current_selectors);
    let changed_keys = changed_material_keys(&prior_material.material, &current_material.material);
    Ok(PlanPublicationMaterialDecisionV1 {
        prior_material,
        current_material,
        changed_keys,
        selector_sets_changed,
    })
}

/// Derive a Reviewer's selector exclusively from validated manifest identity and routes.
pub fn derive_plan_reviewer_selector(
    manifest: &NormalizedManifest,
    plan_reviewer_node_id: &str,
    material: &BoundPlanMaterialMap,
) -> Result<MaterialSelectorV1, PlanMaterialError> {
    validate_bound_material(material)?;
    validate_manifest_binding(manifest, material)?;
    let reviewer_cohort_node_ids = plan_reviewer_cohort_node_ids(manifest)?;
    if !reviewer_cohort_node_ids.contains(plan_reviewer_node_id) {
        return Err(PlanMaterialError::new(
            PlanMaterialErrorKind::ReviewerCohortMismatch,
            format!("Plan Reviewer node {plan_reviewer_node_id} is outside the active cohort"),
        ));
    }
    let reviewer = manifest
        .nodes
        .iter()
        .find(|node| node.id == plan_reviewer_node_id)
        .ok_or_else(|| {
            PlanMaterialError::new(
                PlanMaterialErrorKind::MissingReviewerNode(plan_reviewer_node_id.into()),
                format!("missing Plan Reviewer node {plan_reviewer_node_id}"),
            )
        })?;
    if reviewer.phase_id.as_deref() != Some(PHASE_PLAN)
        || reviewer.role != Some(ManifestNodeRole::Reviewer)
        || reviewer.task_index.is_some()
        || reviewer.agent_type.is_none()
    {
        return Err(PlanMaterialError::new(
            PlanMaterialErrorKind::InvalidReviewerNode(plan_reviewer_node_id.into()),
            format!("node {plan_reviewer_node_id} is not a durable Plan Reviewer"),
        ));
    }
    let agent_type = reviewer.agent_type.as_deref().expect("checked above");
    let profile_id = reviewer.profile_id.as_deref();
    if manifest.nodes.iter().any(|node| {
        node.id != reviewer.id
            && node.phase_id.as_deref() == Some(PHASE_PLAN)
            && node.role == Some(ManifestNodeRole::Reviewer)
            && node.agent_type.as_deref() == Some(agent_type)
            && node.profile_id.as_deref() == profile_id
    }) {
        return Err(PlanMaterialError::new(
            PlanMaterialErrorKind::AmbiguousReviewerIdentity,
            "Plan Reviewer agent/profile identity is ambiguous",
        ));
    }

    let manifest_tasks = manifest_task_indices(manifest)?;
    if manifest_tasks != material.material.referenced_task_indices {
        return Err(PlanMaterialError::new(
            PlanMaterialErrorKind::ManifestTaskSetMismatch,
            "manifest task policies do not match parsed Plan references",
        ));
    }

    let mut keys = SHARED_KEYS
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    for policy in &manifest.task_policies {
        let task_key = format!("task.{}", policy.task_index);
        if !material.material.materials.contains_key(&task_key) {
            return Err(PlanMaterialError::new(
                PlanMaterialErrorKind::MissingTask(policy.task_index),
                format!("manifest references missing {}", task_key),
            ));
        }
        for route_node_id in &policy.route.reviewer_node_ids {
            let route_node = manifest
                .nodes
                .iter()
                .find(|node| node.id == *route_node_id)
                .ok_or_else(|| {
                    PlanMaterialError::new(
                        PlanMaterialErrorKind::MissingRouteNode(route_node_id.clone()),
                        format!("missing Task Reviewer route node {route_node_id}"),
                    )
                })?;
            if route_node.role != Some(ManifestNodeRole::Reviewer)
                || route_node.task_index != Some(policy.task_index)
                || route_node.agent_type.is_none()
            {
                return Err(PlanMaterialError::new(
                    PlanMaterialErrorKind::InvalidRouteNode(route_node_id.clone()),
                    format!("route node {route_node_id} is not a durable Task Reviewer"),
                ));
            }
            if route_node.agent_type.as_deref() == Some(agent_type)
                && route_node.profile_id.as_deref() == profile_id
            {
                keys.insert(task_key.clone());
            }
        }
    }

    let selector = MaterialSelectorV1::from_authorized_keys(material, keys);
    selector.selected_keys(material)?;
    Ok(selector)
}

/// Mint `all` only from server-owned holistic/full-cohort policy code.
pub fn derive_holistic_full_cohort_selector(material: &BoundPlanMaterialMap) -> MaterialSelectorV1 {
    MaterialSelectorV1 {
        kind: MaterialSelectorKindV1::All,
        material_binding_sha256: material.binding_sha256.clone(),
    }
}

/// Any normalized key-set or body-hash change at estimated publication resets lineage.
pub fn plan_publication_requires_new_lineage(
    prior: &BoundPlanMaterialMap,
    current: &BoundPlanMaterialMap,
) -> bool {
    validate_bound_material(prior).is_err()
        || validate_bound_material(current).is_err()
        || material_hashes(&prior.material) != material_hashes(&current.material)
}

/// Bind current Plan Reviewer outcomes to manifest-derived selectors.
pub fn localized_plan_change_context(
    manifest: &NormalizedManifest,
) -> Result<PlanLocalizedChangeAuthorizationV1, PlanMaterialError> {
    let (reviewer_cohort_node_ids, _) = plan_reviewer_gate_node_ids(manifest)?;
    Ok(PlanLocalizedChangeAuthorizationV1 {
        reviewer_cohort_node_ids,
        localized_change: None,
    })
}

/// Bind current required Plan Reviewer outcomes to manifest-derived selectors.
pub fn authorize_localized_plan_change(
    manifest: &NormalizedManifest,
    current: &BoundPlanMaterialMap,
    authorization_id: &str,
    reviewer_states: &BTreeMap<String, bool>,
) -> Result<PlanLocalizedChangeAuthorizationV1, PlanMaterialError> {
    validate_bound_material(current)?;
    if authorization_id.trim().is_empty() {
        return Err(PlanMaterialError::new(
            PlanMaterialErrorKind::InvalidAuthorization,
            "localized Plan authorization ID is empty",
        ));
    }
    let (reviewer_cohort_node_ids, required_reviewer_node_ids) =
        plan_reviewer_gate_node_ids(manifest)?;
    if reviewer_states.keys().cloned().collect::<BTreeSet<_>>() != required_reviewer_node_ids {
        return Err(PlanMaterialError::new(
            PlanMaterialErrorKind::ReviewerCohortMismatch,
            "reviewer states do not exactly match the required Plan reviewers",
        ));
    }
    let mut reviewers = BTreeMap::new();
    for node_id in &required_reviewer_node_ids {
        reviewers.insert(
            node_id.clone(),
            PlanReviewerMaterialV1 {
                node_id: node_id.clone(),
                selector: derive_plan_reviewer_selector(manifest, node_id, current)?,
                is_passing: reviewer_states[node_id],
            },
        );
    }
    Ok(PlanLocalizedChangeAuthorizationV1 {
        reviewer_cohort_node_ids,
        localized_change: Some(AuthorizedLocalizedPlanChangeV1 {
            authorization_id: authorization_id.to_owned(),
            material_binding_sha256: current.binding_sha256.clone(),
            reviewers,
        }),
    })
}

/// Classify a post-review correction conservatively, returning a full-cohort reset on doubt.
pub fn classify_plan_change(
    prior: &PlanMaterialChangeInputV1,
    current: &PlanMaterialChangeInputV1,
    authorization: &PlanLocalizedChangeAuthorizationV1,
) -> PlanChangeClassification {
    let cohort = authorization.reviewer_cohort_node_ids.clone();
    let (Some(prior), Some(current)) = (prior.material(), current.material()) else {
        return new_lineage(
            BTreeSet::new(),
            PlanLineageResetReason::UnparseableMaterial,
            cohort,
        );
    };
    let changed_keys = changed_material_keys(&prior.material, &current.material);
    if validate_bound_material(prior).is_err() || validate_bound_material(current).is_err() {
        return new_lineage(
            changed_keys,
            PlanLineageResetReason::PolicyOrRouteChanged,
            cohort,
        );
    }
    if prior.material.materials.keys().collect::<BTreeSet<_>>()
        != current.material.materials.keys().collect::<BTreeSet<_>>()
    {
        return new_lineage(
            changed_keys,
            PlanLineageResetReason::AmbiguousKeySet,
            cohort,
        );
    }
    if changed_keys.contains(POLICIES_FINGERPRINT_KEY) {
        return new_lineage(
            changed_keys,
            PlanLineageResetReason::PolicyOrRouteChanged,
            cohort,
        );
    }
    if changed_keys.iter().any(|key| {
        matches!(
            key.as_str(),
            FRONT_MATTER_KEY | GLOBAL_CONSTRAINTS_KEY | GLOBAL_PREAMBLE_KEY
        )
    }) {
        return new_lineage(
            changed_keys,
            PlanLineageResetReason::SharedMaterialChanged,
            cohort,
        );
    }
    let Some(localized_change) = authorization.localized_change.as_ref() else {
        return new_lineage(
            changed_keys,
            PlanLineageResetReason::MissingAuthorization,
            cohort,
        );
    };
    if localized_change.material_binding_sha256 != current.binding_sha256 {
        return new_lineage(
            changed_keys,
            PlanLineageResetReason::SelectorMismatch,
            cohort,
        );
    }

    let mut covered_keys = BTreeSet::new();
    let mut corrective_reviewer_node_ids = BTreeSet::new();
    for reviewer in localized_change.reviewers.values() {
        if !reviewer.is_passing || reviewer.selector.intersects(&changed_keys) {
            corrective_reviewer_node_ids.insert(reviewer.node_id.clone());
            match reviewer.selector.selected_keys(current) {
                Ok(keys) => covered_keys.extend(keys),
                Err(_) => {
                    return new_lineage(
                        changed_keys,
                        PlanLineageResetReason::SelectorMismatch,
                        cohort,
                    )
                }
            }
        }
    }
    if !changed_keys.is_subset(&covered_keys) {
        return new_lineage(
            changed_keys,
            PlanLineageResetReason::UncoveredChange,
            cohort,
        );
    }

    PlanChangeClassification::Localized {
        change: PlanLocalizedChangeV2 {
            schema: PLAN_LOCALIZED_CHANGE_SCHEMA_V2.into(),
            prior_plan_digest: prior.material.plan_sha256.clone(),
            current_plan_digest: current.material.plan_sha256.clone(),
            changed_keys,
            classifier_version: PLAN_LOCALIZED_CHANGE_CLASSIFIER_VERSION.into(),
            authorization_id: localized_change.authorization_id.clone(),
        },
        corrective_reviewer_node_ids,
    }
}

pub fn select_corrective_reviewers(change: &PlanChangeClassification) -> BTreeSet<String> {
    match change {
        PlanChangeClassification::Localized {
            corrective_reviewer_node_ids,
            ..
        } => corrective_reviewer_node_ids.clone(),
        PlanChangeClassification::NewLineage {
            reviewer_cohort_node_ids,
            ..
        } => reviewer_cohort_node_ids.clone(),
    }
}

impl PlanMaterialMap {
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.materials.keys()
    }

    pub fn body(&self, key: &str) -> Option<&str> {
        self.materials
            .get(key)
            .map(|entry| entry.normalized_body.as_str())
    }

    pub fn task_identity(
        &self,
        task_index: u32,
    ) -> Result<TaskSpecificationIdentityV1, PlanMaterialError> {
        let key = format!("task.{task_index}");
        let entry = self.materials.get(&key).ok_or_else(|| {
            PlanMaterialError::new(
                PlanMaterialErrorKind::MissingTask(task_index),
                format!("missing {key}"),
            )
        })?;
        Ok(TaskSpecificationIdentityV1 {
            schema: PLAN_MATERIAL_SCHEMA_V1.into(),
            task_index,
            body_sha256: entry.body_sha256.clone(),
        })
    }

    pub fn with_manifest_policies(
        &self,
        manifest: &NormalizedManifest,
    ) -> Result<BoundPlanMaterialMap, PlanMaterialError> {
        bind_plan_material(manifest, self)
    }

    fn replace_policies_material(&mut self, bytes: &[u8]) -> Result<(), PlanMaterialError> {
        let body = std::str::from_utf8(bytes).map_err(|_| {
            PlanMaterialError::new(
                PlanMaterialErrorKind::InvalidPoliciesMaterial,
                "policy material is not valid UTF-8",
            )
        })?;
        let entry = material_entry(POLICIES_FINGERPRINT_KEY, body)?;
        self.materials
            .insert(POLICIES_FINGERPRINT_KEY.into(), entry);
        Ok(())
    }

    pub fn golden_projection(&self) -> PlanMaterialGoldenProjectionV1 {
        let task_identities = self
            .referenced_task_indices
            .iter()
            .filter_map(|task_index| {
                let identity = self.task_identity(*task_index).ok()?;
                Some((
                    task_index.to_string(),
                    TaskIdentityGoldenProjectionV1 {
                        identity_sha256: identity.identity_sha256(),
                        identity,
                    },
                ))
            })
            .collect();
        PlanMaterialGoldenProjectionV1 {
            schema: self.schema.clone(),
            materials: self.materials.clone(),
            task_identities,
        }
    }
}

impl BoundPlanMaterialMap {
    pub fn material(&self) -> &PlanMaterialMap {
        &self.material
    }

    pub fn body(&self, key: &str) -> Option<&str> {
        self.material.body(key)
    }

    pub fn body_sha256(&self, key: &str) -> Option<&str> {
        self.material
            .materials
            .get(key)
            .map(|entry| entry.body_sha256.as_str())
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.material.keys()
    }

    pub fn plan_sha256(&self) -> &str {
        &self.material.plan_sha256
    }
}

impl PlanPublicationMaterialDecisionV1 {
    pub fn prior_material(&self) -> &BoundPlanMaterialMap {
        &self.prior_material
    }

    pub fn current_material(&self) -> &BoundPlanMaterialMap {
        &self.current_material
    }

    pub fn changed_keys(&self) -> &BTreeSet<String> {
        &self.changed_keys
    }

    pub fn selector_sets_changed(&self) -> bool {
        self.selector_sets_changed
    }

    pub fn requires_new_lineage(&self) -> bool {
        self.selector_sets_changed || !self.changed_keys.is_empty()
    }
}

impl MaterialSelectorV1 {
    fn from_authorized_keys(material: &BoundPlanMaterialMap, keys: BTreeSet<String>) -> Self {
        Self {
            kind: MaterialSelectorKindV1::Keys { keys },
            material_binding_sha256: material.binding_sha256.clone(),
        }
    }

    pub fn selected_keys(
        &self,
        material: &BoundPlanMaterialMap,
    ) -> Result<BTreeSet<String>, PlanMaterialError> {
        validate_bound_material(material)?;
        if self.material_binding_sha256 != material.binding_sha256 {
            return Err(PlanMaterialError::new(
                PlanMaterialErrorKind::ReviewerCohortMismatch,
                "selector was not derived for the current bound Plan material",
            ));
        }
        match &self.kind {
            MaterialSelectorKindV1::All => {
                Ok(material.material.materials.keys().cloned().collect())
            }
            MaterialSelectorKindV1::Keys { keys } => {
                let mut selected = BTreeSet::new();
                for key in keys {
                    if key == "plan.global_*" {
                        selected.insert(GLOBAL_CONSTRAINTS_KEY.to_owned());
                        selected.insert(GLOBAL_PREAMBLE_KEY.to_owned());
                    } else if material.material.materials.contains_key(key) {
                        selected.insert(key.clone());
                    } else {
                        return Err(PlanMaterialError::new(
                            PlanMaterialErrorKind::SelectorKeyMissing(key.clone()),
                            format!("selector references missing material key {key}"),
                        ));
                    }
                }
                Ok(selected)
            }
        }
    }

    pub fn intersects(&self, changed_keys: &BTreeSet<String>) -> bool {
        match &self.kind {
            MaterialSelectorKindV1::All => !changed_keys.is_empty(),
            MaterialSelectorKindV1::Keys { keys } => {
                !keys.is_disjoint(changed_keys)
                    || (keys.contains("plan.global_*")
                        && (changed_keys.contains(GLOBAL_CONSTRAINTS_KEY)
                            || changed_keys.contains(GLOBAL_PREAMBLE_KEY)))
            }
        }
    }

    pub fn subject_still_current(&self, change: &PlanChangeClassification) -> bool {
        matches!(change, PlanChangeClassification::Localized { .. })
            && !self.intersects(change.changed_keys())
    }

    pub fn is_holistic_full_cohort(&self) -> bool {
        matches!(self.kind, MaterialSelectorKindV1::All)
    }
}

impl TaskSpecificationIdentityV1 {
    pub fn canonical_json(&self) -> String {
        serde_json::to_string(self).expect("Task specification identity must serialize")
    }

    pub fn identity_sha256(&self) -> String {
        sha256_prefixed(self.canonical_json().as_bytes())
    }
}

impl PlanChangeClassification {
    pub fn changed_keys(&self) -> &BTreeSet<String> {
        match self {
            Self::Localized { change, .. } => &change.changed_keys,
            Self::NewLineage { changed_keys, .. } => changed_keys,
        }
    }
}

fn collect_headings_and_front_matter(source: &str) -> (Vec<HeadingSpan>, Option<Range<usize>>) {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);

    let mut headings = Vec::new();
    let mut front_matter = None;
    let mut active_heading: Option<ActiveHeading> = None;
    let mut container_depth = 0usize;
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        match event {
            Event::Start(
                Tag::BlockQuote(_)
                | Tag::List(_)
                | Tag::Item
                | Tag::FootnoteDefinition(_)
                | Tag::Table(_),
            ) => container_depth += 1,
            Event::Start(Tag::MetadataBlock(MetadataBlockKind::YamlStyle))
                if container_depth == 0 && front_matter.is_none() =>
            {
                front_matter = Some(range);
            }
            Event::Start(Tag::Heading { level, .. }) => {
                active_heading = Some(ActiveHeading {
                    level: heading_level(level),
                    range,
                    text: String::new(),
                });
            }
            Event::Text(text) | Event::Code(text) if active_heading.is_some() => {
                active_heading
                    .as_mut()
                    .expect("checked above")
                    .text
                    .push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak if active_heading.is_some() => {
                active_heading
                    .as_mut()
                    .expect("checked above")
                    .text
                    .push(' ');
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(active) = active_heading.take() {
                    let line_start = source[..active.range.start]
                        .rfind('\n')
                        .map(|index| index + 1)
                        .unwrap_or(0);
                    headings.push(HeadingSpan {
                        level: active.level,
                        line_start,
                        body_start: line_after(source, active.range.end.saturating_sub(1)),
                        is_atx: is_atx_heading_line(
                            &source[active.range.start..line_after(source, active.range.start)],
                            active.level,
                        ),
                        text: active.text.trim().to_owned(),
                    });
                }
            }
            Event::End(
                TagEnd::BlockQuote(_)
                | TagEnd::List(_)
                | TagEnd::Item
                | TagEnd::FootnoteDefinition
                | TagEnd::Table,
            ) => container_depth = container_depth.saturating_sub(1),
            _ => {}
        }
    }
    (headings, front_matter)
}

fn normalize_source_for_parsing(source: &str) -> String {
    source
        .strip_prefix('\u{feff}')
        .unwrap_or(source)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .nfc()
        .collect()
}

fn normalize_material_body(body: &str) -> String {
    let body = normalize_source_for_parsing(body);
    let mut lines = body
        .split('\n')
        .map(|line| line.trim_end_matches([' ', '\t']).to_owned())
        .collect::<Vec<_>>();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if lines.is_empty() {
        return "\n".into();
    }
    let mut normalized = lines.join("\n");
    normalized.push('\n');
    normalized
}

fn validate_referenced_tasks(tasks: &[u32]) -> Result<BTreeSet<u32>, PlanMaterialError> {
    let mut unique = BTreeSet::new();
    for task_index in tasks {
        if *task_index == 0 {
            return Err(PlanMaterialError::new(
                PlanMaterialErrorKind::InvalidReferencedTask(*task_index),
                "referenced Task index must be 1-based",
            ));
        }
        if !unique.insert(*task_index) {
            return Err(PlanMaterialError::new(
                PlanMaterialErrorKind::DuplicateReference(*task_index),
                format!("duplicate referenced Task {task_index}"),
            ));
        }
    }
    Ok(unique)
}

fn manifest_task_indices(
    manifest: &NormalizedManifest,
) -> Result<BTreeSet<u32>, PlanMaterialError> {
    active_plan_material_task_indices(manifest).map_err(|error| {
        PlanMaterialError::new(
            PlanMaterialErrorKind::ManifestTaskSetMismatch,
            format!("invalid manifest Task policy set: {error}"),
        )
    })
}

fn canonical_policy_material(manifest: &NormalizedManifest) -> Result<Vec<u8>, PlanMaterialError> {
    let mut policies = manifest.task_policies.clone();
    policies.sort_by_key(|policy| policy.task_index);
    for policy in &mut policies {
        policy.route.reviewer_node_ids.sort();
        for trigger in &mut policy.risk.hard_triggers {
            trigger.evidence.sort();
        }
        for signal in &mut policy.risk.soft_signals {
            signal.evidence.sort();
        }
        policy
            .risk
            .hard_triggers
            .sort_by_key(|trigger| serde_json::to_string(&trigger.kind).unwrap_or_default());
        policy
            .risk
            .soft_signals
            .sort_by_key(|signal| serde_json::to_string(&signal.kind).unwrap_or_default());
    }
    serde_json::to_vec(&policies).map_err(|error| {
        PlanMaterialError::new(
            PlanMaterialErrorKind::InvalidPoliciesMaterial,
            format!("serialize manifest task policies: {error}"),
        )
    })
}

fn validate_manifest_binding(
    manifest: &NormalizedManifest,
    material: &BoundPlanMaterialMap,
) -> Result<(), PlanMaterialError> {
    if manifest_task_indices(manifest)? != material.material.referenced_task_indices {
        return Err(PlanMaterialError::new(
            PlanMaterialErrorKind::ManifestTaskSetMismatch,
            "manifest task policies do not match bound Plan references",
        ));
    }
    let expected = material_entry(
        POLICIES_FINGERPRINT_KEY,
        &String::from_utf8(canonical_policy_material(manifest)?).map_err(|_| {
            PlanMaterialError::new(
                PlanMaterialErrorKind::InvalidPoliciesMaterial,
                "canonical manifest policies are not valid UTF-8",
            )
        })?,
    )?;
    if material.material.materials.get(POLICIES_FINGERPRINT_KEY) != Some(&expected) {
        return Err(PlanMaterialError::new(
            PlanMaterialErrorKind::InvalidPoliciesMaterial,
            "bound policy fingerprint does not match the manifest",
        ));
    }
    Ok(())
}

fn plan_reviewer_cohort_node_ids(
    manifest: &NormalizedManifest,
) -> Result<BTreeSet<String>, PlanMaterialError> {
    plan_reviewer_gate_node_ids(manifest).map(|(cohort, _)| cohort)
}

fn plan_reviewer_gate_node_ids(
    manifest: &NormalizedManifest,
) -> Result<(BTreeSet<String>, BTreeSet<String>), PlanMaterialError> {
    let mut plan_gates = manifest
        .gates
        .iter()
        .filter(|gate| gate.gate_kind == DocumentGateKind::Plan);
    let gate = plan_gates.next().ok_or_else(|| {
        PlanMaterialError::new(PlanMaterialErrorKind::MissingPlanGate, "missing Plan gate")
    })?;
    if plan_gates.next().is_some() {
        return Err(PlanMaterialError::new(
            PlanMaterialErrorKind::AmbiguousPlanGate,
            "multiple Plan gates are not valid material authority",
        ));
    }
    let cohort = gate
        .reviewer_cohort_node_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if cohort.is_empty() || cohort.len() != gate.reviewer_cohort_node_ids.len() {
        return Err(PlanMaterialError::new(
            PlanMaterialErrorKind::ReviewerCohortMismatch,
            "Plan reviewer cohort is empty or contains duplicates",
        ));
    }
    let required = gate
        .required_reviewer_node_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if required.is_empty()
        || required.len() != gate.required_reviewer_node_ids.len()
        || !required.is_subset(&cohort)
    {
        return Err(PlanMaterialError::new(
            PlanMaterialErrorKind::ReviewerCohortMismatch,
            "required Plan reviewers are empty, duplicated, or outside the cohort",
        ));
    }
    Ok((cohort, required))
}

fn derive_plan_reviewer_selectors(
    manifest: &NormalizedManifest,
    material: &BoundPlanMaterialMap,
) -> Result<BTreeMap<String, MaterialSelectorV1>, PlanMaterialError> {
    plan_reviewer_cohort_node_ids(manifest)?
        .into_iter()
        .map(|node_id| {
            derive_plan_reviewer_selector(manifest, &node_id, material)
                .map(|selector| (node_id, selector))
        })
        .collect()
}

fn selector_kinds(
    selectors: &BTreeMap<String, MaterialSelectorV1>,
) -> BTreeMap<&str, &MaterialSelectorKindV1> {
    selectors
        .iter()
        .map(|(node_id, selector)| (node_id.as_str(), &selector.kind))
        .collect()
}

fn task_heading_index(text: &str) -> Result<Option<u32>, PlanMaterialError> {
    let Some(captures) = task_heading_regex().captures(text) else {
        return Ok(None);
    };
    captures[1].parse::<u32>().map(Some).map_err(|_| {
        PlanMaterialError::new(
            PlanMaterialErrorKind::InvalidTaskHeading,
            "Task heading index does not fit u32",
        )
    })
}

fn task_heading_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"^Task\s+([0-9]+)\b").expect("valid Task heading regex"))
}

fn global_constraints_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)^Global Constraints\b").expect("valid Global Constraints regex")
    })
}

fn is_atx_heading_line(line: &str, level: u8) -> bool {
    let line = line.trim_end_matches(['\r', '\n']);
    let leading_spaces = line.bytes().take_while(|byte| *byte == b' ').count();
    if leading_spaces > 3 {
        return false;
    }
    let content = &line[leading_spaces..];
    let hashes = content.bytes().take_while(|byte| *byte == b'#').count();
    hashes == usize::from(level)
        && content
            .as_bytes()
            .get(hashes)
            .is_none_or(|byte| matches!(byte, b' ' | b'\t'))
}

fn line_after(source: &str, start: usize) -> usize {
    source[start..]
        .find('\n')
        .map(|relative| start + relative + 1)
        .unwrap_or(source.len())
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn insert_material(
    materials: &mut BTreeMap<String, PlanMaterialEntryV1>,
    key: &str,
    body: &str,
) -> Result<(), PlanMaterialError> {
    materials.insert(key.into(), material_entry(key, body)?);
    Ok(())
}

fn insert_material_bytes(
    materials: &mut BTreeMap<String, PlanMaterialEntryV1>,
    key: &str,
    bytes: &[u8],
) -> Result<(), PlanMaterialError> {
    let body = std::str::from_utf8(bytes).map_err(|_| {
        PlanMaterialError::new(
            PlanMaterialErrorKind::InvalidPoliciesMaterial,
            format!("{key} is not valid UTF-8"),
        )
    })?;
    insert_material(materials, key, body)
}

fn material_entry(key: &str, body: &str) -> Result<PlanMaterialEntryV1, PlanMaterialError> {
    let normalized_body = normalize_material_body(body);
    if normalized_body.len() > MAX_PLAN_SECTION_BYTES {
        return Err(PlanMaterialError::new(
            PlanMaterialErrorKind::SectionTooLarge(key.into()),
            format!("material section {key} exceeds {MAX_PLAN_SECTION_BYTES} bytes"),
        ));
    }
    Ok(PlanMaterialEntryV1 {
        body_sha256: sha256_prefixed(normalized_body.as_bytes()),
        normalized_body,
    })
}

fn validate_material_map(material: &PlanMaterialMap) -> Result<(), PlanMaterialError> {
    let invalid = |message: String| {
        PlanMaterialError::new(PlanMaterialErrorKind::InvalidMaterialMap, message)
    };
    if material.schema != PLAN_MATERIAL_SCHEMA_V1 {
        return Err(invalid("invalid Plan material schema".into()));
    }
    if material.source_bytes.len() > MAX_PLAN_MATERIAL_BYTES
        || !is_sha256_digest(&material.plan_sha256)
        || sha256_prefixed(&material.source_bytes) != material.plan_sha256
    {
        return Err(invalid("invalid full Plan digest".into()));
    }
    if material.referenced_task_indices.len() > MAX_TASKS
        || material
            .referenced_task_indices
            .iter()
            .any(|task_index| *task_index == 0)
    {
        return Err(invalid("invalid referenced Task set".into()));
    }
    for key in SHARED_KEYS {
        if !material.materials.contains_key(key) {
            return Err(invalid(format!("missing mandatory material key {key}")));
        }
    }

    let mut parsed_task_indices = BTreeSet::new();
    for (key, entry) in &material.materials {
        if !SHARED_KEYS.contains(&key.as_str()) {
            let Some(index) = key
                .strip_prefix("task.")
                .and_then(|value| value.parse().ok())
            else {
                return Err(invalid(format!("invalid material key {key}")));
            };
            if index == 0 || key != &format!("task.{index}") || !parsed_task_indices.insert(index) {
                return Err(invalid(format!("invalid Task material key {key}")));
            }
        }
        if entry.normalized_body.len() > MAX_PLAN_SECTION_BYTES
            || normalize_material_body(&entry.normalized_body) != entry.normalized_body
            || sha256_prefixed(entry.normalized_body.as_bytes()) != entry.body_sha256
        {
            return Err(invalid(format!("invalid material entry {key}")));
        }
    }
    if parsed_task_indices.len() > MAX_TASKS
        || !material
            .referenced_task_indices
            .is_subset(&parsed_task_indices)
    {
        return Err(invalid(
            "material Task set does not cover references".into(),
        ));
    }
    Ok(())
}

fn validate_bound_material(material: &BoundPlanMaterialMap) -> Result<(), PlanMaterialError> {
    validate_material_map(&material.material)?;
    if material_binding_digest(&material.material)? != material.binding_sha256 {
        return Err(PlanMaterialError::new(
            PlanMaterialErrorKind::InvalidMaterialMap,
            "bound Plan material identity does not match its contents",
        ));
    }
    Ok(())
}

fn material_binding_digest(material: &PlanMaterialMap) -> Result<String, PlanMaterialError> {
    let bytes = serde_json::to_vec(&(material.plan_sha256.as_str(), material_hashes(material)))
        .map_err(|error| {
            PlanMaterialError::new(
                PlanMaterialErrorKind::InvalidMaterialMap,
                format!("serialize Plan material binding: {error}"),
            )
        })?;
    Ok(sha256_prefixed(&bytes))
}

fn is_sha256_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value.as_bytes()[7..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn material_hashes(material: &PlanMaterialMap) -> BTreeMap<&str, &str> {
    material
        .materials
        .iter()
        .map(|(key, entry)| (key.as_str(), entry.body_sha256.as_str()))
        .collect()
}

fn changed_material_keys(prior: &PlanMaterialMap, current: &PlanMaterialMap) -> BTreeSet<String> {
    prior
        .materials
        .keys()
        .chain(current.materials.keys())
        .filter(|key| {
            prior.materials.get(*key).map(|entry| &entry.body_sha256)
                != current.materials.get(*key).map(|entry| &entry.body_sha256)
        })
        .cloned()
        .collect()
}

fn new_lineage(
    changed_keys: BTreeSet<String>,
    reason: PlanLineageResetReason,
    reviewer_cohort_node_ids: BTreeSet<String>,
) -> PlanChangeClassification {
    PlanChangeClassification::NewLineage {
        changed_keys,
        reason,
        reviewer_cohort_node_ids,
    }
}

fn sha256_prefixed(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity("sha256:".len() + digest.len() * 2);
    output.push_str("sha256:");
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    use super::*;
    use crate::acp::delegation::workflow::types::{
        DocumentGateKind, ManifestNodeKind, ManifestNodeRole, ManifestTaskPolicy, ManifestTaskRisk,
        ManifestTaskRoute, ManifestWorkflowState, NormalizedGate, NormalizedManifest,
        NormalizedNode, ResolutionMode, TaskRiskLevel, MANIFEST_SCHEMA_VERSION,
        TASK_RISK_POLICY_VERSION, WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
    };

    const BASIC: &[u8] = include_bytes!("fixtures/plan_material/basic.md");
    const DUPLICATE_TASK: &[u8] = include_bytes!("fixtures/plan_material/duplicate-task.md");
    const NORMALIZATION: &[u8] = include_bytes!("fixtures/plan_material/normalization.md");

    #[derive(Debug, Deserialize)]
    struct PlanMaterialVectors {
        schema: String,
        cases: Vec<PlanMaterialVector>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum GoldenMaterialSelector {
        All,
        Keys { keys: BTreeSet<String> },
    }

    #[derive(Debug, Deserialize)]
    struct PlanMaterialVector {
        file: String,
        referenced_tasks: Vec<u32>,
        keys: Vec<String>,
        selector: Option<GoldenMaterialSelector>,
        selected_keys: Vec<String>,
        expected_projection: Option<Value>,
        expected_error: Option<String>,
    }

    impl PlanMaterialVector {
        fn bytes(&self) -> &'static [u8] {
            match self.file.as_str() {
                "basic.md" => BASIC,
                "duplicate-task.md" => DUPLICATE_TASK,
                "normalization.md" => NORMALIZATION,
                other => panic!("unknown Plan material fixture {other}"),
            }
        }
    }

    fn plan_material_vectors() -> PlanMaterialVectors {
        serde_json::from_str(include_str!("fixtures/plan_material_vectors.json")).unwrap()
    }

    #[test]
    fn plan_material_vectors_select_literal_commonmark_spans() {
        let vectors = plan_material_vectors();
        assert_eq!(vectors.schema, "PlanMaterialVectorsV1");

        for case in vectors.cases {
            let result = parse_plan_material(case.bytes(), &case.referenced_tasks);
            match case.expected_error.as_deref() {
                None => {
                    let material = result.unwrap();
                    assert_eq!(
                        material.keys().cloned().collect::<Vec<_>>(),
                        case.keys,
                        "key projection for {}",
                        case.file
                    );
                    assert_eq!(
                        serde_json::to_value(material.golden_projection()).unwrap(),
                        case.expected_projection.unwrap(),
                        "golden projection for {}",
                        case.file
                    );
                    let bound = bind_fixture_policies(material.clone());
                    let expected_selector = case.selector.unwrap();
                    let selector = match &expected_selector {
                        GoldenMaterialSelector::All => derive_holistic_full_cohort_selector(&bound),
                        GoldenMaterialSelector::Keys { keys } => {
                            MaterialSelectorV1::from_authorized_keys(&bound, keys.clone())
                        }
                    };
                    assert_eq!(
                        serde_json::to_value(&selector).unwrap(),
                        serde_json::to_value(expected_selector).unwrap(),
                        "selector projection for {}",
                        case.file
                    );
                    assert_eq!(
                        selector.selected_keys(&bound).unwrap(),
                        case.selected_keys.into_iter().collect(),
                        "selector projection for {}",
                        case.file
                    );
                }
                Some(code) => assert_eq!(result.unwrap_err().code(), code),
            }
        }
    }

    #[test]
    fn plan_material_bounds_and_missing_references_fail_closed() {
        assert_eq!(
            parse_plan_material(&vec![b'x'; 2 * 1024 * 1024 + 1], &[1])
                .unwrap_err()
                .code(),
            "completion_plan_material_invalid"
        );
        assert_eq!(
            parse_plan_material(b"## Task 1\nbody\n", &[2])
                .unwrap_err()
                .kind(),
            PlanMaterialErrorKind::MissingTask(2)
        );
        assert!(parse_plan_material(plan_with_101_tasks().as_bytes(), &[1]).is_err());
        assert!(
            parse_plan_material(plan_with_section_bytes(512 * 1024 + 1).as_bytes(), &[1]).is_err()
        );
        assert!(parse_plan_material(b"## Task 1\n\xff\n", &[1]).is_err());
        assert!(parse_plan_material(b"## Task 1\nbody\n", &[1, 1]).is_err());
    }

    #[test]
    fn task_grammar_uses_atx_h2_h3_and_literal_boundaries() {
        let source = b"\
### Task 1: nested\n\
body one\n\
#### lower heading\n\
```markdown\n\
## Task 99\n\
```\n\
## Task 2\n\
body two\n\
";
        let material = parse_plan_material(source, &[1, 2]).unwrap();
        assert_eq!(
            material.body("task.1").unwrap(),
            "body one\n#### lower heading\n```markdown\n## Task 99\n```\n"
        );
        assert_eq!(material.body("task.2").unwrap(), "body two\n");

        for rejected in [
            b"## task 1\nbody\n".as_slice(),
            "## Task １\nbody\n".as_bytes(),
            b"Task 1\n------\nbody\n".as_slice(),
            b"#### Task 1\nbody\n".as_slice(),
        ] {
            assert_eq!(
                parse_plan_material(rejected, &[1]).unwrap_err().kind(),
                PlanMaterialErrorKind::MissingTask(1)
            );
        }
        assert_eq!(
            parse_plan_material(DUPLICATE_TASK, &[1])
                .unwrap_err()
                .kind(),
            PlanMaterialErrorKind::DuplicateTask(1)
        );
    }

    #[test]
    fn task_grammar_accepts_nested_commonmark_atx_headings() {
        let quoted = parse_plan_material(b"> ## Task 1\n> quoted body\n", &[1]).unwrap();
        assert_eq!(quoted.body("task.1"), Some("> quoted body\n"));

        let listed = parse_plan_material(b"- ### Task 1\n\n  listed body\n", &[1]).unwrap();
        assert_eq!(listed.body("task.1"), Some("\n  listed body\n"));
    }

    #[test]
    fn normalization_removes_bom_line_endings_trailing_space_and_composes_nfc() {
        let source = b"\xef\xbb\xbf## Task 1\r\nCafe\xcc\x81  \rnext\t\r\n\r\n";
        let material = parse_plan_material(source, &[1]).unwrap();
        assert_eq!(material.body("task.1").unwrap(), "Caf\u{e9}\nnext\n");
        assert_eq!(material.body("plan.front_matter").unwrap(), "\n");
        assert!(material
            .materials
            .values()
            .all(|entry| entry.normalized_body.ends_with('\n')));
    }

    #[test]
    fn global_constraints_setext_body_starts_after_the_full_heading_span() {
        let material = parse_plan_material(
            b"Global Constraints\n------------------\npolicy\n\n## Task 1\nbody\n",
            &[1],
        )
        .unwrap();
        assert_eq!(
            material.body("plan.global_constraints").unwrap(),
            "policy\n"
        );
    }

    #[test]
    fn task_identity_uses_exact_canonical_schema_json() {
        let material = parse_plan_material(b"## Task 1\nbody\n", &[1]).unwrap();
        let identity = material.task_identity(1).unwrap();
        assert_eq!(
            identity.canonical_json(),
            format!(
                "{{\"schema\":\"PlanMaterialSchemaV1\",\"task_index\":1,\"body_sha256\":\"{}\"}}",
                material.materials["task.1"].body_sha256
            )
        );
        assert_eq!(
            identity.identity_sha256(),
            "sha256:2aae24d4a7d77e244cfb39d678620c31dfa5fc4d9db64797024454609ecef9d2"
        );
    }

    #[test]
    fn reviewer_selectors_derive_from_durable_agent_profile_routes() {
        let material = parse_plan_material(BASIC, &[1, 2]).unwrap();
        let manifest = selector_manifest();
        let bound = bind_plan_material(&manifest, &material).unwrap();
        let selector =
            derive_plan_reviewer_selector(&manifest, "plan-reviewer-codex", &bound).unwrap();
        assert_eq!(
            selector,
            MaterialSelectorV1::from_authorized_keys(
                &bound,
                [
                    "plan.front_matter",
                    "plan.global_constraints",
                    "plan.global_preamble",
                    "plan.policies_fingerprint",
                    "task.1",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            )
        );

        let mut missing_route = manifest.clone();
        missing_route.task_policies[0]
            .route
            .reviewer_node_ids
            .push("missing-reviewer".into());
        let missing_route_bound = bind_plan_material(&missing_route, &material).unwrap();
        assert_eq!(
            derive_plan_reviewer_selector(
                &missing_route,
                "plan-reviewer-codex",
                &missing_route_bound,
            )
            .unwrap_err()
            .kind(),
            PlanMaterialErrorKind::MissingRouteNode("missing-reviewer".into())
        );

        let mut ambiguous = manifest.clone();
        let mut duplicate_identity = ambiguous
            .nodes
            .iter()
            .find(|node| node.id == "plan-reviewer-codex")
            .unwrap()
            .clone();
        duplicate_identity.id = "plan-reviewer-codex-duplicate".into();
        ambiguous.nodes.push(duplicate_identity);
        assert!(derive_plan_reviewer_selector(&ambiguous, "plan-reviewer-codex", &bound).is_err());

        let material_without_task_2 = parse_plan_material(b"## Task 1\nbody\n", &[1]).unwrap();
        assert!(bind_plan_material(&manifest, &material_without_task_2).is_err());
    }

    #[test]
    fn holistic_all_selector_requires_bound_server_provenance() {
        let manifest = selector_manifest();
        let material = parse_plan_material(BASIC, &[1, 2]).unwrap();
        let bound = bind_plan_material(&manifest, &material).unwrap();
        let selector = derive_holistic_full_cohort_selector(&bound);
        assert!(selector.is_holistic_full_cohort());
        assert_eq!(
            selector.selected_keys(&bound).unwrap(),
            bound.keys().cloned().collect()
        );

        let changed = bind_plan_material(
            &manifest,
            &parse_plan_material(b"## Task 1\none changed\n\n## Task 2\ntwo\n", &[1, 2]).unwrap(),
        )
        .unwrap();
        assert_eq!(
            selector.selected_keys(&changed).unwrap_err().kind(),
            PlanMaterialErrorKind::ReviewerCohortMismatch
        );
    }

    #[test]
    fn manifest_policy_fingerprint_changes_for_risk_and_route_edits() {
        let material = parse_plan_material(BASIC, &[1, 2]).unwrap();
        let manifest = selector_manifest();
        let bound = bind_plan_material(&manifest, &material).unwrap();

        let mut route_edit = manifest.clone();
        route_edit.task_policies[0].route.reviewer_node_ids[1] = "task-2-grok".into();
        let route_bound = bind_plan_material(&route_edit, &material).unwrap();
        assert_ne!(
            bound.body_sha256("plan.policies_fingerprint"),
            route_bound.body_sha256("plan.policies_fingerprint")
        );

        let mut risk_edit = manifest;
        risk_edit.task_policies[0].risk.reason = "changed reason".into();
        let risk_bound = bind_plan_material(&risk_edit, &material).unwrap();
        assert_ne!(
            bound.body_sha256("plan.policies_fingerprint"),
            risk_bound.body_sha256("plan.policies_fingerprint")
        );
    }

    #[test]
    fn manifest_binding_rejects_corrupt_material_invariants() {
        let manifest = selector_manifest();
        let parsed = parse_plan_material(BASIC, &[1, 2]).unwrap();
        let bound = bind_plan_material(&manifest, &parsed).unwrap();
        assert_eq!(bound.body("task.1"), parsed.body("task.1"));

        let mut corrupt = parsed.clone();
        corrupt.schema = "PlanMaterialSchemaV0".into();
        assert!(bind_plan_material(&manifest, &corrupt).is_err());

        let mut corrupt = parsed.clone();
        corrupt.materials.get_mut("task.1").unwrap().body_sha256 =
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".into();
        assert!(bind_plan_material(&manifest, &corrupt).is_err());

        let mut corrupt = parsed.clone();
        corrupt
            .materials
            .get_mut("task.1")
            .unwrap()
            .normalized_body
            .push(' ');
        assert!(bind_plan_material(&manifest, &corrupt).is_err());

        let mut corrupt = parsed.clone();
        corrupt.materials.remove("plan.global_preamble");
        assert!(bind_plan_material(&manifest, &corrupt).is_err());

        let mut corrupt = parsed.clone();
        corrupt.referenced_task_indices.insert(3);
        assert!(bind_plan_material(&manifest, &corrupt).is_err());

        let mut corrupt = parsed;
        corrupt.plan_sha256 = "sha256:not-a-digest".into();
        assert!(bind_plan_material(&manifest, &corrupt).is_err());
    }

    #[test]
    fn localized_authorization_and_selection_share_one_validated_cohort() {
        let manifest = selector_manifest();
        let prior = bind_plan_material(
            &manifest,
            &parse_plan_material(b"## Task 1\none\n\n## Task 2\ntwo\n", &[1, 2]).unwrap(),
        )
        .unwrap();
        let current = bind_plan_material(
            &manifest,
            &parse_plan_material(b"## Task 1\none\n\n## Task 2\ntwo edited\n", &[1, 2]).unwrap(),
        )
        .unwrap();
        let reviewer_states = BTreeMap::from([
            ("plan-reviewer-codex".to_string(), false),
            ("plan-reviewer-grok".to_string(), true),
        ]);
        let authorization = authorize_localized_plan_change(
            &manifest,
            &current,
            "authorization-1",
            &reviewer_states,
        )
        .unwrap();
        let change = classify_plan_change(
            &PlanMaterialChangeInputV1::parsed(prior.clone()),
            &PlanMaterialChangeInputV1::parsed(current.clone()),
            &authorization,
        );
        assert!(matches!(change, PlanChangeClassification::Localized { .. }));
        assert_eq!(
            select_corrective_reviewers(&change),
            BTreeSet::from([
                "plan-reviewer-codex".to_string(),
                "plan-reviewer-grok".to_string(),
            ])
        );
        let authorized_reviewers = &authorization.localized_change.as_ref().unwrap().reviewers;
        assert!(authorized_reviewers["plan-reviewer-codex"]
            .selector
            .subject_still_current(&change));
        assert!(!authorized_reviewers["plan-reviewer-grok"]
            .selector
            .subject_still_current(&change));

        let stale_authorization =
            authorize_localized_plan_change(&manifest, &prior, "authorization-2", &reviewer_states)
                .unwrap();
        assert!(authorize_localized_plan_change(
            &manifest,
            &current,
            "authorization-3",
            &BTreeMap::from([("plan-reviewer-codex".to_string(), false)]),
        )
        .is_err());
        let mismatch = classify_plan_change(
            &PlanMaterialChangeInputV1::parsed(prior),
            &PlanMaterialChangeInputV1::parsed(current),
            &stale_authorization,
        );
        assert!(matches!(
            mismatch,
            PlanChangeClassification::NewLineage {
                reason: PlanLineageResetReason::SelectorMismatch,
                ..
            }
        ));
        assert_eq!(
            select_corrective_reviewers(&mismatch),
            reviewer_states.into_keys().collect()
        );
    }

    #[test]
    fn localized_change_fails_closed_for_shared_policy_unparseable_and_uncovered_edits() {
        let manifest = selector_manifest();
        let plan = b"preamble\n\n## Task 1\none\n\n## Task 2\ntwo\n";
        let prior =
            bind_plan_material(&manifest, &parse_plan_material(plan, &[1, 2]).unwrap()).unwrap();
        let states = BTreeMap::from([
            ("plan-reviewer-codex".to_string(), true),
            ("plan-reviewer-grok".to_string(), true),
        ]);
        let shared = bind_plan_material(
            &manifest,
            &parse_plan_material(
                b"changed preamble\n\n## Task 1\none\n\n## Task 2\ntwo\n",
                &[1, 2],
            )
            .unwrap(),
        )
        .unwrap();
        let shared_authorization =
            authorize_localized_plan_change(&manifest, &shared, "shared-auth", &states).unwrap();
        assert!(matches!(
            classify_plan_change(
                &PlanMaterialChangeInputV1::parsed(prior.clone()),
                &PlanMaterialChangeInputV1::parsed(shared),
                &shared_authorization,
            ),
            PlanChangeClassification::NewLineage {
                reason: PlanLineageResetReason::SharedMaterialChanged,
                ..
            }
        ));

        let unparseable_edit = PlanMaterialChangeInputV1::invalid(
            "sha256:invalid",
            PlanMaterialErrorKind::InvalidUtf8,
        );
        assert!(matches!(
            classify_plan_change(
                &PlanMaterialChangeInputV1::parsed(prior.clone()),
                &unparseable_edit,
                &shared_authorization,
            ),
            PlanChangeClassification::NewLineage {
                reason: PlanLineageResetReason::UnparseableMaterial,
                ..
            }
        ));

        let mut policy_manifest = manifest.clone();
        policy_manifest.task_policies[0].risk.reason = "changed policy".into();
        let policy = bind_plan_material(
            &policy_manifest,
            &parse_plan_material(plan, &[1, 2]).unwrap(),
        )
        .unwrap();
        let policy_authorization =
            authorize_localized_plan_change(&policy_manifest, &policy, "policy-auth", &states)
                .unwrap();
        assert!(matches!(
            classify_plan_change(
                &PlanMaterialChangeInputV1::parsed(prior),
                &PlanMaterialChangeInputV1::parsed(policy),
                &policy_authorization,
            ),
            PlanChangeClassification::NewLineage {
                reason: PlanLineageResetReason::PolicyOrRouteChanged,
                ..
            }
        ));

        let mut uncovered_manifest = manifest;
        for node_id in ["task-2-codex", "task-2-grok"] {
            uncovered_manifest
                .nodes
                .iter_mut()
                .find(|node| node.id == node_id)
                .unwrap()
                .profile_id = Some("unmatched-profile".into());
        }
        let uncovered_prior = bind_plan_material(
            &uncovered_manifest,
            &parse_plan_material(plan, &[1, 2]).unwrap(),
        )
        .unwrap();
        let uncovered_current = bind_plan_material(
            &uncovered_manifest,
            &parse_plan_material(
                b"preamble\n\n## Task 1\none\n\n## Task 2\ntwo edited\n",
                &[1, 2],
            )
            .unwrap(),
        )
        .unwrap();
        let uncovered_authorization = authorize_localized_plan_change(
            &uncovered_manifest,
            &uncovered_current,
            "uncovered-auth",
            &states,
        )
        .unwrap();
        assert!(matches!(
            classify_plan_change(
                &PlanMaterialChangeInputV1::parsed(uncovered_prior),
                &PlanMaterialChangeInputV1::parsed(uncovered_current),
                &uncovered_authorization,
            ),
            PlanChangeClassification::NewLineage {
                reason: PlanLineageResetReason::UncoveredChange,
                ..
            }
        ));
    }

    #[test]
    fn localized_change_requires_platform_authorization() {
        let manifest = selector_manifest();
        let context = localized_plan_change_context(&manifest).unwrap();
        let prior = bind_plan_material(
            &manifest,
            &parse_plan_material(b"## Task 1\none\n\n## Task 2\ntwo\n", &[1, 2]).unwrap(),
        )
        .unwrap();
        let current = bind_plan_material(
            &manifest,
            &parse_plan_material(b"## Task 1\none edited\n\n## Task 2\ntwo\n", &[1, 2]).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            classify_plan_change(
                &PlanMaterialChangeInputV1::parsed(prior),
                &PlanMaterialChangeInputV1::parsed(current),
                &context,
            ),
            PlanChangeClassification::NewLineage {
                reason: PlanLineageResetReason::MissingAuthorization,
                ..
            }
        ));
        assert_eq!(
            select_corrective_reviewers(&classify_plan_change(
                &PlanMaterialChangeInputV1::invalid(
                    "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                    PlanMaterialErrorKind::InvalidUtf8,
                ),
                &PlanMaterialChangeInputV1::invalid(
                    "sha256:1111111111111111111111111111111111111111111111111111111111111111",
                    PlanMaterialErrorKind::InvalidUtf8,
                ),
                &context,
            )),
            BTreeSet::from([
                "plan-reviewer-codex".to_string(),
                "plan-reviewer-grok".to_string(),
            ])
        );
    }

    #[test]
    fn localized_authorization_requires_only_the_current_required_set() {
        let mut manifest = selector_manifest();
        manifest.gates[0].required_reviewer_node_ids = vec!["plan-reviewer-codex".into()];
        let prior = bind_plan_material(
            &manifest,
            &parse_plan_material(b"## Task 1\none\n\n## Task 2\ntwo\n", &[1, 2]).unwrap(),
        )
        .unwrap();
        let current = bind_plan_material(
            &manifest,
            &parse_plan_material(b"## Task 1\none edited\n\n## Task 2\ntwo\n", &[1, 2]).unwrap(),
        )
        .unwrap();
        let authorization = authorize_localized_plan_change(
            &manifest,
            &current,
            "required-subset-auth",
            &BTreeMap::from([("plan-reviewer-codex".to_string(), true)]),
        )
        .unwrap();
        let change = classify_plan_change(
            &PlanMaterialChangeInputV1::parsed(prior),
            &PlanMaterialChangeInputV1::parsed(current),
            &authorization,
        );
        assert!(matches!(change, PlanChangeClassification::Localized { .. }));
        assert_eq!(
            select_corrective_reviewers(&change),
            BTreeSet::from(["plan-reviewer-codex".to_string()])
        );
    }

    #[test]
    fn estimated_publication_resets_lineage_for_any_material_change() {
        let manifest = selector_manifest();
        let prior = bind_plan_material(
            &manifest,
            &parse_plan_material(b"## Task 1\none\n\n## Task 2\ntwo\n", &[1, 2]).unwrap(),
        )
        .unwrap();
        let same = prior.clone();
        let changed = bind_plan_material(
            &manifest,
            &parse_plan_material(b"## Task 1\none\n\n## Task 2\ntwo edited\n", &[1, 2]).unwrap(),
        )
        .unwrap();
        assert!(!plan_publication_requires_new_lineage(&prior, &same));
        assert!(plan_publication_requires_new_lineage(&prior, &changed));
        assert!(
            super::super::store::estimated_plan_publication_material_decision(
                &manifest,
                prior.material(),
                &manifest,
                changed.material(),
            )
            .unwrap()
            .requires_new_lineage()
        );
    }

    fn plan_with_101_tasks() -> String {
        (1..=101)
            .map(|index| format!("## Task {index}\nbody\n"))
            .collect()
    }

    fn plan_with_section_bytes(bytes: usize) -> String {
        format!("## Task 1\n{}", "x".repeat(bytes))
    }

    fn bind_fixture_policies(mut material: PlanMaterialMap) -> BoundPlanMaterialMap {
        material
            .replace_policies_material(b"{\"fixture\":\"bound-policy\"}\n")
            .unwrap();
        validate_material_map(&material).unwrap();
        BoundPlanMaterialMap {
            binding_sha256: material_binding_digest(&material).unwrap(),
            material,
        }
    }

    fn selector_manifest() -> NormalizedManifest {
        let nodes = vec![
            node(
                "plan-reviewer-codex",
                "plan",
                ManifestNodeRole::Reviewer,
                "codex",
                Some("profile-codex"),
                None,
            ),
            node(
                "plan-reviewer-grok",
                "plan",
                ManifestNodeRole::Reviewer,
                "grok",
                Some("profile-grok"),
                None,
            ),
            node(
                "task-1-implementer",
                "tasks",
                ManifestNodeRole::Implementer,
                "codex",
                None,
                Some(1),
            ),
            node(
                "task-1-codex",
                "tasks",
                ManifestNodeRole::Reviewer,
                "codex",
                Some("profile-codex"),
                Some(1),
            ),
            node(
                "task-1-grok",
                "tasks",
                ManifestNodeRole::Reviewer,
                "grok",
                Some("profile-grok"),
                Some(1),
            ),
            node(
                "task-2-implementer",
                "tasks",
                ManifestNodeRole::Implementer,
                "codex",
                None,
                Some(2),
            ),
            node(
                "task-2-codex",
                "tasks",
                ManifestNodeRole::Reviewer,
                "codex",
                Some("other-profile"),
                Some(2),
            ),
            node(
                "task-2-grok",
                "tasks",
                ManifestNodeRole::Reviewer,
                "grok",
                Some("profile-grok"),
                Some(2),
            ),
        ];
        NormalizedManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.into(),
            plan_target_rel_path: "docs/plan.md".into(),
            risk_policy_version: TASK_RISK_POLICY_VERSION.into(),
            workflow_id: Some("workflow-1".into()),
            expected_manifest_revision: Some(1),
            publication_token: "publication-1".into(),
            workflow_state: ManifestWorkflowState::Estimated,
            design: None,
            plan: None,
            phases: Vec::new(),
            nodes,
            edges: Vec::new(),
            gates: vec![NormalizedGate {
                id: "plan-gate".into(),
                reviewer_cohort_node_ids: vec![
                    "plan-reviewer-codex".into(),
                    "plan-reviewer-grok".into(),
                ],
                required_reviewer_node_ids: vec![
                    "plan-reviewer-codex".into(),
                    "plan-reviewer-grok".into(),
                ],
                resolution_mode: ResolutionMode::ParentAdjudication,
                gate_kind: DocumentGateKind::Plan,
            }],
            task_policies: vec![
                task_policy(1, &["task-1-codex", "task-1-grok"]),
                task_policy(2, &["task-2-codex", "task-2-grok"]),
            ],
            task_count: 2,
        }
    }

    fn node(
        id: &str,
        phase_id: &str,
        role: ManifestNodeRole,
        agent_type: &str,
        profile_id: Option<&str>,
        task_index: Option<u32>,
    ) -> NormalizedNode {
        NormalizedNode {
            id: id.into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(phase_id.into()),
            role: Some(role),
            agent_type: Some(agent_type.into()),
            profile_id: profile_id.map(str::to_owned),
            task_index,
            work_unit_key: Some(format!("test|{id}")),
            deps: Vec::new(),
            required: true,
            node_outcome: None,
            title: None,
        }
    }

    fn task_policy(task_index: u32, reviewer_node_ids: &[&str]) -> ManifestTaskPolicy {
        ManifestTaskPolicy {
            task_index,
            risk: ManifestTaskRisk {
                level: TaskRiskLevel::High,
                hard_triggers: Vec::new(),
                soft_signals: Vec::new(),
                score: 0,
                reason: "fixture".into(),
            },
            route: ManifestTaskRoute {
                implementer_node_id: format!("task-{task_index}-implementer"),
                reviewer_node_ids: reviewer_node_ids
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect(),
            },
            allow_noop_verification: false,
        }
    }
}
