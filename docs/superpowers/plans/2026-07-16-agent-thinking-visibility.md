# Agent Thinking Visibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a database-backed, per-agent switch that defaults off and hides live and historical thinking on screen while preserving activity, copying, and exports.

**Architecture:** Store `show_thinking` on `agent_setting` and expose it through `AcpAgentInfo`. A dedicated ACP display-preference command updates only that column and emits the existing agents-updated event, so it never participates in runtime configuration staleness. `MessageListView` resolves one boolean for its agent and passes it into the historical and live render projections; canonical transcript data is never filtered.

**Tech Stack:** SeaORM migrations/entities/services, Rust Tauri commands, Axum handlers, React 19, TypeScript, Zustand, next-intl, Vitest, Testing Library.

## Global Constraints

- `agent_setting.show_thinking` is `BOOLEAN NOT NULL DEFAULT FALSE`, including upgraded rows.
- The switch is global per agent, not per project or conversation.
- The update must not touch environment/native config, refresh session fingerprints, mark connections stale, or request an agent restart.
- Hidden historical reasoning and live thinking must not mount Markdown/reasoning views.
- `LiveTurnStats`, tools, plans, elapsed time, and edit statistics remain visible.
- Message copy includes reasoning even while hidden; Markdown, HTML, and image exports retain canonical thinking.
- Author new copy in `en` and `zh-CN`; use the English values in the other locale catalogs.
- Preserve all unrelated worktree changes and stage only the files named by each task.

---

### Task 1: Persist the Agent Display Preference

**Files:**
- Create: `src-tauri/src/db/migration/m20260716_000001_agent_show_thinking.rs`
- Modify: `src-tauri/src/db/migration/mod.rs`
- Modify: `src-tauri/src/db/entities/agent_setting.rs`
- Modify: `src-tauri/src/db/service/agent_setting_service.rs`

**Interfaces:**
- Produces: `agent_setting::Model.show_thinking: bool`
- Produces: `agent_setting_service::update_show_thinking(&DatabaseConnection, AgentType, bool) -> Result<(), DbError>`
- Preserves: `AgentSettingsUpdate`, which remains limited to runtime-affecting enabled/env/provider values.

- [ ] **Step 1: Create and register a no-op migration with a failing upgrade test**

Create `m20260716_000001_agent_show_thinking.rs` with a deliberately no-op
`up`; the test represents an existing pre-upgrade row and must fail until the
column is implemented:

```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[derive(DeriveIden)]
enum AgentSetting {
    Table,
    ShowThinking,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    #[tokio::test]
    async fn existing_agent_rows_migrate_with_thinking_hidden() {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open sqlite");
        conn.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE agent_setting (id INTEGER PRIMARY KEY, agent_type TEXT NOT NULL)"
                .to_string(),
        ))
        .await
        .expect("create old schema");
        conn.execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO agent_setting (id, agent_type) VALUES (1, 'codex')".to_string(),
        ))
        .await
        .expect("seed old row");

        Migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("run migration");

        let row = conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT show_thinking FROM agent_setting WHERE id = 1".to_string(),
            ))
            .await
            .expect("query migrated row")
            .expect("migrated row");
        let show_thinking: bool = row
            .try_get("", "show_thinking")
            .expect("read show_thinking");
        assert!(!show_thinking);
    }
}
```

Register it after the current final migration in `migration/mod.rs`:

```rust
mod m20260716_000001_agent_show_thinking;
```

```rust
Box::new(m20260716_000001_agent_show_thinking::Migration),
```

- [ ] **Step 2: Run the upgrade test and verify the missing column failure**

```powershell
cd src-tauri
cargo test --features test-utils existing_agent_rows_migrate_with_thinking_hidden
```

Expected: FAIL while reading `show_thinking` because the no-op migration did
not add the column.

- [ ] **Step 3: Implement the migration and make the upgrade test pass**

Replace the no-op `up` and `down` methods with:

```rust
async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .alter_table(
            Table::alter()
                .table(AgentSetting::Table)
                .add_column(
                    ColumnDef::new(AgentSetting::ShowThinking)
                        .boolean()
                        .not_null()
                        .default(false),
                )
                .to_owned(),
        )
        .await
}

async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .alter_table(
            Table::alter()
                .table(AgentSetting::Table)
                .drop_column(AgentSetting::ShowThinking)
                .to_owned(),
        )
        .await
}
```

Run:

```powershell
cd src-tauri
cargo test --features test-utils existing_agent_rows_migrate_with_thinking_hidden
```

Expected: PASS; the old row reads `show_thinking == false`.

- [ ] **Step 4: Add a failing service test for new defaults and isolated updates**

