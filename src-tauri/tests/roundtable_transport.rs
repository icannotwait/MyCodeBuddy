//! Private subscriptions, frozen pages, and one command executor.

#[path = "roundtable_support/mod.rs"]
pub mod support;

#[path = "roundtable_cases/product_commands.rs"]
mod product_commands;

#[path = "roundtable_cases/product_review.rs"]
mod product_review;

use roundtable_protocol::{
    ActorContext, ClientIdentity, ClientKind, DurableEnvelopeV1, ErrorCode, Hash256, ObjectKind,
    ObjectRefV1, OperatorScope, PhaseKind, PhaseRefV1, PhaseState, PrincipalId, ProjectionBodyV1,
    ProjectionRef, ProjectionV1, RoomId, RoomState, SafeInt, SpeakerId,
};
use roundtable_protocol::{DurationMs, Epoch, Revision, Seq};

use codeg_lib::roundtable::{
    apply_projection, authorize_room, execute, http_status, moderator_preview_allowed,
    projection_hash, read_manifest_page, RoomDirectory, RoomGrant, RoundtableRequestV1,
    SubscriptionHub,
};
use codeg_lib::web::event_bridge::roundtable_does_not_use_global_session_emit;
use codeg_lib::web::ws::roundtable_frames_stay_on_private_sink;
use codeg_lib::web::ws_attach::roundtable_attach_is_room_scoped;

fn principal(n: u8) -> PrincipalId {
    format!("00000000-0000-4000-8000-00000000000{n}")
        .parse()
        .expect("principal")
}

fn actor(n: u8) -> ActorContext {
    ActorContext::from_trusted_entry(
        principal(n),
        OperatorScope::SingleOperator,
        ClientIdentity {
            kind: ClientKind::Web,
            session_ref: "session".to_string(),
        },
    )
}

fn room() -> RoomId {
    "00000000-0000-4000-8000-000000000010"
        .parse()
        .expect("room")
}

fn projection(hash: Hash256) -> ProjectionV1 {
    ProjectionV1 {
        projection_ref: ProjectionRef {
            id: "00000000-0000-4000-8000-000000000020"
                .parse()
                .expect("projection"),
            hash,
        },
        body: ProjectionBodyV1 {
            schema_version: 1,
            room_id: room(),
            revision: Revision(1),
            run_epoch: Epoch(1),
            last_seq: Seq(4),
            status: RoomState::Running,
            blocked_reason: None,
            config_hash: Hash256::sha256(b"config"),
            moderator_speaker_id: None,
            phase_refs: vec![PhaseRefV1 {
                phase_id: "00000000-0000-4000-8000-000000000030"
                    .parse()
                    .expect("phase"),
                index: 0,
                revision: Revision(1),
                kind: PhaseKind::Proposal,
                state: PhaseState::Published,
            }],
            messages: vec![],
            evidence_manifests: vec![],
            replay: Default::default(),
            ledger_seq: Seq(1),
            sampled_active_ms: DurationMs(0),
            sampled_at_utc: "2026-10-05T00:00:00Z".to_string(),
        },
    }
}

fn envelope(hash: Hash256, schema: u32) -> DurableEnvelopeV1 {
    DurableEnvelopeV1 {
        schema_version: schema,
        object_ref: ObjectRefV1 {
            object_id: "object".to_string(),
            kind: ObjectKind::Projection,
            content_hash: hash,
            total_bytes: SafeInt(1),
        },
    }
}

