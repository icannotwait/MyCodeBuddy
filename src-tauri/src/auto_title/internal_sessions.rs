//! Registry of internal-only agent sessions (e.g. automatic title runs).
//!
//! Hides registered sessions from every Codeg parser-backed discovery path by
//! external ID, discovery lease, and reserved working-directory root.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use serde_json::Value;
use tokio::sync::{Mutex, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

use crate::db::entities::internal_agent_session::{self, InternalAgentSessionPurpose};
use crate::db::error::DbError;
use crate::models::AgentType;
use crate::parsers::normalize_path_for_matching;

/// Why Codeg registered an external agent session as internal-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InternalSessionPurpose {
    /// Automatic conversation-title generation.
    Title,
    /// On-demand document translation (hidden generation).
    Translate,
}

impl InternalSessionPurpose {
    fn as_entity(self) -> InternalAgentSessionPurpose {
        match self {
            InternalSessionPurpose::Title => InternalAgentSessionPurpose::Title,
            InternalSessionPurpose::Translate => InternalAgentSessionPurpose::Translate,
        }
    }
}

/// Immutable snapshot used while a shared discovery lease is held.
#[derive(Clone)]
pub struct InternalSessionFilter {
    ids: Arc<HashSet<(AgentType, String)>>,
    reserved_root: PathBuf,
}

impl InternalSessionFilter {
    pub fn contains(
        &self,
        agent_type: AgentType,
        external_id: Option<&str>,
        working_dir: Option<&str>,
    ) -> bool {
        external_id.is_some_and(|id| self.ids.contains(&(agent_type, id.to_owned())))
            || working_dir.is_some_and(|path| is_lexically_below(path, &self.reserved_root))
    }

    /// Test/debug accessor for the reserved title-run root.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn reserved_root(&self) -> &Path {
        &self.reserved_root
    }
}

/// Live ID set behind a mutex so post-start registrations stay visible.
/// Outer `Arc` shares the mutex; inner `Arc` clones cheap immutable snapshots.
type LiveInternalSessionIds = Arc<Mutex<Arc<HashSet<(AgentType, String)>>>>;

/// Process-wide registry of internal agent sessions + discovery ordering.
pub struct InternalAgentSessionRegistry {
    conn: DatabaseConnection,
    discovery: Arc<RwLock<()>>,
    ids: LiveInternalSessionIds,
    reserved_root: PathBuf,
}

impl InternalAgentSessionRegistry {
    /// Load persisted rows and prepare the reserved title-run root.
    pub async fn load(conn: DatabaseConnection, data_dir: &Path) -> Result<Arc<Self>, DbError> {
        let reserved_root = ensure_reserved_root(data_dir)?;
        let rows = internal_agent_session::Entity::find().all(&conn).await?;
        let mut set = HashSet::new();
        for row in rows {
            let agent_type = agent_type_from_db(&row.agent_type)?;
            set.insert((agent_type, row.external_id));
        }
        Ok(Arc::new(Self {
            conn,
            discovery: Arc::new(RwLock::new(())),
            ids: Arc::new(Mutex::new(Arc::new(set))),
            reserved_root,
        }))
    }

    /// Empty registry for freshly migrated test fixtures (no pre-existing rows).
    #[cfg(any(test, feature = "test-utils"))]
    pub fn new_empty_for_test(
        conn: DatabaseConnection,
        data_dir: &Path,
    ) -> Result<Arc<Self>, DbError> {
        let reserved_root = ensure_reserved_root(data_dir)?;
        Ok(Arc::new(Self {
            conn,
            discovery: Arc::new(RwLock::new(())),
            ids: Arc::new(Mutex::new(Arc::new(HashSet::new()))),
            reserved_root,
        }))
    }

    /// Absolute canonical reserved parent: `<data_dir>/internal/title-runs`.
    pub fn reserved_root(&self) -> &Path {
        &self.reserved_root
    }

    /// Exclusive discovery lease — held across title-run handshake registration.
    pub async fn exclusive_discovery_lease(&self) -> OwnedRwLockWriteGuard<()> {
        self.discovery.clone().write_owned().await
    }

    /// Shared discovery lease + immutable filter snapshot for list/detail/import.
    pub async fn shared_filter(
        &self,
    ) -> Result<(OwnedRwLockReadGuard<()>, InternalSessionFilter), DbError> {
        let guard = self.discovery.clone().read_owned().await;
        let ids = {
            let locked = self.ids.lock().await;
            Arc::clone(&*locked)
        };
        let filter = InternalSessionFilter {
            ids,
            reserved_root: self.reserved_root.clone(),
        };
        Ok((guard, filter))
    }

