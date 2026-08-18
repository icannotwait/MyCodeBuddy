# Brainstorm-to-Delivery Generic Task Agent Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make brainstorm-to-delivery use a user-selectable Task Agent with Grok as the default, independent Codex document producers, and deterministic normal/high Task routing while preserving Simple's non-blocking projection and legacy run readability.

**Architecture:** Extend the existing generic work-unit key grammar and Simple Plan/progress parsers instead of restoring the retired manifest workflow. The Plan owns a bounded `codeg-b2d-routing-v1` block, progress mirrors the selected route, the JavaScript validator fails closed before dispatch, and Rust projects route disagreements only as warnings. The Skill remains the root coordinator but delegates every Design, Plan, Task, and review production unit to an independent child conversation.

**Tech Stack:** Markdown Skill contracts, Node.js ESM and `node:test`, Rust 2021, serde/serde_json, SeaORM-backed delegation runs, and the existing Simple workflow graph DTO.

## Global Constraints

- Simple remains the only writable brainstorm-to-delivery mode. Do not add a workflow manifest, platform Gate, gate settlement, completion Card, artifact digest, reviewed task ID, or platform-owned completion decision.
- Resolve exactly one initial Task Agent generation from the invocation. Omitted selection means `agent_type: "grok"` and `profile_id: null`; never silently substitute an unavailable, invalid, or ambiguous Agent/profile.
- Permit a Task Agent change only after every earlier Task is completed, no Task is active, the Plan Author has revised pending routes, deterministic validation passes, and the complete latest Plan has been re-reviewed.
- Preserve `b2d_task_risk_v1` exactly: any hard trigger is `high`; otherwise a unique-evidence soft score of `0..=2` is `normal` and `>= 3` is `high`.
- Normal route: selected Task Agent implements and fixes; an independent Codex primary reviewer reviews. High route: an independent Codex implements and fixes; an independent Codex primary reviewer and independent selected Task Agent auxiliary reviewer both review the latest producer result.
- Keep every producer and reviewer on a distinct work-unit key and child conversation, even when Agent type and profile are identical. A continuation may reuse only its own work unit.
- Requirement, scope, architecture, and user-data decisions remain user-owned. A valid review finding that changes any of them pauses production for an explicit user decision.
- Keep Task execution serial. The primary and auxiliary reviewers of one high Task may fan out concurrently only after its implementer settles; both must settle before the next Task starts.
- Any implementation/fix invalidates every prior review conclusion for that Task. Final findings return to the owning normal/high producer and reopen the affected Task route plus final review.
- The parent orchestrator coordinates, adjudicates, updates progress, and delivers. It must not edit Design, Plan, or Task implementation content.
- Preserve the existing generic recovery rails: at most two unexpected continuations and one logical replacement per stable work-unit lineage; pre-admission retries retain current semantics.
- Existing five-part Task reviewer keys remain readable as legacy primary reviewers. New runs always emit explicit six-part primary/auxiliary keys.
- Plan/progress/run disagreement is a deterministic Skill-validator failure before dispatch, but only a bounded projection warning in Rust. It must never become a platform admission Gate.
- Keep the Plan at or below 2 MiB, the routing block at or below 256 KiB, progress at or below 512 KiB, and the progress block at or below 64 KiB.
- Follow RED-GREEN-REFACTOR. Every production behavior change starts with a focused test observed failing for the intended reason.
- Every filtered test command must execute at least one test. A zero-test success is not GREEN evidence.
- Run commands from the repository/worktree directory explicitly named in each step. Preserve unrelated changes and never stage `.superpowers/sdd/**` reports.
- Use one focused commit per Task. Do not merge, push, or open a PR unless separately requested.

## File Map

- `src-tauri/src/acp/delegation/workflow/types.rs`: canonical reviewer-slot and expanded parsed-key identity types.
- `src-tauri/src/acp/delegation/workflow/key.rs`: backward-compatible builders/parsers for Design Fixer and slotted Task reviewer keys.
- `src-tauri/src/acp/delegation/workflow/admission.rs`: exhaustive parsed-key role/readiness plumbing; no new Simple admission policy.
- `src-tauri/src/acp/delegation/workflow/simple_parse.rs`: bounded, non-authoritative routing/progress metadata parsing.
- `src-tauri/src/acp/delegation/workflow/project.rs`: Simple reconciliation warnings and separate route-node projection.
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`: authoritative Skill v2, routing, risk, route, progress, identity, and generation validation.
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`: deterministic positive/negative fixtures for every new contract.
- `.agents/skills/brainstorm-to-delivery/SKILL.md`: operational v2 workflow and exact role ownership.
- `src-tauri/tests/delegation_session_reuse_integration.rs`: repository-level Skill contract and independent-session scenario matrix.

## Canonical Plan Routing Contract

This Plan and every Plan produced by the revised Skill contain exactly one unfenced block of this shape. The block is the only machine-readable source of planned risk and routes; Task bodies contain concise human-readable evidence but no second routing table.

