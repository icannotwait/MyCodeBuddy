# 工作流小窗口布局与内容呈现优化（紧凑步骤条 + 展开分组列表）

**Date:** 2026-07-30
**Status:** Approved (errata 2026-07-30: design-review gate fixes)
**Scope:** 主会话窗内嵌的工作流小窗口（`SubAgentOverlay` 的 workflow 段）：紧凑态 `WorkflowPhaseRail`、摘要区、展开态 `WorkflowGraphPanel`。不包含 sessions 段、折叠 chip、overlay resize、头部区；不改后端、types、store。

## Problem

工作流小窗口默认宽 288px（可拖至 224–448px，展开图时 448–768px）。现状问题：

1. **PhaseRail**：4 张文字卡片在 288px 下每张仅 ~65px，阶段名/状态文字严重截断；状态仅靠小字文本表达，无一目了然的图形语言。
2. **"当前工作"大框**与 PhaseRail 信息重复，独立占位高。
3. **展开图（WorkflowGraphPanel）**：compact 模式 4 条泳道纵排长滚动；节点卡固定 `h-12` 标题截断；审查者与主节点平分宽度互相挤压；依赖区是裸 node-id mono 文本；节点详情跳到面板最底部，选中后失去上下文位置。

用户已确认全部数据词表（门禁/任务进度、B12、风险、依赖）必须保留，本次只做布局与视觉重组。

## Design

### 共享状态视觉语言

新增 `src/components/chat/workflow-status-icon.tsx`，供步骤条、摘要区、展开图三处复用。形状+颜色双编码（不只依赖颜色）。

**输入契约：** 组件接收已归一化的 `visualStatus` 字符串（phase rail 的 phase status 或 node 的 `nodeStatus`）。调用方负责把 phase 的 `current` 映射为与 `running` 相同的视觉桶；节点侧直接传 `node.status`。

**完整映射表（冻结；禁止实现期发明）：**

| visualStatus | 图形 | 颜色 | 备注 |
|---|---|---|---|
| `completed` | ✓ 实心圆 | emerald | phase + node |
| `current` / `running` | 脉冲圆点（`motion-safe:animate-pulse`） | blue | phase `current` 与 node `running` 同桶 |
| `blocked` / `failed` / `missing_summary` | ✕ 圆 | destructive | `missing_summary` 与 blocked 同桶（store 亦视其为阻塞邻接） |
| `waiting_review` / `waiting_adjudication` | 空心圆 + 内点 | amber | 等待外部动作；区别于 pure pending |
| `canceled` / `pending` / `superseded` | 空心圆 | muted | `superseded` 与 canceled 同桶（历史非活跃） |
| `reserving` | 时钟图标 | amber | |
| `estimated` | 虚线空心圆 | muted | |
| **fallback（任何未列出字符串）** | 空心圆 | muted | 永不 throw；`aria-hidden` 图标 + 调用方仍渲染 `nodeStatus.*` / `phaseStatus.*` 文本 |

脉冲动画必须使用 `motion-safe:`（尊重 `prefers-reduced-motion`）。

### 紧凑态（方案 A：横向步骤条）

#### 选择状态契约（跨组件，冻结）

- **所有权：** `SubAgentOverlay` 持有 `selectedPhaseKind: PhaseKind | null` 与 `phaseSelectionDirty: boolean`（用户是否手动点过阶段）。
- **`WorkflowPhaseRail` 为受控组件**，props：
  - `phases: PhaseRailItem[]`
  - `selectedKind: PhaseKind`
  - `onSelectKind: (kind: PhaseKind) => void`
  - 不含摘要区本身；摘要区是 overlay 内 rail 的**兄弟**节点。
- **默认选择（`phaseSelectionDirty === false`）：** 跟随 `current` 阶段；无 current 时回退到最后一个非 pending 阶段；均无则最后一个阶段。每次 `phaseRail` 快照更新时，若 `!phaseSelectionDirty`，重新计算并覆盖 `selectedKind`。
- **用户点击阶段：** `phaseSelectionDirty = true`，`selectedKind = 点击值`；之后快照更新**不**自动改写 `selectedKind`（粘性），直到 overlay remount 或离开 workflow 段再回来（重置 dirty）。
- **选中阶段不存在于新 rail：** 回退到默认规则（即使 dirty）。

**`workflow-phase-rail.tsx` 重写：**

