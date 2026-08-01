use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection,
    DatabaseTransaction, EntityTrait, PaginatorTrait, QueryFilter, Set,
};
use serde_json::{Map, Value};

use crate::db::entities::recovery_authorization::{self, RecoveryAuthorizationStatus};

use super::{
    AuthorizationConsumeExpectation, RecoveryAuthorizationError, RecoveryChallenge,
    TERMINAL_AUTHORIZATION_RETENTION_DAYS,
};

#[derive(Clone)]
pub struct RecoveryAuthorizationStore {
    conn: DatabaseConnection,
}

impl RecoveryAuthorizationStore {
    pub fn new(conn: DatabaseConnection) -> Self {
        Self { conn }
    }

    pub fn connection(&self) -> &DatabaseConnection {
        &self.conn
    }

    pub async fn find_by_id(
        &self,
        authorization_id: &str,
    ) -> Result<Option<recovery_authorization::Model>, RecoveryAuthorizationError> {
        Ok(recovery_authorization::Entity::find_by_id(authorization_id)
            .one(&self.conn)
            .await?)
    }

    pub async fn find_active(
        &self,
        challenge: &RecoveryChallenge,
    ) -> Result<Option<recovery_authorization::Model>, RecoveryAuthorizationError> {
        Ok(recovery_authorization::Entity::find()
            .filter(
                recovery_authorization::Column::ParentConversationId
                    .eq(challenge.parent_conversation_id),
            )
            .filter(recovery_authorization::Column::SubjectKind.eq(challenge.subject_kind.as_str()))
            .filter(recovery_authorization::Column::SubjectId.eq(&challenge.subject_id))
            .filter(
                recovery_authorization::Column::SourceStateFingerprint
                    .eq(&challenge.source_state_fingerprint),
            )
            .filter(recovery_authorization::Column::Status.is_in([
                RecoveryAuthorizationStatus::Pending,
                RecoveryAuthorizationStatus::Approved,
            ]))
            .one(&self.conn)
            .await?)
    }

    pub async fn insert_pending(
        &self,
        challenge: &RecoveryChallenge,
        now: DateTime<Utc>,
    ) -> Result<recovery_authorization::Model, RecoveryAuthorizationError> {
        let identity = challenge.delegation_identity.as_ref();
        Ok(recovery_authorization::ActiveModel {
            authorization_id: Set(uuid::Uuid::new_v4().to_string()),
            parent_conversation_id: Set(challenge.parent_conversation_id),
            subject_kind: Set(challenge.subject_kind.as_str().to_string()),
            subject_id: Set(challenge.subject_id.clone()),
            source_task_id: Set(identity.map(|value| value.source_task_id.clone())),
            child_conversation_id: Set(identity.and_then(|value| value.child_conversation_id)),
            lineage_root_task_id: Set(identity.map(|value| value.lineage_root_task_id.clone())),
            work_unit_key: Set(identity.and_then(|value| value.work_unit_key.clone())),
            source_state_fingerprint: Set(challenge.source_state_fingerprint.clone()),
            allowed_action: Set(challenge.allowed_action.as_str().to_string()),
            action_payload_json: Set(canonical_json(&challenge.action_payload)?),
            cause_code: Set(challenge.cause_code.clone()),
            risk_class: Set(challenge.risk_class.clone()),
            display_reason: Set(challenge.display_reason.clone()),
            status: Set(RecoveryAuthorizationStatus::Pending),
            question_id: Set(None),
            requested_at: Set(now),
            approved_at: Set(None),
            expires_at: Set(None),
            consumed_at: Set(None),
            consumed_by_kind: Set(None),
            consumed_by_id: Set(None),
            consumer_correlation_id: Set(None),
        }
        .insert(&self.conn)
        .await?)
    }

    pub async fn bind_question(
        &self,
        authorization_id: &str,
        question_id: &str,
    ) -> Result<recovery_authorization::Model, RecoveryAuthorizationError> {
        recovery_authorization::Entity::update_many()
            .col_expr(
                recovery_authorization::Column::QuestionId,
                Expr::value(Some(question_id.to_string())),
            )
            .filter(recovery_authorization::Column::AuthorizationId.eq(authorization_id))
            .filter(recovery_authorization::Column::Status.eq(RecoveryAuthorizationStatus::Pending))
            .filter(
                recovery_authorization::Column::QuestionId
                    .is_null()
                    .or(recovery_authorization::Column::QuestionId.eq(question_id)),
            )
            .exec(&self.conn)
            .await?;
        let row = self
            .find_by_id(authorization_id)
            .await?
            .ok_or(RecoveryAuthorizationError::NotFound)?;
        if row.status == RecoveryAuthorizationStatus::Pending
            && row.question_id.as_deref() != Some(question_id)
        {
            return Err(RecoveryAuthorizationError::QuestionBindingConflict);
        }
        Ok(row)
    }