Append this test module to `agent_setting_service.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::fresh_in_memory_db;

    #[tokio::test]
    async fn thinking_visibility_defaults_off_and_updates_in_isolation() {
        let db = fresh_in_memory_db().await;
        let defaults = [AgentDefaultInput {
            agent_type: AgentType::Codex,
            registry_id: "codex-acp".to_string(),
            default_sort_order: 3,
        }];
        ensure_defaults(&db.conn, &defaults)
            .await
            .expect("ensure default");

        update(
            &db.conn,
            AgentType::Codex,
            AgentSettingsUpdate {
                enabled: false,
                env_json: Some(r#"{"KEEP":"1"}"#.to_string()),
                model_provider_id: None,
            },
        )
        .await
        .expect("seed runtime settings");

        let before = get_by_agent_type(&db.conn, AgentType::Codex)
            .await
            .expect("read default")
            .expect("agent row");
        assert!(!before.show_thinking);

        update_show_thinking(&db.conn, AgentType::Codex, true)
            .await
            .expect("update display preference");

        let after = get_by_agent_type(&db.conn, AgentType::Codex)
            .await
            .expect("read updated")
            .expect("agent row");
        assert!(after.show_thinking);
        assert!(!after.enabled);
        assert_eq!(after.env_json.as_deref(), Some(r#"{"KEEP":"1"}"#));
        assert_eq!(after.sort_order, 3);
        assert_eq!(after.model_provider_id, None);
    }
}
```

- [ ] **Step 5: Run the service test and confirm the entity/API are missing**

Run:

```powershell
cd src-tauri
cargo test --features test-utils thinking_visibility_defaults_off_and_updates_in_isolation
```

Expected: compilation fails because `show_thinking` and `update_show_thinking` do not exist.

- [ ] **Step 6: Extend the entity, default insert, and focused service update**

Add this field after `enabled` in `db/entities/agent_setting.rs`:

```rust
pub show_thinking: bool,
```

Add this field to the `agent_setting::ActiveModel` in `ensure_defaults`:

```rust
show_thinking: Set(false),
```

Add this focused service function after `update`:

```rust
pub async fn update_show_thinking(
    conn: &DatabaseConnection,
    agent_type: AgentType,
    show_thinking: bool,
) -> Result<(), DbError> {
    let agent_type_str = serde_json::to_string(&agent_type)
        .map_err(|e| DbError::Migration(format!("agent_type serialize failed: {e}")))?;
    let model = agent_setting::Entity::find()
        .filter(agent_setting::Column::AgentType.eq(agent_type_str.clone()))
        .one(conn)
        .await?
        .ok_or_else(|| DbError::Migration(format!("agent setting not found: {agent_type_str}")))?;

    let mut active = model.into_active_model();
    active.show_thinking = Set(show_thinking);
    active.updated_at = Set(Utc::now());
    active.update(conn).await?;
    Ok(())
}
```

- [ ] **Step 7: Format, run both focused tests, and verify formatting**

Run:

```powershell
cd src-tauri
cargo fmt
cargo test --features test-utils existing_agent_rows_migrate_with_thinking_hidden
cargo test --features test-utils thinking_visibility_defaults_off_and_updates_in_isolation
cargo fmt --check
```

Expected: both focused tests and the final formatting check pass.

- [ ] **Step 8: Commit the persistence layer**

```powershell
git add src-tauri/src/db/migration/m20260716_000001_agent_show_thinking.rs src-tauri/src/db/migration/mod.rs src-tauri/src/db/entities/agent_setting.rs src-tauri/src/db/service/agent_setting_service.rs
git commit -m "feat(agents): persist thinking visibility"
```

---

### Task 2: Expose a Non-Runtime ACP Preference API

**Files:**
- Modify: `src-tauri/src/acp/types.rs:538`
- Modify: `src-tauri/src/commands/acp.rs:6326,6477,6513`
- Modify: `src-tauri/src/web/handlers/acp.rs:536`
- Modify: `src-tauri/src/web/router.rs:637`
- Modify: `src-tauri/src/lib.rs:1111`
- Modify: `src/lib/types.ts:1704`
- Modify: `src/lib/api.ts:342`

**Interfaces:**
- Consumes: `agent_setting_service::update_show_thinking`
- Produces: `AcpAgentInfo.show_thinking: bool` in Rust and TypeScript.
- Produces: `acp_update_agent_display_preferences_core(AgentType, bool, &AppDatabase, &EventEmitter) -> Result<(), AcpError>`
- Produces: transport command `acp_update_agent_display_preferences`
- Produces: `acpUpdateAgentDisplayPreferences(agentType: AgentType, showThinking: boolean): Promise<void>`

- [ ] **Step 1: Add a failing core test for projection and event emission**

