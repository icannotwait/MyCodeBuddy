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

父会话只负责任务简报、上下文答疑、结果裁决、进度账本（含 thread ledger）和验证协调；
父会话不得亲自实现或修复 Task 代码。若计划、隔离工作区、委派能力或指定 agent
不满足 SDD 前置条件，暂停并报告阻塞；不得切换为直接实施或替换 agent 类型。

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
| `work_unit_key` | 见下表；与 MCP 调用中传入的 key 一致（≤ 200 字符） |
| 角色 / agent_type | 如 implementer / reviewer + `grok` / `codex` / 可选文档审核 agent |
| profile_id | 不可变 profile 身份；无 profile 时记 `none` |
| child_conversation_id | 子会话 id |
| latest_task_id | 该线程**最新**一次 run 的 task id（后续 `continue_delegation` 的目标） |
| state | 活跃 / 终态 / 已替换 等 |
| recovery_count | 本工作单元已用的意外中断 continue 次数（Skill 侧计数，可严于平台） |
| replacement | 若发生替换：`replaced_task_id`、`replacement_reason`、新 child id |

Compaction 或父会话压缩后，恢复编排时**只**依据 ledger + 平台 durable run/budget
行，不得仅凭记忆重放已完成的委派序列。

### `work_unit_key` 材料

Skill 构造的 key 必须稳定、可复现，并与角色 / profile 绑定。推荐材料（`|` 分隔）：

| 工作单元 | `work_unit_key` 材料 |
| --- | --- |
| Design 审核 | `design\|{absolute_doc_path}\|{role}\|{profile_id\|none}` |
| Plan 审核 | `plan\|{absolute_plan_path}\|{role}\|{profile_id\|none}` |
| Task 实现者 | `task\|{task_index}\|implementer\|{profile_id\|none}` |
| Task 审核者 | `task\|{task_index}\|reviewer\|{profile_id\|none}` |
| 最终全分支审核 | `final_review\|{branch_ref}\|reviewer\|{profile_id\|none}` |

规则：

- Design 与 Plan 即使使用同一审核者 profile，也是**不同**工作单元（不同 key）。
- Task N 与 Task N+1 使用不同 `task_index`，**禁止**跨 Task 复用线程。
- 可选文档审核 agent 仅参与 Design/Plan 文档审核组；不得成为 Task 或最终代码审核者。
- 本技能路径下的编排调用一律带 `work_unit_key`；无 key 的 ad-hoc 冷启动不属于
  brainstorm-to-delivery 编排。

### 首次分派 vs `continue_delegation` 偏好

| 工作单元 | 首次分派 | 同一单元后续工作 |
| --- | --- | --- |
| Design + 审核者/profile | `delegate_to_agent` 新建审核线程 | 修订/复审 → **`continue_delegation`** 同一审核者 |
| Plan + 审核者/profile | `delegate_to_agent` 新建审核线程 | 修订/复审 → **`continue_delegation`** 同一审核者 |
| Task N + Grok 实现者 | `delegate_to_agent` 新建 Grok | 追问与修复 → **`continue_delegation`** 该 Grok |
| Task N + Codex 审核者 | `delegate_to_agent` 新建独立 Codex | Task N 复审 → **`continue_delegation`** 该 Codex |
| Task N+1 | 新建 Grok **与** 新建 Codex | **永不**复用 Task N 的线程 |
| 最终全分支审核 | **始终**新建 Codex | 仅在**该**最终审核线程发生可恢复意外中断后 `continue_delegation`；**永不**继续 Task 审核者 |

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
   - 仅在证据充分后更新报告与 card summary。  

意外中断恢复示例意图（嵌入实际 `task` 时按角色裁剪）：

```text
Previous turn was interrupted. This is a NEW turn on the same work unit.
Re-inspect the repository and durable artifacts now. Treat any prior partial
reasoning as provisional. Recreate any final report that was not durably written.
If you are an implementer: audit partial filesystem changes and re-run covering
tests before reporting completion. Do not assume the interrupted instruction
already finished.
```