- `<ol>` 横向排列，每阶段 = 状态图标 + 阶段名（text-xs）；无卡片边框/背景，无状态文字行。
- 阶段间连接线按"流出"阶段状态着色（completed → emerald，current → blue，其余 → border 色）。RTL：依赖文档 `dir` / 逻辑属性（`ms`/`me` 或 `border-inline`），连接线顺序随行内方向翻转，不硬编码 LTR。
- 每阶段为 button：
  - 点击调用 `onSelectKind(kind)`（由 overlay 更新选中阶段与摘要行）。
  - `title` / `aria-label` 含完整进度文本（组合现有 `gateProgress` / `gateRunning` / `gateBlocked` / `taskProgress` key；可用 `phaseProgressAria`）。
  - 当前业务 current 阶段标 `aria-current="step"`；选中阶段另有视觉选中态（如 `data-selected`）。

**`sub-agent-overlay.tsx` compact body 重组：**

- 步骤条下方为**摘要区**（合并并删除现有独立"当前工作"大框）：
  - 第一行（`data-testid="workflow-phase-summary"`）：**选中阶段**的状态文字 + 进度（`门禁 2/3 · 1 进行中` 或 `任务 3/5`），从 `phaseRail` 中对应 `selectedKind` 的 item 读取。
  - 第二行（`data-testid="workflow-summary-current-nodes"`，有 currentNodes 时）：继续使用全局 `selectCurrentNodes`（**不是**选中阶段过滤后的节点）—— intentional：进度行跟选中阶段，"当前工作"行始终反映图级进行中节点。最多 2 个 + `+N`（`moreCurrentNodes`）；`+N` 为**静态** `<span>`，不可点击。每个节点 = 状态图标 + 标题（truncate）+ agent/role chip + round_count；`canOpenWorkflowNode` 的节点点击打开会话（`openDelegatedChildSession`）。
- 头部（标题、overallState Badge、展开/收起按钮）、分段页签、sessions 段、resize 手柄全部不动。

### 展开态（方案 E1：可折叠分组列表）

**`workflow-graph-panel.tsx` 重写：**

- 组件仅被 overlay 以 `compact` 使用 → 去掉 `compact` prop 双路径，只保留新渲染。
- 每阶段一个可折叠 `<section data-testid="workflow-graph-lane-{kind}">`：
  - 组头 = 状态图标 + 阶段名 + 状态文字 + 进度摘要 + chevron；button + `aria-expanded` + `data-testid` 可用 `workflow-lane-toggle-{kind}`。
  - **初始默认：** 空阶段折叠（单行 `emptyLane`）；非空展开。
  - **实时快照语义（冻结）：**
    - 折叠 map 仅存组件 `useState`，不持久化到 store/localStorage；panel unmount（含收起展开图、切换离开 workflow 段）后状态丢弃。
    - 对每个 `kind`：若用户**尚未**手动 toggle 过该 lane（`laneDirty[kind] !== true`），则每当该 lane 的 empty/non-empty 布尔变化时，重置为默认（空→折叠，非空→展开）。
    - 用户 toggle 过的 lane 保持用户选择，即使 empty 状态翻转，直到 unmount。
    - 若选中节点所在 lane 被折叠：保留 selection，详情随 lane 一并隐藏；不强制展开、不 clear selection（用户展开 lane 后详情仍在选中行下）。
- **节点行**（整行，高度自适应，`data-testid="workflow-graph-node-{id}"`，保留 `data-estimated` / `data-openable` / `data-status`）：
  - 结构 = 状态图标 + 标题（`line-clamp-2`）+ agent/角色/可选 chips + run/replace 计数（复用现有 `runCount` / `replacementCount` i18n key，紧凑缩写允许符号前缀但字符串仍走 i18n）+ 状态 Badge。
  - `canOpenWorkflowNode` 的行渲染为 button（点击选中 + 打开会话）；estimated 不可打开行：优先 `button` + `disabled` + `aria-disabled="true"` + `title={estimatedNonActionable}`（保留 a11y 树中的 disabled 语义）；若实现为 `div`，必须带 `role="button"` + `aria-disabled="true"` + 同样 title，以及相同 data-* 属性。
  - `aria-current` 标选中行。
- **审查者**：主节点行下方缩进次级行（左竖线引导，`pl-6`），同样行结构；不再与主节点平分宽度。`optionalReviewer`（可选）chips 保留。
- **任务行组头**（tasks 阶段每个 taskIndex）：`任务 N` + 门禁 `2/3` 计数（`gateProgress`），替代现有右侧 `w-10` 计数盒。
- **依赖区**：
  - node-id mono 文本 → 节点标题 chips（`标题A → 标题B`；用 `snapshot.nodes` 映射 title，缺失回退 id）。RTL 下箭头/顺序随逻辑方向。
  - 默认折叠为"依赖（N）"按钮（新 key `dependenciesToggle`，`data-testid="workflow-dependencies-toggle"`），展开后完整列表，无数据丢失。
