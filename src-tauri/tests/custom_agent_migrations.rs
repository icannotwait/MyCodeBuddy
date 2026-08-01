use std::collections::BTreeSet;

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait};

use codeg_lib::db::migration::Migrator;

const CUSTOM_MIGRATIONS: [&str; 4] = [
    "m20260731_000001_custom_agent",
    "m20260731_000002_custom_agent_skills",
    "m20260731_000003_custom_agent_skills_dir",
    "m20260731_000004_custom_agent_source",
];

const UPSTREAM_CUSTOM_MIGRATIONS: [&str; 4] = [
    "m20260726_000001_custom_agent",
    "m20260727_000001_custom_agent_skills",
    "m20260728_000001_custom_agent_skills_dir",
    "m20260728_000002_custom_agent_source",
];

const FORK_WORKFLOW_MIGRATIONS: [&str; 4] = [
    "m20260726_000001_delegation_workflows",
    "m20260727_000001_workflow_structural_revision",
    "m20260727_000002_workflow_gate_fingerprints",
    "m20260727_000003_workflow_manifest_v2",
];

const WORKFLOW_TABLES: [&str; 5] = [
    "delegation_workflows",
    "delegation_workflow_manifest_revisions",
    "delegation_workflow_node_bindings",
    "delegation_workflow_gate_settlements",
    "delegation_workflow_run_bindings",
];

fn sql(text: impl Into<String>) -> Statement {
    Statement::from_string(DbBackend::Sqlite, text.into())
}

async fn open_db() -> DatabaseConnection {
    Database::connect("sqlite::memory:").await.unwrap()
}

async fn table_exists(db: &DatabaseConnection, table: &str) -> bool {
    db.query_one(sql(format!(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = '{table}'"
    )))
    .await
    .unwrap()
    .is_some()
}

async fn table_columns(db: &DatabaseConnection, table: &str) -> BTreeSet<String> {
    db.query_all(sql(format!("PRAGMA table_info({table})")))
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.try_get::<String>("", "name").unwrap())
        .collect()
}

async fn migration_versions(db: &DatabaseConnection) -> Vec<String> {
    db.query_all(sql("SELECT version FROM seaql_migrations ORDER BY version"))
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.try_get::<String>("", "version").unwrap())
        .collect()
}

fn assert_recorded_once(versions: &[String], names: &[&str]) {
    for name in names {
        assert_eq!(
            versions.iter().filter(|version| version == name).count(),
            1,
            "migration {name} must be recorded exactly once"
        );
    }
}

struct PreSyncForkMigrator;

#[async_trait::async_trait]
impl MigratorTrait for PreSyncForkMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        Migrator::migrations()
            .into_iter()
            .filter(|migration| {
                let name = migration.name().to_string();
                !CUSTOM_MIGRATIONS.contains(&name.as_str())
            })
            .collect()
    }
}

#[tokio::test]
async fn fresh_db_applies_renumbered_custom_agent_migrations() {
    let db = open_db().await;

    Migrator::up(&db, None).await.unwrap();

    assert!(table_exists(&db, "custom_agent").await);
    let columns = table_columns(&db, "custom_agent").await;
    for column in [
        "skills_shared_store",
        "skills_dir",
        "source",
        "version_probe",
    ] {
        assert!(columns.contains(column), "missing custom_agent.{column}");
    }

    let versions = migration_versions(&db).await;
    assert_recorded_once(&versions, &CUSTOM_MIGRATIONS);
    for upstream_name in UPSTREAM_CUSTOM_MIGRATIONS {
        assert!(
            !versions.iter().any(|version| version == upstream_name),
            "upstream migration identity {upstream_name} must be renumbered"
        );
    }
}

#[tokio::test]
async fn pre_sync_fork_db_adds_only_custom_agent_migrations() {
    let db = open_db().await;

    PreSyncForkMigrator::up(&db, None).await.unwrap();

    for table in WORKFLOW_TABLES {
        assert!(table_exists(&db, table).await, "missing fork table {table}");
    }
    let before = migration_versions(&db).await;
    assert_recorded_once(&before, &FORK_WORKFLOW_MIGRATIONS);
    for custom in CUSTOM_MIGRATIONS {
        assert!(!before.iter().any(|version| version == custom));
    }

    Migrator::up(&db, None).await.unwrap();

    let after = migration_versions(&db).await;
    let before_set: BTreeSet<_> = before.iter().cloned().collect();
    let newly_recorded: BTreeSet<_> = after
        .iter()
        .filter(|version| !before_set.contains(*version))
        .cloned()
        .collect();
    let expected_new: BTreeSet<_> = CUSTOM_MIGRATIONS
        .iter()
        .map(|name| (*name).to_string())
        .collect();

    assert_eq!(newly_recorded, expected_new);
    assert_recorded_once(&after, &CUSTOM_MIGRATIONS);
    assert_recorded_once(&after, &FORK_WORKFLOW_MIGRATIONS);
    assert!(table_exists(&db, "custom_agent").await);
}