## 工作流

### 1. 理解与条件式 Brainstorm 审核

自行检查 Brainstorm 的完整性、一致性、可实施性和范围。出现下列任一条件时，
由文档审核组并行审核 Brainstorm：跨模块架构或大改动面、迁移、并发、安全、
实质歧义或矛盾，或高风险设计缺少独立审核证据。

文档审核组中每个审核者/profile 使用独立 `work_unit_key`（Design 单元）。
同一文档的修订复审优先 `continue_delegation` 对应审核线程。

等待全部审核完成后再汇总。修复每个有效的 Critical 或 Important，并交回原
审核组复审，直到清零。Minor 要么修复，要么记录保留理由。

若修复会实质改变需求、范围、架构或用户数据处理方式，暂停并请用户决定。
小歧义采用保守假设，并在计划和最终报告中说明。

### 2. 编写并审核实施计划

**REQUIRED SUB-SKILL:** 使用 `writing-plans` 编写任何实施计划；不得以普通
分析、口头步骤或已有 Brainstorm 替代。

用该技能产出可执行的计划：任务拆分、依赖、精确文件触点、测试、风险和
完成标准都必须明确。实施计划无论规模都必须由完整的文档审核组并行审核。

计划审核使用 Plan 单元的 `work_unit_key`（与 Design 分离）。同一计划的修订复审
优先 continue 对应审核线程。

计划必须拆成能由新 Grok 子会话逐个执行的 Task，并用明确接口和顺序表达前后依赖。
若 Task 仍无法独立委派，先修订计划并重新审核；不能把耦合作为父会话实施的理由。

修复每个有效的 Critical 或 Important，然后交回同一审核组复审至清零。Minor
要么修复，要么写明保留理由。实施期间若计划发生实质变化，先修订计划并重复
本节审核循环；不要直接按旧计划继续实现。

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
  审核、修复复审、进度账本（含 thread ledger）和最终全局审核完整流程。
- 实现 Task 必须串行分派，不能并行启动多个实现者；审核与修复循环通过后才能进入
  下一个 Task。
- 每个 Task 的**首次**实现 / **首次**独立审核使用 `delegate_to_agent` + 对应
  `work_unit_key`；同一 Task 上的修复与复审优先 `continue_delegation`。
- 进入 Task N+1 时必须新建 Grok 与 Codex 线程，更新 ledger，不得复用 Task N。
- 计划中的依赖通过明确接口和前序 Task 提交传递。任务耦合不是回退到父会话实施的
  理由；无法形成可委派 Task 时，先修订并重新审核计划。
- 遵守项目规则和适用技能，按 SDD 要求由 Grok 实现者运行针对性验证并提交。
- 保持改动聚焦，不做无关重构，也不覆盖已存在的用户修改。
- 遇到 `unresumable` / `budget_exhausted_continue` / `not_supported` 时按上文
  replacement 规则处理；其他错误按类型处理或阻塞，不得偷偷新开同 key 线程。

### 5. 固定代码审核与修复循环

每个 Task 提交后都由该 Task 的 Codex 审核线程独立审核规格符合性和代码质量
（首次 `delegate_to_agent`，复审 `continue_delegation`）；不能按风险跳过 Task
审核。每个有效 Critical 或 Important 都交给**同一 Task 的** Grok 修复者
`continue_delegation` 处理，补齐覆盖测试和报告后再由 Codex 复审，直到两个
verdict 都通过。Minor 要么修复，要么记录到 SDD 进度账本并交给最终 Codex 审核
统一裁决。

