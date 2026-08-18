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

