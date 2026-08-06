use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"CREATE TABLE delegation_plan_round_authorizations (
                   workflow_id TEXT NOT NULL,
                   gate_id TEXT NOT NULL,
                   gate_lineage TEXT NOT NULL,
                   review_round INTEGER NOT NULL,
                   author_task_id TEXT NOT NULL,
                   authorization_json TEXT NOT NULL,
                   authorization_digest TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   PRIMARY KEY(workflow_id, gate_id),
                   FOREIGN KEY(workflow_id, gate_id)
                     REFERENCES delegation_workflow_gate_states(workflow_id, gate_id)
                     ON DELETE CASCADE
                 )"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE delegation_plan_round_authorizations")
            .await?;
        Ok(())
    }
}
