# Windows x64-Only Release Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove Windows ARM64 from all future MyCodeBuddy release builds while preserving Windows x64 desktop and server artifacts.

**Architecture:** Keep the existing matrix-based desktop job so target-qualified sidecar and Tauri arguments remain unchanged, but reduce the matrix to its x64 entry. Make the release-policy validator authoritative by allowing only `x86_64-pc-windows-msvc`, and align tests and operator documentation with that policy.

**Tech Stack:** GitHub Actions YAML, Node.js release-policy checks, `node:test`, Markdown

## Global Constraints

- Future desktop releases target `x86_64-pc-windows-msvc` only.
- The already-published `v0.20.2-mycodebuddy.1` release remains unchanged.
- Windows x64 desktop updater artifacts and the Windows x64 server ZIP remain required.
- Do not touch unrelated in-progress CodeBuddy or bundled Codex ACP files.

---

### Task 1: Enforce x64-only release policy

**Files:**
- Modify: `scripts/release-policy.test.mjs`
- Modify: `scripts/release-policy.mjs`
- Modify: `.github/workflows/release.yml`
- Modify: `docs/RELEASING_WINDOWS.md`

**Interfaces:**
- Consumes: `assertWindowsReleaseWorkflow(workflowText)` and the existing desktop matrix contract.
- Produces: A release policy and workflow that accept exactly `x86_64-pc-windows-msvc` and reject `aarch64-pc-windows-msvc`.

- [ ] **Step 1: Write the failing policy tests**

Remove the ARM64 entry from `validWindowsWorkflow`, change the repository workflow assertion to reject `Windows ARM64` and `aarch64-pc-windows-msvc`, and include `aarch64-pc-windows-msvc` in the unsupported-target test cases.

- [ ] **Step 2: Run the focused test to verify it fails**

Run: `node --test scripts/release-policy.test.mjs`

Expected: FAIL because the production policy still requires ARM64 and the repository workflow still contains the ARM64 matrix entry.

- [ ] **Step 3: Implement the minimal x64-only policy**

Set `WINDOWS_RELEASE_TARGETS` to only `x86_64-pc-windows-msvc`, remove the ARM64 desktop matrix entry and redundant `max-parallel: 1`, and update the release guide to require x64 desktop artifacts only.

- [ ] **Step 4: Run release verification**

Run:

```bash
pnpm test:release
pnpm release:check
git diff --check
```

Expected: all commands exit 0, with 44 release tests passing and the repository configuration accepted.

- [ ] **Step 5: Commit only the release-policy files**

```bash
git add docs/superpowers/plans/2026-07-13-windows-x64-only-release.md \
  .github/workflows/release.yml \
  scripts/release-policy.mjs \
  scripts/release-policy.test.mjs \
  docs/RELEASING_WINDOWS.md
git commit -m "build(release): drop Windows ARM64 artifacts"
```

- [ ] **Step 6: Push the policy for future releases**

Run: `git push origin main`

Expected: the new commit is present on `origin/main`; no new release tag is created.
