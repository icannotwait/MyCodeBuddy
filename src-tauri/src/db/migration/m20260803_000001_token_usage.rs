//! Token-usage analytics store: a per-turn fact table plus per-conversation
//! sync bookkeeping.
//!
//! Token counts are not stored anywhere today — they are re-derived every time
//! a conversation is opened, by parsing that agent's transcript from disk. That
//! is fine for one conversation and impossible for a dashboard, which needs to
//! sum across every conversation at once. So the dashboard reads a materialized
//! copy: [`TokenUsageTurn`] holds one row per usage-bearing turn, written by the
//! sync pass in `commands::token_usage`.
//!
//! Deliberately NOT denormalized: `folder_id`, `agent_type` and `deleted_at`
//! stay on `conversation` and are reached by JOIN at query time. A conversation
//! that moves folders (worktree re-parenting), changes agent, or is soft-deleted
//! is then reflected in the dashboard immediately — without a re-sync and
//! without a backfill migration. Only `model` is copied onto the fact, because
//! it genuinely varies turn-to-turn within one conversation.
//!
//! [`TokenUsageSync`] records what the last sync saw for a conversation. Its
//! `source_updated_at` mirrors `conversation.updated_at` as of that sync;
//! staleness is exact inequality (not `>`), so a row is re-parsed whenever the
//! conversation moved in either direction and a sync that lands in the same
//! millisecond as a turn is not silently skipped.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(TokenUsageTurn::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TokenUsageTurn::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(TokenUsageTurn::ConversationId)
                            .integer()
                            .not_null(),
                    )
                    // The parser's turn id. Not a uniqueness key — a sync
                    // replaces a conversation's rows wholesale inside one
                    // transaction — but it makes a fact traceable back to the
                    // turn it came from when a number looks wrong.
                    .col(ColumnDef::new(TokenUsageTurn::TurnKey).string().not_null())
                    .col(
                        ColumnDef::new(TokenUsageTurn::OccurredAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TokenUsageTurn::Model).string().null())
                    .col(
                        ColumnDef::new(TokenUsageTurn::InputTokens)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TokenUsageTurn::OutputTokens)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TokenUsageTurn::CacheCreationTokens)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TokenUsageTurn::CacheReadTokens)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    // Sum of the four counters, materialized so the hot
                    // "total tokens" aggregate never re-adds four columns.
                    .col(
                        ColumnDef::new(TokenUsageTurn::TotalTokens)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TokenUsageTurn::DurationMs)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;

        // Drives the per-conversation delete+reinsert of a re-sync, and the
        // JOIN back to `conversation` on every dashboard query.
        manager
            .create_index(
                Index::create()
                    .name("idx_token_usage_turn_conversation")
                    .table(TokenUsageTurn::Table)
                    .col(TokenUsageTurn::ConversationId)
                    .to_owned(),
            )
            .await?;

        // Every dashboard query is a range scan on this column.
        manager
            .create_index(
                Index::create()
                    .name("idx_token_usage_turn_occurred_at")
                    .table(TokenUsageTurn::Table)
                    .col(TokenUsageTurn::OccurredAt)
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(TokenUsageSync::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TokenUsageSync::ConversationId)
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(TokenUsageSync::SourceUpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TokenUsageSync::TurnCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TokenUsageSync::TotalTokens)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TokenUsageSync::SyncedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(TokenUsageTurn::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(TokenUsageSync::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum TokenUsageTurn {
    Table,
    Id,
    ConversationId,
    TurnKey,
    OccurredAt,
    Model,
    InputTokens,
    OutputTokens,
    CacheCreationTokens,
    CacheReadTokens,
    TotalTokens,
    DurationMs,
}

#[derive(DeriveIden)]
enum TokenUsageSync {
    Table,
    ConversationId,
    SourceUpdatedAt,
    TurnCount,
    TotalTokens,
    SyncedAt,
}