<!-- codeg-b2d-routing-v1
{
  "schema_version": 1,
  "risk_policy_version": "b2d_task_risk_v1",
  "task_agent_generations": [
    {
      "generation": 1,
      "agent_type": "grok",
      "profile_id": null,
      "effective_from_task_index": 1
    }
  ],
  "tasks": [
    {
      "index": 1,
      "task_agent_generation": 1,
      "risk": {
        "level": "high",
        "hard_triggers": [
          {
            "kind": "public_compatibility",
            "evidence": [
              "canonical work_unit_key grammar",
              "src-tauri/src/acp/delegation/workflow/types.rs",
              "src-tauri/src/acp/delegation/workflow/key.rs"
            ]
          }
        ],
        "soft_signals": [
          {
            "kind": "shared_interface",
            "score": 1,
            "evidence": [
              "parse_recognized_work_unit_key consumers in admission and projection"
            ]
          }
        ],
        "score": 1,
        "reason": "Changes the externally recorded work-unit key grammar while retaining legacy reviewer readability."
      },
      "route": {
        "implementer": { "agent_type": "codex", "profile_id": null },
        "reviewers": [
          {
            "slot": "primary",
            "agent_type": "codex",
            "profile_id": null
          },
          {
            "slot": "auxiliary",
            "agent_type": "grok",
            "profile_id": null
          }
        ]
      }
    },
    {
      "index": 2,
      "task_agent_generation": 1,
      "risk": {
        "level": "high",
        "hard_triggers": [
          {
            "kind": "public_compatibility",
            "evidence": [
              "codeg-b2d-routing-v1 Plan JSON",
              "additive codeg-simple-progress-v1 route metadata"
            ]
          }
        ],
        "soft_signals": [
          {
            "kind": "shared_interface",
            "score": 1,
            "evidence": [
              "SimplePlanDocument and SimpleProgressDocument consumed by project.rs"
            ]
          }
        ],
        "score": 1,
        "reason": "Adds serialized Plan/progress routing metadata consumed across validator and projector surfaces."
      },
      "route": {
        "implementer": { "agent_type": "codex", "profile_id": null },
        "reviewers": [
          {
            "slot": "primary",
            "agent_type": "codex",
            "profile_id": null
          },
          {
            "slot": "auxiliary",
            "agent_type": "grok",
            "profile_id": null
          }
        ]
      }
    },
    {
      "index": 3,
      "task_agent_generation": 1,
      "risk": {
        "level": "high",
        "hard_triggers": [
          {
            "kind": "public_compatibility",
            "evidence": [
              "Simple route reconciliation warning behavior"
            ]
          }
        ],
        "soft_signals": [
          {
            "kind": "shared_interface",
            "score": 1,
            "evidence": [
              "Plan/progress/run route identities consumed by project.rs"
            ]
          }
        ],
        "score": 1,
        "reason": "Adds externally visible non-blocking reconciliation warnings across Plan, progress, and durable run identity."
      },
      "route": {
        "implementer": { "agent_type": "codex", "profile_id": null },
        "reviewers": [
          {
            "slot": "primary",
            "agent_type": "codex",
            "profile_id": null
          },
          {
            "slot": "auxiliary",
            "agent_type": "grok",
            "profile_id": null
          }
        ]
      }
    },
    {
      "index": 4,
      "task_agent_generation": 1,
      "risk": {
        "level": "high",
        "hard_triggers": [
          {
            "kind": "public_compatibility",
            "evidence": [
              "Simple workflow graph node identity and fan-out behavior"
            ]
          }
        ],
        "soft_signals": [
          {
            "kind": "shared_interface",
            "score": 1,
            "evidence": [
              "WorkflowGraphSnapshot nodes consumed by desktop and server clients"
            ]
          }
        ],
        "score": 1,
        "reason": "Changes externally visible Simple projection from one aggregate Task node to route-aware producer/reviewer nodes."
      },
      "route": {
        "implementer": { "agent_type": "codex", "profile_id": null },
        "reviewers": [
          {
            "slot": "primary",
            "agent_type": "codex",
            "profile_id": null
          },
          {
            "slot": "auxiliary",
            "agent_type": "grok",
            "profile_id": null
          }
        ]
      }
    },
    {
      "index": 5,
      "task_agent_generation": 1,
      "risk": {
        "level": "high",
        "hard_triggers": [
          {
            "kind": "public_compatibility",
            "evidence": [
              "codeg-b2d-skill-contract-v2",
              "brainstorm-to-delivery invocation and dispatch behavior"
            ]
          }
        ],
        "soft_signals": [
          {
            "kind": "cross_runtime_or_process",
            "score": 2,
            "evidence": [
              "Markdown Skill instructions dispatch through generic MCP delegation tools"
            ]
          },
          {
            "kind": "multiple_ownership_modules",
            "score": 1,
            "evidence": [
              ".agents Skill/Node validator and src-tauri integration contract"
            ]
          },
          {
            "kind": "shared_interface",
            "score": 1,
            "evidence": [
              "Skill, Plan, progress, and generic delegation run contracts"
            ]
          }
        ],
        "score": 4,
        "reason": "Atomically changes the public Skill contract and cross-process dispatch semantics."
      },
      "route": {
        "implementer": { "agent_type": "codex", "profile_id": null },
        "reviewers": [
          {
            "slot": "primary",
            "agent_type": "codex",
            "profile_id": null
          },
          {
            "slot": "auxiliary",
            "agent_type": "grok",
            "profile_id": null
          }
        ]
      }
    }
  ]
}
-->

---

### Task 1: Add Design Fixer and slotted Task reviewer key identity

**Dependencies:** None. This lands reader/runtime vocabulary before the Skill can emit it.

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/types.rs` (`WorkUnitKeyParts`, `ParsedWorkUnitKey`)
- Modify: `src-tauri/src/acp/delegation/workflow/key.rs` (builder/parser and unit tests)
- Modify: `src-tauri/src/acp/delegation/workflow/admission.rs` (all exhaustive parsed-key matches only)
- Modify: `src-tauri/src/acp/delegation/workflow/project.rs` (`parsed_meta`, observed synthetic identity, Task-run matching)
- Report: `.superpowers/sdd/b2d-generic-task-agent-routing/task-1-report.md` (do not commit)

**Interfaces:**

- Consumes: existing `normalize_rel_path`, `validate_agent_type`, profile token validation, `MAX_WORK_UNIT_KEY_LEN`, and generic no-manifest admission behavior.
- Produces: `ReviewerSlot`, `WorkUnitKeyParts::DesignFixer`, `WorkUnitKeyParts::TaskReviewerSlotted`, and parsed Design Fixer/slotted reviewer identities used by Tasks 2-4.

Add these exact variants while retaining the historical builders:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerSlot {
    Primary,
    Auxiliary,
}

impl ReviewerSlot {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Auxiliary => "auxiliary",
        }
    }
}

pub enum WorkUnitKeyParts<'a> {
    // Keep Design as the legacy Design reviewer builder.
    Design { /* existing fields unchanged */ },
    DesignFixer {
        rel_doc_path: &'a str,
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    // Keep TaskReviewer as the five-part legacy reviewer builder.
    TaskReviewer { /* existing fields unchanged */ },
    TaskReviewerSlotted {
        task_index: u32,
        slot: ReviewerSlot,
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    // all existing Plan/Task/Final variants remain unchanged
}

pub enum ParsedWorkUnitKey {
    Design { /* existing fields unchanged */ },
    DesignFixer {
        rel_doc_path: String,
        agent_type: String,
        profile_id: Option<String>,
    },
    TaskReviewer {
        task_index: u32,
        slot: ReviewerSlot,
        agent_type: String,
        profile_id: Option<String>,
    },
    // all existing Plan/Task/Final variants remain unchanged
}
```

Canonical new output:

