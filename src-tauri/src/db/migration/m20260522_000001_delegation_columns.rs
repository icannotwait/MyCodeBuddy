use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const IDX_PARENT_TOOL_USE_ID: &str = "idx_conversation_parent_tool_use_id";
const IDX_DELEGATION_CALL_ID: &str = "idx_conversation_delegation_call_id";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Conversation::Table)
                    .add_column(ColumnDef::new(Conversation::ParentToolUseId).text().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Conversation::Table)
                    .add_column(ColumnDef::new(Conversation::DelegationCallId).text().null())
                    .to_owned(),
            )
            .await?;

        // Both columns are queried as filter predicates from the conversation
        // list path (`include_children` filter + `list_child_conversations`)
        // once delegation starts producing sub-sessions. Without indexes SQLite
        // falls back to a full table scan over conversation, which grows
        // linearly with session history.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(IDX_PARENT_TOOL_USE_ID)
                    .table(Conversation::Table)
                    .col(Conversation::ParentToolUseId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(IDX_DELEGATION_CALL_ID)
                    .table(Conversation::Table)
                    .col(Conversation::DelegationCallId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name(IDX_DELEGATION_CALL_ID)
                    .table(Conversation::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name(IDX_PARENT_TOOL_USE_ID)
                    .table(Conversation::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Conversation::Table)
                    .drop_column(Conversation::DelegationCallId)
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Conversation::Table)
                    .drop_column(Conversation::ParentToolUseId)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Conversation {
    Table,
    ParentToolUseId,
    DelegationCallId,
}