#[test]
fn subscription_authorization_and_fold() {
    assert!(roundtable_frames_stay_on_private_sink());
    assert!(roundtable_attach_is_room_scoped());
    assert!(roundtable_does_not_use_global_session_emit());
    let directory = RoomDirectory {
        rooms: vec![RoomGrant {
            room_id: room(),
            principal: principal(1),
            hidden: false,
        }],
    };
    let mut hub = SubscriptionHub::new();
    assert!(authorize_room(&actor(2), &room(), &directory).is_err());
    hub.attach(&actor(2), &room(), &directory, "other")
        .expect_err("hidden from other");
    assert!(hub.frames_for("other").is_empty());
    hub.attach(&actor(1), &room(), &directory, "owner")
        .expect("owner");
    hub.publish("owner", "frame".to_string());
    hub.publish("other", "secret".to_string());
    assert_eq!(hub.frames_for("owner"), vec!["frame".to_string()]);
    assert!(hub.frames_for("other").is_empty());
    hub.note_preview("speaker", 1, 4);
    assert_eq!(hub.preview_bounds("speaker"), Some((1, 4)));
    assert!(hub.admit_incarnation(2));
    assert!(!hub.admit_incarnation(1));
    let speaker: SpeakerId = "00000000-0000-4000-8000-000000000040"
        .parse()
        .expect("speaker");
    assert!(moderator_preview_allowed(Some(&speaker), None));

    let hash = Hash256::sha256(b"projection");
    let fetched = projection(hash);
    let mut seq = 3;
    let folded = apply_projection(&mut seq, &envelope(hash, 1), &fetched).expect("fold");
    assert_eq!(projection_hash(&folded), projection_hash(&fetched));
    assert!(apply_projection(&mut seq, &envelope(hash, 2), &fetched).is_err());
    assert_eq!(seq, 4);

    let entries: Vec<_> = (0..250).map(|index| format!("m{index}")).collect();
    let body = Hash256::sha256(b"manifest");
    let first = read_manifest_page(&entries, body, None, 100).expect("page");
    let second = read_manifest_page(&entries, body, Some(&first.cursor), 100).expect("next");
    assert_eq!(first.entries.len(), 100);
    assert_eq!(second.body_hash, first.body_hash);
    assert_eq!(first.entries[0], "m0");
}

#[test]
fn command_parity_and_closed_errors() {
    let request = RoundtableRequestV1 {
        command: "roundtable_pause".to_string(),
        method: "POST".to_string(),
        room_id: Some(room().to_string()),
        body_has_principal: false,
        completion_token: false,
        known_room: true,
        hidden_room: false,
        page_limit: None,
    };
    let tauri = execute(request.clone()).expect("tauri");
    let http = execute(request).expect("http");
    assert_eq!(
        serde_json::to_string(&tauri.body).unwrap(),
        serde_json::to_string(&http.body).unwrap()
    );
    assert_eq!(tauri.room_state, Some(RoomState::Pausing));
    assert_eq!(tauri.page_limit, 100);
    let limited = roundtable_protocol::RtError {
        code: ErrorCode::CapacityLimited,
        message: "full".to_string(),
        retryable: true,
        current_revision: None,
        details: roundtable_protocol::ErrorDetails {
            reason: None,
            field_errors: Vec::new(),
        },
    };
    assert_eq!(http_status(&limited), 429);
    let unknown = execute(RoundtableRequestV1 {
        command: "roundtable_get".to_string(),
        method: "POST".to_string(),
        room_id: Some("missing".to_string()),
        body_has_principal: false,
        completion_token: false,
        known_room: false,
        hidden_room: false,
        page_limit: Some(100),
    })
    .expect("unknown");
    let hidden = execute(RoundtableRequestV1 {
        command: "roundtable_get".to_string(),
        method: "POST".to_string(),
        room_id: Some("hidden".to_string()),
        body_has_principal: false,
        completion_token: false,
        known_room: true,
        hidden_room: true,
        page_limit: Some(100),
    })
    .expect("hidden");
    assert_eq!(unknown.status(), hidden.status());
    assert_eq!(unknown.status(), 404);
    assert!(execute(RoundtableRequestV1 {
        command: "roundtable_get".to_string(),
        method: "GET".to_string(),
        room_id: None,
        body_has_principal: false,
        completion_token: false,
        known_room: true,
        hidden_room: false,
        page_limit: None,
    })
    .is_err());
    assert!(execute(RoundtableRequestV1 {
        command: "roundtable_create".to_string(),
        method: "POST".to_string(),
        room_id: None,
        body_has_principal: true,
        completion_token: false,
        known_room: true,
        hidden_room: false,
        page_limit: None,
    })
    .is_err());
    assert!(execute(RoundtableRequestV1 {
        command: "roundtable_start".to_string(),
        method: "POST".to_string(),
        room_id: None,
        body_has_principal: false,
        completion_token: true,
        known_room: true,
        hidden_room: false,
        page_limit: Some(501),
    })
    .is_err());
}