```text
design|docs/design.md|fixer|codex|none
task|7|reviewer|primary|codex|none
task|7|reviewer|auxiliary|grok|release
```

`task|7|reviewer|codex|none` remains accepted and parses as `slot: ReviewerSlot::Primary`. `WorkUnitKeyParts::TaskReviewer` continues to build that exact five-part key so manifest/history fixtures are not rewritten. Only `TaskReviewerSlotted` builds the six-part form.

- [ ] **Step 1: Write failing key grammar tests**

Add these focused cases to `key.rs`:

```rust
#[test]
fn design_fixer_and_slotted_reviewers_round_trip() {
    let fixer = build_work_unit_key(&WorkUnitKeyParts::DesignFixer {
        rel_doc_path: "docs/design.md",
        agent_type: "codex",
        profile_id: None,
    })
    .unwrap();
    assert_eq!(fixer, "design|docs/design.md|fixer|codex|none");
    assert!(matches!(
        parse_recognized_work_unit_key(&fixer),
        Some(ParsedWorkUnitKey::DesignFixer { .. })
    ));

    for (slot, expected) in [
        (ReviewerSlot::Primary, "task|7|reviewer|primary|codex|none"),
        (ReviewerSlot::Auxiliary, "task|7|reviewer|auxiliary|codex|none"),
    ] {
        let key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewerSlotted {
            task_index: 7,
            slot,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        assert_eq!(key, expected);
        assert!(matches!(
            parse_recognized_work_unit_key(&key),
            Some(ParsedWorkUnitKey::TaskReviewer { task_index: 7, slot: parsed, .. })
                if parsed == slot
        ));
    }
}

#[test]
fn legacy_task_reviewer_is_primary_and_invalid_slots_fail() {
    assert!(matches!(
        parse_recognized_work_unit_key("task|7|reviewer|codex|none"),
        Some(ParsedWorkUnitKey::TaskReviewer {
            task_index: 7,
            slot: ReviewerSlot::Primary,
            ..
        })
    ));
    for key in [
        "task|7|reviewer|secondary|codex|none",
        "task|0|reviewer|primary|codex|none",
        "task|7|reviewer|primary|unknown-agent|none",
        "design|../design.md|fixer|codex|none",
        "design|docs/design.md|fixer|codex|bad|profile",
    ] {
        assert_eq!(parse_recognized_work_unit_key(key), None, "{key}");
    }
}
```

Also retain the existing invalid control-character, path, profile, Agent, index, and 200-Unicode-scalar boundary tests for every new branch.

In `project.rs`, add `observed_projection_slotted_keys_have_distinct_synthetic_ids`: parse explicit primary and auxiliary Codex keys for the same Task/profile plus one Design Fixer key, then assert `parsed_meta` returns the required phase/role and all three `synthetic_node_id` values differ. This is the RED test for the observed projection identity change.

- [ ] **Step 2: Run the key tests and verify RED**

From `src-tauri/`:

```bash
cargo test --lib --features test-utils workflow::key::tests -- --nocapture
cargo test --lib --features test-utils observed_projection_slotted_keys -- --nocapture
```

Expected: FAIL to compile because `ReviewerSlot`, `DesignFixer`, and `TaskReviewerSlotted` do not exist.

- [ ] **Step 3: Implement the minimal builder and parser branches**

Use the existing path/Agent/profile validators. Add parser arms in this order so the explicit six-part reviewer cannot be confused with the legacy five-part form:

```rust
["task", index, "reviewer", slot, agent, profile] => {
    let task_index = parse_task_index_str(index)?;
    let slot = match *slot {
        "primary" => ReviewerSlot::Primary,
        "auxiliary" => ReviewerSlot::Auxiliary,
        _ => return None,
    };
    Some(ParsedWorkUnitKey::TaskReviewer {
        task_index,
        slot,
        agent_type: validate_agent_type(agent).ok()?.to_string(),
        profile_id: parse_profile(profile)?,
    })
}
["task", index, "reviewer", agent, profile] => Some(
    ParsedWorkUnitKey::TaskReviewer {
        task_index: parse_task_index_str(index)?,
        slot: ReviewerSlot::Primary,
        agent_type: validate_agent_type(agent).ok()?.to_string(),
        profile_id: parse_profile(profile)?,
    },
),
```

- [ ] **Step 4: Update exhaustive admission and projection matches**

Use these exact semantics:

- `validate_identity_match(DesignFixer)` expects role `fixer`, phase `design`.
- `enforce_phase_readiness(DesignFixer)` follows the same always-ready document-producer branch as `PlanAuthor`; it does not settle or require a document-review Gate.
- `document_gate_content_fingerprint(DesignFixer)` and `document_gate_stamp(DesignFixer)` return no Gate/fingerprint association.
- `stamp_admission_fields(DesignFixer)` returns `(None, None, None, None, None, None)` because Simple document production is coordinated by generic runs, not manifest evidence.
- Both reviewer slots map to role `reviewer`, phase `tasks`, and retain existing Task reviewer route/readiness/artifact behavior.
- `run_matches_task_index` recognizes implementers plus both explicit and legacy reviewer parses.
- `parsed_meta(DesignFixer)` returns `("design", "fixer", None)`.
- Observed Design Fixer IDs use `observed-design-fixer-{key_tag}`.
- Observed Task reviewer IDs use `observed-task-{task_index}-rev-{slot}-{key_tag}` so legacy primary and explicit primary keys cannot collide if both appear in one historical root.
- Do not add a Simple admission Gate or change manifest route policy.

- [ ] **Step 5: Run focused GREEN and compile coverage**

From `src-tauri/`:

```bash
cargo test --lib --features test-utils workflow::key::tests -- --nocapture
cargo test --lib --features test-utils observed_projection_slotted_keys -- --nocapture
cargo check --lib --features test-utils
```

Expected: key tests PASS; the shared Rust library compiles with every exhaustive match updated.

- [ ] **Step 6: Commit Task 1**

```bash
git add -- src-tauri/src/acp/delegation/workflow/types.rs src-tauri/src/acp/delegation/workflow/key.rs src-tauri/src/acp/delegation/workflow/admission.rs src-tauri/src/acp/delegation/workflow/project.rs
git commit -m "feat(workflow): add slotted reviewer work units"
```

- [ ] **Step 7: Write the Task report**