Inside the existing `#[cfg(test)] mod tests` in `commands/acp.rs`, add:

```rust
#[tokio::test]
async fn display_preferences_update_projection_and_emit() {
    use crate::db::test_helpers::fresh_in_memory_db;
    use crate::web::event_bridge::WebEventBroadcaster;
    use std::sync::Arc;

    let db = fresh_in_memory_db().await;
    let broadcaster = Arc::new(WebEventBroadcaster::new());
    let emitter = EventEmitter::test_web_only(broadcaster.clone());
    let mut rx = broadcaster.subscribe();

    acp_update_agent_display_preferences_core(
        AgentType::Codex,
        true,
        &db,
        &emitter,
    )
    .await
    .expect("update display preference");

    let agents = acp_list_agents_core(&db).await.expect("list agents");
    let codex = agents
        .iter()
        .find(|agent| agent.agent_type == AgentType::Codex)
        .expect("codex projection");
    assert!(codex.show_thinking);

    let event = rx.try_recv().expect("agents-updated event");
    assert_eq!(event.channel, ACP_AGENTS_UPDATED_EVENT);
    assert_eq!(event.payload["reason"], "display_preferences_updated");
    assert_eq!(event.payload["agent_type"], "codex");
}
```

- [ ] **Step 2: Run the core test and confirm it fails**

```powershell
cd src-tauri
cargo test --features test-utils display_preferences_update_projection_and_emit
```

Expected: compilation fails because the core function and projected field do not exist.

- [ ] **Step 3: Add `show_thinking` to the wire models and list projection**

Add after `enabled` in Rust `AcpAgentInfo`:

```rust
pub show_thinking: bool,
```

Add after `enabled` in TypeScript `AcpAgentInfo`:

```typescript
show_thinking: boolean
```

Add this field to the `AcpAgentInfo` literal in `acp_list_agents_core`:

```rust
show_thinking: setting.map(|model| model.show_thinking).unwrap_or(false),
```

- [ ] **Step 4: Implement the focused core and Tauri command without staleness refresh**

Add beside the other agent preference cores in `commands/acp.rs`:

```rust
pub(crate) async fn acp_update_agent_display_preferences_core(
    agent_type: AgentType,
    show_thinking: bool,
    db: &AppDatabase,
    emitter: &EventEmitter,
) -> Result<(), AcpError> {
    let default = agent_setting_service::AgentDefaultInput {
        agent_type,
        registry_id: registry::registry_id_for(agent_type).to_string(),
        default_sort_order: i32::MAX / 2,
    };
    agent_setting_service::ensure_defaults(&db.conn, &[default])
        .await
        .map_err(|e| AcpError::protocol(e.to_string()))?;
    agent_setting_service::update_show_thinking(&db.conn, agent_type, show_thinking)
        .await
        .map_err(|e| AcpError::protocol(e.to_string()))?;
    emit_acp_agents_updated(
        emitter,
        "display_preferences_updated",
        Some(agent_type),
    );
    Ok(())
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn acp_update_agent_display_preferences(
    agent_type: AgentType,
    show_thinking: bool,
    db: State<'_, AppDatabase>,
    app: tauri::AppHandle,
) -> Result<(), AcpError> {
    let emitter = EventEmitter::Tauri(app);
    acp_update_agent_display_preferences_core(agent_type, show_thinking, &db, &emitter).await
}
```

Do not call `refresh_config_staleness` from either function.

- [ ] **Step 5: Add the Axum handler, route, and Tauri registration**

Add to `web/handlers/acp.rs`:

```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpUpdateAgentDisplayPreferencesParams {
    pub agent_type: AgentType,
    pub show_thinking: bool,
}

pub async fn acp_update_agent_display_preferences(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpUpdateAgentDisplayPreferencesParams>,
) -> Result<Json<()>, AppCommandError> {
    acp_commands::acp_update_agent_display_preferences_core(
        params.agent_type,
        params.show_thinking,
        &state.db,
        &state.emitter,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}
```

Register this route next to the existing preference routes in `web/router.rs`:

```rust
.route(
    "/acp_update_agent_display_preferences",
    post(handlers::acp::acp_update_agent_display_preferences),
)
```

Register this command next to `acp_update_agent_preferences` in `lib.rs`:

```rust
acp_commands::acp_update_agent_display_preferences,
```

- [ ] **Step 6: Add the frontend transport wrapper**

Add after `acpListAgents` in `src/lib/api.ts`:

```typescript
export async function acpUpdateAgentDisplayPreferences(
  agentType: AgentType,
  showThinking: boolean
): Promise<void> {
  return getTransport().call("acp_update_agent_display_preferences", {
    agentType,
    showThinking,
  })
}
```

