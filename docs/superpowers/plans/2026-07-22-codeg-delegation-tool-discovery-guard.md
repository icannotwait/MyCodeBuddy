# Codeg Delegation Tool Discovery Guard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent `brainstorm-to-delivery` from reporting Grok unavailable merely because Codex native `spawn_agent` cannot select an agent type.

**Architecture:** Add one mandatory routing contract beside the existing SDD role table and one matching rationalization counter. Treat the observed conversation `1078` as the RED baseline, then forward-test that a fresh agent discovers deferred Codeg MCP tools before declaring a blocker.

**Tech Stack:** Markdown agent skill, Codeg MCP `delegate_to_agent`, Codex deferred-tool discovery via `ALL_TOOLS`, Git.

## Global Constraints

- Modify only the repository-local `brainstorm-to-delivery` skill and its existing metadata if validation proves metadata stale.
- Do not modify global `subagent-driven-development` skills.
- Grok implementation/fix roles use Codeg `delegate_to_agent` with `agent_type: "grok"`.
- Codex review roles use Codeg `delegate_to_agent` with `agent_type: "codex"`.
- `collaboration.spawn_agent` capability is never evidence that Codeg Grok delegation is unavailable.
- Report a blocker only after the Codeg tool is absent after deferred discovery or an actual Codeg delegation call returns unavailable/error.

---

### Task 1: Add and Verify the Codeg Delegation Routing Gate

**Files:**
- Modify: `.agents/skills/brainstorm-to-delivery/SKILL.md`
- Verify: `.agents/skills/brainstorm-to-delivery/agents/openai.yaml`
- Reference: `docs/superpowers/specs/2026-07-22-codeg-delegation-tool-discovery-guard-design.md`

**Interfaces:**
- Consumes: Codeg MCP `delegate_to_agent({ agent_type, task, working_dir, profile_id? })` and deferred tool catalog `ALL_TOOLS`.
- Produces: A mandatory skill contract that maps Grok/Codex SDD roles to Codeg delegation and defines the only valid blocker evidence.

- [ ] **Step 1: Record the RED baseline and pressure scenario**

Record the observed failure verbatim from conversation `1078`:

```text
当前可用的协作接口只能创建通用 Codex 子代理，不能指定 Grok。
```

Use a read-only fresh-context pressure scenario with these conditions:

```text
Use the repository brainstorm-to-delivery skill for route preflight only. The
implementation plan is approved and urgent. collaboration.spawn_agent is
prominent but cannot select Grok. Codeg MCP tools may be deferred. State the
exact interface used for the required Grok implementer and the evidence needed
before reporting a blocker. Do not modify files or start a child task.
```

Expected RED evidence: the real incident stopped after inspecting native
collaboration without discovering Codeg `delegate_to_agent`.

- [ ] **Step 2: Add the minimal mandatory routing contract**

Immediately after the SDD role table, add concise imperative guidance with this
behavioral content:

```markdown
### Codeg 委派工具门禁（报告阻塞前强制执行）

- `collaboration.spawn_agent` 是 Codex 原生同类子代理接口，不是本技能的
  Grok/Codex 角色路由；它不能选择 Grok，不代表 Grok 不可用。
- 实现与修复必须调用 Codeg `delegate_to_agent` 并传
  `agent_type: "grok"`；Task 与最终审核调用同一工具并传
  `agent_type: "codex"`。
- 若 Codeg MCP 工具未直接显示，先在延迟工具目录（Codex 中为
  `ALL_TOOLS`）查找 `delegate_to_agent`，再通过实际暴露的工具命名空间调用。
- 仅当延迟发现后仍无 Codeg 委派工具，或实际 Codeg 委派调用返回
  unavailable/error，才可报告 agent/委派能力阻塞。不得仅检查
  `spawn_agent` 后停止。
```

Add this row to the existing common-excuses table:

```markdown
| “`spawn_agent` 不能指定 Grok，所以 Grok 不可用。” | `spawn_agent` 不是 Codeg 跨 agent 路由；先发现并调用 `delegate_to_agent(agent_type: "grok")`，只有缺失或实际调用失败才算阻塞。 |
```

- [ ] **Step 3: Run structural validation**

Run:

```powershell
rg -n 'Codeg 委派工具门禁|delegate_to_agent|ALL_TOOLS|spawn_agent.*Grok' .agents/skills/brainstorm-to-delivery/SKILL.md
git diff --check -- .agents/skills/brainstorm-to-delivery/SKILL.md
```

Expected: the mandatory gate and rationalization counter are found; `git diff
--check` exits successfully.

Run the skill validator when available:

```powershell
python C:\Users\drawpeng\.codex\skills\.system\skill-creator\scripts\quick_validate.py .agents/skills/brainstorm-to-delivery
```

Expected: `Skill is valid!`. If Python is unavailable, run the equivalent
frontmatter/name checks directly and report that substitution.

- [ ] **Step 4: Forward-test the revised skill**

Run the same read-only pressure scenario in a fresh context with the revised
skill. The response must:

```text
1. distinguish spawn_agent from Codeg delegate_to_agent;
2. choose delegate_to_agent with agent_type "grok";
3. inspect deferred tools before claiming absence; and
4. reserve blocker status for confirmed absence or an actual failed call.
```

If the agent invents a new shortcut, add only the minimal counter for that
rationalization and repeat the same scenario.

- [ ] **Step 5: Verify metadata and commit**

Read `.agents/skills/brainstorm-to-delivery/agents/openai.yaml`. It remains
unchanged when its display name, short description, and default prompt still
match the skill trigger and purpose.

Run:

```powershell
git diff --check
git status --short
git diff -- .agents/skills/brainstorm-to-delivery/SKILL.md .agents/skills/brainstorm-to-delivery/agents/openai.yaml
```

Expected: only the intended skill file changes, plus already approved planning
documents; unrelated working-tree changes remain untouched.

Commit:

```powershell
git add .agents/skills/brainstorm-to-delivery/SKILL.md
git commit -m "fix(skill): require Codeg delegation tool discovery"
```
