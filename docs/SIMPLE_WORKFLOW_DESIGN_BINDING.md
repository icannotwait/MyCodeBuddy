# Simple workflow Design binding

`register_simple_workflow` registers the current Plan and progress paths. Its
optional `design_rel_path` identifies the Design document whose active review or
fixer runs belong to that workflow. Paths use the same workspace-relative
normalization and validation as `plan_rel_path`.

| Registration input | Result |
| --- | --- |
| `"design_rel_path": "docs/design.md"` | Bind or replace the current Design |
| `"design_rel_path": null` | Explicitly clear the binding |
| Field omitted, same normalized Plan path | Preserve the existing binding |
| Field omitted, different Plan path | Clear the old binding |
| Legacy registration or migrated row without a binding | Remain unbound |

When renaming/re-registering a Plan while keeping the same Design, pass the
Design path explicitly. Supplying the same normalized locators and binding is
idempotent. Invalid Design paths reject the whole registration without changing
the existing row. The registration response includes the effective
`design_rel_path` (a normalized string or null).

Design reviewer and fixer cards from the parent conversation remain visible as
observed history. Only runs for the bound Design can change the current nodes,
current phase, or overall workflow state. An unrelated or unbound active Design
therefore cannot reopen completed task work. Active runs for the current Plan
continue to affect workflow state, and Plan cards for other paths remain
excluded. A blocked task/final-review state retains its existing precedence.

The additive database migration leaves existing rows unbound rather than
guessing an association from filenames or timestamps. Corrupt stored Design
paths are ignored for current-state selection and reported by the safe
`simple_design_invalid_path` projection warning. Design paths are not added to
the frontend graph DTO.

Focused regression command (from the repository root):

```sh
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features \
  --features test-utils --test simple_workflow_document_scope_integration
```
