# 工作流小窗口布局与内容呈现优化（紧凑步骤条 + 展开分组列表）

**Date:** 2026-07-30
**Status:** Approved
**Scope:** 主会话窗内嵌的工作流小窗口（`SubAgentOverlay` 的 workflow 段）：紧凑态 `WorkflowPhaseRail`、摘要区、展开态 `WorkflowGraphPanel`。不包含 sessions 段、折叠 chip、overlay resize、头部区；不改后端、types、store。

## Problem

工作流小窗口默认宽 288px（可拖至 224–448px，展开图时 448–768px）。现状问题：

1. **PhaseRail**：4 张文字卡片在 288px 下每张仅 ~65px，阶段名/状态文字严重截断；状态仅靠小字文本表达，无一目了然的图形语言。
2. **"当前工作"大框**与 PhaseRail 信息重复，独立占位高。
3. **展开图（WorkflowGraphPanel）**：compact 模式 4 条泳道纵排长滚动；节点卡固定 `h-12` 标题截断；审查者与主节点平分宽度互相挤压；依赖区是裸 node-id mono 文本；节点详情跳到面板最底部，选中后失去上下文位置。

用户已确认全部数据词表（门禁/任务进度、B12、风险、依赖）必须保留，本次只做布局与视觉重组。

## Design

### 共享状态视觉语言

新增 `src/components/chat/workflow-status-icon.tsx`，供步骤条、摘要区、展开图三处复用。形状+颜色双编码（不只依赖颜色）：

| 状态 | 图形 | 颜色 |
|---|---|---|
| completed | ✓ 实心圆 | emerald |
| current / running | 脉冲圆点（`animate-pulse`） | blue |
| blocked / failed | ✕ 圆 | destructive |
| canceled / pending | 空心圆 | muted |
| reserving | 时钟图标 | amber |
| estimated | 虚线空心圆 | muted |

### 紧凑态（方案 A：横向步骤条）

**`workflow-phase-rail.tsx` 重写：**

- `<ol>` 横向排列，每阶段 = 状态图标 + 阶段名（text-xs）；无卡片边框/背景，无状态文字行。
- 阶段间连接线按"流出"阶段状态着色（completed → emerald，current → blue，其余 → border 色）。
- 每阶段为 button：
  - 点击切换下方摘要行显示该阶段的进度详情；默认跟随 current 阶段，无 current 阶段时回退到最后一个非 pending 阶段（均无则最后一个阶段）。
  - `title` / `aria-label` 含完整进度文本（组合现有 `gateProgress` / `gateRunning` / `gateBlocked` / `taskProgress` key）。
  - current 阶段标 `aria-current`。

**`sub-agent-overlay.tsx` compact body 重组：**

- 步骤条下方为**摘要区**（合并并删除现有独立"当前工作"大框）：
  - 第一行：选中阶段的状态文字 + 进度（`门禁 2/3 · 1 进行中` 或 `任务 3/5`）。
  - 第二行（有 currentNodes 时）：当前节点列表，最多 2 个 + `+N`；每个 = 状态图标 + 标题（truncate）+ agent/role chip + round_count；`canOpenWorkflowNode` 的节点点击打开会话（`openDelegatedChildSession`）。
- 头部（标题、overallState Badge、展开/收起按钮）、分段页签、sessions 段、resize 手柄全部不动。

### 展开态（方案 E1：可折叠分组列表）

**`workflow-graph-panel.tsx` 重写：**

- 组件仅被 overlay 以 `compact` 使用 → 去掉 `compact` prop 双路径，只保留新渲染。
- 每阶段一个可折叠 `<section>`：
  - 组头 = 状态图标 + 阶段名 + 状态文字 + 进度摘要 + chevron；button + `aria-expanded`。
  - 空阶段默认折叠，单行显示"无工作单元"（`emptyLane`）；非空默认展开。
  - 折叠状态仅存组件 `useState`，不持久化。
- **节点行**（整行，高度自适应）：
  - 结构 = 状态图标 + 标题（2 行 clamp）+ agent/角色/可选 chips + `↻run ×replace` 计数 + 状态 Badge。
  - `canOpenWorkflowNode` 的行渲染为 button（点击选中 + 打开会话）；estimated 不可打开行渲染为 div，虚线左边框 + muted。
  - `aria-current` 标选中行。
