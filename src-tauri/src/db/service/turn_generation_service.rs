//! Persist and overlay live request-usage generation stats.

use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
};

use crate::db::entities::turn_generation_stat;
use crate::db::error::DbError;
use crate::models::{MessageTurn, TurnRole};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationStat {
    pub user_ordinal: i32,
    pub generation_ms: u64,
    pub generation_tokens: u64,
}

pub async fn upsert(
    conn: &DatabaseConnection,
    conversation_id: i32,
    stat: GenerationStat,
) -> Result<(), DbError> {
    if stat.generation_ms == 0 || stat.generation_tokens == 0 {
        return Ok(());
    }
    let model = turn_generation_stat::ActiveModel {
        conversation_id: Set(conversation_id),
        user_ordinal: Set(stat.user_ordinal),
        generation_ms: Set(stat.generation_ms as i64),
        generation_tokens: Set(stat.generation_tokens as i64),
    };
    turn_generation_stat::Entity::insert(model)
        .on_conflict(
            OnConflict::columns([
                turn_generation_stat::Column::ConversationId,
                turn_generation_stat::Column::UserOrdinal,
            ])
            .update_columns([
                turn_generation_stat::Column::GenerationMs,
                turn_generation_stat::Column::GenerationTokens,
            ])
            .to_owned(),
        )
        .exec(conn)
        .await?;
    Ok(())
}

pub async fn list_for_conversation(
    conn: &DatabaseConnection,
    conversation_id: i32,
) -> Result<Vec<GenerationStat>, DbError> {
    let rows = turn_generation_stat::Entity::find()
        .filter(turn_generation_stat::Column::ConversationId.eq(conversation_id))
        .order_by_asc(turn_generation_stat::Column::UserOrdinal)
        .all(conn)
        .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            Some(GenerationStat {
                user_ordinal: row.user_ordinal,
                generation_ms: u64::try_from(row.generation_ms).ok()?,
                generation_tokens: u64::try_from(row.generation_tokens).ok()?,
            })
        })
        .collect())
}

/// Stamp the first assistant after each matching user ordinal.
pub fn overlay_generation_stats(turns: &mut [MessageTurn], stats: &[GenerationStat]) {
    if stats.is_empty() {
        return;
    }
    let mut by_ordinal = std::collections::HashMap::new();
    for stat in stats {
        by_ordinal.insert(stat.user_ordinal, *stat);
    }
    let mut user_idx = 0i32;
    let mut i = 0;
    while i < turns.len() {
        if !matches!(turns[i].role, TurnRole::User) {
            i += 1;
            continue;
        }
        if let Some(stat) = by_ordinal.get(&user_idx) {
            let end = turns.len();
            let mut j = i + 1;
            while j < end {
                match turns[j].role {
                    TurnRole::User => break,
                    TurnRole::Assistant => {
                        if turns[j].generation_ms.is_none() {
                            turns[j].generation_ms = Some(stat.generation_ms);
                            turns[j].generation_tokens = Some(stat.generation_tokens);
                        }
                        break;
                    }
                    _ => {}
                }
                j += 1;
            }
        }
        user_idx += 1;
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn turn(id: &str, role: TurnRole) -> MessageTurn {
        MessageTurn {
            id: id.into(),
            role,
            blocks: vec![],
            timestamp: Utc::now(),
            usage: None,
            duration_ms: None,
            model: None,
            reasoning_effort: None,
            completed_at: None,
            outcome: None,
            autonomous_origin: None,
            generation_ms: None,
            generation_tokens: None,
        }
    }

    #[test]
    fn overlay_stamps_first_assistant_after_matching_user() {
        let mut turns = vec![
            turn("u0", TurnRole::User),
            turn("a0", TurnRole::Assistant),
            turn("u1", TurnRole::User),
            turn("a1", TurnRole::Assistant),
            turn("a1b", TurnRole::Assistant),
        ];
        overlay_generation_stats(
            &mut turns,
            &[GenerationStat {
                user_ordinal: 1,
                generation_ms: 2500,
                generation_tokens: 400,
            }],
        );
        assert_eq!(turns[1].generation_ms, None);
        assert_eq!(turns[3].generation_ms, Some(2500));
        assert_eq!(turns[3].generation_tokens, Some(400));
        assert_eq!(turns[4].generation_ms, None);
    }

    #[test]
    fn overlay_skips_already_stamped_assistant() {
        let mut turns = vec![turn("u0", TurnRole::User), turn("a0", TurnRole::Assistant)];
        turns[1].generation_ms = Some(1);
        overlay_generation_stats(
            &mut turns,
            &[GenerationStat {
                user_ordinal: 0,
                generation_ms: 99,
                generation_tokens: 9,
            }],
        );
        assert_eq!(turns[1].generation_ms, Some(1));
    }

    #[test]
    fn overlay_does_not_cross_the_next_user() {
        let mut turns = vec![
            turn("u0", TurnRole::User),
            turn("u1", TurnRole::User),
            turn("a1", TurnRole::Assistant),
        ];
        overlay_generation_stats(
            &mut turns,
            &[GenerationStat {
                user_ordinal: 0,
                generation_ms: 900,
                generation_tokens: 10,
            }],
        );
        assert_eq!(turns[2].generation_ms, None);
    }
}
