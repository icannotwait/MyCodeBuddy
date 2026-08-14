use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // work_task_template: reusable task blueprints (name + title seed +
        // the same JSON `WorkTaskConfig` a task stores). Global on purpose —
        // templates are prose prompts, not folder-bound state; the folder is
        // picked at creation time.
        manager
            .create_table(
                Table::create()
                    .table(WorkTaskTemplate::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(WorkTaskTemplate::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(WorkTaskTemplate::Name).text().not_null())
                    .col(ColumnDef::new(WorkTaskTemplate::Title).text().not_null())
                    .col(ColumnDef::new(WorkTaskTemplate::Config).text().not_null())
                    .col(
                        ColumnDef::new(WorkTaskTemplate::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(WorkTaskTemplate::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(WorkTaskTemplate::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum WorkTaskTemplate {
    Table,
    Id,
    Name,
    Title,
    Config,
    CreatedAt,
    UpdatedAt,
}