- **审查者**：主节点行下方缩进次级行（左竖线引导，`pl-6`），同样行结构；不再与主节点平分宽度。`optionalReviewer`（可选）chips 保留。
- **任务行组头**（tasks 阶段每个 taskIndex）：`任务 N` + 门禁 `2/3` 计数（`gateProgress`），替代现有右侧 `w-10` 计数盒。
- **依赖区**：
  - node-id mono 文本 → 节点标题 chips（`标题A → 标题B`；用 `snapshot.nodes` 映射 title，缺失回退 id）。
  - 默认折叠为"依赖（N）"按钮（新 key `dependenciesToggle`），展开后完整列表，无数据丢失。
- **节点详情**：`WorkflowNodeDetail` 原样保留（B12 五字段、风险词表、打开会话按钮、estimated 提示），改为在选中行下方内联渲染，不再置于面板底部。

### i18n

新增少量 key，10 个 `src/i18n/messages/*.json` 同步：

- `laneToggleAria` — 分组折叠按钮 aria-label
- `dependenciesToggle` — "依赖（{count}）"
- `moreCurrentNodes` — "+{count}"
- `phaseProgressAria` — 阶段完整进度的读屏文本

其余文案组合现有 key（`phase.*`、`phaseStatus.*`、`nodeStatus.*`、`gateProgress` 等）。

### 无障碍

- 步骤条保持 `<ol>` + `aria-label`；阶段 button 带完整 `aria-label`（阶段+状态+进度）。
- 分组折叠 button + `aria-expanded`；依赖折叠同理。
- 状态用形状+颜色双编码。

## Data vocabulary preservation

以下全部保留，仅改变承载位置：

- PhaseRail 的 `gateProgress / gateRunning / gateBlocked / taskProgress` → 摘要行 + aria/title
- currentNodes 的 title / role / agent / status / round_count → 摘要区第二行
- 图的 `nodeStatus / runCount / replacementCount / optionalReviewer / emptyLane / dependencies / edges(from,to)` → 展开态行与依赖区
- header 的 `overallState` 全值
- 详情的 B12 五字段（runCountLabel / activeGenerationLabel / replacementCountLabel / gateCycleLabel / roundCountLabel）+ riskLevel / riskReason / openSession / estimatedNonActionable

## Implementation

| File | Change |
|---|---|
| `src/components/chat/workflow-status-icon.tsx` | 新增共享状态图标组件 |
| `src/components/chat/workflow-phase-rail.tsx` | 重写为横向步骤条 |
| `src/components/chat/sub-agent-overlay.tsx` | compact body 重组（步骤条 + 摘要区，删旧"当前工作"框） |
| `src/components/chat/workflow-graph-panel.tsx` | 重写为可折叠分组列表；去 `compact` prop；详情内联 |
| `src/components/chat/workflow-node-detail.tsx` | 不改 |
| `src/i18n/messages/*.json` | 4 个新 key × 10 语言 |
| `src/components/chat/workflow-overlay.test.tsx` | 更新 9 处图结构断言；新增摘要行/lane 折叠/依赖折叠/详情内联用例 |

尽量保留现有 data-testid（`workflow-phase-rail`、`workflow-phase-{kind}`、`data-status`、`workflow-overall-state`、页签、`workflow-expand-toggle`、detail 全套）。

## Out of scope

- sessions 段、折叠 chip、overlay resize、头部区
- 横向滚动泳道、连线图可视化
- 后端、types、`workflow-graph-store` 改动
- lane 折叠状态持久化

## Acceptance

1. 288px 默认宽度下，4 阶段名称与状态图标完整可见，无截断。
2. 步骤条点击任一阶段，摘要行切换为该阶段进度；默认跟随 current 阶段。
3. 当前节点在摘要区可见（最多 2 个 + `+N`），可打开节点点击直达会话。
4. 展开图每阶段可折叠/展开，空阶段默认折叠；折叠状态刷新后不保留。
5. 节点行标题最多 2 行，计数/状态/chips 齐全；审查者缩进于主节点下方；任务行组头显示 `任务 N` + 门禁 `x/y`。
6. 依赖区默认折叠为"依赖（N）"，展开后为标题 chips 完整列表。
7. 点击节点行，详情在该行下方内联展开（B12 词表完整）。
8. `pnpm eslint .` 与 `pnpm test` 全绿。
