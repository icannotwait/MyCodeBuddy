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
| Task 实现者 | [@Grok](codeg://agent/grok) / `agent_type: "grok"` | 每个 Task 使用新的 Grok 子会话。 |
| Task 修复者 | [@Grok](codeg://agent/grok) / `agent_type: "grok"` | 所有实现和修复都由 Grok 子会话完成。 |
| Task 独立审核者 | [@Codex CLI](codeg://agent/codex) / `agent_type: "codex"` | 读取 SDD brief、report、review package，保持只读。 |
| 最终全局审核者 | [@Codex CLI](codeg://agent/codex) / `agent_type: "codex"` | 独立执行最终全分支审核，保持只读。 |

### Codeg 委派工具门禁（报告阻塞前强制执行）

本表的 `@Grok` / `@Codex CLI` 是 Codeg 跨 agent 路由，不是 Codex 原生
`collaboration.spawn_agent`。按此顺序执行：

1. 实现与修复调用 Codeg `delegate_to_agent` 并传 `agent_type: "grok"`；Task
   审核与最终审核调用同一工具并传 `agent_type: "codex"`。
2. 若 Codeg MCP 工具未直接显示，先在延迟工具目录中发现它（Codex 中为
   `ALL_TOOLS`，查找 `mcp__codeg_mcp__delegate_to_agent`），再通过实际暴露的
   工具命名空间调用。
3. `spawn_agent` 不能选择 Grok 只说明该原生接口不能选模型，不代表 Grok 不可用。
4. 仅当延迟发现后仍无兼容的 Codeg 委派工具，或实际 Codeg 委派调用返回
   unavailable/error，才可报告 agent 或委派能力阻塞。

父会话只负责任务简报、上下文答疑、结果裁决、进度账本和验证协调；父会话不得亲自
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

## 工作流

### 1. 理解与条件式 Brainstorm 审核

自行检查 Brainstorm 的完整性、一致性、可实施性和范围。出现下列任一条件时，
由文档审核组并行审核 Brainstorm：跨模块架构或大改动面、迁移、并发、安全、
实质歧义或矛盾，或高风险设计缺少独立审核证据。

等待全部审核完成后再汇总。修复每个有效的 Critical 或 Important，并交回原
审核组复审，直到清零。Minor 要么修复，要么记录保留理由。

若修复会实质改变需求、范围、架构或用户数据处理方式，暂停并请用户决定。
小歧义采用保守假设，并在计划和最终报告中说明。

### 2. 编写并审核实施计划

**REQUIRED SUB-SKILL:** 使用 `writing-plans` 编写任何实施计划；不得以普通
分析、口头步骤或已有 Brainstorm 替代。

用该技能产出可执行的计划：任务拆分、依赖、精确文件触点、测试、风险和
完成标准都必须明确。实施计划无论规模都必须由完整的文档审核组并行审核。

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
  审核、修复复审、进度账本和最终全局审核完整流程。
- 实现 Task 必须串行分派，不能并行启动多个实现者；审核与修复循环通过后才能进入
  下一个 Task。
- 计划中的依赖通过明确接口和前序 Task 提交传递。任务耦合不是回退到父会话实施的
  理由；无法形成可委派 Task 时，先修订并重新审核计划。
- 遵守项目规则和适用技能，按 SDD 要求由 Grok 实现者运行针对性验证并提交。
- 保持改动聚焦，不做无关重构，也不覆盖已存在的用户修改。

### 5. 固定代码审核与修复循环

每个 Task 提交后都由新的 Codex 子会话独立审核规格符合性和代码质量；不能按风险
跳过 Task 审核。每个有效 Critical 或 Important 都交给 Grok 修复者处理，补齐覆盖
测试和报告后再由 Codex 复审，直到两个 verdict 都通过。Minor 要么修复，要么记录
到 SDD 进度账本并交给最终 Codex 审核统一裁决。

所有 Task 完成后，必须由新的 Codex 子会话执行 SDD 最终全分支审核。文档审核组中
的可选 agent 不得进入 Task、修复复审或最终代码审核。

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

## 使用示例

用户消息：`请基于 docs/brainstorm/payment.md 完成交付。并行审核模型：<agent 引用>`

处理顺序：以该文件为基线，按条件审核 Brainstorm，调用 `writing-plans` 写并审核
实施计划，在计划即将执行前运行工作区门禁，然后完整调用
`subagent-driven-development`：每个 Task 由 `agent_type: "grok"` 实现和修复，由新的
`agent_type: "codex"` 子会话独立审核，最后再由 Codex 执行全局审核。重新验证后，
创建仅含任务改动的本地提交。