所有 Task 完成后，必须由**新建的** Codex 子会话执行 SDD 最终全分支审核
（新 `work_unit_key` / 新线程）。文档审核组中的可选 agent 不得进入 Task、修复
复审或最终代码审核。最终审核若意外中断，仅可 continue **该**最终审核线程。

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
| 恢复 / 中断后续派 | 新 turn + 恢复 prompt 语义；实现者须重审 FS 与测试。 |
| 最终全分支审核 | 始终新建 Codex；永不 continue Task 审核者线程。 |

## 常见借口

| 借口 | 正确处理 |
| --- | --- |
| “Brainstorm 已经很好，计划显而易见。” | Brainstorm 是需求基线，不替代 `writing-plans` 和强制计划审核。 |
| “先改一小段，审核随后补上。” | 不执行已审核计划前的实现；先完成计划审核和工作区门禁。 |
| “不冲突的脏工作区总能继续。” | 只有证据表明改动少、清楚且不重叠时才能继续；否则让用户选择。 |
| “仓库太大或构建太慢，父会话做更省成本。” | 成本影响验证范围，不改变 SDD 或角色路由；不能因此直接实施。 |
| “Task 有依赖，所以不算独立。” | SDD 串行执行 Task；用接口传递依赖，无法委派就修订计划。 |
| “父会话修一下更快。” | 父会话只能协调；任何代码修复仍分派给 `agent_type: "grok"`。 |
| “直接派几个子代理也算 SDD。” | 不算；必须调用并完整遵守 `subagent-driven-development`。 |
| “已经让 Codex 看过里程碑。” | 每个 Task 独立审核以及最终全局审核都不能合并或跳过。 |
| “Grok 已经自审或可选审核者已经看过代码。” | Grok 自审是实现者职责，可选 agent 只能审核文档；二者都不能替代 `agent_type: "codex"` 的独立审核。 |
| “再 `delegate_to_agent` 一次更简单。” | 可恢复则必须 `continue_delegation`；同 key 冷启动是错误路径。 |
| “审核挂了，换个 Codex / Grok 顶上。” | 禁止替换 agent 类型；仅允许同角色同 profile 的记录型 replacement 或阻塞。 |
| “最终审核接着用 Task 审核会话。” | 禁止；最终全分支审核必须新线程。 |
| “中断前差不多做完了，直接当完成。” | 恢复后须重检仓库；未落盘报告重写；实现者须重跑测试。 |

## 路由场景自检（九条）

父会话编排时用下列场景自检（与设计 Skill Forward 一致）：

1. Design / Plan 修订复审 → continue **匹配** 的审核者/profile 线程。  
2. Task 修复 → continue 该 Task 的 Grok 实现者。  
3. Task 复审 → continue 该 Task 的独立 Codex 审核者。  
4. 下一 Task → 新建 Grok **与** 新建 Codex（不复用上一 Task）。  
5. 最终全分支审核 → **始终**新建 Codex（不复用 Task 审核者）。  
6. 可恢复性失败 → 记录型同角色/同 profile replacement（一次上限）。  
7. 最终审核者意外中断且未出 verdict → continue **其自身**最终审核会话。  
8. 业务错误与必需 agent 不可用 → **不**替换、**不**换 agent。  
9. Skill 预算 → 每单元最多 2 次自动意外 continue + 1 次 replacement。  

## 使用示例

用户消息：`请基于 docs/brainstorm/payment.md 完成交付。并行审核模型：<agent 引用>`

处理顺序：以该文件为基线，按条件审核 Brainstorm（各审核者带 Design
`work_unit_key`，复审 continue），调用 `writing-plans` 写并审核实施计划（Plan
单元同样 continue 复审），在计划即将执行前运行工作区门禁，然后完整调用
`subagent-driven-development`：每个 Task 首次由 `agent_type: "grok"` /
`agent_type: "codex"` `delegate_to_agent`，同 Task 修复与复审 `continue_delegation`，
最终再由**新的** Codex 执行全局审核。维护 thread ledger。重新验证后，创建仅含
任务改动的本地提交。
