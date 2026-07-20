//! Cross-project conversation keyset cursor for reference search.

use std::collections::HashSet;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{
    ColumnTrait, Condition, DatabaseConnection, EntityTrait, JoinType, QueryFilter, QueryOrder,
    QuerySelect, RelationTrait,
};
use tokio_util::sync::CancellationToken;

use crate::app_error::{AppCommandError, AppErrorCode};
use crate::db::entities::conversation::{ConversationKind, ConversationStatus};
use crate::db::entities::{conversation, folder};
use crate::models::agent::AgentType;
use crate::parsers::fold_reference_links;
use crate::reference_search::matcher::{
    build_session_uri, match_reference_candidate, SearchPattern,
};
use crate::reference_search::sources::{ReferenceSourceCursor, SourcePage};
use crate::reference_search::types::{
    ReferenceCandidate, ReferenceCandidateMetadata, ReferenceDoneReason, ReferenceSearchSource,
};

/// Upper bound on seen IDs (matches max reference search limit).
const MAX_SEEN_IDS: usize = 500;
/// Rows fetched per SQL batch while scanning for matches.
const SQL_BATCH_SIZE: u64 = 64;

/// Pull-driven conversation cursor spanning all live projects.
pub struct ConversationCursor {
    conn: DatabaseConnection,
    pattern: SearchPattern,
    limit: usize,
    source_ordinal: u64,
    published: usize,
    /// Keyset: last *eligible* row visited `(updated_at, id)` for DESC paging.
    keyset: Option<(DateTime<Utc>, i32)>,
    seen_ids: HashSet<i32>,
    finished: bool,
}

impl ConversationCursor {
    pub fn open(conn: DatabaseConnection, pattern: SearchPattern, limit: usize) -> Self {
        Self {
            conn,
            pattern,
            limit,
            source_ordinal: 0,
            published: 0,
            keyset: None,
            seen_ids: HashSet::new(),
            finished: false,
        }
    }
}

/// Build the authoritative conversation candidate (no regex rank). Shared with
/// Task 5 validation so field mapping cannot diverge.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_conversation_candidate(
    conversation_id: i32,
    title: Option<&str>,
    agent_type: AgentType,
    status: ConversationStatus,
    branch: Option<String>,
    project_name: String,
    project_path: String,
    source_ordinal: u64,
) -> ReferenceCandidate {
    let status_wire = conversation_status_wire(&status);
    let agent_wire = agent_type_wire(agent_type);
    let folded = fold_reference_links(title.unwrap_or(""));
    let label = if folded.is_empty() {
        format!("#{conversation_id}")
    } else {
        folded
    };
    let detail = Some(branch.clone().unwrap_or_else(|| status_wire.clone()));
    let keywords = format!("{label} {agent_wire}");

    ReferenceCandidate {
        source: ReferenceSearchSource::Conversation,
        uri: build_session_uri(conversation_id),
        id: conversation_id.to_string(),
        label,
        detail,
        keywords,
        metadata: ReferenceCandidateMetadata::Conversation {
            conversation_id,
            agent_type,
            status: status_wire,
            branch,
            project_name,
            project_path,
        },
        source_ordinal,
        regex_rank: None,
    }
}

fn conversation_status_wire(status: &ConversationStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "in_progress".to_string())
}

fn agent_type_wire(agent_type: AgentType) -> String {
    serde_json::to_value(agent_type)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{agent_type:?}").to_lowercase())
}

fn parse_agent_type(raw: &str) -> Option<AgentType> {
    serde_json::from_value(serde_json::Value::String(raw.to_string())).ok()
}

fn cancelled() -> AppCommandError {
    AppCommandError::new(AppErrorCode::Cancelled, "reference search cancelled")
}