    /// Register under an already-held exclusive discovery lease.
    pub async fn register_with_lease(
        &self,
        _lease: &mut OwnedRwLockWriteGuard<()>,
        agent_type: AgentType,
        external_id: &str,
        purpose: InternalSessionPurpose,
    ) -> Result<(), DbError> {
        self.register_inner(agent_type, external_id, purpose).await
    }

    /// Acquire a short exclusive lease and register (post-handshake).
    pub async fn register(
        &self,
        agent_type: AgentType,
        external_id: &str,
        purpose: InternalSessionPurpose,
    ) -> Result<(), DbError> {
        let mut lease = self.exclusive_discovery_lease().await;
        self.register_with_lease(&mut lease, agent_type, external_id, purpose)
            .await
    }

    async fn register_inner(
        &self,
        agent_type: AgentType,
        external_id: &str,
        purpose: InternalSessionPurpose,
    ) -> Result<(), DbError> {
        let key = (agent_type, external_id.to_owned());

        // Insert into the in-memory set first so concurrent discovery cannot
        // observe the session even if persistence fails afterward.
        {
            let mut locked = self.ids.lock().await;
            if !locked.contains(&key) {
                let mut next = (**locked).clone();
                next.insert(key.clone());
                *locked = Arc::new(next);
            }
        }

        let agent_type_str = agent_type_to_db(agent_type)?;
        let existing = internal_agent_session::Entity::find()
            .filter(internal_agent_session::Column::AgentType.eq(&agent_type_str))
            .filter(internal_agent_session::Column::ExternalId.eq(external_id))
            .one(&self.conn)
            .await?;

        if let Some(row) = existing {
            // Same purpose is idempotent; never create a second row.
            if row.purpose == purpose.as_entity() {
                return Ok(());
            }
            return Err(DbError::Validation(format!(
                "internal session ({agent_type_str}, {external_id}) already registered for a different purpose"
            )));
        }

        let now = chrono::Utc::now();
        let model = internal_agent_session::ActiveModel {
            agent_type: Set(agent_type_str),
            external_id: Set(external_id.to_owned()),
            purpose: Set(purpose.as_entity()),
            created_at: Set(now),
        };
        model.insert(&self.conn).await?;
        Ok(())
    }
}

fn ensure_reserved_root(data_dir: &Path) -> Result<PathBuf, DbError> {
    let root = data_dir.join("internal").join("title-runs");
    std::fs::create_dir_all(&root)?;
    // Canonicalize once so lexical comparisons stay absolute even after
    // individual run directories are removed.
    let canonical = std::fs::canonicalize(&root)?;
    Ok(canonical)
}

fn agent_type_to_db(agent_type: AgentType) -> Result<String, DbError> {
    match serde_json::to_value(agent_type) {
        Ok(Value::String(s)) => Ok(s),
        Ok(other) => Err(DbError::Validation(format!(
            "AgentType did not serialize as string: {other}"
        ))),
        Err(e) => Err(DbError::Validation(format!(
            "failed to serialize AgentType: {e}"
        ))),
    }
}

fn agent_type_from_db(stored: &str) -> Result<AgentType, DbError> {
    serde_json::from_value(Value::String(stored.to_owned())).map_err(|e| {
        DbError::Validation(format!(
            "invalid agent_type in internal_agent_sessions: {stored}: {e}"
        ))
    })
}

