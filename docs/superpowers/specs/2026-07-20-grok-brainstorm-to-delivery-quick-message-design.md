# Grok Brainstorm-to-Delivery Quick Message

Date: 2026-07-20

Status: Design approved in brainstorming; awaiting final document review

## Summary

Define one reusable Codeg quick message for Grok sessions that turns an already
completed Brainstorm document into a locally deliverable implementation. The
message must carry the work through implementation planning, independent
review, implementation, verification, review-driven fixes, and local commits.
It must not stop after analysis or planning unless a defined hard gate requires
the user's decision.

The workflow is adaptive rather than uniformly heavyweight. Brainstorm review
is conditional, implementation-plan review is mandatory, and code-review
frequency scales with implementation risk. Additional user-supplied reviewers
may join document review, but every Task-level, milestone, and final code review
is reserved for `[@Codex CLI](codeg://agent/codex)`.

## Source Experience

The design is distilled from conversation 401, "首条慢回复时的标题生成方案讨论".
That session reached a high-quality result through these gates:

1. Complete and approve the Brainstorm design.
2. Independently review the design and resolve Important findings.
3. Write a concrete implementation plan.
4. Independently review and repair the plan.
5. Implement in bounded Tasks.
6. Review risky Tasks, fix Important findings, and re-review.
7. Run a whole-change review and fresh verification before claiming completion.

The reusable lesson is not that every task needs the longest possible process.
It is that every high-risk handoff needs an explicit entry condition, exit
condition, reviewer, and repair loop.

## Goals

- Accept an already completed Brainstorm file as the requirement baseline.
- Continue autonomously from that file to a verified local implementation.
- Review the Brainstorm only when its scope or risk warrants it.
- Always independently review the implementation plan before execution.
- Allow optional user-specified models to review documents in parallel.
- Reserve all implementation-code reviews for
  `[@Codex CLI](codeg://agent/codex)`.
- Stop before implementation when the existing dirty worktree creates material
  risk, leaving the decision with the user.
- Adapt direct execution, Subagent-Driven execution, and worktree usage to the
  actual task and repository.
- Produce local commits without merging, pushing, or creating a pull request.
- Require fresh verification evidence and cleared Critical / Important review
  findings before reporting success.

## Non-goals

- Re-running Brainstorm by default.
- Requiring a worktree for every implementation.
- Copying a large repository into a worktree when dependency or build cost
  makes that counterproductive.
- Using optional document reviewers for Task-level or final code review.
- Automatically stashing, committing, reverting, or discarding user changes.
- Automatically merging, pushing, or opening a pull request.
- Treating a written plan, partial implementation, stale test result, or
  unresolved Important finding as a completed delivery.

## Confirmed Decisions

| Area | Decision |
| --- | --- |
| Message form | Structured adaptive execution protocol |
| Input | A Brainstorm file referenced in the same user message |
| Default autonomy | Continue through local delivery without routine approval prompts |
| Material ambiguity | Pause only when it changes scope, architecture, or user data |
| Minor ambiguity | Make and report a conservative assumption |
| Brainstorm review | Conditional, based on scope, risk, ambiguity, and prior review evidence |
| Plan review | Mandatory before implementation |
| Optional reviewers | May join Brainstorm and plan review only |
| Code reviewer | Only `[@Codex CLI](codeg://agent/codex)` |
| Code-review depth | Adaptive: final-only, risk milestones, or every Task |
| Dirty-worktree gate | Immediately before executing the reviewed implementation plan |
| Dirty-worktree threshold | Evidence-based judgment, not a fixed file or line count |
| Worktree | Optional; avoid for large or expensive repositories |
| Completion | Implemented, freshly verified, reviewed, repaired, and locally committed |
| External integration | No merge, push, or pull request |

## Input Contract

The user sends the quick message together with a reference to one completed
Brainstorm file.

The user may also add a line beginning with `并行审核模型：` followed by one or
more Codeg agent references. Those agents join the document-review group:

- Brainstorm review, when the conditional review gate fires.
- Implementation-plan review, which always fires.