#[async_trait]
impl ReferenceSourceCursor for ConversationCursor {
    async fn next_page(
        &mut self,
        page_size: usize,
        token: CancellationToken,
    ) -> Result<SourcePage, AppCommandError> {
        if token.is_cancelled() {
            return Err(cancelled());
        }
        if self.finished || self.published >= self.limit {
            self.finished = true;
            return Ok(SourcePage {
                items: Vec::new(),
                source_epoch: None,
                done: true,
                done_reason: Some(if self.published >= self.limit {
                    ReferenceDoneReason::Limit
                } else {
                    ReferenceDoneReason::Exhausted
                }),
            });
        }

        let remaining = self.limit - self.published;
        let want = page_size.min(remaining);
        if want == 0 {
            self.finished = true;
            return Ok(SourcePage {
                items: Vec::new(),
                source_epoch: None,
                done: true,
                done_reason: Some(ReferenceDoneReason::Limit),
            });
        }

        let mut items = Vec::with_capacity(want);
        let mut exhausted = false;

        while items.len() < want {
            if token.is_cancelled() {
                return Err(cancelled());
            }

            let batch = self.fetch_batch().await?;
            if batch.is_empty() {
                exhausted = true;
                break;
            }

            for (conv, folder_row) in batch {
                if token.is_cancelled() {
                    return Err(cancelled());
                }

                // Advance keyset for every eligible SQL row so paging continues.
                self.keyset = Some((conv.updated_at, conv.id));

                if self.seen_ids.contains(&conv.id) {
                    continue;
                }

                let Some(folder_row) = folder_row else {
                    continue;
                };

                let Some(agent_type) = parse_agent_type(&conv.agent_type) else {
                    tracing::debug!(
                        conversation_id = conv.id,
                        agent_type = %conv.agent_type,
                        "skipping conversation with unknown agent_type"
                    );
                    continue;
                };

                // Eligible row: ordinal before matching.
                self.source_ordinal = self.source_ordinal.saturating_add(1);
                let ordinal = self.source_ordinal;

                let candidate = build_conversation_candidate(
                    conv.id,
                    conv.title.as_deref(),
                    agent_type,
                    conv.status,
                    conv.git_branch,
                    folder_row.name,
                    folder_row.path,
                    ordinal,
                );

                let Some(field_match) = match_reference_candidate(&self.pattern, &candidate) else {
                    continue;
                };

                // Bound seen IDs by the protocol max limit (500).
                if self.seen_ids.len() < MAX_SEEN_IDS {
                    self.seen_ids.insert(conv.id);
                }

                let mut published = candidate;
                published.regex_rank = field_match.regex_rank;
                items.push(published);

                if items.len() >= want {
                    break;
                }
            }
        }

        self.published += items.len();
        let hit_limit = self.published >= self.limit;
        let done = exhausted || hit_limit || items.len() < want;
        if done {
            self.finished = true;
        }
        let done_reason = if done {
            Some(if hit_limit {
                ReferenceDoneReason::Limit
            } else {
                ReferenceDoneReason::Exhausted
            })
        } else {
            None
        };

        Ok(SourcePage {
            items,
            source_epoch: None,
            done,
            done_reason,
        })
    }

    async fn close(&mut self) {
        self.finished = true;
    }
}