- [ ] **Step 7: Run focused backend tests and both compile modes**

```powershell
cd src-tauri
cargo fmt
cargo fmt --check
cargo test --features test-utils display_preferences_update_projection_and_emit
cargo check
cargo check --no-default-features --bin codeg-server
```

Expected: all commands pass; no test reports a stale-session refresh.

- [ ] **Step 8: Commit the shared API**

```powershell
git add src-tauri/src/acp/types.rs src-tauri/src/commands/acp.rs src-tauri/src/web/handlers/acp.rs src-tauri/src/web/router.rs src-tauri/src/lib.rs src/lib/types.ts src/lib/api.ts
git commit -m "feat(agents): expose thinking display preference"
```

---

### Task 3: Add the Optimistic Agent-Panel Switch

**Files:**
- Create: `src/components/settings/agent-thinking-visibility-switch.tsx`
- Create: `src/components/settings/agent-thinking-visibility-switch.test.tsx`
- Modify: `src/components/settings/acp-agent-settings.tsx:7140`
- Modify: `src/i18n/messages/en.json`
- Modify: `src/i18n/messages/zh-CN.json`
- Modify: `src/i18n/messages/ar.json`
- Modify: `src/i18n/messages/de.json`
- Modify: `src/i18n/messages/es.json`
- Modify: `src/i18n/messages/fr.json`
- Modify: `src/i18n/messages/ja.json`
- Modify: `src/i18n/messages/ko.json`
- Modify: `src/i18n/messages/pt.json`
- Modify: `src/i18n/messages/zh-TW.json`

**Interfaces:**
- Consumes: `acpUpdateAgentDisplayPreferences`
- Produces: `AgentThinkingVisibilitySwitch`
- Consumes callback: `(agentType: AgentType, showThinking: boolean) => void` to patch the settings panel's local agent row.

- [ ] **Step 1: Create failing optimistic-success and rollback tests**

Create `agent-thinking-visibility-switch.test.tsx`:

```tsx
import { fireEvent, render, screen, waitFor } from "@testing-library/react"
import { useState } from "react"
import { NextIntlClientProvider } from "next-intl"
import { beforeEach, describe, expect, it, vi } from "vitest"
import enMessages from "@/i18n/messages/en.json"

const mocks = vi.hoisted(() => ({
  update: vi.fn(),
  toastError: vi.fn(),
}))

vi.mock("@/lib/api", () => ({
  acpUpdateAgentDisplayPreferences: mocks.update,
}))
vi.mock("sonner", () => ({
  toast: { error: mocks.toastError },
}))

import { AgentThinkingVisibilitySwitch } from "./agent-thinking-visibility-switch"

function Harness() {
  const [checked, setChecked] = useState(false)
  return (
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <output data-testid="value">{String(checked)}</output>
      <AgentThinkingVisibilitySwitch
        agentType="codex"
        checked={checked}
        onCheckedChange={(_, next) => setChecked(next)}
      />
    </NextIntlClientProvider>
  )
}

describe("AgentThinkingVisibilitySwitch", () => {
  beforeEach(() => {
    mocks.update.mockReset()
    mocks.toastError.mockReset()
  })

  it("updates optimistically and keeps the saved value", async () => {
    mocks.update.mockResolvedValue(undefined)
    render(<Harness />)
    fireEvent.click(screen.getByRole("switch", { name: "Show thinking" }))
    expect(screen.getByTestId("value")).toHaveTextContent("true")
    await waitFor(() => {
      expect(mocks.update).toHaveBeenCalledWith("codex", true)
      expect(
        screen.getByRole("switch", { name: "Show thinking" })
      ).toBeEnabled()
    })
    expect(screen.getByTestId("value")).toHaveTextContent("true")
  })

  it("rolls back and reports a failed save", async () => {
    mocks.update.mockRejectedValue(new Error("disk full"))
    render(<Harness />)
    fireEvent.click(screen.getByRole("switch", { name: "Show thinking" }))
    expect(screen.getByTestId("value")).toHaveTextContent("true")
    await waitFor(() => {
      expect(screen.getByTestId("value")).toHaveTextContent("false")
      expect(mocks.toastError).toHaveBeenCalledTimes(1)
    })
  })
})
```

- [ ] **Step 2: Run the component test and confirm it fails**

```powershell
pnpm exec vitest run src/components/settings/agent-thinking-visibility-switch.test.tsx
```

Expected: FAIL because the component and translation keys do not exist.

- [ ] **Step 3: Implement the focused switch component**

Create `agent-thinking-visibility-switch.tsx`:

