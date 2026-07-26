---
name: brainstorm-to-delivery
description: Use when a Codeg conversation provides a completed Brainstorm file and asks for a high-quality locally deliverable implementation.
---

# Brainstorm 到本地交付

将本次消息引用的、已经完成的 Brainstorm 文件视为需求基线。不要重新或重复
Brainstorm，也不要停在分析或计划阶段；除明确的硬门禁外，自主推进到可本地
交付的结果。

## 强制执行模式与角色（最高优先级）

**REQUIRED SUB-SKILL:** 实施阶段必须调用并完整遵守
`subagent-driven-development`。这是唯一实施路径；不得由父会话直接实现，也不得只
模仿该技能的部分步骤。违反这些角色和流程的字面要求，就是违反其目的。

| SDD 角色 | Codeg 路由 | 强制要求 |
| --- | --- | --- |
| Task 实现者 | [@Grok](codeg://agent/grok) / `agent_type: "grok"` | Task N **首次**实现：`delegate_to_agent` 新建 Grok 线程。同一 Task 的追问与修复优先 `continue_delegation`。 |
| Task 修复者 | [@Grok](codeg://agent/grok) / `agent_type: "grok"` | 继续该 Task 的实现者线程；不得为修复新开无关 Grok 会话。 |
| Task 独立审核者 | [@Codex CLI](codeg://agent/codex) / `agent_type: "codex"` | Task N **首次**审核：新建独立 Codex 线程。同一 Task 复审优先 `continue_delegation`。只读。 |
| 最终全局审核者 | [@Codex CLI](codeg://agent/codex) / `agent_type: "codex"` | **始终**新建独立 Codex 线程；禁止复用任何 Task 审核者线程。意外中断后仅可继续**该**最终审核线程。只读。 |
| 最终修复者（Final fixer） | [@Grok](codeg://agent/grok) / `agent_type: "grok"` | 仅当最终审核 verdict 为 `request_changes` / `block` 时新建（或 continue 已有）Grok Final fixer 线程；**禁止**复用 Task 实现者线程。Final 复审须在 fixer 终端通过后再 `continue_delegation` 同一 Final 审核者。 |

### Codeg 委派工具门禁（报告阻塞前强制执行）

本表的 `@Grok` / `@Codex CLI` 是 Codeg 跨 agent 路由，不是 Codex 原生
`collaboration.spawn_agent`。按此顺序执行：

1. 实现与修复调用 Codeg `delegate_to_agent` 并传 `agent_type: "grok"`；Task
   审核与最终审核调用同一工具并传 `agent_type: "codex"`。同一工作单元的追问、
   修复与复审优先 `continue_delegation`（见下方会话复用契约）。
2. 若 Codeg MCP 工具未直接显示，先在延迟工具目录中发现它们（Codex 中为
   `ALL_TOOLS`，查找 `mcp__codeg_mcp__delegate_to_agent` 与
   `mcp__codeg_mcp__continue_delegation`），再通过实际暴露的工具命名空间调用。
3. `spawn_agent` 不能选择 Grok 只说明该原生接口不能选模型，不代表 Grok 不可用。
4. 仅当延迟发现后仍无兼容的 Codeg 委派工具，或实际 Codeg 委派调用返回
   unavailable/error，才可报告 agent 或委派能力阻塞。

父会话只负责任务简报、上下文答疑、结果裁决、进度账本（含 thread ledger）和验证协调；父会话不得亲自
实现或修复 Task 代码。若计划、隔离工作区、委派能力或指定 agent 不满足 SDD 前置
条件，暂停并报告阻塞；不得切换为直接实施或替换 agent 类型。

## 输入与审核组

- 先阅读项目指令、Brainstorm、相关代码和测试、以及近期变更。
- 用户可添加一行 `并行审核模型：...`。其中的 agent 与
  [@Codex CLI](codeg://agent/codex) 组成文档审核组；未提供时，文档审核组
  仅含 [@Codex CLI](codeg://agent/codex)。
- 可选 agent 只能审核 Brainstorm 和实施计划，不能审核 Task、里程碑或最终
  代码。所有独立 Task、修复复审和最终代码审核只由
  [@Codex CLI](codeg://agent/codex) 执行。Grok 实现者仍须完成 SDD 要求的自审，
  但自审不能替代独立 Codex 审核。
- 所有审核结论都要根据项目约束和代码证据去重、分级和裁决，不给任何审核者
  预设优先级。

## 委托会话复用与恢复（Skill 路由契约）

本技能通过 MCP 编排子会话时，**必须**使用 `work_unit_key` 参与平台预算与 lineage，
并在 SDD 进度账本中维护 durable **thread ledger**。禁止把每次修订都当成全新
冷启动 `delegate_to_agent`（除非该工作单元尚无已建立 lineage 的线程，或已按
替换规则启动了合法 replacement）。

### Thread ledger（进度账本）

在 `.superpowers/sdd/progress.md`（或等价 SDD 进度账本）中维护 thread 表。每个
可复用线程至少记录：

| 字段 | 说明 |
| --- | --- |
| `work_unit_key` | 见下表；与 MCP 调用中传入的 A1 key 一致（≤ 200 Unicode 标量） |
| 角色 / agent_type | 如 implementer / reviewer + `grok` / `codex` / 可选文档审核 agent |
| profile_id | 不可变 profile 身份；无 profile 时记 `none` |
| child_conversation_id | 子会话 id |
| latest_task_id | 该线程**最新**一次 run 的 task id（后续 `continue_delegation` 的目标） |
| state | 活跃 / 终态 / 已替换 等 |
| recovery_count | 本工作单元已用的意外中断 continue 次数（Skill 侧计数，可严于平台） |
| replacement | 若发生替换：`replaced_task_id`、`replacement_reason`、新 child id |

Workflow 行（v1 capability 激活时同步维护，可与 thread 表同文件）：

| 字段 | 说明 |
| --- | --- |
| `workflow_id` | `publish_workflow_manifest` 返回值 |
| `publication_token` | skeleton 创建时的 UUID（A3/B8）；见下 |
| `manifest_revision` / `graph_revision` | 最近一次成功 publish 的 CAS 修订 |
| capability mode | `legacy` / `v1` |
| 最近 gate settlement | gate_id、cycle、outcome |
| design/plan `rel_path` + digest | 与 manifest 一致的归一化相对路径与文档摘要 |

Compaction 或父会话压缩后，恢复编排时**只**依据 ledger + 平台 durable run/budget
行 + `get_workflow_state`，不得仅凭记忆重放已完成的委派序列。

### `work_unit_key` 材料（A1 规范，强制）

Skill 构造的 key 必须稳定、可复现，并与角色 / `agent_type` / profile 绑定。
**禁止**绝对路径；路径字段必须是 **workspace-relative**，按 B1 归一化后再入 key：
UTF-8 **NFC**、路径分隔符 → `/`、Windows 下路径字段小写、拒绝 `|` / 控制字符 /
`..` / 空组件。材料格式（`|` 分隔）：

| 工作单元 | `work_unit_key` 材料（A1） | 黄金向量示例（`workflow::key`） |
| --- | --- | --- |
| Design 审核 | `design\|{rel_doc_path}\|reviewer\|{agent_type}\|{profile_id\|none}` | `design\|docs/superpowers/specs/x.md\|reviewer\|code_buddy\|a1c14cde-f9c0-4fce-9d7f-66c3f8e85039` |
| Plan 审核 | `plan\|{rel_plan_path}\|reviewer\|{agent_type}\|{profile_id\|none}` | `plan\|docs/superpowers/plans/p.md\|reviewer\|codex\|none` |
| Task 实现者 | `task\|{task_index}\|implementer\|{agent_type}\|{profile_id\|none}` | `task\|2\|implementer\|grok\|none` |
| Task 审核者 | `task\|{task_index}\|reviewer\|{agent_type}\|{profile_id\|none}` | `task\|2\|reviewer\|codex\|prof-1` |
| 最终全分支审核 | `final_review\|reviewer\|{agent_type}\|{profile_id\|none}` | `final_review\|reviewer\|codex\|none` |
| 最终修复者 | `final_review\|fixer\|{agent_type}\|{profile_id\|none}` | `final_review\|fixer\|grok\|none` |

规则：

- `agent_type` 必须是 Codeg 枚举 **wire** 字符串（如 `grok`、`codex`、
  `code_buddy`），**不是**显示名（如 `Codex CLI`）。
- 无 profile 时字段字面量必须为 `none`。
- `task_index` 为正十进制整数，**禁止**前导零（`02` / `0` 非法）。
- 归一化后 key **≤ 200** Unicode 标量。路径字段**必须**与仓库内真实
  workspace-relative 路径一致（先 NFC + B1 归一化，再拼 key）。**禁止**为塞进
  200 上限而发明更短假路径、别名、截断路径或改用绝对路径；也不得截断 `|`
  字段结构。若真实相对路径过长导致 key 超限：先 **move/rename** 文档到更短的
  真实相对路径，再 **republish** manifest 并用新路径重建 key。文档 digest 是
  manifest 独立字段，**不**嵌入 key。
- Design 与 Plan 即使使用同一审核者 profile / agent，也是**不同**工作单元。
- Task N 与 Task N+1 使用不同 `task_index`，**禁止**跨 Task 复用线程。
- Final reviewer 与 Final fixer 是不同 key；Final fixer **仅**在最终审核要求
  修改时启用，默认 `agent_type: "grok"`。
- 可选文档审核 agent 仅参与 Design/Plan 文档审核组；不得成为 Task 或最终代码
  审核者 / Final fixer。
- Pre-A1 key（绝对路径、缺 `agent_type` 字段、旧 `final_review\|{branch_ref}\|…`
  形态）**不被** observed-only 识别；编排必须改用 A1 语法。
- 本技能路径下的编排调用一律带 A1 `work_unit_key`；无 key 的 ad-hoc 冷启动不
  属于 brainstorm-to-delivery 编排。

### 首次分派 vs `continue_delegation` 偏好

| 工作单元 | 首次分派 | 同一单元后续工作 |
| --- | --- | --- |
| Design + 审核者/profile | `delegate_to_agent` 新建审核线程 | 修订/复审 → **`continue_delegation`** 同一审核者 |
| Plan + 审核者/profile | `delegate_to_agent` 新建审核线程 | 修订/复审 → **`continue_delegation`** 同一审核者 |
| Task N + Grok 实现者 | `delegate_to_agent` 新建 Grok | 追问与修复 → **`continue_delegation`** 该 Grok |
| Task N + Codex 审核者 | `delegate_to_agent` 新建独立 Codex | Task N 复审 → **`continue_delegation`** 该 Codex |
| Task N+1 | 新建 Grok **与** 新建 Codex | **永不**复用 Task N 的线程 |
| 最终全分支审核 | **始终**新建 Codex（`final_review\|reviewer\|codex\|…`） | 意外中断 → continue **该**最终审核线程；**scoped re-review after Final fix** → 仅在 Final fixer 终端通过后 continue **同一** Final 审核者；**永不**继续 Task 审核者 |
| 最终修复（Final fixer） | 仅当 Final 审核非通过时 `delegate_to_agent` 新建 Grok（`final_review\|fixer\|grok\|…`） | 同 cycle 追问/修复 → continue 该 fixer；**禁止**复用任何 Task 实现者线程 |

**强制偏好：** 当 ledger 显示该 `work_unit_key` 已有可恢复线程（平台已对某 run
设置 `reached_running_at`，且最新终态 run 可 continue）时，父会话**必须**调用
`continue_delegation(task_id=ledger.latest_task_id, task=…)`，**禁止**再发无
`replaces_task_id` 的同 key `delegate_to_agent`（平台会以 `invalid_replacement`
等拒绝，且会绕过会话复用）。

Pre-admission 例外（lineage 尚未建立）：gen-1 在 `reserving` 阶段失败且从未
`reached_running_at` 时，可用**相同** `work_unit_key`、**不带** `replaces_task_id`
的 `delegate_to_agent` 重新首派；这不是 replacement，也不消耗 replacement 轨道。

### 替换（replacement）规则

平台**不会**从 `continue_delegation` 静默创建 replacement。仅当 continue 返回
类型化失败且原因属于可替换集合时，Skill 才可发起同角色/同 profile 的新子会话。

允许的 `replacement_reason`（且须与平台 durable 状态一致）：

| reason | 何时使用 |
| --- | --- |
| `unresumable` | 历史会话缺失、resume/load 失败、会话损坏、握手失败、external-id 不匹配、launch 配置/profile 不可用 |
| `budget_exhausted_continue` | 意外中断 continue 轨道已用尽（平台/Skill 均为 ≤ 2） |
| `not_supported` | 该 agent 类型未开启子会话复用能力（continue 返回 `not_supported`） |

发起 replacement 时 **必须** 同时提供：

1. `replaces_task_id` = 被替换线程最新终态 run 的 task id
2. `replacement_reason` ∈ 上表
3. 与原线程**相同**的 `work_unit_key`
4. 相同 agent_type 与 profile（不得换 agent）

然后用 `delegate_to_agent` 启动新的 generation-1 子会话。成功后更新 ledger：
新 `child_conversation_id`、新 `latest_task_id`、replacement 关系；被替换线程
标记为不可再 continue（后续对该旧 task id 的 continue 应为 `not_continuable`）。

**禁止**因下列原因发起 replacement 或换 agent：

- 业务/路由错误：`not_found`、`stale_task_id`、`busy_thread`、authorization、
  route-policy
- 所需 Grok 或 Codex 不可用 → **硬阻塞**，报告用户；不得替换 agent 类型
- 用户/父会话显式取消、来源不明的取消

Pre-admission replacement 重试：replacement gen-1 在进入 `running` 前死于
`reserving` 时，计数器未扣，可用相同 `replaces_task_id` / `replacement_reason` /
`work_unit_key` 再调 `delegate_to_agent`（不是 continue）。

### Skill 恢复预算（可严于平台，不可更宽）

与平台 lineage rails 对齐的 Skill 策略：

1. 初始 run **不**消耗 continue / replacement 预算。
2. 每个工作单元最多 **2** 次意外中断 `continue_delegation`（`unexpected_continue`）。
3. 预算经同一 `work_unit_key`（及平台 `lineage_root_task_id`）在原线程与
   replacement 之间共享。
4. 每个工作单元最多 **1** 次同角色/同 profile 的 fresh replacement。
5. 轨道耗尽后停止自动恢复，升级给用户。

平台在 `running` 入场时收费且不退款；Skill 侧 `recovery_count` 用于编排决策，
即使父会话压缩也须以 durable 状态为准。

### 恢复 prompt 语义（强制）

恢复（`continue_delegation` 或合法 replacement 的首轮 prompt）启动的是**新的
子 turn**，**不是**把中断前的进程指令原样续跑。父会话构造的 `task` 文本**必须**
明确要求子代理：

1. **重新检查**当前仓库与相关产物（git status、diff、报告文件、测试结果），
   以磁盘与命令输出为真相来源。
2. 将中断前的部分推理与记忆视为**临时/provisional**，不得当作已验证结论。
3. 若最终报告或 SDD 产物（如 `.superpowers/sdd/task-N-report.md`）**未**持久
   写入，必须**重新创建**完整报告；不得假设“已写过”。
4. **只读审核者**可复用会话中已积累的分析，但仍须对照当前仓库证据更新结论。
5. **实现者（Grok）**在声称完成前必须：
   - 审计可能残留的部分文件系统改动（未提交 diff、半成品文件）；
   - 重新运行覆盖性测试 / 项目要求的针对性验证；
   - 仅在证据充分后更新报告与 **validated terminal card summary**（见 A16）。

意外中断恢复示例意图（嵌入实际 `task` 时按角色裁剪）：

```text
Previous turn was interrupted. This is a NEW turn on the same work unit.
Re-inspect the repository and durable artifacts now. Treat any prior partial
reasoning as provisional. Recreate any final report that was not durably written.
If you are an implementer: audit partial filesystem changes and re-run covering
tests before reporting completion. Do not assume the interrupted instruction
already finished. Emit a validated terminal card summary before finishing.
```

## Workflow Graph 契约（capability / publish / settle / recovery）

本技能在 Codeg companion 支持 **workflow_manifest_v1** 时必须编排 manifest
与门禁；**不得**修改通用 `writing-plans` 或 `subagent-driven-development` 技能
文本。后端自动从 run 生命周期投影节点 running/completed；Skill **不**手写
实现者/审核者节点的运行态。

### Capability 发现（B9）

在**第一次**条件式 Design 审核分派之前，发现并记录 capability：

1. 调用 root companion 的 `get_workflow_capabilities`（只读；无 workflow id 也可）。
2. 核对工具目录与返回值一致。`workflow_manifest_v1` **要求**以下四者同时存在且
   一致：
   - `get_workflow_capabilities`
   - `get_workflow_state`
   - `publish_workflow_manifest`
   - `settle_workflow_gate`
3. 结果处理：

| 结果 | Skill 行为 |
| --- | --- |
| 能力工具与全部 v1 工具均缺失 | **legacy**：记录 mode=legacy，继续既有 Sessions/无 manifest 路径 |
| 能力返回 v1=false 且 mutation/recovery 工具缺失 | **legacy**（新 companion 关闭 v1） |
| 能力返回 v1=true 且四工具齐全 | **v1 mode**：后续 publish/settle/恢复均为硬门禁 |
| 其他组合 / 能力调用或校验失败 | **inconsistent companion**：硬阻塞；**不得**当 legacy 静默降级 |

v1 模式下每次 required 的 manifest/gate 失败（校验、所有权、持久化、授权）
都暂停并报告类型化错误。

### Manifest 生命周期（skeleton → estimated → approved）

v1 模式强制步骤（进度账本同步记录 `workflow_id`、`publication_token`、
`manifest_revision`、`graph_revision`、capability mode、最近 gate settlement）：

1. **Skeleton publish**（工作流入口、首次 Design 分派前）：
   - 生成一次 `publication_token`（UUID），写入 progress ledger，并随
     `publish_workflow_manifest` 提交（A3）。
   - `workflow_state=skeleton`，含 workflow 身份、从 prompt 已知的 Design/Plan
     审核组、高层 phase 顺序、Task/Final 占位。
   - Design 门可先用**临时** gate；在条件式 Design 审核决策后修订（见 A12）。
2. **A12 Design gate 定型**（skeleton 之后、Design 分派或 self-review settle 前）。
   Wire 字段 `resolution_mode` **仅允许**精确枚举（禁止中文或其他同义词）：
   `parent_adjudication` | `self_review`。
   - **有外部 Design 审核者**：`required_reviewer_node_ids` 列出全部 Design 审核
     work-unit 节点；`resolution_mode` **必须**为 `parent_adjudication`（父会话
     汇总并行审核结果后 settle；**不是** `self_review`）。
   - **零外部审核者（仅 Skill 自审 Design）**：canonical self-review shape：
     - `resolution_mode` **必须**为 `self_review`
     - `required_reviewer_node_ids = []`（空）
     - Design 文档 **rel_path + digest** 必须存在
     - **Plan gates 禁止**使用 `self_review` / 空 required 集合；Plan 门
       `resolution_mode` **必须**为 `parent_adjudication`
3. **Estimated publish**（`writing-plans` 写出计划后、**Plan 审核分派前**）：
   完整 Task 链 + Final reviewer（及可选 Final fixer 占位）的 estimated 修订。
4. **计划实质修订后、复审前**：CAS 发布新的 estimated 修订
   （`expected_manifest_revision`）。
5. **Gate settlement**：每个并发 Design/Plan 文档门禁，在父会话裁决后调用
   `settle_workflow_gate` 写入 adjudicated 结果（不得假设后端“自动通过”
   文档门）。self-review Design 门同样须显式 settle。
6. **Approved**：仅当完整 Plan 文档审核门通过后，将 manifest 标为
   `workflow_state=approved`。
7. **SDD 期间**：每个委派的 `work_unit_key` 必须匹配已批准（或当前合法阶段）
   manifest 节点的 role / agent / profile / task_index / 相对路径。
8. **approved 后实质改计划**：发布 demote 为 `estimated` 的新修订并重开 Plan
   gate cycle；未启动节点可替换；已 observed 节点保留绑定（B14 冻结规则见下）。

#### `publication_token`（A3 / B8）

| 场景 | 行为 |
| --- | --- |
| 首次 skeleton create | 新建 UUID → ledger + publish 载荷；无 `workflow_id` 的 create 依赖此 token 幂等 |
| 同 normalized digest 重试 / 幂等回放 | **复用** ledger 中同一 `publication_token`（及已返回的 `workflow_id`）；不得新造 token |
| 同一 token + **不同** normalized digest | 平台返回类型化 idempotency mismatch（B8）→ **硬停止**；`get_workflow_state` 重载真相，修正文档/路径/digest 后决定新 CAS 更新路径；**禁止**静默换 token 强行 create 第二个 active workflow |
| 后续 CAS 更新（已有 `workflow_id`） | 带 `workflow_id` + `expected_manifest_revision`；token 行为以平台为准，ledger 仍保留原 create token 供审计 |

`publish_workflow_manifest` 文档要点：`schema_version=1`、
`workflow_kind=brainstorm_to_delivery`、`publication_token`、可选 `workflow_id`、
CAS 修订、workspace-relative 显示路径 + document digest、稳定
phase/node/edge/gate id、委派节点的 role / agent_type / profile_id / A1
`work_unit_key`、gate 的 `resolution_mode` 与 `required_reviewer_node_ids`。

#### A15.2 文档 / 图 bounds（Skill 构造时遵守）

构造 manifest 时不得超过平台校验上限（与 UI 一致）：

| 边界 | 上限 |
| --- | --- |
| Tasks | ≤ 100 |
| nodes | ≤ 400 |
| edges | ≤ 800 |
| gates | ≤ 50 |
| adjudication summary | ≤ 4 KiB |
| card summary 字段 | 见下方 `card_summary.rs` 现有限制 |
| 归一化 manifest JSON | ≤ 512 KiB |

超限则拆分计划 / 减少占位节点，不得提交超大 manifest。

### Card summary 义务（A16）

对 **Design/Plan 审核者、Task 实现者、Task 审核者、Final fixer、Final 审核者**
的委派 prompt **必须**要求：在最终助手文本末尾附加 **一个** well-formed
`<!-- codeg-card-summary-v1 ... -->` HTML 注释块（与
`src-tauri/src/acp/delegation/card_summary.rs` 一致）。平台校验**最后**一个
合法块；缺失或无效 summary：

- 阻塞文档门 `settle_workflow_gate`（A2 要求 terminal + validated summary）；
- 阻塞 Task/Final execution-gate 前进（A7）。

**是否 validated** 以平台解析结果为准（父会话通过 run 终态 / 恢复时
`get_workflow_state` 的 card-summary 证据核对，不得仅凭子代理口头“已写
summary”前进）。

不得用自由文本 SHA 充当 artifact 覆盖证据（B3：权威在 run-binding）。

### 可选：父会话核对指引（v1 active；非每轮强制探针）

正常编排路径**已经**调用 capability 发现、skeleton/estimated publish、文档门
`settle_workflow_gate`，以及恢复时的 `get_workflow_state`。父会话应**消费这些
真实调用的返回值**（成功 revisions、类型化错误、validated summary 证据）来决定
是否前进——**禁止**为“自检”而每轮额外提交故意非法的 publish / settle 探针
（避免污染 manifest 修订、CAS 时钟与 gate cycle）。

可选（怀疑 companion/后端异常、排障或首次接入 v1 时）只读核对：

- 再调一次 `get_workflow_capabilities` / `get_workflow_state`（只读）对齐
  ledger；
- 对照失败 publish/settle 的类型化错误信息，而非重放坏载荷。

**后端契约不在本 Skill 文档任务中用假测试覆盖。** 非法 manifest 拒绝、
`resolution_mode` 枚举、A1 key、gate settle 前置条件等由既有自动化测试保证
（本工作流 Task 3 manifest 校验 / store 与 Task 5 admission / settle 相关
`cargo test` 套件；勿在 Skill 仓库为父会话编排添加伪造 publish 探测用例）。

#### Review 模板（Design / Plan / Task / Final 审核者）

Wire 字段：`kind=review`，`verdict` ∈
`approve` | `approve_with_minors` | `request_changes` | `block`，
`critical` / `important` / `minor` 为非负计数，`summary` ≤ 240 字符。

Pass 集合（execution / 文档门前进）：`approve`、`approve_with_minors`。

```html
<!-- codeg-card-summary-v1
{"kind":"review","verdict":"approve_with_minors","critical":0,"important":0,"minor":2,"summary":"Two Minor findings remain."}
-->
```

`request_changes` / `block` 示例（非 pass）：

```html
<!-- codeg-card-summary-v1
{"kind":"review","verdict":"request_changes","critical":1,"important":0,"minor":0,"summary":"Missing tests for gate settle path."}
-->
```

#### Implementation 模板（Task 实现者 / Final fixer）

Wire 字段：`kind=implementation`，`phase` ∈ `implementation` | `fix`，
`status` ∈ `done` | `done_with_concerns` | `blocked` | `needs_context`，
`summary` ≤ 240 字符；可选 `commits[]`（≤20，每项 `sha`≤64 / `subject`≤200）、
`tests`（`status`≤64，`passed`/`failed` 计数，`summary`）、`concerns[]`（≤20，
每项 ≤240）、`report_file`（workspace-relative，禁止 `..`）。

Pass 集合：`done`、`done_with_concerns`。首次实现用 `phase=implementation`；
修复轮用 `phase=fix`。

```html
<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done","summary":"Implemented the cleaning component and automation tests.","commits":[{"sha":"a1b2c3d","subject":"feat: add cleaning component"}],"tests":{"status":"passed","passed":14,"failed":0,"summary":"14/14 passing, output pristine"},"concerns":[],"report_file":".superpowers/sdd/task-3-report.md"}
-->
```

Fixer / 修复轮示例：

```html
<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"fix","status":"done_with_concerns","summary":"Addressed review Critical; one Minor deferred.","commits":[{"sha":"f00ba12","subject":"fix: gate settlement tests"}],"tests":{"status":"passed","passed":3,"failed":0,"summary":"targeted tests ok"},"concerns":["Minor: docs wording still rough"],"report_file":".superpowers/sdd/task-3-fix1-report.md"}
-->
```

### `get_workflow_state` 恢复（A5 / B4）

Compaction、父会话恢复或中断后续派前：

1. **先** `get_workflow_state`（及本地 progress ledger），**禁止**凭记忆重放
   publish 序列或新建第二个 active workflow。
2. 载荷含：`workflow_id`、capability mode、manifest state / revisions、
   相对路径 + digests、gate id/cycle/settlement、节点 role/agent/profile、
   `work_unit_key`、依赖 readiness，以及有界 per-node 证据（latest task_id /
   status / generation / replacement、card summary 是否已校验、artifact digests）。
3. 缺失的 durable 报告仍须重建；gate 与委派以平台状态为准。
4. Frontend 只用脱敏 `WorkflowGraphSnapshot`；agent 恢复读用本工具（可含 key）。

### Final fixer 与 Final 复审（A6 / B6）

v1 + approved（或 post-plan）manifest 下：

1. Final **reviewer** 首次分派仅当每个 **active** Task execution gate 已通过。
2. Final **fixer**（`final_review|fixer|grok|…`）仅在当前 Final cycle 的审核
   终端为非通过（`request_changes` / `block`）后才可首次分派。
3. Final **re-review** 只能在该 cycle 的 Final fixer 终端 **pass** 之后，对
   **同一** Final reviewer work unit `continue_delegation`（新 run / 新 gate
   cycle 评估；A2 新鲜度）。
4. Task 级修/复审仍走既有 implementer + reviewer 节点（continue，不新建节点）。
5. 若 Final 首轮即通过，Final fixer 节点可保持 estimated 未使用。

### B14.3 冻结 Task 对的取消 / 放弃

任一 Task implementer/reviewer 对中**一方**首次 admission（observed）后：

1. 双方绑定 **冻结**，计划修订**不得**静默退休/删除未 observed 的 partner。
2. 若计划不再需要该 Task，Skill **必须**二选一：
   - 在冻结对下完成该 Task 的 execution gate；或
   - 发布新的 manifest 修订：整体 `workflow_state=blocked`，和/或将该对节点
     `node_outcome=canceled`（**保留**双方 bindings）。
3. **禁止**静默 drop unobserved partner。
4. **禁止**仅靠“停止对话 / 会话结束”当作 durable cancel；恢复后仍须看到冻结
   绑定，直到显式 publish 记录 cancel/block。

## 工作流

### 1. 理解与条件式 Brainstorm 审核

在任何 Design 审核分派前完成 **capability 发现**（见上文）。v1 模式下：创建
`publication_token` → ledger → `publish_workflow_manifest`（skeleton）→ 按
条件式审核决策将 Design gate **定型**（A12：外部审核者集合，或
`self_review` + 空 required + design rel_path/digest），再分派或 self-settle。

自行检查 Brainstorm 的完整性、一致性、可实施性和范围。出现下列任一条件时，
由文档审核组并行审核 Brainstorm：跨模块架构或大改动面、迁移、并发、安全、
实质歧义或矛盾，或高风险设计缺少独立审核证据。若不触发外部审核，v1 仍须
A12 self-review Design gate + 显式 settle（不得跳过 manifest 形状）。

文档审核组中每个审核者/profile 使用独立 A1 `work_unit_key`（Design 单元，含
`agent_type`）。同一文档的修订复审优先 `continue_delegation` 对应审核线程。
每个审核者 prompt 必须要求 HTML card summary 模板（A16）；父会话用平台校验
结果确认 validated，不凭口头声明。

等待全部审核完成后再汇总。修复每个有效的 Critical 或 Important，并交回原
审核组复审，直到清零。Minor 要么修复，要么记录保留理由。父会话裁决后对
Design 门调用 `settle_workflow_gate`（v1）。

若修复会实质改变需求、范围、架构或用户数据处理方式，暂停并请用户决定。
小歧义采用保守假设，并在计划和最终报告中说明。

### 2. 编写并审核实施计划

**REQUIRED SUB-SKILL:** 使用 `writing-plans` 编写任何实施计划；不得以普通
分析、口头步骤或已有 Brainstorm 替代。

用该技能产出可执行的计划：任务拆分、依赖、精确文件触点、测试、风险和
完成标准都必须明确。实施计划无论规模都必须由完整的文档审核组并行审核。

**计划文件写出后、Plan 审核分派前**（v1）：publish **estimated** manifest
（完整 Task 链 + Final 节点）。计划审核使用 Plan 单元的 A1 `work_unit_key`
（与 Design 分离，含 `agent_type` 与 **相对** plan 路径）。同一计划的修订复审
优先 continue 对应审核线程；实质修订后、复审前 publish 新 estimated 修订。

计划必须拆成能由新 Grok 子会话逐个执行的 Task，并用明确接口和顺序表达前后依赖。
若 Task 仍无法独立委派，先修订计划并重新审核；不能把耦合作为父会话实施的理由。

修复每个有效的 Critical 或 Important，然后交回同一审核组复审至清零。Minor
要么修复，要么写明保留理由。父会话裁决后 `settle_workflow_gate`；门全部通过后
将 manifest 标为 **approved**。实施期间若计划发生实质变化，先修订计划、
publish demoted estimated、重复本节审核循环；不要直接按旧计划继续实现。
冻结 Task 对的放弃走 B14.3，不得静默删节点。

### 3. 实施计划执行前的工作区门禁

只在已审核计划即将开始执行时运行此门禁；不要把它提前成开始分析、写计划或
普通代码编辑前的固定检查。对经实质修订并重新审核的计划，在其开始执行前再
运行一次。

检查 `git status`、完整的未暂存 diff 和完整的已暂存 diff。根据未提交文件数量、
diff 规模与分布、与计划触点的重叠、以及改动来源是否清楚来判断风险。

未提交改动较多、存在重叠或来源不明时，立即暂停，展示简洁证据并请用户决定
是否在该状态下继续。不得自行 stash、提交、覆盖、还原或丢弃用户改动。仅有少量、
来源清楚且不重叠的改动，不要求为“工作区不干净”这一事实单独中断。

### 4. 使用 SDD 实施

- 运行 `subagent-driven-development` 的预检、Task brief、Grok 实现、Codex Task
  审核、修复复审、进度账本（含 thread ledger 与 workflow id/revisions）和最终
  全局审核完整流程。**不**改通用 SDD 技能文件；workflow 编排仅由本技能承担。
- 实现 Task 必须串行分派，不能并行启动多个实现者；审核与修复循环通过后才能进入
  下一个 Task。
- 每个 Task 的**首次**实现 / **首次**独立审核使用 `delegate_to_agent` + 对应
  A1 `work_unit_key`（含 `agent_type`）；同一 Task 上的修复与复审优先
  `continue_delegation`。
- 进入 Task N+1 时必须新建 Grok 与 Codex 线程，更新 ledger，不得复用 Task N。
- v1 下 Task 委派 key 必须匹配 approved manifest 的 active（或合法
  retained-observed continue）绑定；依赖 readiness 由 admission 执行。
- 每个实现者/审核者 brief **必须**要求 validated terminal card summary。
- 计划中的依赖通过明确接口和前序 Task 提交传递。任务耦合不是回退到父会话实施的
  理由；无法形成可委派 Task 时，先修订并重新审核计划。
- 遵守项目规则和适用技能，按 SDD 要求由 Grok 实现者运行针对性验证并提交。
- 保持改动聚焦，不做无关重构，也不覆盖已存在的用户修改。
- 遇到 `unresumable` / `budget_exhausted_continue` / `not_supported` 时按上文
  replacement 规则处理；其他错误按类型处理或阻塞，不得偷偷新开同 key 线程。
- Compaction / 恢复：先 `get_workflow_state` + ledger，再 continue 或合法
  replacement。

### 5. 固定代码审核与修复循环

每个 Task 提交后都由该 Task 的 Codex 审核线程独立审核规格符合性和代码质量
（首次 `delegate_to_agent`，复审 `continue_delegation`）；不能按风险跳过 Task
审核。每个有效 Critical 或 Important 都交给**同一 Task 的** Grok 修复者
`continue_delegation` 处理，补齐覆盖测试、报告与 card summary 后再由 Codex
复审，直到两个 verdict 都通过。Minor 要么修复，要么记录到 SDD 进度账本并交给
最终 Codex 审核统一裁决。

所有 **active** Task execution gate 通过后，必须由**新建的** Codex 子会话执行
SDD 最终全分支审核（`final_review|reviewer|codex|…` / 新线程）。若 Final 要求
修改：分派 **Final fixer** Grok（`final_review|fixer|grok|…`，不复用 Task
实现者）；fixer 终端 pass 后，对**同一** Final 审核者 `continue_delegation`
做 scoped re-review。文档审核组中的可选 agent 不得进入 Task、修复复审、Final
fixer 或最终代码审核。最终审核若意外中断，仅可 continue **该**最终审核线程。

### 6. 验证、提交与报告

- 每个 Task 运行针对性检查；最终按改动范围运行测试、lint、构建和项目要求的检查。
- 审核修复会使旧验证失效，必须重新运行相关检查，不能以旧结果宣称通过。
- 提交前检查最终 diff，只暂存本任务拥有的修改。无法安全分离用户修改时暂停询问。
- 只创建本地提交；不要合并、推送或创建 PR。
- 最终报告必须列出：完成结果、关键改动、验证命令及结果、文档与代码审核结论、
  保留的 Minor 或风险、本地提交、工作区位置，以及任何阻塞项。

## 硬规则速查

| 情况 | 必须采取的行动 |
| --- | --- |
| “很紧急”或“任务很小” | 仍调用 `writing-plans`，完成计划审核后才执行。 |
| 计划审核还在进行 | 等待并修复 Critical / Important；不能先实现后补审核。 |
| 工作区有少量、清楚且不重叠的修改 | 在实施前门禁记录证据后可继续，始终保留这些修改。 |
| 工作区修改较多、重叠或来源不明 | 停止并交由用户选择；不得以隔离、stash 或提交绕过。 |
| 实施中发现删除数据、破坏兼容性或改变公开接口 | 视为实质范围或架构变化，暂停等待用户决定并修订、复审计划。 |
| 项目很大、依赖复制昂贵或构建很慢 | 仍完整执行 SDD；复用已准备的隔离工作区并运行针对性验证，不得直接实施。 |
| Task 有前后依赖或高度耦合 | 用接口和串行顺序表达依赖；无法委派时修订并重新审核计划。 |
| Grok 或 Codex 子会话不可用 | 报告阻塞；不得由父会话实现、修复或替换 agent。 |
| 有可选并行审核 agent | 仅用于文档审核；独立 Task 和最终代码审核仍只由 [@Codex CLI](codeg://agent/codex) 进行。 |
| 同一工作单元可恢复 | 优先 `continue_delegation`；禁止无 `replaces_task_id` 的同 key 再 `delegate_to_agent`。 |
| continue 返回 `unresumable` / continue 预算耗尽 / `not_supported` | 至多一次同角色 replacement（带 `replaces_task_id` + reason + 同 key）；否则升级用户。 |
| 业务错误 / 显式取消 / agent 不可用 | 不发起 replacement；不换 agent 类型。 |
| 恢复 / 中断后续派 | 先 `get_workflow_state`；新 turn + 恢复 prompt 语义；实现者须重审 FS 与测试并写 card summary。 |
| 最终全分支审核 | 始终新建 Codex；永不 continue Task 审核者线程。 |
| Final 审核要求修改 | 新建/continue Final fixer（Grok）；fixer pass 后再 continue 同一 Final 审核者。 |
| v1 capability 齐全 | skeleton → estimated → settle 文档门 → approved；缺 publish/settle 硬阻塞。 |
| capability 不一致 / 校验失败 | 硬阻塞；不得当 legacy。 |
| 审核/实现结束无 validated card summary | 不得 settle 文档门；不得前进 Task/Final execution gate；以平台解析为准。 |
| 冻结 Task 对需放弃（B14.3） | publish `workflow_state=blocked` 和/或 pair `node_outcome=canceled`（保留绑定）；禁止静默 drop；禁止只靠停对话。 |
| `work_unit_key` | 仅 A1（真实相对路径 + NFC/B1 + agent_type）；≤200 标量；禁止绝对路径、假短路径与 pre-A1 语法。 |
| skeleton create | UUID `publication_token` 入 ledger；同 digest 重试用同一 token；digest 冲突硬停 + `get_workflow_state`。 |
| 零外部 Design 审核（A12） | `resolution_mode` 精确为 `self_review` + 空 `required_reviewer_node_ids` + design rel_path/digest；有外部审核者用 `parent_adjudication`；Plan 门禁止 `self_review`。 |
| v1 父会话核对 | 消费真实 publish/settle/capability/恢复读的返回值再前进；**禁止**每轮故意非法 publish 探针。后端拒绝契约见 Task 3/5 自动化测试。 |
| manifest 体积 | 遵守 A15.2：Tasks≤100、nodes≤400、edges≤800、gates≤50、adj≤4KiB、JSON≤512KiB。 |

## 常见借口

| 借口 | 正确处理 |
| --- | --- |
| “Brainstorm 已经很好，计划显而易见。” | Brainstorm 是需求基线，不替代 `writing-plans` 和强制计划审核。 |
| “先改一小段，审核随后补上。” | 不执行已审核计划前的实现；先完成计划审核和工作区门禁。 |
| “不冲突的脏工作区总能继续。” | 只有证据表明改动少、清楚且不重叠时才能继续；否则让用户选择。 |
| “仓库太大或构建太慢，父会话做更省成本。” | 成本影响验证范围，不改变 SDD 或角色路由；不能因此直接实施。 |
| “Task 有依赖，所以不算独立。” | SDD 串行执行 Task；用接口传递依赖，无法委派就修订计划。 |
| “父会话修一下更快。” | 父会话只能协调；任何代码修复仍分派给 `agent_type: "grok"`。 |
| “`spawn_agent` 不能指定 Grok，所以 Grok 不可用。” | `spawn_agent` 不是 Codeg 跨 agent 路由；先发现并调用 `delegate_to_agent(agent_type: "grok")`，只有工具缺失或实际委派返回 unavailable/error 才算阻塞。 |
| “直接派几个子代理也算 SDD。” | 不算；必须调用并完整遵守 `subagent-driven-development`。 |
| “已经让 Codex 看过里程碑。” | 每个 Task 独立审核以及最终全局审核都不能合并或跳过。 |
| “Grok 已经自审或可选审核者已经看过代码。” | Grok 自审是实现者职责，可选 agent 只能审核文档；二者都不能替代 `agent_type: "codex"` 的独立审核。 |
| “再 `delegate_to_agent` 一次更简单。” | 可恢复则必须 `continue_delegation`；同 key 冷启动是错误路径。 |
| “审核挂了，换个 Codex / Grok 顶上。” | 禁止替换 agent 类型；仅允许同角色同 profile 的记录型 replacement 或阻塞。 |
| “最终审核接着用 Task 审核会话。” | 禁止；最终全分支审核必须新线程。 |
| “中断前差不多做完了，直接当完成。” | 恢复后须重检仓库；未落盘报告重写；实现者须重跑测试与 card summary。 |
| “key 用绝对路径更稳。” | 禁止；A1 只要 workspace-relative；绝对路径不被识别且易超 200。 |
| “Final 要改，让 Task 实现者继续修。” | 禁止；Final 修复走 `final_review\|fixer\|grok\|…`。 |
| “plan 改了，旧 Task 审核节点直接删掉。” | 冻结对禁止静默 drop；完成 gate 或 B14.3 cancel/block publish。 |
| “停会话就算取消该 Task。” | 不是 durable cancel；必须显式 publish blocked/canceled。 |
| “没 card summary 也能 settle / 进下一 Task。” | 禁止；A16 + A2/A7 硬依赖 validated terminal summary。 |
| “capability 半套也能当 legacy。” | 不一致组合硬阻塞，不得降级 legacy。 |
| “路径太长，key 里写个短假路径。” | 禁止；须真实相对路径；过长则 move/rename 后 republish。 |
| “publish 失败换个 publication_token 再 create。” | 禁止；同 digest 复用 token；digest 变则硬停并 reload state。 |
| “子代理说写了 summary，口头也算。” | 不算；父会话须见平台 validated 证据。 |
| “没有并行审核就不用 Design gate。” | v1 仍用 A12 self_review 形状并 settle。 |

## 路由场景自检

父会话编排时用下列场景自检（与设计 A1–A18 / B1–B14 及 Skill Forward 一致）：

1. 入口 → capability 发现；v1 → `publication_token` + skeleton publish → A12
   Design gate 定型 → 再 Design 分派或 self-settle。
2. Design / Plan 修订复审 → continue **匹配** 的审核者/profile 线程；父会话
   `settle_workflow_gate`。
3. 计划写出后 → estimated publish → Plan 审核；通过后 approved。
4. Task 修复 → continue 该 Task 的 Grok 实现者。
5. Task 复审 → continue 该 Task 的独立 Codex 审核者。
6. 下一 Task → 新建 Grok **与** 新建 Codex（不复用上一 Task）。
7. 最终全分支审核 → **始终**新建 Codex（不复用 Task 审核者）。
8. Final 要求修改 → Final fixer（Grok）→ fixer pass 后 continue 同一 Final 审核者。
9. 可恢复性失败 → 记录型同角色/同 profile replacement（一次上限）。
10. 最终审核者意外中断且未出 verdict → continue **其自身**最终审核会话。
11. 业务错误与必需 agent 不可用 → **不**替换、**不**换 agent。
12. Skill 预算 → 每单元最多 2 次自动意外 continue + 1 次 replacement。
13. Compaction / 恢复 → `get_workflow_state` + ledger（含 publication_token），
    禁止凭记忆重放 manifest。
14. 冻结 Task 对放弃 → B14.3 publish blocked/canceled；永不静默 drop / 仅停对话。
15. 所有委派 work unit → A1 key + HTML card summary 模板；父会话核对平台
    validated 证据后再 settle / 前进 gate。
16. B8 digest mismatch → 硬停 + reload；禁止换 token 双 create。

## 使用示例

用户消息：`请基于 docs/brainstorm/payment.md 完成交付。并行审核模型：<agent 引用>`

处理顺序：以该**真实相对**路径为基线（NFC/B1 归一化入 key）→ capability 发现 →
（v1）UUID `publication_token` 入 ledger + skeleton publish → A12 Design gate
定型 → 条件审核 Brainstorm（各审核者 A1 Design key：
`design|docs/brainstorm/payment.md|reviewer|{agent_type}|{profile|none}`，复审
continue，HTML review card summary，settle Design 门）→ `writing-plans` 写计划 →
estimated publish → 审核计划（Plan A1 key + settle → approved）→ 工作区门禁 →
完整 `subagent-driven-development`：每个 Task 首次 `agent_type: "grok"` /
`agent_type: "codex"` + A1 key `delegate_to_agent`，同 Task 修复/复审
`continue_delegation`，实现者/审核者使用 Implementation/Review HTML 模板；
父会话以平台 validated 证据 settle/前进 → 全部 Task gate 通过后**新的** Codex
Final 审核；若需修改则 Final fixer Grok 再 Final re-review continue。维护
thread ledger、`publication_token` 与 workflow revisions；恢复时
`get_workflow_state`。重新验证后，创建仅含任务改动的本地提交。