    pub async fn approve_pending(
        &self,
        authorization_id: &str,
        approved_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<recovery_authorization::Model, RecoveryAuthorizationError> {
        recovery_authorization::Entity::update_many()
            .col_expr(
                recovery_authorization::Column::Status,
                Expr::value(RecoveryAuthorizationStatus::Approved),
            )
            .col_expr(
                recovery_authorization::Column::ApprovedAt,
                Expr::value(Some(approved_at)),
            )
            .col_expr(
                recovery_authorization::Column::ExpiresAt,
                Expr::value(Some(expires_at)),
            )
            .filter(recovery_authorization::Column::AuthorizationId.eq(authorization_id))
            .filter(recovery_authorization::Column::Status.eq(RecoveryAuthorizationStatus::Pending))
            .exec(&self.conn)
            .await?;
        self.find_by_id(authorization_id)
            .await?
            .ok_or(RecoveryAuthorizationError::NotFound)
    }

    pub async fn decline_pending(
        &self,
        authorization_id: &str,
    ) -> Result<recovery_authorization::Model, RecoveryAuthorizationError> {
        recovery_authorization::Entity::update_many()
            .col_expr(
                recovery_authorization::Column::Status,
                Expr::value(RecoveryAuthorizationStatus::Declined),
            )
            .filter(recovery_authorization::Column::AuthorizationId.eq(authorization_id))
            .filter(recovery_authorization::Column::Status.eq(RecoveryAuthorizationStatus::Pending))
            .exec(&self.conn)
            .await?;
        self.find_by_id(authorization_id)
            .await?
            .ok_or(RecoveryAuthorizationError::NotFound)
    }

    pub async fn abandon_pending(
        &self,
        authorization_id: &str,
        question_id: Option<&str>,
    ) -> Result<recovery_authorization::Model, RecoveryAuthorizationError> {
        let mut update = recovery_authorization::Entity::update_many()
            .col_expr(
                recovery_authorization::Column::Status,
                Expr::value(RecoveryAuthorizationStatus::Abandoned),
            )
            .filter(recovery_authorization::Column::AuthorizationId.eq(authorization_id))
            .filter(
                recovery_authorization::Column::Status.eq(RecoveryAuthorizationStatus::Pending),
            );
        update = match question_id {
            Some(question_id) => update.filter(
                recovery_authorization::Column::QuestionId
                    .eq(question_id)
                    .or(recovery_authorization::Column::QuestionId.is_null()),
            ),
            None => update.filter(recovery_authorization::Column::QuestionId.is_null()),
        };
        update.exec(&self.conn).await?;
        self.find_by_id(authorization_id)
            .await?
            .ok_or(RecoveryAuthorizationError::NotFound)
    }

    pub async fn expire_if_due(
        &self,
        authorization_id: &str,
        now: DateTime<Utc>,
    ) -> Result<recovery_authorization::Model, RecoveryAuthorizationError> {
        recovery_authorization::Entity::update_many()
            .col_expr(
                recovery_authorization::Column::Status,
                Expr::value(RecoveryAuthorizationStatus::Expired),
            )
            .filter(recovery_authorization::Column::AuthorizationId.eq(authorization_id))
            .filter(
                recovery_authorization::Column::Status.eq(RecoveryAuthorizationStatus::Approved),
            )
            .filter(recovery_authorization::Column::ExpiresAt.lte(now))
            .exec(&self.conn)
            .await?;
        self.find_by_id(authorization_id)
            .await?
            .ok_or(RecoveryAuthorizationError::NotFound)
    }

    pub async fn count_active(
        &self,
        parent_conversation_id: i32,
    ) -> Result<u64, RecoveryAuthorizationError> {
        Ok(recovery_authorization::Entity::find()
            .filter(recovery_authorization::Column::ParentConversationId.eq(parent_conversation_id))
            .filter(recovery_authorization::Column::Status.is_in([
                RecoveryAuthorizationStatus::Pending,
                RecoveryAuthorizationStatus::Approved,
            ]))
            .count(&self.conn)
            .await?)
    }

    pub async fn list_for_parent(
        &self,
        parent_conversation_id: i32,
    ) -> Result<Vec<recovery_authorization::Model>, RecoveryAuthorizationError> {
        Ok(recovery_authorization::Entity::find()
            .filter(recovery_authorization::Column::ParentConversationId.eq(parent_conversation_id))
            .all(&self.conn)
            .await?)
    }

    pub async fn prune_terminal(
        &self,
        now: DateTime<Utc>,
    ) -> Result<u64, RecoveryAuthorizationError> {
        let cutoff = now - Duration::days(TERMINAL_AUTHORIZATION_RETENTION_DAYS);
        Ok(recovery_authorization::Entity::delete_many()
            .filter(
                Condition::any()
                    .add(
                        Condition::all()
                            .add(
                                recovery_authorization::Column::Status
                                    .eq(RecoveryAuthorizationStatus::Declined),
                            )
                            // Task 1 has no decline transition timestamp. Match
                            // automation history retention by terminal status
                            // plus creation time rather than expanding schema.
                            .add(recovery_authorization::Column::RequestedAt.lt(cutoff)),
                    )
                    .add(
                        Condition::all()
                            .add(
                                recovery_authorization::Column::Status
                                    .eq(RecoveryAuthorizationStatus::Consumed),
                            )
                            .add(recovery_authorization::Column::ConsumedAt.lt(cutoff)),
                    )
                    .add(
                        Condition::all()
                            .add(
                                recovery_authorization::Column::Status
                                    .eq(RecoveryAuthorizationStatus::Expired),
                            )
                            .add(recovery_authorization::Column::ExpiresAt.lt(cutoff)),
                    )
                    .add(
                        Condition::all()
                            .add(
                                recovery_authorization::Column::Status
                                    .eq(RecoveryAuthorizationStatus::Abandoned),
                            )
                            // As above, abandonment has no transition timestamp
                            // in the approved authorization model.
                            .add(recovery_authorization::Column::RequestedAt.lt(cutoff)),
                    ),
            )
            .exec(&self.conn)
            .await?
            .rows_affected)
    }
}

pub fn canonical_json(value: &Value) -> Result<String, RecoveryAuthorizationError> {
    fn canonicalize(value: &Value) -> Value {
        match value {
            Value::Object(object) => {
                let mut keys: Vec<_> = object.keys().collect();
                keys.sort_unstable();
                let mut ordered = Map::new();
                for key in keys {
                    ordered.insert(key.clone(), canonicalize(&object[key]));
                }
                Value::Object(ordered)
            }
            Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
            other => other.clone(),
        }
    }

    serde_json::to_string(&canonicalize(value))
        .map_err(|error| RecoveryAuthorizationError::Database(error.to_string()))
}

fn validate_challenge_fields(
    row: &recovery_authorization::Model,
    expected: &AuthorizationConsumeExpectation<'_>,
) -> Result<(), RecoveryAuthorizationError> {
    if row.parent_conversation_id != expected.parent_conversation_id {
        return Err(RecoveryAuthorizationError::ParentMismatch);
    }
    if row.subject_kind != expected.subject_kind.as_str() {
        return Err(RecoveryAuthorizationError::SubjectKindMismatch);
    }
    if row.subject_id != expected.subject_id {
        return Err(RecoveryAuthorizationError::SubjectIdMismatch);
    }
    if row.source_state_fingerprint != expected.source_state_fingerprint {
        return Err(RecoveryAuthorizationError::FingerprintMismatch);
    }
    if row.allowed_action != expected.allowed_action.as_str() {
        return Err(RecoveryAuthorizationError::ActionMismatch);
    }
    if row.action_payload_json != canonical_json(expected.action_payload)? {
        return Err(RecoveryAuthorizationError::PayloadMismatch);
    }
    Ok(())
}

fn exact_consumed_replay(
    row: &recovery_authorization::Model,
    expected: &AuthorizationConsumeExpectation<'_>,
) -> Result<bool, RecoveryAuthorizationError> {
    Ok(
        row.parent_conversation_id == expected.parent_conversation_id
            && row.subject_kind == expected.subject_kind.as_str()
            && row.subject_id == expected.subject_id
            && row.source_state_fingerprint == expected.source_state_fingerprint
            && row.allowed_action == expected.allowed_action.as_str()
            && row.action_payload_json == canonical_json(expected.action_payload)?
            && row.consumed_by_kind.as_deref() == Some(expected.consumer_kind.as_str())
            && row.consumed_by_id.as_deref() == Some(expected.consumer_id)
            && row.consumer_correlation_id.as_deref() == Some(expected.consumer_correlation_id),
    )
}

pub async fn validate_for_consumption_txn(
    txn: &DatabaseTransaction,
    authorization_id: &str,
    expected: &AuthorizationConsumeExpectation<'_>,
    now: DateTime<Utc>,
) -> Result<recovery_authorization::Model, RecoveryAuthorizationError> {
    let mut row = recovery_authorization::Entity::find_by_id(authorization_id)
        .one(txn)
        .await?
        .ok_or(RecoveryAuthorizationError::NotFound)?;

    if row.status == RecoveryAuthorizationStatus::Consumed {
        return if exact_consumed_replay(&row, expected)? {
            Ok(row)
        } else {
            Err(RecoveryAuthorizationError::ConsumedConflict)
        };
    }
    validate_challenge_fields(&row, expected)?;
    if row.status == RecoveryAuthorizationStatus::Approved
        && row.expires_at.is_some_and(|expires_at| now >= expires_at)
    {
        recovery_authorization::Entity::update_many()
            .col_expr(
                recovery_authorization::Column::Status,
                Expr::value(RecoveryAuthorizationStatus::Expired),
            )
            .filter(recovery_authorization::Column::AuthorizationId.eq(authorization_id))
            .filter(
                recovery_authorization::Column::Status.eq(RecoveryAuthorizationStatus::Approved),
            )
            .filter(recovery_authorization::Column::ExpiresAt.lte(now))
            .exec(txn)
            .await?;
        row.status = RecoveryAuthorizationStatus::Expired;
    }
    match row.status {
        RecoveryAuthorizationStatus::Approved => Ok(row),
        RecoveryAuthorizationStatus::Pending => Err(RecoveryAuthorizationError::Pending),
        RecoveryAuthorizationStatus::Declined => Err(RecoveryAuthorizationError::Declined),
        RecoveryAuthorizationStatus::Expired => Err(RecoveryAuthorizationError::Expired),
        RecoveryAuthorizationStatus::Abandoned => Err(RecoveryAuthorizationError::Abandoned),
        RecoveryAuthorizationStatus::Consumed => unreachable!("handled above"),
    }
}

pub async fn consume_txn(
    txn: &DatabaseTransaction,
    row: recovery_authorization::Model,
    expected: &AuthorizationConsumeExpectation<'_>,
    now: DateTime<Utc>,
) -> Result<(), RecoveryAuthorizationError> {
    if row.status == RecoveryAuthorizationStatus::Consumed {
        return if exact_consumed_replay(&row, expected)? {
            Ok(())
        } else {
            Err(RecoveryAuthorizationError::ConsumedConflict)
        };
    }
    validate_challenge_fields(&row, expected)?;
    let result = recovery_authorization::Entity::update_many()
        .col_expr(
            recovery_authorization::Column::Status,
            Expr::value(RecoveryAuthorizationStatus::Consumed),
        )
        .col_expr(
            recovery_authorization::Column::ConsumedAt,
            Expr::value(Some(now)),
        )
        .col_expr(
            recovery_authorization::Column::ConsumedByKind,
            Expr::value(Some(expected.consumer_kind.as_str().to_string())),
        )
        .col_expr(
            recovery_authorization::Column::ConsumedById,
            Expr::value(Some(expected.consumer_id.to_string())),
        )
        .col_expr(
            recovery_authorization::Column::ConsumerCorrelationId,
            Expr::value(Some(expected.consumer_correlation_id.to_string())),
        )
        .filter(recovery_authorization::Column::AuthorizationId.eq(&row.authorization_id))
        .filter(recovery_authorization::Column::Status.eq(RecoveryAuthorizationStatus::Approved))
        .filter(recovery_authorization::Column::ExpiresAt.gt(now))
        .exec(txn)
        .await?;
    if result.rows_affected == 1 {
        return Ok(());
    }
    let current = recovery_authorization::Entity::find_by_id(&row.authorization_id)
        .one(txn)
        .await?
        .ok_or(RecoveryAuthorizationError::NotFound)?;
    if current.status == RecoveryAuthorizationStatus::Consumed
        && exact_consumed_replay(&current, expected)?
    {
        Ok(())
    } else if current.status == RecoveryAuthorizationStatus::Expired {
        Err(RecoveryAuthorizationError::Expired)
    } else {
        Err(RecoveryAuthorizationError::ConsumedConflict)
    }
}