- **节点详情**：`WorkflowNodeDetail` 原样保留（B12 五字段、风险词表、打开会话按钮、estimated 提示），改为在选中行下方内联渲染，不再置于面板底部。

### i18n

新增少量 key，10 个 `src/i18n/messages/*.json` 同步：

- `laneToggleAria` — 分组折叠按钮 aria-label
- `dependenciesToggle` — "依赖（{count}）"
- `moreCurrentNodes` — "+{count}"
- `phaseProgressAria` — 阶段完整进度的读屏文本

其余文案组合现有 key（`phase.*`、`phaseStatus.*`、`nodeStatus.*`、`gateProgress`、`runCount`、`replacementCount` 等）。禁止为 run/replace 硬编码无 i18n 字符串。

### 无障碍

- 步骤条保持 `<ol>` + `aria-label`；阶段 button 带完整 `aria-label`（阶段+状态+进度）。
- 分组折叠 button + `aria-expanded`；依赖折叠同理。
- 状态用形状+颜色双编码；动画用 `motion-safe:`。
- estimated 不可打开行保留 disabled 语义（见上）。

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

**保留的 data-testid（必须迁移后仍存在）：**
`workflow-phase-rail`、`workflow-phase-{kind}`、`data-status`、`workflow-overall-state`、页签、`workflow-expand-toggle`、detail 全套、`workflow-graph-panel`、`workflow-graph-node-{id}`、`workflow-graph-lane-{kind}`、`workflow-graph-edges`（若仍渲染依赖容器）、`workflow-phase-gate-{kind}` 可迁到摘要区对应进度节点或删除并在测试中改断言到 `workflow-phase-summary`。

**新增推荐 testid：** `workflow-phase-summary`、`workflow-summary-current-nodes`、`workflow-lane-toggle-{kind}`、`workflow-dependencies-toggle`。

## Out of scope

- sessions 段、折叠 chip、overlay resize、头部区
- 横向滚动泳道、连线图可视化
- 后端、types、`workflow-graph-store` 改动
- lane 折叠状态持久化

## Acceptance

自动化门禁 = AC8（`pnpm eslint .` + `pnpm test`）。下列 AC 在测试中的判定方式：

1. **288px 结构门禁（自动化）+ 视觉抽检（人工）**  
   - 自动化：4 阶段名与状态图标均在 rail DOM 中；阶段名**不**使用 `truncate`/`line-clamp-1` 类（允许 `text-xs` / wrap）；图标组件对全部 `nodeStatus` 与 phase status 有映射用例。  
   - 人工/可选：在默认 288px 宽度下目视无严重截断。AC8 不依赖布局引擎量测像素裁剪。
2. 步骤条点击任一阶段，摘要行切换为该阶段进度；默认跟随 current 阶段（及 §选择状态契约 的 dirty/粘性规则）。自动化可测。
3. 当前节点在摘要区可见（最多 2 个 + 静态 `+N`），可打开节点点击直达会话。自动化可测。
4. 展开图每阶段可折叠/展开，空阶段默认折叠；折叠状态 unmount 后不保留；实时 empty 翻转规则见 §实时快照语义。自动化可测。
5. 节点行标题使用 `line-clamp-2`（自动化断言 class）；计数/状态/chips 齐全；审查者缩进于主节点下方；任务行组头显示 `任务 N` + 门禁 `x/y`。"视觉最多 2 行"为 class 门禁，非 jsdom 像素测量。
6. 依赖区默认折叠为"依赖（N）"，展开后为标题 chips 完整列表。自动化可测。
7. 点击节点行，详情在该行下方内联展开（B12 词表完整）。自动化可测。
8. `pnpm eslint .` 与 `pnpm test` 全绿。

## Design errata log (2026-07-30)

Closed Design-review Important findings without scope expansion:

| Finding | Resolution |
|---|---|
| Incomplete status icon map | Full table + fallback for all 11 `nodeStatus` values |
| Phase selection ownership | Overlay-owned controlled rail + summary sibling contract |
| Live snapshot semantics | dirty flags for phase selection and per-lane collapse; empty flip defaults |
| AC1/AC5 visual-only | Structural/class automation + optional manual visual |

Minors folded: `motion-safe` pulse, estimated disabled semantics, RTL logical props, sticky selection, static `+N`, expanded testid preserve list, run/replace via i18n keys.