```tsx
"use client"

import { useState } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { Switch } from "@/components/ui/switch"
import { acpUpdateAgentDisplayPreferences } from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import type { AgentType } from "@/lib/types"

export interface AgentThinkingVisibilitySwitchProps {
  agentType: AgentType
  checked: boolean
  onCheckedChange: (agentType: AgentType, checked: boolean) => void
}

export function AgentThinkingVisibilitySwitch({
  agentType,
  checked,
  onCheckedChange,
}: AgentThinkingVisibilitySwitchProps) {
  const t = useTranslations("AcpAgentSettings")
  const [saving, setSaving] = useState(false)

  const handleChange = async (next: boolean) => {
    if (saving) return
    onCheckedChange(agentType, next)
    setSaving(true)
    try {
      await acpUpdateAgentDisplayPreferences(agentType, next)
    } catch (error) {
      onCheckedChange(agentType, !next)
      toast.error(t("toasts.saveThinkingVisibilityFailed"), {
        description: toErrorMessage(error),
      })
    } finally {
      setSaving(false)
    }
  }

  return (
    <div className="mt-3 flex min-h-8 items-center justify-between gap-3 border-t pt-3">
      <label
        htmlFor={`show-thinking-${agentType}`}
        className="text-xs font-medium text-foreground"
      >
        {t("showThinking")}
      </label>
      <Switch
        id={`show-thinking-${agentType}`}
        checked={checked}
        disabled={saving}
        onCheckedChange={(next) => void handleChange(next)}
        aria-label={t("showThinking")}
      />
    </div>
  )
}
```

- [ ] **Step 4: Integrate the switch with the settings panel's local agent array**

Import the component in `acp-agent-settings.tsx`:

```tsx
import { AgentThinkingVisibilitySwitch } from "./agent-thinking-visibility-switch"
```

Add this callback near the other selected-agent callbacks:

```tsx
const handleThinkingVisibilityChange = useCallback(
  (agentType: AgentType, showThinking: boolean) => {
    setAgents((current) =>
      current.map((agent) =>
        agent.agent_type === agentType
          ? { ...agent, show_thinking: showThinking }
          : agent
      )
    )
  },
  []
)
```

Render this directly below the selected agent description:

```tsx
<AgentThinkingVisibilitySwitch
  agentType={selectedAgent.agent_type}
  checked={selectedAgent.show_thinking}
  onCheckedChange={handleThinkingVisibilityChange}
/>
```

- [ ] **Step 5: Add source copy and locale-safe fallback values**

Add these keys at the root of `AcpAgentSettings` in `en.json`:

```json
"showThinking": "Show thinking"
```

Add this key under `AcpAgentSettings.toasts` in `en.json`:

```json
"saveThinkingVisibilityFailed": "Failed to save thinking visibility"
```

Use these authored values in `zh-CN.json`:

```json
"showThinking": "显示思考"
```

```json
"saveThinkingVisibilityFailed": "保存思考显示设置失败"
```

Add the two English values to `ar.json`, `de.json`, `es.json`, `fr.json`, `ja.json`, `ko.json`, `pt.json`, and `zh-TW.json` so no locale has a missing key.

- [ ] **Step 6: Run the focused component test and lint**

```powershell
pnpm exec vitest run src/components/settings/agent-thinking-visibility-switch.test.tsx
pnpm eslint src/components/settings/agent-thinking-visibility-switch.tsx src/components/settings/agent-thinking-visibility-switch.test.tsx src/components/settings/acp-agent-settings.tsx
```

Expected: both commands pass.

- [ ] **Step 7: Commit the Agent-panel UI**

```powershell
git add src/components/settings/agent-thinking-visibility-switch.tsx src/components/settings/agent-thinking-visibility-switch.test.tsx src/components/settings/acp-agent-settings.tsx src/i18n/messages/en.json src/i18n/messages/zh-CN.json src/i18n/messages/ar.json src/i18n/messages/de.json src/i18n/messages/es.json src/i18n/messages/fr.json src/i18n/messages/ja.json src/i18n/messages/ko.json src/i18n/messages/pt.json src/i18n/messages/zh-TW.json
git commit -m "feat(agents): add thinking visibility switch"
```

---

### Task 4: Apply Visibility to History and Live Rendering

**Files:**
- Modify: `src/hooks/use-acp-agents.ts`
- Modify: `src/hooks/use-acp-agents.test.ts`
- Modify: `src/components/message/content-parts-renderer.tsx:2805`
- Modify: `src/components/message/content-parts-renderer.test.tsx`
- Modify: `src/components/message/live-transcript-row.tsx:61,411,507`
- Modify: `src/components/message/live-transcript-row.test.tsx`
- Modify: `src/components/message/message-list-view.tsx:273,726`
- Modify: `src/components/message/message-list-view.test.tsx`
- Modify: `src/lib/export-conversation.test.ts`

