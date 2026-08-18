### Spec Compliance

- `FAIL`: The shared extractor does not enforce exact marker boundaries (`src-tauri/src/acp/delegation/workflow/simple_parse.rs:395`) and diverges from the repository’s Markdown fence semantics (`src-tauri/src/acp/delegation/workflow/simple_parse.rs:331`).
- `Cannot verify`: The RED-before-implementation chronology is only asserted in `task-2-report.md:47` and `task-2-report.md:70`; the final diff cannot establish test ordering. This does not independently block approval.

### Strengths

- Required routing/progress models and byte limits were added without introducing platform workflow authority.
- Routing failures retain parsed Plan tasks and emit bounded warnings.
- Legacy progress fields remain optional, while canonical legacy and six-part keys remain readable.
- Tests cover requested bounds, safe partial behavior, unknown states, additive metadata, and distinct reviewer slots.

### Issues

#### Critical

- None.

#### Important

- `src-tauri/src/acp/delegation/workflow/simple_parse.rs:395`: Marker recognition uses only `starts_with(marker)`, contrary to the exact-marker requirement in `task-2-brief.md:193`. Prefix lookalikes such as `<!-- codeg-b2d-routing-v10` or `<!-- codeg-simple-progress-v1-extra` are counted as v1 blocks. If such a block precedes a real v1 block, the parser selects the lookalike body, emits misleading warnings, and discards valid metadata. Require a documented delimiter boundary after the marker, then add routing and progress tests covering version/prefix lookalikes followed by a valid marker.

#### Minor

- `src-tauri/src/acp/delegation/workflow/simple_parse.rs:331`: A backtick run is always treated as a fence opener, even when its info string contains a backtick. CommonMark rejects that opener; the repository’s unchanged Markdown parsing is based on `pulldown_cmark` at `src-tauri/src/acp/delegation/workflow/plan_material.rs:7`. This can silently hide a later live marker until a closing-fence-looking line. Reject backticks in the remainder of backtick fence openers and add an edge-case test.

### Assessment

- Task quality: `Needs fixes`
- Reasoning: The bounded models and safe-partial behavior are otherwise solid, but prefix markers can displace real routing or progress metadata. That violates the extractor’s binding exact-marker contract.