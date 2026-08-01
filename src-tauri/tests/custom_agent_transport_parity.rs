use std::sync::Arc;

use codeg_lib::acp::custom_registry::{
    BinaryPlatformSpec, CustomAgentSource, CustomAgentSpec, CustomDistributionKind, NpxSpec,
};
use codeg_lib::acp::registry;
use codeg_lib::acp::remote_registry::RegistryCatalogAgent;
use codeg_lib::commands::custom_agents::{
    acp_add_registry_agent_core, acp_add_registry_agent_from_catalog_core,
    acp_current_platform_core, acp_delete_custom_agent_core, acp_fetch_registry_catalog_with_core,
    acp_list_custom_agents_core, acp_save_custom_agent_params_core, SaveCustomAgentParams,
};
use codeg_lib::db::test_helpers::fresh_in_memory_db;
use codeg_lib::web::event_bridge::{EventEmitter, WebEventBroadcaster};

const OPERATIONS: [&str; 6] = [
    "acp_list_custom_agents",
    "acp_save_custom_agent",
    "acp_delete_custom_agent",
    "acp_fetch_registry_catalog",
    "acp_add_registry_agent",
    "acp_current_platform",
];

const LIB_RS: &str = include_str!("../src/lib.rs");
const WEB_ROUTER_RS: &str = include_str!("../src/web/router.rs");
const API_TS: &str = include_str!("../../src/lib/api.ts");
const CUSTOM_AGENT_COMMANDS_RS: &str = include_str!("../src/commands/custom_agents.rs");

#[test]
fn all_six_operations_are_wired_through_tauri_axum_and_typescript() {
    for operation in OPERATIONS {
        assert!(
            LIB_RS.contains(operation),
            "Tauri registration missing {operation}"
        );
        assert!(
            WEB_ROUTER_RS.contains(&format!("/{operation}")),
            "Axum route missing {operation}"
        );
        assert!(
            API_TS.contains(&format!("\"{operation}\"")),
            "TypeScript transport missing {operation}"
        );
    }

    for core in [
        "acp_list_custom_agents_core",
        "acp_save_custom_agent_params_core",
        "acp_delete_custom_agent_core",
        "acp_fetch_registry_catalog_core",
        "acp_add_registry_agent_core",
        "acp_current_platform_core",
    ] {
        assert!(
            CUSTOM_AGENT_COMMANDS_RS.contains(&format!("pub async fn {core}"))
                || CUSTOM_AGENT_COMMANDS_RS.contains(&format!("pub fn {core}")),
            "shared core implementation missing {core}"
        );
    }
}

#[tokio::test]
async fn shared_core_apis_cover_custom_agent_crud_and_guards() {
    let db = fresh_in_memory_db().await;
    let emitter = EventEmitter::test_web_only(Arc::new(WebEventBroadcaster::new()));

    assert!(acp_list_custom_agents_core(&db).await.unwrap().is_empty());
    assert!(!acp_current_platform_core().is_empty());

    acp_save_custom_agent_params_core(
        SaveCustomAgentParams {
            registry_id: "goose".into(),
            name: "Goose".into(),
            description: "Test custom agent".into(),
            version: "1.0.0".into(),
            distribution_kind: "npx".into(),
            spec: CustomAgentSpec {
                npx: Some(NpxSpec {
                    package: "goose-acp@1.0.0".into(),
                    cmd: Some("goose".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            icon_url: None,
            skills_shared_store: false,
            skills_dir: None,
            source: None,
            version_probe: None,
        },
        &db,
        &emitter,
    )
    .await
    .unwrap();

    let saved = acp_list_custom_agents_core(&db).await.unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].agent_type.as_wire(), "custom:goose");

    acp_delete_custom_agent_core("goose".into(), false, &db, &emitter)
        .await
        .unwrap();
    assert!(acp_list_custom_agents_core(&db).await.unwrap().is_empty());

    // This guard exercises the add core without depending on the public ACP
    // registry network: built-ins must be rejected before any fetch occurs.
    let add_error = acp_add_registry_agent_core("cline".into(), None, &db, &emitter)
        .await
        .unwrap_err();
    assert!(add_error.to_string().contains("already built into codeg"));

    let fixture = RegistryCatalogAgent {
        registry_id: "fixture-agent".into(),
        name: "Fixture Agent".into(),
        description: "Deterministic registry fixture".into(),
        version: Some("1.2.3".into()),
        icon_url: None,
        website: None,
        repository: None,
        license: Some("Apache-2.0".into()),
        distribution_kinds: vec!["binary".into()],
        builtin: false,
        installed: false,
        supported_on_platform: true,
        spec: CustomAgentSpec {
            binary: std::iter::once((
                registry::current_platform().to_string(),
                BinaryPlatformSpec {
                    archive: "https://example.com/fixture.tar.gz".into(),
                    cmd: "./fixture-agent".into(),
                    ..Default::default()
                },
            ))
            .collect(),
            ..Default::default()
        },
    };

    let fetched =
        acp_fetch_registry_catalog_with_core(
            &db,
            move |_installed| async move { Ok(vec![fixture]) },
        )
        .await
        .unwrap();
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].registry_id, "fixture-agent");

    acp_add_registry_agent_from_catalog_core(
        "fixture-agent".into(),
        Some(CustomDistributionKind::Binary),
        &fetched,
        &db,
        &emitter,
    )
    .await
    .unwrap();

    let saved = acp_list_custom_agents_core(&db).await.unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].agent_type.as_wire(), "custom:fixture-agent");
    assert_eq!(saved[0].source, CustomAgentSource::Registry.as_str());
}