**Interfaces:**
- Produces: `useAgentThinkingVisibility(agentType: AgentType): boolean`, defaulting to `false` until loaded.
- Extends: `ContentPartsRendererProps.showThinking?: boolean`, default `true` for non-conversation callers.
- Extends: `LiveTranscriptRowProps.showThinking: boolean`.
- Produces: exported `extractTextFromParts(parts: AdaptedContentPart[]): string` including text and reasoning.

- [ ] **Step 1: Add a failing selector test that defaults closed**

Extend `use-acp-agents.test.ts` and ensure its `makeAgent` factory includes `show_thinking: false`. Add:

```tsx
it("selects thinking visibility without flashing the loaded value", async () => {
  mockAcpListAgents.mockResolvedValue([
    { ...makeAgent("codex", 0), show_thinking: true },
  ])
  const { result } = renderHook(() => useAgentThinkingVisibility("codex"))
  expect(result.current).toBe(false)
  await waitFor(() => expect(result.current).toBe(true))
})

it("refreshes thinking visibility after the shared agent event", async () => {
  mockAcpListAgents
    .mockResolvedValueOnce([
      { ...makeAgent("codex", 0), show_thinking: false },
    ])
    .mockResolvedValueOnce([
      { ...makeAgent("codex", 0), show_thinking: true },
    ])
  const { result } = renderHook(() => useAgentThinkingVisibility("codex"))
  await waitFor(() => expect(mockAcpListAgents).toHaveBeenCalledTimes(1))
  expect(result.current).toBe(false)

  act(() => mockEventHandler?.())

  await waitFor(() => expect(result.current).toBe(true))
  expect(mockAcpListAgents).toHaveBeenCalledTimes(2)
})
```

Import `useAgentThinkingVisibility` beside `useAcpAgents` in that test.

- [ ] **Step 2: Add failing historical, live, and copy tests**

Add to `content-parts-renderer.test.tsx`:

```tsx
it("omits reasoning when showThinking is false", () => {
  const reasoning: AdaptedContentPart = {
    type: "reasoning",
    content: "private chain",
    isStreaming: false,
  }
  wrap(<ContentPartsRenderer parts={[reasoning]} showThinking={false} />)
  expect(screen.queryByText("private chain")).not.toBeInTheDocument()
})

it("omits reasoning nested in a goal run", () => {
  const start: AdaptedToolCallPart = {
    type: "tool-call",
    toolCallId: "goal-1",
    toolName: "update_goal",
    input: null,
    state: "input-available",
  }
  const goalRun: AdaptedContentPart = {
    type: "goal-run",
    start,
    end: null,
    items: [
      { type: "reasoning", content: "nested private chain", isStreaming: false },
      { type: "text", text: "visible result" },
    ],
    isRunning: false,
  }
  wrap(<ContentPartsRenderer parts={[goalRun]} showThinking={false} />)
  fireEvent.click(screen.getByRole("button"))
  expect(screen.queryByText("nested private chain")).not.toBeInTheDocument()
  expect(screen.getByText("visible result")).toBeInTheDocument()
})
```

Add `AdaptedContentPart` and `AdaptedToolCallPart` to that test's type imports.

Replace the `renderRow` helper in `live-transcript-row.test.tsx` with:

```tsx
function renderRow(
  onToolRender?: (id: string) => void,
  showThinking = true
) {
  return render(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <LiveTranscriptRow
        conversationId={CID}
        agentType="codex"
        showThinking={showThinking}
        onToolRender={onToolRender}
      />
    </NextIntlClientProvider>
  )
}
```

Add:

```tsx
it("does not mount a thinking segment when visibility is off", () => {
  const message: LiveMessage = {
    id: "thinking-only",
    role: "assistant",
    content: [{ type: "thinking", text: "hidden live thought" }],
    startedAt: 1,
  }
  liveTranscriptStore.rebuild(CID, "c1", message, 1)
  renderRow(undefined, false)
  expect(screen.queryByTestId("reasoning")).not.toBeInTheDocument()
  expect(screen.queryByTestId("live-transcript-row")).not.toBeInTheDocument()
})

it("keeps tools visible when a thinking segment is hidden", () => {
  const message: LiveMessage = {
    id: "thinking-and-tool",
    role: "assistant",
    content: [
      { type: "thinking", text: "hidden live thought" },
      { type: "tool_call", info: tool("visible-tool") },
    ],
    startedAt: 1,
  }
  liveTranscriptStore.rebuild(CID, "c1", message, 2)
  renderRow(undefined, false)
  expect(screen.queryByTestId("reasoning")).not.toBeInTheDocument()
  expect(screen.getByTestId("tool-part-visible-tool")).toBeInTheDocument()
})
```

