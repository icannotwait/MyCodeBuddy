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