The document-review group always contains
`[@Codex CLI](codeg://agent/codex)`. Optional reviewers do not participate in
Task, milestone, or final implementation-code reviews.

## Workflow

### 1. Context and conditional Brainstorm review

Grok reads repository instructions, the referenced Brainstorm file, relevant
code and tests, and recent changes. It self-reviews the document for
completeness, consistency, implementability, and scope.

The document-review group reviews the Brainstorm in parallel when one or more
of these conditions apply:

- Cross-module architecture or a large change surface.
- Data migration, concurrency, security, or similarly high-risk behavior.
- Material ambiguity or internal contradiction.
- Missing evidence that a high-risk design has already been independently
  reviewed.

All reviewers must finish before findings are synthesized. Findings are
deduplicated and judged against repository constraints and code evidence; no
reviewer has automatic precedence. Every valid Critical / Important finding is
fixed and sent back to the original review group. A change that materially
alters requirements, scope, or architecture triggers a user decision instead
of an autonomous rewrite.

### 2. Mandatory implementation-plan review

Grok writes an executable plan with bounded Tasks, dependencies, file touch
points, tests, risks, and completion criteria. The complete document-review
group reviews the plan in parallel every time.

Grok waits for all reviewers, synthesizes their findings, fixes every valid
Critical / Important item, and submits the revised plan to the original group
until none remain. Minor findings are fixed or retained with a reason.

If implementation later requires a material plan change, the revised plan goes
through the same mandatory review loop before work continues.

### 3. Pre-implementation worktree gate

Only after the plan is reviewed, and immediately before executing it, Grok
inspects `git status` and the relevant diff. It judges risk from:

- Number of changed files.
- Diff size and distribution.
- Overlap with planned touch points.
- Whether the origin and ownership of existing changes are clear.

If uncommitted changes are substantial, overlapping, or unclear, Grok stops,
shows concise evidence, and asks the user whether to proceed in that state.
Grok must not autonomously stash, commit, overwrite, restore, or discard those
changes.

### 4. Adaptive implementation

Grok chooses direct execution or Subagent-Driven execution from task size,
independence, and risk. It does not default to a worktree. A large repository,
large dependency tree, expensive build, or expensive copy favors careful work
in the current workspace. A worktree is considered only when the repository is
an appropriate size and isolation provides a clear benefit.

Implementation follows repository instructions and applicable skills. Tests
are written first where practical, each Task gets targeted verification, and
unrelated refactoring is excluded.

### 5. Adaptive code review

| Implementation risk | Required review |
| --- | --- |
| Small | Final review by `[@Codex CLI](codeg://agent/codex)` |
| Medium | High-risk milestone reviews plus final review by `[@Codex CLI](codeg://agent/codex)` |
| High or complex | Every Task plus final global review by `[@Codex CLI](codeg://agent/codex)` |

Optional document reviewers are excluded from this stage. Grok implements the
fixes so the independent reviewer does not replace the implementer. Every
Critical / Important finding is repaired and re-reviewed until cleared. Minor
findings are fixed or reported with a retention reason.

### 6. Verification and local delivery

Each Task gets targeted checks. The final change receives tests, lint, builds,
and repository-required checks proportionate to its blast radius. Any review
fix invalidates prior evidence and requires fresh verification.

Before committing, Grok checks the final diff and stages only task-owned
changes. If user changes cannot be separated safely, it stops for a decision.
The terminal state is local commits with no merge, push, or pull request.

The final report contains the outcome, key changes, exact verification commands
and results, document- and code-review conclusions, retained Minor findings or
risks, local commits, and workspace location. A blocked or incomplete outcome
must be reported as such.

## Final Quick Message

Suggested title: `按 Brainstorm 端到端交付`