impl ConversationCursor {
    async fn fetch_batch(
        &self,
    ) -> Result<Vec<(conversation::Model, Option<folder::Model>)>, AppCommandError> {
        let mut query = conversation::Entity::find()
            .join(JoinType::InnerJoin, conversation::Relation::Folder.def())
            .filter(conversation::Column::DeletedAt.is_null())
            .filter(conversation::Column::ParentId.is_null())
            .filter(
                conversation::Column::Kind
                    .is_in([ConversationKind::Regular, ConversationKind::Chat]),
            )
            .filter(folder::Column::DeletedAt.is_null())
            .order_by_desc(conversation::Column::UpdatedAt)
            .order_by_desc(conversation::Column::Id)
            .limit(SQL_BATCH_SIZE);

        if let Some((updated_at, id)) = self.keyset {
            // DESC keyset: continue with rows strictly after the last visited.
            query = query.filter(
                Condition::any()
                    .add(conversation::Column::UpdatedAt.lt(updated_at))
                    .add(
                        Condition::all()
                            .add(conversation::Column::UpdatedAt.eq(updated_at))
                            .add(conversation::Column::Id.lt(id)),
                    ),
            );
        }

        query
            .select_also(folder::Entity)
            .all(&self.conn)
            .await
            .map_err(|err| AppCommandError::from(crate::db::error::DbError::from(err)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::delegation::spawner::DelegationLink;
    use crate::db::entities::folder as folder_entity;
    use crate::db::service::{conversation_service, folder_service};
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::db::AppDatabase;
    use crate::models::agent::AgentType;
    use crate::reference_search::sources::{drain_cursor, literal};
    use chrono::Duration;
    use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};
    use std::collections::HashSet;

    pub struct ConversationSearchFixture {
        pub db: AppDatabase,
        pub folder_a: i32,
        pub folder_b: i32,
    }

    pub async fn conversation_search_fixture() -> ConversationSearchFixture {
        let db = fresh_in_memory_db().await;
        let folder_a = seed_folder(&db, "/tmp/ref-search-project-a").await;
        let folder_b = seed_folder(&db, "/tmp/ref-search-project-b").await;

        // Rename folder B so metadata asserts can match the display name.
        if let Some(row) = folder_entity::Entity::find_by_id(folder_b)
            .one(&db.conn)
            .await
            .expect("load folder b")
        {
            let mut active = row.into_active_model();
            active.name = Set("Project B".to_string());
            active.update(&db.conn).await.expect("rename folder b");
        }

        ConversationSearchFixture {
            db,
            folder_a,
            folder_b,
        }
    }

    impl ConversationSearchFixture {
        pub async fn seed_regular_chat_delegate_loop_and_deleted(&self) {
            // Project A: several matching roots + excluded kinds.
            for i in 0..4 {
                conversation_service::create(
                    &self.db.conn,
                    self.folder_a,
                    AgentType::ClaudeCode,
                    Some(format!("match A regular {i}")),
                    Some("main".to_string()),
                )
                .await
                .expect("regular a");
            }
            conversation_service::create_chat(
                &self.db.conn,
                self.folder_a,
                AgentType::Codex,
                Some("match A chat".to_string()),
                None,
            )
            .await
            .expect("chat a");

            let parent = conversation_service::create(
                &self.db.conn,
                self.folder_a,
                AgentType::ClaudeCode,
                Some("match parent for delegate".to_string()),
                None,
            )
            .await
            .expect("parent");
            conversation_service::create_with_delegation(
                &self.db.conn,
                self.folder_a,
                AgentType::Codex,
                Some("match delegate child".to_string()),
                None,
                Some(DelegationLink {
                    parent_conversation_id: parent.id,
                    parent_tool_use_id: "tool-1".to_string(),
                    delegation_call_id: "call-1".to_string(),
                }),
            )
            .await
            .expect("delegate");

            let loop_row = conversation_service::create(
                &self.db.conn,
                self.folder_a,
                AgentType::ClaudeCode,
                Some("match loop row".to_string()),
                None,
            )
            .await
            .expect("loop seed");
            {
                let mut active = loop_row.into_active_model();
                active.kind = Set(ConversationKind::Loop);
                active.update(&self.db.conn).await.expect("mark loop");
            }

            let deleted = conversation_service::create(
                &self.db.conn,
                self.folder_a,
                AgentType::ClaudeCode,
                Some("match deleted row".to_string()),
                None,
            )
            .await
            .expect("deleted seed");
            conversation_service::soft_delete(&self.db.conn, deleted.id)
                .await
                .expect("soft delete");

            // Project B: more matching roots for cross-project coverage.
            for i in 0..3 {
                conversation_service::create(
                    &self.db.conn,
                    self.folder_b,
                    AgentType::Gemini,
                    Some(format!("match B regular {i}")),
                    Some("feature".to_string()),
                )
                .await
                .expect("regular b");
            }
            conversation_service::create_chat(
                &self.db.conn,
                self.folder_b,
                AgentType::OpenCode,
                Some("match B chat".to_string()),
                None,
            )
            .await
            .expect("chat b");

            // Ensure folders stay open/live (seed_folder already opens them).
            let _ = folder_service::list_open_folders(&self.db.conn)
                .await
                .expect("open folders");
        }

        pub async fn move_below_cursor(&self, conversation_id: i32) {
            let row = conversation::Entity::find_by_id(conversation_id)
                .one(&self.db.conn)
                .await
                .expect("load")
                .expect("exists");
            // Strictly older than any first-page keyset boundary so DESC paging
            // will re-encounter the row after the captured keyset.
            let older = row.updated_at - Duration::days(30);
            let mut active = row.into_active_model();
            active.updated_at = Set(older);
            active.update(&self.db.conn).await.expect("move below");
        }
    }

    #[tokio::test]
    async fn conversation_cursor_spans_projects_excludes_non_roots_and_deduplicates_moves() {
        let fixture = conversation_search_fixture().await;
        fixture.seed_regular_chat_delegate_loop_and_deleted().await;
        let mut cursor = ConversationCursor::open(fixture.db.conn.clone(), literal("match"), 12);
        let first = cursor.next_page(5, CancellationToken::new()).await.unwrap();
        fixture
            .move_below_cursor(first.items[0].id.parse().unwrap())
            .await;
        let rest = drain_cursor(&mut cursor).await;
        let ids: HashSet<_> = first
            .items
            .iter()
            .chain(rest.iter())
            .map(|item| item.id.clone())
            .collect();
        assert_eq!(ids.len(), first.items.len() + rest.len());
        assert!(first.items.iter().chain(rest.iter()).all(|item| matches!(
            &item.metadata,
            ReferenceCandidateMetadata::Conversation { .. }
        )));
        assert!(first.items.iter().chain(rest.iter()).any(|item| matches!(
            &item.metadata,
            ReferenceCandidateMetadata::Conversation { project_name, .. }
                if project_name == "Project B"
        )));
    }
}