/// True when `path` is the reserved root or a lexical child of it.
/// Rejects `..` traversal before prefix comparison.
pub fn is_lexically_below(path: &str, reserved_root: &Path) -> bool {
    let p = Path::new(path);
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    let norm_path = normalize_path_for_matching(path);
    let norm_root = normalize_path_for_matching(&reserved_root.to_string_lossy());
    if norm_path.is_empty() || norm_root.is_empty() {
        return false;
    }
    if norm_path == norm_root {
        return true;
    }
    let prefix = format!("{norm_root}/");
    norm_path.starts_with(&prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::entities::internal_agent_session::{self, InternalAgentSessionPurpose};
    use crate::db::test_helpers::fresh_in_memory_db;
    use crate::db::AppDatabase;
    use sea_orm::{
        ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter, Statement,
    };
    use tempfile::TempDir;

    pub struct RegistryFixture {
        pub db: AppDatabase,
        pub data_dir: TempDir,
        pub reserved_root: PathBuf,
        pub registry: Arc<InternalAgentSessionRegistry>,
    }

    pub async fn registry_fixture() -> RegistryFixture {
        let db = fresh_in_memory_db().await;
        let data_dir = TempDir::new().expect("tempdir");
        let registry = InternalAgentSessionRegistry::load(db.conn.clone(), data_dir.path())
            .await
            .expect("load empty registry");
        let reserved_root = registry.reserved_root().to_path_buf();
        RegistryFixture {
            db,
            data_dir,
            reserved_root,
            registry,
        }
    }

    #[tokio::test]
    async fn persisted_id_and_reserved_root_hide_sessions_after_restart() {
        let fixture = registry_fixture().await;
        fixture
            .registry
            .register(AgentType::Codex, "hidden-id", InternalSessionPurpose::Title)
            .await
            .expect("register");
        let restarted =
            InternalAgentSessionRegistry::load(fixture.db.conn.clone(), fixture.data_dir.path())
                .await
                .expect("reload");
        let (_, filter) = restarted.shared_filter().await.expect("filter");
        assert!(filter.contains(AgentType::Codex, Some("hidden-id"), None));
        assert!(filter.contains(
            AgentType::Gemini,
            None,
            Some(&fixture.reserved_root.join("orphan").to_string_lossy()),
        ));
    }

    #[tokio::test]
    async fn reserved_root_rejects_parent_dir_traversal() {
        let fixture = registry_fixture().await;
        let (_, filter) = fixture.registry.shared_filter().await.expect("filter");
        let under = fixture
            .reserved_root
            .join("child")
            .to_string_lossy()
            .into_owned();
        assert!(
            filter.contains(AgentType::Codex, None, Some(&under)),
            "child under reserved root must be hidden"
        );
        // Build a lexical `..` escape as a raw string. PathBuf::join on Windows
        // may collapse parent components after canonicalize, which would not
        // exercise the ParentDir rejection in is_lexically_below.
        let sneaky = format!(
            "{}{}..{}outside",
            fixture.reserved_root.to_string_lossy(),
            std::path::MAIN_SEPARATOR,
            std::path::MAIN_SEPARATOR
        );
        assert!(
            !filter.contains(AgentType::Codex, None, Some(&sneaky)),
            "paths with .. must not match via prefix escape"
        );
    }

    #[tokio::test]
    async fn register_is_idempotent_for_same_title_purpose() {
        let fixture = registry_fixture().await;
        fixture
            .registry
            .register(
                AgentType::ClaudeCode,
                "same-id",
                InternalSessionPurpose::Title,
            )
            .await
            .expect("first");
        fixture
            .registry
            .register(
                AgentType::ClaudeCode,
                "same-id",
                InternalSessionPurpose::Title,
            )
            .await
            .expect("second must succeed");
        let rows = internal_agent_session::Entity::find()
            .all(&fixture.db.conn)
            .await
            .expect("query");
        assert_eq!(rows.len(), 1, "duplicate must not create another row");
        assert_eq!(rows[0].agent_type, "claude_code");
        assert_eq!(rows[0].purpose, InternalAgentSessionPurpose::Title);
    }

    #[tokio::test]
    async fn register_translate_purpose_persists_and_hides_from_discovery() {
        let fixture = registry_fixture().await;
        fixture
            .registry
            .register(
                AgentType::Codex,
                "translate-id",
                InternalSessionPurpose::Translate,
            )
            .await
            .expect("register translate");
        let row = internal_agent_session::Entity::find()
            .filter(internal_agent_session::Column::ExternalId.eq("translate-id"))
            .one(&fixture.db.conn)
            .await
            .expect("query")
            .expect("row");
        assert_eq!(row.purpose, InternalAgentSessionPurpose::Translate);
        let (_, filter) = fixture.registry.shared_filter().await.expect("filter");
        assert!(filter.contains(AgentType::Codex, Some("translate-id"), None));

        // Same translate purpose is idempotent.
        fixture
            .registry
            .register(
                AgentType::Codex,
                "translate-id",
                InternalSessionPurpose::Translate,
            )
            .await
            .expect("idempotent translate");

        // Cross-purpose conflict still fails.
        let conflict = fixture
            .registry
            .register(
                AgentType::Codex,
                "translate-id",
                InternalSessionPurpose::Title,
            )
            .await;
        assert!(
            conflict.is_err(),
            "title must not overwrite translate purpose"
        );
    }

    #[tokio::test]
    async fn agent_type_persists_via_serde_string_not_display() {
        let fixture = registry_fixture().await;
        for (agent, expected) in [
            (AgentType::ClaudeCode, "claude_code"),
            (AgentType::Codex, "codex"),
            (AgentType::OpenCode, "open_code"),
            (AgentType::Gemini, "gemini"),
            (AgentType::Cline, "cline"),
            (AgentType::Hermes, "hermes"),
            (AgentType::CodeBuddy, "code_buddy"),
            (AgentType::KimiCode, "kimi_code"),
            (AgentType::Pi, "pi"),
            (AgentType::Grok, "grok"),
        ] {
            let ext = format!("id-{expected}");
            fixture
                .registry
                .register(agent, &ext, InternalSessionPurpose::Title)
                .await
                .expect("register");
            let row = internal_agent_session::Entity::find()
                .filter(internal_agent_session::Column::ExternalId.eq(&ext))
                .one(&fixture.db.conn)
                .await
                .expect("query")
                .expect("row");
            assert_eq!(
                row.agent_type, expected,
                "must store serde snake_case, not Display label {}",
                agent
            );
            assert_ne!(row.agent_type, agent.to_string());
        }

        let restarted =
            InternalAgentSessionRegistry::load(fixture.db.conn.clone(), fixture.data_dir.path())
                .await
                .expect("reload");
        let (_, filter) = restarted.shared_filter().await.expect("filter");
        assert!(filter.contains(AgentType::OpenCode, Some("id-open_code"), None));
        assert!(filter.contains(AgentType::CodeBuddy, Some("id-code_buddy"), None));
    }

    #[tokio::test]
    async fn register_with_lease_reuses_existing_exclusive_guard() {
        let fixture = registry_fixture().await;
        let mut lease = fixture.registry.exclusive_discovery_lease().await;
        fixture
            .registry
            .register_with_lease(
                &mut lease,
                AgentType::Pi,
                "leased-id",
                InternalSessionPurpose::Title,
            )
            .await
            .expect("register under lease");
        drop(lease);
        let (_, filter) = fixture.registry.shared_filter().await.expect("filter");
        assert!(filter.contains(AgentType::Pi, Some("leased-id"), None));
    }

    #[tokio::test]
    async fn exclusive_discovery_lease_blocks_shared_filter_until_released() {
        let fixture = registry_fixture().await;
        let exclusive = fixture.registry.exclusive_discovery_lease().await;
        let blocked = tokio::time::timeout(
            std::time::Duration::from_millis(80),
            fixture.registry.shared_filter(),
        )
        .await;
        assert!(
            blocked.is_err(),
            "shared filter must wait while exclusive discovery lease is held"
        );
        drop(exclusive);
        let got = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            fixture.registry.shared_filter(),
        )
        .await
        .expect("shared after exclusive release")
        .expect("filter ok");
        let _ = got;
    }

    #[tokio::test]
    async fn in_memory_exclusion_visible_after_successful_register_and_reload() {
        let fixture = registry_fixture().await;
        fixture
            .registry
            .register(AgentType::Grok, "persist-me", InternalSessionPurpose::Title)
            .await
            .expect("register");
        let (_, before) = fixture.registry.shared_filter().await.expect("filter");
        assert!(before.contains(AgentType::Grok, Some("persist-me"), None));
        let restarted =
            InternalAgentSessionRegistry::load(fixture.db.conn.clone(), fixture.data_dir.path())
                .await
                .expect("reload");
        let (_, after) = restarted.shared_filter().await.expect("filter");
        assert!(after.contains(AgentType::Grok, Some("persist-me"), None));
    }

    /// Spec: leave in-memory exclusion in place if persistence fails so discovery
    /// still hides the session; runner sees Err and sends no prompt.
    #[tokio::test]
    async fn in_memory_exclusion_survives_persistence_failure() {
        let fixture = registry_fixture().await;

        // Deterministic real DB failure after registry construction: drop the
        // persistence table through the same connection the registry uses.
        fixture
            .db
            .conn
            .execute(Statement::from_string(
                DatabaseBackend::Sqlite,
                "DROP TABLE internal_agent_sessions".to_owned(),
            ))
            .await
            .expect("drop internal_agent_sessions for forced persistence failure");

        let result = fixture
            .registry
            .register(
                AgentType::Codex,
                "db-fail-id",
                InternalSessionPurpose::Title,
            )
            .await;
        assert!(
            result.is_err(),
            "register must surface the persistence error; got {result:?}"
        );

        let (_, filter) = fixture.registry.shared_filter().await.expect("filter");
        assert!(
            filter.contains(AgentType::Codex, Some("db-fail-id"), None),
            "in-memory exclusion must remain after persistence failure"
        );
    }
}
