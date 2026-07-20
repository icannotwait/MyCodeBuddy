---
name: brainstorm-to-delivery
description: Use when a Codeg conversation provides a completed Brainstorm file and asks for a high-quality locally deliverable implementation.
---

# Brainstorm 到本地交付

将本次消息引用的、已经完成的 Brainstorm 文件视为需求基线。不要重新或重复
Brainstorm，也不要停在分析或计划阶段；除明确的硬门禁外，自主推进到可本地
交付的结果。

## 执行模式选择（最高优先级）

在开始实施前只能选择一个模式，不能混用：

| 条件 | 模式 | 必须做什么 |
| --- | --- | --- |
| 隔离工作区合适，且任务多数独立 | Subagent-Driven | 调用并完整遵守 `subagent-driven-development`。 |
| 项目很大，或复制、依赖、构建成本使隔离工作区不合适 | 直接实施 | 在当前工作区实施；**不得调用** `subagent-driven-development`，也不得复用其部分流程。 |

这个选择先于 `subagent-driven-development` 的通用适用性判断。独立任务、紧急程度、
已通过工作区门禁或想保留其审核流程，都不能把“直接实施”改称为 Subagent-Driven。

直接实施路径仅使用第 5 节的 [@Codex CLI](codeg://agent/codex) 代码审核矩阵。
不要沿用 `subagent-driven-development` 的预检、进度账本、Task brief、实现者/修复者/
任务审核者分派或全分支审核流程。

## 输入与审核组

- 先阅读项目指令、Brainstorm、相关代码和测试、以及近期变更。
- 用户可添加一行 `并行审核模型：...`。其中的 agent 与
  [@Codex CLI](codeg://agent/codex) 组成文档审核组；未提供时，文档审核组
  仅含 [@Codex CLI](codeg://agent/codex)。
- 可选 agent 只能审核 Brainstorm 和实施计划，不能审核 Task、里程碑或最终
  代码。所有代码审核只由 [@Codex CLI](codeg://agent/codex) 执行。
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

### 4. 实施

- 按任务规模、独立性和风险选择直接实施或 Subagent-Driven。本文的
  Subagent-Driven 专指调用 `subagent-driven-development` 技能执行，不能只按名称
  临时组织子代理；不要机械套用重流程。
- 不默认使用 worktree。项目很大、复制仓库或依赖成本高、构建昂贵时，应在当前
  工作区谨慎直接实施。若 `subagent-driven-development` 所需的隔离工作区不合适，
  不得调用该技能，也不得借用其部分流程并称为 Subagent-Driven；按本节的直接实施
  路径和第 5 节的代码审核继续。
- 遵守项目规则和适用技能，优先以测试定义行为，逐 Task 运行针对性验证。
- 保持改动聚焦，不做无关重构，也不覆盖已存在的用户修改。

### 5. 代码审核与修复

| 风险 | 必需审核 |
| --- | --- |
| 小 | 完成后由 [@Codex CLI](codeg://agent/codex) 最终审核。 |
| 中 | 由 [@Codex CLI](codeg://agent/codex) 审核高风险里程碑，并作最终审核。 |
| 高或复杂 | 每个 Task 后由 [@Codex CLI](codeg://agent/codex) 审核，并作最终全局审核。 |

当前会话负责修复发现的问题。每个有效的 Critical 或 Important 必须修复并由
[@Codex CLI](codeg://agent/codex) 复审至清零；Minor 要么修复，要么记录保留理由。
文档审核组中的可选 agent 不得进入本阶段。

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
| 选择 Subagent-Driven | 必须完整调用 `subagent-driven-development` 并满足其隔离前置条件。 |
| 该技能的隔离工作区不合适 | 选择直接实施；不得调用该技能或借用其部分流程。 |
| 有可选并行审核 agent | 仅用于文档审核；代码审核仍只由 [@Codex CLI](codeg://agent/codex) 进行。 |

## 常见借口

| 借口 | 正确处理 |
| --- | --- |
| “Brainstorm 已经很好，计划显而易见。” | Brainstorm 是需求基线，不替代 `writing-plans` 和强制计划审核。 |
| “先改一小段，审核随后补上。” | 不执行已审核计划前的实现；先完成计划审核和工作区门禁。 |
| “不冲突的脏工作区总能继续。” | 只有证据表明改动少、清楚且不重叠时才能继续；否则让用户选择。 |
| “用 worktree 就能规避用户改动风险。” | 隔离不替代用户决定，也不是大型或昂贵项目的默认选择。 |
| “直接派几个子代理也算 Subagent-Driven。” | 不算；必须调用 `subagent-driven-development` 技能。 |
| “在当前工作区只采用该技能的部分流程也算 Subagent-Driven。” | 不算；隔离前置条件不合适时，改走直接实施路径。 |
| “可选审核者已经看过代码。” | 文档审核不等于代码审核；按风险仍需 [@Codex CLI](codeg://agent/codex) 审核。 |

## 使用示例

用户消息：`请基于 docs/brainstorm/payment.md 完成交付。并行审核模型：<agent 引用>`

处理顺序：以该文件为基线，按条件审核 Brainstorm，调用 `writing-plans` 写并审核
实施计划，在计划即将执行前运行工作区门禁，然后直接实施或调用
`subagent-driven-development` 进行 Subagent-Driven 实施，按风险接受
[@Codex CLI](codeg://agent/codex) 代码审核、重新验证并创建仅含任务改动的本地提交。
