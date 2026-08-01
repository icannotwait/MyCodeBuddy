use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(CustomAgent::Table)
                    // Absolute path of the agent's own, dedicated skills
                    // directory, for agents that do not follow the shared
                    // `.agents/skills` convention. NULL means "not declared";
                    // it composes with `skills_shared_store` rather than
                    // replacing it.
                    .add_column(ColumnDef::new(CustomAgent::SkillsDir).text().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(CustomAgent::Table)
                    .drop_column(CustomAgent::SkillsDir)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum CustomAgent {
    Table,
    SkillsDir,
}