Create `.superpowers/sdd/b2d-generic-task-agent-routing/task-1-report.md` containing the changed files, exact commands/outcomes, legacy-key compatibility evidence, commit hash, and any retained Minor findings. Do not stage the report.

---

### Task 2: Parse bounded Plan routing and additive progress metadata

**Dependencies:** Task 1's `ReviewerSlot` and canonical key parser are available.

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/simple_parse.rs` (types, bounded markers, parsing, unit tests)
- Report: `.superpowers/sdd/b2d-generic-task-agent-routing/task-2-report.md` (do not commit)

**Interfaces:**

- Consumes: `parse_recognized_work_unit_key`, `ReviewerSlot`, existing 2 MiB Plan reader, existing 512 KiB/64 KiB progress bounds, and legacy progress-v1 fields.
- Produces: optional `SimplePlanDocument.routing`, additive `SimpleProgressTask` route fields, and warning codes consumed by Task 3. Absence of routing metadata remains a valid legacy projection input.

Add these constants and models:

```rust
pub const MAX_SIMPLE_ROUTING_BLOCK_BYTES: usize = 256 * 1024;
const ROUTING_MARKER: &str = "<!-- codeg-b2d-routing-v1";
pub const WARNING_ROUTING_MULTIPLE: &str = "simple_routing_multiple_blocks";
pub const WARNING_ROUTING_TRUNCATED: &str = "simple_routing_block_truncated";
pub const WARNING_ROUTING_TOO_LARGE: &str = "simple_routing_block_too_large";
pub const WARNING_ROUTING_INVALID_JSON: &str = "simple_routing_invalid_json";
pub const WARNING_ROUTING_SCHEMA: &str = "simple_routing_schema_unsupported";
pub const WARNING_ROUTING_POLICY: &str = "simple_routing_policy_unsupported";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleAgentSelection {
    pub agent_type: String,
    pub profile_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleTaskAgentGeneration {
    pub generation: u32,
    pub agent_type: String,
    pub profile_id: Option<String>,
    pub effective_from_task_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleRiskEvidence {
    pub kind: String,
    pub score: Option<u32>,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleTaskRisk {
    pub level: String,
    pub hard_triggers: Vec<SimpleRiskEvidence>,
    pub soft_signals: Vec<SimpleRiskEvidence>,
    pub score: u32,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleTaskReviewerRoute {
    pub slot: ReviewerSlot,
    pub agent_type: String,
    pub profile_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleTaskRoute {
    pub implementer: SimpleAgentSelection,
    pub reviewers: Vec<SimpleTaskReviewerRoute>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleRoutingTask {
    pub index: u32,
    pub task_agent_generation: u32,
    pub risk: SimpleTaskRisk,
    pub route: SimpleTaskRoute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleRoutingSnapshot {
    pub schema_version: u32,
    pub risk_policy_version: String,
    pub task_agent_generations: Vec<SimpleTaskAgentGeneration>,
    pub tasks: Vec<SimpleRoutingTask>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleExpectedReviewerKeys {
    pub primary: String,
    pub auxiliary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleExpectedWorkUnitKeys {
    pub implementer: String,
    pub reviewers: SimpleExpectedReviewerKeys,
}
```

Extend existing models additively:

```rust
pub struct SimplePlanDocument {
    pub tasks: Vec<SimplePlanTask>,
    pub routing: Option<SimpleRoutingSnapshot>,
    pub warning_codes: Vec<String>,
}

pub struct SimpleProgressTask {
    pub index: u32,
    pub status: SimpleDeclaredStatus,
    pub commit: Option<String>,
    pub risk_level: Option<String>,
    pub task_agent_generation: Option<u32>,
    pub expected_work_unit_keys: Option<SimpleExpectedWorkUnitKeys>,
    pub runs: Vec<SimpleProgressRun>,
}
```

The exact new progress JSON for a high Task is:

```json
{
  "index": 2,
  "status": "pending",
  "risk_level": "high",
  "task_agent_generation": 1,
  "expected_work_unit_keys": {
    "implementer": "task|2|implementer|codex|none",
    "reviewers": {
      "primary": "task|2|reviewer|primary|codex|none",
      "auxiliary": "task|2|reviewer|auxiliary|grok|none"
    }
  },
  "runs": []
}
```

- [ ] **Step 1: Write failing bounded routing parser tests**

Add literal Plan fixtures proving:

- one valid routing block is parsed alongside real H2/H3 Task headings;
- no routing marker returns `routing: None` without a warning (legacy compatibility);
- two markers, missing `-->`, invalid JSON, schema other than `1`, policy other than `b2d_task_risk_v1`, and a block over 256 KiB return the largest safe Plan task model plus the exact bounded warning;
- fenced examples are not treated as the live routing marker;
- full Plan size and invalid UTF-8 remain hard bounded-read errors.

Use an assertion shaped like:

```rust
let parsed = parse_simple_plan(valid_routed_plan.as_bytes()).expect("parse");
let routing = parsed.routing.expect("routing");
assert_eq!(routing.risk_policy_version, "b2d_task_risk_v1");
assert_eq!(routing.tasks[0].route.reviewers[1].slot, ReviewerSlot::Auxiliary);
assert!(parsed.warning_codes.is_empty());
```

- [ ] **Step 2: Run routing parser tests and verify RED**

From `src-tauri/`:

```bash
cargo test --lib --features test-utils simple_parse::tests::simple_parse_routing -- --nocapture
```

Expected: FAIL because `SimplePlanDocument` has no routing model or bounded marker parser.

- [ ] **Step 3: Implement one shared unfenced comment extractor**

Implement a private helper used by both routing and progress parsing:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimpleCommentProblem {
    Truncated,
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SimpleCommentBlock<'a> {
    body: Option<&'a str>,
    marker_count: usize,
    problem: Option<SimpleCommentProblem>,
}

fn extract_unfenced_comment<'a>(
    source: &'a str,
    marker: &str,
    max_block_bytes: usize,
) -> SimpleCommentBlock<'a>;
```

It must walk lines with the existing Markdown fence rules, count only unfenced exact marker starts, return the first complete body, stop it at the matching `-->`, and check the UTF-8 byte slice against `max_block_bytes`. `marker_count > 1` supplies the existing multiple-block warning while still parsing the first body; `problem` distinguishes `Truncated` and `TooLarge`. Keep the current progress warning semantics when migrating progress parsing to this helper.

- [ ] **Step 4: Write failing additive progress parser tests**

Add tests that parse the exact JSON above and prove:

- all three expected keys survive into `SimpleProgressTask`;
- omitted route fields deserialize as `None` for archived/legacy progress;
- unknown Task status/run state still warns without becoming completed;
- malformed nested routing fields make the progress block invalid JSON/schema state rather than panicking;
- explicit primary and auxiliary runs keep separate six-part keys and profiles.

- [ ] **Step 5: Run progress metadata tests and verify RED**

From `src-tauri/`:

```bash
cargo test --lib --features test-utils simple_parse::tests::simple_parse_progress -- --nocapture
```

Expected: FAIL because `RawProgressTask` ignores and the public model omits the new fields.

- [ ] **Step 6: Implement minimal serde parsing and safe partial behavior**

Routing semantic enforcement remains in the JavaScript validator in Task 5. Rust only accepts the bounded/schema-recognized shape, preserves useful fields, and emits warnings. Do not reject delegation, create workflow headers, or add persistence.

- [ ] **Step 7: Run Task 2 GREEN and regressions**

From `src-tauri/`:

```bash
cargo test --lib --features test-utils simple_parse -- --nocapture
cargo test --lib --features test-utils workflow::key::tests -- --nocapture
```

Expected: all Simple Plan/progress and key tests PASS, including legacy progress fixtures.

- [ ] **Step 8: Commit Task 2**

```bash
git add -- src-tauri/src/acp/delegation/workflow/simple_parse.rs
git commit -m "feat(workflow): parse Simple routing metadata"
```

- [ ] **Step 9: Write the Task report**

Create `.superpowers/sdd/b2d-generic-task-agent-routing/task-2-report.md` with parser bounds, safe-partial outcomes, commands, commit hash, and retained Minors. Do not stage it.

---

### Task 3: Derive and reconcile normal/high Simple routes without Gates

**Dependencies:** Task 1 canonical keys and Task 2 parsed routing/progress models.

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/project.rs` (route derivation, reconciliation warnings, focused tests)
- Report: `.superpowers/sdd/b2d-generic-task-agent-routing/task-3-report.md` (do not commit)

**Interfaces:**

- Consumes: `SimpleRoutingSnapshot`, additive progress route fields, `build_work_unit_key`, `ReviewerSlot`, and durable `delegation_task_run` rows.
- Produces: validated expected-route derivation plus bounded non-blocking reconciliation warnings consumed by Task 4.

Introduce these private projection helpers:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
struct SimpleExpectedRoute {
    risk_level: String,
    task_agent_generation: u32,
    implementer_key: String,
    primary_reviewer_key: String,
    auxiliary_reviewer_key: Option<String>,
}

fn derive_simple_expected_route(
    routing: &SimpleRoutingSnapshot,
    task_index: u32,
) -> Result<SimpleExpectedRoute, &'static str>;

fn reconcile_simple_progress_route(
    expected: &SimpleExpectedRoute,
    progress: Option<&SimpleProgressTask>,
    warnings: &mut Vec<String>,
);

fn run_matches_work_unit_key(
    run: &delegation_task_run::Model,
    expected_key: &str,
) -> bool;
```

`derive_simple_expected_route` uses the Task's recorded Agent/profile selections and Task 1's builders. It returns these exact route shapes:

```text
normal: implementer(selected Task Agent), primary(codex), no auxiliary
high:   implementer(codex), primary(codex), auxiliary(selected Task Agent)
```

It returns an error for unknown level, missing/duplicate Task index, missing generation, wrong reviewer slots/count, invalid Agent/profile, or a route not derived from the referenced generation. The caller adds `simple_plan_routing_invalid` and falls back to the legacy aggregate Task node; it never fails admission.

`reconcile_simple_progress_route` adds only bounded/deduplicated warning codes:

```text
simple_progress_risk_level_mismatch
simple_progress_task_agent_generation_mismatch
simple_progress_implementer_key_mismatch
simple_progress_primary_reviewer_key_mismatch
simple_progress_auxiliary_reviewer_key_mismatch
simple_progress_expected_route_missing
simple_progress_run_outside_expected_route
simple_progress_route_child_not_independent
```

- [ ] **Step 1: Write failing route derivation/reconciliation unit tests**

Add table cases for normal Grok, normal custom Agent/profile, high Grok, and high Task Agent Codex. Assert high Codex produces three different keys even though all three Agent types/profiles match:

```rust
assert_eq!(route.implementer_key, "task|4|implementer|codex|none");
assert_eq!(
    route.primary_reviewer_key,
    "task|4|reviewer|primary|codex|none"
);
assert_eq!(
    route.auxiliary_reviewer_key.as_deref(),
    Some("task|4|reviewer|auxiliary|codex|none")
);
```

Mutate each mirrored progress field and assert the corresponding warning is emitted once. Give two distinct expected keys the same non-null `child_conversation_id` and assert `simple_progress_route_child_not_independent`. These warnings must not return `ProjectError`.

- [ ] **Step 2: Run route helper tests and verify RED**

From `src-tauri/`:

```bash
cargo test --lib --features test-utils simple_projection_route -- --nocapture
```

Expected: FAIL because routing helpers and warning codes do not exist.

- [ ] **Step 3: Implement deterministic route derivation and warnings**

Compare complete work-unit keys, not generic `role`. When checking child independence, group admitted runs by non-null child ID and fail only the reconciliation state when one child appears under two different expected keys. Do not infer Agent fallback or rewrite Plan/progress.

- [ ] **Step 4: Run Task 3 GREEN and focused regressions**

From `src-tauri/`:

```bash
cargo test --lib --features test-utils simple_projection_route -- --nocapture
cargo test --lib --features test-utils simple_projection_warns -- --nocapture
```

Expected: route derivation and warning tests PASS; the pre-existing legacy warning projection tests remain green.

- [ ] **Step 5: Commit Task 3**

```bash
git add -- src-tauri/src/acp/delegation/workflow/project.rs
git commit -m "feat(workflow): reconcile Simple task routes"
```

- [ ] **Step 6: Write the Task report**

Create `.superpowers/sdd/b2d-generic-task-agent-routing/task-3-report.md` with route fixtures, reconciliation warnings, commands, commit hash, and retained Minors. Do not stage it.

---

### Task 4: Project routed producers and reviewers as independent nodes

**Dependencies:** Task 3 provides a validated `SimpleExpectedRoute` and warning-only fallback for every Plan Task.

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/project.rs` (route nodes/edges, state derivation, projection tests)
- Report: `.superpowers/sdd/b2d-generic-task-agent-routing/task-4-report.md` (do not commit)

**Interfaces:**

- Consumes: Task 3's `derive_simple_expected_route`, `reconcile_simple_progress_route`, and `run_matches_work_unit_key`; existing `WorkflowGraphSnapshot`/`WorkflowNodeSnapshot` DTOs.
- Produces: separate implementer/primary/auxiliary Simple nodes when a valid routing block exists; legacy Plans without routing retain the existing one-node-per-Task projection.

- [ ] **Step 1: Write failing graph fan-out tests**

Extend the existing `simple_projection_*` test setup with one normal routed Plan and one high routed Plan. Assert:

- normal creates `simple-task-1-implementer` and `simple-task-1-reviewer-primary` with an implementer-to-reviewer edge;
- high creates `simple-task-1-implementer`, `simple-task-1-reviewer-primary`, and `simple-task-1-reviewer-auxiliary` with two fan-out edges;
- both high reviewers with `agent_type=codex`, identical profile, and separate children remain distinct nodes;
- the next Task implementer depends on every reviewer from the previous Task;
- a reviewer run created before the latest implementer/fix run is stale, makes only that reviewer node out-of-sync, and adds `simple_task_review_stale`;
- completed progress missing one expected latest reviewer run cannot make the delivery complete and adds `simple_completed_task_route_incomplete`;
- a malformed/mismatched route produces warnings and no platform Gate;
- a legacy Plan/progress fixture still projects exactly one `simple-task-N` node per Task;
- archived manifest projection is unchanged.

- [ ] **Step 2: Run graph tests and verify RED**

From `src-tauri/`:

```bash
cargo test --lib --features test-utils simple_projection_ -- --nocapture
```

Expected: routed fixtures still collapse all runs into one aggregate Task node.

- [ ] **Step 3: Implement route-aware node construction**

For routed Tasks, group durable/progress runs by exact expected key. Compute each node from its own latest generation/run. Use stable IDs:

```text
simple-task-{index}-implementer
simple-task-{index}-reviewer-primary
simple-task-{index}-reviewer-auxiliary
```

Use Task title plus `Implementation`, `Primary review`, or `Auxiliary review` as the bounded display title. An admitted `reserving`/`running` run overrides pending state for only its node. A completed route node is current only when its latest terminal run is completed and, for a reviewer, its `created_at` is not older than the latest implementer/fix run's `created_at`; failed/canceled required route nodes are blocked. Aggregate Task `completed` status cannot fill in a missing expected node. Keep `gates: []`, `workflow_id: None`, `manifest_revision: None`, and `compatibility: Simple`.

For routed edges, connect prior Task reviewer node(s) to the next implementer; connect the current implementer to each current reviewer. For legacy Plans, execute the existing aggregate node branch without changing IDs or state rules.

- [ ] **Step 4: Run Task 4 GREEN and projection regressions**

From `src-tauri/`:

```bash
cargo test --lib --features test-utils simple_projection_ -- --nocapture
cargo test --lib --features test-utils workflow::project::tests -- --nocapture
cargo check --lib --features test-utils
```

Expected: all route-aware, legacy Simple, observed-only, and archived projection tests PASS; Rust library compiles.

- [ ] **Step 5: Commit Task 4**

```bash
git add -- src-tauri/src/acp/delegation/workflow/project.rs
git commit -m "feat(workflow): project adaptive Simple task routes"
```

- [ ] **Step 6: Write the Task report**

Create `.superpowers/sdd/b2d-generic-task-agent-routing/task-4-report.md` with node/edge fixtures, state/edge outcomes, commands, commit hash, and retained Minors. Do not stage it.

---

### Task 5: Ship Skill contract v2 and deterministic route validation

**Dependencies:** Tasks 1-4 recognize and project every key and document shape the revised Skill emits. This Task is the emitter switch and must be last.

**Files:**

- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- Modify: `.agents/skills/brainstorm-to-delivery/SKILL.md`
- Modify: `src-tauri/tests/delegation_session_reuse_integration.rs`
- Report: `.superpowers/sdd/b2d-generic-task-agent-routing/task-5-report.md` (do not commit)

**Interfaces:**

- Consumes: Tasks 1-4 canonical keys/projection behavior, live generic delegation Agent identities, `writing-plans`, `subagent-driven-development`, and existing registration/recovery tools.
- Produces: exactly one `codeg-b2d-skill-contract-v2`, authoritative `codeg-b2d-routing-v1` validation, additive progress route validation, and complete operational instructions for independent document/Task roles.

Before editing the Skill, read and follow `/Users/pengchao/.codex/skills/.system/skill-creator/SKILL.md` and `/Users/pengchao/.codex/plugins/cache/gf-team/superpowers/6.2.0/skills/writing-skills/SKILL.md`. Keep `SKILL.md` below 500 lines and imperative.

Replace the v1 contract with this exact positive contract (the JavaScript constant and Markdown JSON must deep-equal after canonical key ordering):

```json
{
  "schema_version": 2,
  "phase_order": [
    "establish-current-truth",
    "resolve-task-agent",
    "review-and-revise-design",
    "author-and-review-plan",
    "maintain-progress",
    "apply-workspace-gate",
    "execute-tasks-serially",
    "recover-generic-runs",
    "complete-final-review"
  ],
  "interfaces": {
    "plan_authoring": "writing-plans",
    "task_execution": "subagent-driven-development",
    "registration": "register_simple_workflow",
    "first_run": "delegate_to_agent",
    "later_run": "continue_delegation",
    "join": "get_delegation_status",
    "recovery_authorization": "request_recovery_authorization"
  },
  "plan_setup_order": [
    "create-progress",
    "dispatch-plan-author",
    "confirm-plan-on-disk",
    "validate-routing",
    "review-plan",
    "register-simple-workflow",
    "sync-plan-tasks"
  ],
  "document_work": {
    "parent_edits": false,
    "design_review": "conditional",
    "design_reviewer": "independent_codex",
    "design_fixer": "independent_codex",
    "plan_author": "independent_codex",
    "plan_reviewer": "independent_codex",
    "producer_reviewer_independence": true,
    "plan_rereview": "full_latest_plan",
    "user_named_reviewers": "design_and_plan_only"
  },
  "conversation_identity": {
    "distinct_work_units": "distinct_child_conversations",
    "continuation": "same_work_unit_only"
  },
  "task_agent": {
    "default_agent_type": "grok",
    "selection_source": "invocation",
    "explicit_substitution": "forbidden",
    "change_boundary": "completed_tasks_after_plan_revision_and_full_rereview"
  },
  "routing": {
    "marker": "codeg-b2d-routing-v1",
    "risk_policy_version": "b2d_task_risk_v1",
    "normal": {
      "implementer": "task_agent",
      "reviewers": ["codex_primary"]
    },
    "high": {
      "implementer": "codex",
      "reviewers": ["codex_primary", "task_agent_auxiliary"]
    },
    "reviewer_slots": ["primary", "auxiliary"],
    "task_order": "serial",
    "high_review_fan_out": "parallel_after_implementation"
  },
  "progress": {
    "marker": "codeg-simple-progress-v1",
    "mutation_order": [
      "record-reserving-intent",
      "delegate",
      "record-admission",
      "record-observed-state"
    ],
    "route_metadata": "additive"
  },
  "workspace_policy": "preserve-user-changes",
  "recovery": {
    "unexpected_continuations": 2,
    "logical_replacements": 1,
    "replacement_retry": "pre-admission-only"
  },
  "final_review": {
    "required": true,
    "independent": true,
    "reviewer": "codex",
    "fix_owner": "task_producer"
  }
}
```

In `validate-contract.lib.mjs`, export and use:

```js
export const MAX_ROUTING_BLOCK_BYTES = 256 * 1024
const SKILL_CONTRACT_MARKER = "<!-- codeg-b2d-skill-contract-v2"
const ROUTING_MARKER = "<!-- codeg-b2d-routing-v1"
const RISK_POLICY_VERSION = "b2d_task_risk_v1"
const SOFT_SIGNAL_SCORES = new Map([
  ["cross_runtime_or_process", 2],
  ["broad_production_surface", 1],
  ["multiple_ownership_modules", 1],
  ["shared_interface", 1],
  ["dependency_or_build", 1],
  ["multi_layer_without_test_seam", 1],
])
const HARD_TRIGGER_KINDS = new Set([
  "concurrency_lifecycle",
  "security_trust_boundary",
  "migration_destructive_persistence",
  "public_compatibility",
  "unsafe_ffi",
  "update_rollback",
])
```

Add and export these pure interfaces:

```js
export function parseSimpleRouting(planMarkdown) {
  // returns { snapshot, failures }
}

export function validateRoutingSnapshot(snapshot, plan, failures) {
  // returns normalized generations/tasks for progress comparison
}

export function deriveExpectedRoute(task, generation, failures) {
  // returns exact implementer/primary/optional auxiliary identities and keys
}

export function validateProgressRouting(snapshot, routing, failures) {
  // enforces Plan/progress agreement and boundary-only generation changes
}
```

- [ ] **Step 1: Replace test fixtures with v2 Skill, routed Plan, and routed progress**

Build test helpers for a normal Grok Task and a high Task. The progress helper must derive these exact fields rather than hand-wave them:

```js
function expectedWorkUnitKeys(index, level, taskAgent) {
  const profile = taskAgent.profile_id ?? "none"
  return {
    implementer:
      level === "normal"
        ? `task|${index}|implementer|${taskAgent.agent_type}|${profile}`
        : `task|${index}|implementer|codex|none`,
    reviewers: {
      primary: `task|${index}|reviewer|primary|codex|none`,
      auxiliary:
        level === "high"
          ? `task|${index}|reviewer|auxiliary|${taskAgent.agent_type}|${profile}`
          : null,
    },
  }
}
```

Update `parseRecognizedWorkUnitKey` so Design Fixer and explicit reviewer slots parse, while legacy five-part Task reviewer keys parse with `slot: "primary", legacy: true`.

- [ ] **Step 2: Write failing Skill v2 ownership tests**

Test exactly one unfenced v2 block, all nine ordered phases, independent Design Fixer/Plan Author/reviewers, Grok default plus invocation selection, no parent Design/Plan/Task writing, conditional Design review, full Plan re-review, serial Tasks, high-review fan-out, owning-producer final fixes, recovery rails, and the ban on every retired v2 workflow mutation identifier.

Add negative prose fixtures for:

```text
The parent revises the Plan directly.
Always use Grok as the implementer.
Use the Task Agent to implement high Tasks.
Reuse one Codex conversation for implementation and review.
Switch Agent immediately inside the active Task.
Skip the auxiliary review after a high-Task fix.
```

- [ ] **Step 3: Write failing risk, generation, and route tests**

Cover all of these deterministic cases with explicit JSON mutations and rule IDs:

- omitted initial override resolves to generation 1 Grok; each built-in and valid `custom:*` identity/profile validates;
- invalid/reserved custom ID, ambiguous/unavailable placeholder, literal profile `"none"`, or malformed Agent never falls back;
- generations start at 1, remain contiguous/strictly increasing, start at Task 1, and each later `effective_from_task_index` equals the first pending Task that references it;
- any Task with a non-empty `runs` list freezes its generation/route; a generation change with an active/blocked/admitted Task or before the completed prefix fails, and its effective Task must still be pending with `runs: []`;
- all six hard triggers force high and require non-empty evidence;
- soft totals 0, 1, 2 are normal and 3+ are high;
- unknown, duplicate, evidence-free, wrong-score, wrong-total, contradictory level, and empty reason fail;
- normal has exactly selected implementer plus Codex primary; high has exactly Codex implementer plus Codex primary and selected auxiliary;
- wrong profile, order, slot, duplicate reviewer, missing reviewer, surplus reviewer, or free-form route fails;
- routing Task indices exactly match ordered Plan headings.

- [ ] **Step 4: Write failing progress agreement and lineage tests**

Require every routed progress Task to match `risk_level`, `task_agent_generation`, and all expected keys. A completed routed Task must contain a terminal completed lineage for every expected key. Runs outside the expected set fail.

Change lineage grouping from `run.role` to `run.work_unit_key`:

```js
const group = groups.get(run.work_unit_key) ?? []
group.push({ run, runIndex })
groups.set(run.work_unit_key, group)
```

Then test:

- primary and auxiliary reviewers both use generic role `reviewer` without merging lineages;
- key/Agent/profile remain stable within each complete-key group;
- replacement source and one-replacement budget are checked per complete key;
- `task_id` remains globally unique;
- two distinct work-unit keys cannot share one non-null child conversation ID;
- a legacy five-part reviewer remains readable only as primary in a legacy Plan/progress fixture; new routed progress requires the explicit six-part primary key;
- Plan/progress level, generation, implementer, primary, or auxiliary mismatch fails deterministically.

- [ ] **Step 5: Run the validator suite and verify RED**

From the repository root:

```bash
node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
```

Expected: FAIL because the production validator and Skill still implement contract v1 and role-grouped lineages.

- [ ] **Step 6: Implement the v2 validator and rewrite the Skill**

Use a shared bounded unfenced-comment extractor for Skill/routing/progress markers. `parseSimplePlan` returns both ordered headings and routing. `validateSimpleDocuments` validates Skill, Plan routing, progress, and their agreement in that order.

Rewrite the seven current operational sections into the nine contract phases. The Skill must explicitly:

- inspect live delegation schemas/Agent discovery and resolve the invocation selection before document work;
- dispatch conditional Design Reviewer and independent Codex Design Fixer keys;
- create progress first, then dispatch independent Codex Plan Author with `writing-plans`; register Simple only after Plan validation/review approval;
- continue the same Design Fixer/Plan Author work unit for revisions and the same separate reviewer units for full re-review;
- prevent parent document/code edits and user-named document reviewers from entering Task/final roles;
- validate `b2d_task_risk_v1` and Plan/progress before every Task dispatch;
- return pre-admission risk-evidence changes through Plan Author revision and full Plan re-review; after admission, block and request a user decision instead of swapping the active route;
- execute the exact normal/high route, re-run all required reviewers after every fix, and defer/block active-Task Agent changes;
- route final findings back to the owning producer and reopen Task/final reviews;
- permit an archived/legacy Simple run to remain on its recorded route, but require a Plan Author revision with a complete routing block before the next pending Task adopts adaptive routing;
- preserve Simple registration, workspace gate, generic continuation/replacement behavior, and local-only delivery.

Do not mention or call retired workflow-v2 tools. Do not restore a Final Fixer work unit; `final_review|reviewer|codex|...` remains the only Final key.

- [ ] **Step 7: Rewrite the Rust Skill-forward contract scenarios**

Update `delegation_session_reuse_integration.rs` to read the v2 marker and assert the exact contract above. Replace the old Grok-hard-coded nine-scenario matrix with the approved eleven scenarios:

1. default normal: Grok implementer plus Codex primary;
2. selected non-Grok normal route;
3. high: Codex implementer plus Codex primary and Task Agent auxiliary;
4. Task Agent Codex still uses three distinct keys/children;
5. high fix continues Codex implementer and both reviewers re-review;
6. conditional Design Reviewer and Design Fixer are separate;
7. initial/revised Plan stays with Plan Author and separate Plan Reviewer;
8. boundary Agent change affects pending Tasks only;
9. active-Task change defers/blocks without handoff;
10. unavailable/recovery/replacement keeps Agent/profile/key and budgets;
11. final findings continue the owning normal/high producer and reopen reviews.

Use the canonical keys from the approved design and assert no two distinct route keys share a child conversation in the reuse integration setup.

- [ ] **Step 8: Run Task 5 GREEN and production checks**

From the repository root:

```bash
node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs
```

Expected: both commands PASS; the second validates the production Skill rather than only fixtures.

From `src-tauri/`:

```bash
cargo test --test delegation_session_reuse_integration skill_forward_ -- --nocapture
cargo test --lib --features test-utils workflow::key::tests -- --nocapture
cargo test --lib --features test-utils simple_parse -- --nocapture
cargo test --lib --features test-utils simple_projection_ -- --nocapture
cargo check --lib --features test-utils
```

Expected: every filter executes at least one test and passes; Rust shared library compiles.

- [ ] **Step 9: Run formatting/lint for changed surfaces**

From the repository root:

```bash
pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
```

From `src-tauri/`:

```bash
cargo fmt --all -- --check
cargo clippy --lib --features test-utils -- -D warnings
```

Expected: all checks PASS with no formatting diff and no warnings.

- [ ] **Step 10: Commit Task 5**

```bash
git add -- .agents/skills/brainstorm-to-delivery/SKILL.md .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs src-tauri/tests/delegation_session_reuse_integration.rs
git commit -m "feat(skill): route brainstorm delivery by task risk"
```

- [ ] **Step 11: Write the Task report**

Create `.superpowers/sdd/b2d-generic-task-agent-routing/task-5-report.md` with the v2 contract result, scenario coverage, exact commands/outcomes, commit hash, and retained Minors. Do not stage it.

---

## Final Verification and Review

After Task 5's independent primary and auxiliary reviews approve its latest producer result:

- [ ] Re-read the approved Design, this Plan/routing block, all five Task reports, commits, and the complete branch diff.
- [ ] Run the Task 5 Step 8 and Step 9 commands again against final HEAD; record exact test counts and outcomes.
- [ ] Run `git status --short --branch` and verify only the ignored `.superpowers/sdd/**` reports remain outside committed Task changes.
- [ ] Dispatch a fresh independent Codex final reviewer on the complete branch. It must inspect spec coverage, Skill/validator contradiction resistance, legacy key/progress compatibility, non-blocking projection warnings, and producer/reviewer independence.
- [ ] Return each Critical/Important final finding to its owning Task producer work unit: Tasks 1-5 to their Codex implementer. After a fix, rerun every reviewer required by that Task's high route and then continue the same final-review work unit.
- [ ] Retain a Minor only with a concrete reason in the final-review ledger. Complete delivery only when covering checks and final review approve the same repository state.

## Recovery and Rollback Boundaries

- If Task 1 cannot preserve legacy five-part parsing, stop before the Skill emitter switch; do not migrate archived keys.
- If routing/progress parsing is malformed or oversized, keep safe Plan tasks/progress partial state and warnings; never convert it into admission authority.
- If routed projection cannot prove a valid expected route, fall back to the legacy aggregate display for that Task with `simple_plan_routing_invalid`; do not invent Agent identity.
- If the selected Task Agent/profile becomes unavailable, keep its generation and route recorded and block/defer according to the Skill. Do not rewrite it to Grok.
- If Task 5 validator/Skill integration fails, revert only the uncommitted Task 5 emitter changes; Tasks 1-4 are backward-compatible readers/projectors and may remain safely on the branch.
- No database rollback, manifest conversion, frontend migration, or data rewrite is required by this plan.