Export `extractTextFromParts` from `message-list-view.tsx`, import it in `message-list-view.test.tsx`, and add:

```tsx
it("copies reasoning even when its view is hidden", () => {
  expect(
    extractTextFromParts([
      { type: "reasoning", content: "hidden thought", isStreaming: false },
      { type: "text", text: "final answer" },
    ])
  ).toBe("hidden thought\nfinal answer")
})
```

Add `AdaptedToolCallPart` to `message-list-view.test.tsx`'s type imports for
the recursive fixture below.

Also add this self-contained recursive copy case:

```tsx
it("copies reasoning recursively through goal runs", () => {
  const start: AdaptedToolCallPart = {
    type: "tool-call",
    toolCallId: "goal-1",
    toolName: "update_goal",
    input: null,
    state: "input-available",
  }
  expect(
    extractTextFromParts([
      {
        type: "goal-run",
        start,
        end: null,
        items: [
          {
            type: "reasoning",
            content: "nested hidden thought",
            isStreaming: false,
          },
        ],
        isRunning: false,
      },
    ])
  ).toBe("nested hidden thought")
})
```

In `message-list-view.test.tsx`, mock the selector closed:

```tsx
vi.mock("@/hooks/use-acp-agents", () => ({
  useAgentThinkingVisibility: () => false,
}))
```

Add a live-activity regression under the existing live-footer describe block:

```tsx
it("keeps live activity visible for a hidden thinking-only footer", () => {
  const message: LiveMessage = {
    id: "thinking-only",
    role: "assistant",
    content: [{ type: "thinking", text: "hidden live thought" }],
    startedAt: 1,
  }
  liveTranscriptStore.rebuild(CID, "c1", message, 1)
  useConversationRuntimeStore
    .getState()
    .actions.setLiveMessage(CID, message, true)

  renderMessageList()

  expect(screen.queryByTestId("live-transcript-row")).not.toBeInTheDocument()
  expect(screen.getByTestId("live-turn-stats")).toBeInTheDocument()
})
```

Add this complete canonical-export fixture and regression to
`export-conversation.test.ts`:

```tsx
function makeDataWithThinking(): ExportConversationData {
  const data = makeData()
  return {
    ...data,
    turns: [
      {
        id: "thinking-turn",
        role: "assistant",
        blocks: [
          { type: "thinking", text: "hidden thought" },
          { type: "text", text: "visible answer" },
        ],
        timestamp: "2026-05-27T00:00:00Z",
      },
    ],
  }
}

it("keeps canonical thinking in Markdown, HTML, and image export source", async () => {
  mockIsDesktop.mockReturnValue(true)
  mockSave.mockResolvedValue("/Users/me/out.md")
  mockInvoke.mockResolvedValue(undefined)

  await exportAsMarkdown(makeDataWithThinking())
  const markdown = (
    mockInvoke.mock.calls[0][1] as { contents: string }
  ).contents
  expect(markdown).toContain("> hidden thought")

  mockSave.mockResolvedValue("/Users/me/out.html")
  mockInvoke.mockClear()
  await exportAsHtml(makeDataWithThinking())
  const html = (mockInvoke.mock.calls[0][1] as { contents: string }).contents
  expect(html).toContain(
    '<blockquote class="thinking">hidden thought</blockquote>'
  )
  // `exportAsImage` consumes this same `buildHtmlDocument` output before
  // rasterization, so this assertion locks both HTML and image source data.
})
```

Do not add `showThinking` to `ExportConversationData` or change
`export-conversation.ts`; both HTML and image already share the canonical
`buildHtmlDocument` path.

- [ ] **Step 3: Run the focused frontend tests and confirm they fail**

```powershell
pnpm exec vitest run src/hooks/use-acp-agents.test.ts src/components/message/content-parts-renderer.test.tsx src/components/message/live-transcript-row.test.tsx src/components/message/message-list-view.test.tsx src/lib/export-conversation.test.ts
```

Expected: selector/render/copy tests fail because the new selector and
visibility props do not exist and copy omits reasoning. The canonical export
regression already passes; it characterizes behavior that this feature must not
change.

- [ ] **Step 4: Implement the narrow Zustand selector**

Add to `use-acp-agents.ts`:

```typescript
export function useAgentThinkingVisibility(agentType: AcpAgentInfo["agent_type"]): boolean {
  useEffect(() => acquireSharedSubscription(), [])
  return useAcpAgentsStore(
    (state) =>
      state.agents.find((agent) => agent.agent_type === agentType)
        ?.show_thinking ?? false
  )
}
```

This selector subscribes each keep-alive conversation only to its boolean, not the full agent array.