```text
请以本消息引用的、已经完成的 Brainstorm 文件作为需求基线，将任务持续推进到可本地交付的高质量结果。

若本消息通过“并行审核模型：”额外指定了一个或多个 agent，则将其与 [@Codex CLI](codeg://agent/codex) 组成文档审核组。额外模型只能审核 Brainstorm 和实施计划，不得参与逐 Task、里程碑或最终代码审核。未指定时，文档审核组仅包含 [@Codex CLI](codeg://agent/codex)。

不要重复 Brainstorm，不要停在分析或计划阶段，也不要在普通步骤中反复询问是否继续。除下述硬门禁外，自主完成计划、实施、验证、审核、修复和本地提交。

1. 理解与 Brainstorm 审核
- 阅读项目指令、Brainstorm 文件、相关代码、测试和近期变更。
- 自检 Brainstorm 的完整性、一致性、可实施性和范围。
- 若涉及跨模块架构、迁移、并发、安全、较大改动，或存在歧义、矛盾、缺少审核证据，则由文档审核组并行审核。
- 等待全部审核完成，按项目约束和代码证据去重、分级、裁决，不设置模型优先级。
- 修复所有有效的 Critical / Important，并交原审核组复审至清零。
- 若修复会实质改变需求、范围或架构，暂停并请用户决定；小歧义采用保守假设并记录。

2. 实施计划
- 编写具体、可执行的实施计划，明确任务拆分、依赖、文件触点、测试、风险和完成标准。
- 实施计划必须由文档审核组并行审核。
- 汇总并修复所有有效的 Critical / Important，再交原审核组复审至清零。
- Minor 应修复或记录保留理由。
- 实施中若计划发生实质变化，先修订计划并重新审核。

3. 实施前工作区门禁
- 仅在正式执行实施计划前检查 git status 和 diff。
- 综合判断未提交文件数量、diff 规模、与计划触点的重叠程度及改动来源。
- 若未提交改动较多、存在重叠或来源不明，立即暂停，展示简洁证据并交由用户选择。
- 不得擅自 stash、提交、覆盖、还原或丢弃用户改动。

4. 执行策略
- 根据复杂度选择直接实施或 Subagent-Driven，不机械套用重流程。
- 不默认使用 worktree。大型项目或复制、依赖、构建成本较高时，直接在当前工作区谨慎实施；仅在规模合适且隔离收益明显时考虑 worktree。
- 遵守项目规范和适用技能；尽可能先补测试再实现。
- 保持改动聚焦，不进行无关重构，不覆盖用户已有修改。

5. 代码审核
- 小任务：完成后由 [@Codex CLI](codeg://agent/codex) 进行最终审核。
- 中型任务：由 [@Codex CLI](codeg://agent/codex) 审核高风险里程碑，并进行最终审核。
- 高风险或复杂任务：每个 Task 完成后由 [@Codex CLI](codeg://agent/codex) 审核，并进行最终全局审核。
- 所有代码审核仅允许使用 [@Codex CLI](codeg://agent/codex)；当前 Grok 会话负责修复。
- Critical / Important 必须修复并复审至清零；Minor 应修复或说明保留原因。

6. 验证与交付
- 每个任务运行针对性检查，最终运行与改动范围相称的测试、lint、构建及项目要求的检查。
- 审核修复后必须重新验证，不能用旧结果宣称通过。
- 检查最终 diff，只提交本次任务产生的修改，不得夹带用户改动；若无法安全拆分，暂停询问。
- 创建本地提交，但不要合并、推送或创建 PR。
- 只有实现完成、相关验证通过、所有审核均无未解决的 Critical / Important、修改已安全提交时，才能宣称完成。

最终报告必须包含：完成结果、关键改动、验证命令及结果、文档与代码审核结论、残留 Minor/风险、本地提交和所在工作区。若未完成，明确说明阻塞点，不得包装成成功。
```

## Success Criteria

1. The message can be stored verbatim as a Codeg quick message.
2. A referenced Brainstorm file is treated as the baseline rather than
   automatically restarted.
3. Optional parallel reviewers affect document review only.
4. The implementation plan is always reviewed before execution.
5. The dirty-worktree decision gate runs after planning and before code edits.
6. Large repositories are not pushed toward worktrees by default.
7. Code-review depth scales with risk and only
   `[@Codex CLI](codeg://agent/codex)` performs those reviews.
8. Critical / Important findings require repair and re-review.
9. Completion requires fresh verification and task-owned local commits.
10. No merge, push, or pull request occurs automatically.