- [ ] **Step 5: Gate recursive historical reasoning rendering**

Extend the renderer props and default:

```tsx
interface ContentPartsRendererProps {
  parts: AdaptedContentPart[]
  role?: MessageRole
  showThinking?: boolean
}

export const ContentPartsRenderer = memo(function ContentPartsRenderer({
  parts,
  role,
  showThinking = true,
}: ContentPartsRendererProps) {
```

Replace the reasoning branch inside the recursive `renderPart` closure with:

```tsx
if (part.type === "reasoning") {
  return showThinking ? (
    <ReasoningPart key={`reasoning-${keyId}`} part={part} />
  ) : null
}
```

Add `showThinking: boolean` to `HistoricalMessageGroup` and `CollapsibleSystemMessage`, and pass it to every `ContentPartsRenderer` they render. In `MessageListView`, resolve the value once:

```tsx
const showThinking = useAgentThinkingVisibility(agentType)
```

Pass it into every `HistoricalMessageGroup` from `renderThreadItem`, and include it in that callback's dependency array.

- [ ] **Step 6: Filter live thinking before mounting segment subscribers**

Extend `LiveTranscriptRowProps`:

```tsx
showThinking: boolean
```

Extend `buildLiveFooterItems` with the boolean and skip hidden thinking before tool grouping output is appended:

```tsx
function buildLiveFooterItems(
  conversationId: number,
  segmentIds: readonly string[],
  groupIds: readonly string[],
  showThinking: boolean
): LiveFooterItem[] {
```

Inside its segment loop, immediately after reading the segment, add:

```tsx
if (segment?.type === "thinking" && !showThinking) continue
```

Pass `showThinking` into the memoized call and dependency array. After the existing typing-indicator branch, add:

```tsx
if (items.length === 0) return null
```

Pass `showThinking` from `MessageListView` into `LiveTranscriptRow` and add it to the `liveFooter` memo dependencies.

- [ ] **Step 7: Include reasoning in assistant message copy without filtering canonical export data**

Change the helper to:

```typescript
export function extractTextFromParts(parts: AdaptedContentPart[]): string {
  return parts
    .flatMap((part): string[] => {
      if (part.type === "text") return [part.text]
      if (part.type === "reasoning") return [part.content]
      if (part.type === "goal-run") return [extractTextFromParts(part.items)]
      return []
    })
    .filter((text) => text.length > 0)
    .join("\n")
}
```

Do not pass `showThinking` to this helper or to `export-conversation.ts`.

- [ ] **Step 8: Update typed test fixtures with the new required field**

Add `show_thinking: false` to complete `AcpAgentInfo` factories in:

```typescript
show_thinking: false,
```

```text
src/hooks/use-acp-agents.test.ts
src/components/chat/agent-selector.test.tsx
src/components/settings/acp-agent-settings.test.tsx
src/components/settings/codebuddy-config-panel.test.tsx
```

Fixtures intentionally cast through `unknown as AcpAgentInfo` do not require mechanical expansion.

- [ ] **Step 9: Run focused tests, lint, and the full frontend suite**

```powershell
pnpm exec vitest run src/hooks/use-acp-agents.test.ts src/components/settings/agent-thinking-visibility-switch.test.tsx src/components/message/content-parts-renderer.test.tsx src/components/message/live-transcript-row.test.tsx src/components/message/message-list-view.test.tsx src/lib/export-conversation.test.ts
pnpm eslint src/hooks/use-acp-agents.ts src/components/settings/agent-thinking-visibility-switch.tsx src/components/settings/acp-agent-settings.tsx src/components/message/content-parts-renderer.tsx src/components/message/live-transcript-row.tsx src/components/message/message-list-view.tsx
pnpm test
pnpm build
```

Expected: every command passes. The live-visibility test proves the hidden thinking child never mounts, and the copy/export tests still contain reasoning.

- [ ] **Step 10: Run final Rust checks for all supported binaries**

```powershell
cd src-tauri
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
```

Expected: all commands pass.

- [ ] **Step 11: Commit rendering and copy behavior**

```powershell
git add src/hooks/use-acp-agents.ts src/hooks/use-acp-agents.test.ts src/components/message/content-parts-renderer.tsx src/components/message/content-parts-renderer.test.tsx src/components/message/live-transcript-row.tsx src/components/message/live-transcript-row.test.tsx src/components/message/message-list-view.tsx src/components/message/message-list-view.test.tsx src/lib/export-conversation.test.ts src/components/chat/agent-selector.test.tsx src/components/settings/acp-agent-settings.test.tsx src/components/settings/codebuddy-config-panel.test.tsx
git commit -m "feat(chat): honor agent thinking visibility"
```
