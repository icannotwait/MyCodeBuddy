use codeg_eui_core::EuiBootstrap;
use codeg_lib::web::event_bridge::EventEmitter;

#[test]
fn eui_profile_is_web_only_and_keeps_auxiliary_services_dormant() {
    let test_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");

    let (bootstrap, _temp) = test_runtime.block_on(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let bootstrap = EuiBootstrap::start_for_test(temp.path())
            .await
            .expect("EUI bootstrap");

        assert_eq!(bootstrap.state.data_dir, temp.path());
        assert!(matches!(
            &bootstrap.state.emitter,
            EventEmitter::WebOnly { .. }
        ));
        assert_eq!(
            bootstrap
                .state
                .connection_manager
                .list_connections()
                .await
                .len(),
            0
        );
        assert!(!bootstrap.state.delegation_socket_path.exists());
        assert!(!bootstrap.started_services.web_server);
        assert!(!bootstrap.started_services.auto_title);
        assert!(!bootstrap.started_services.automation);
        assert!(!bootstrap.started_services.chat_channels);
        assert!(!bootstrap.started_services.pet_mapper);
        assert!(!bootstrap.started_services.document_translation);
        assert!(!bootstrap.started_services.reference_search);
        assert!(!bootstrap.started_services.delegation_listener);
        assert!(!bootstrap.started_services.delegation_supervisor);
        assert!(!bootstrap.started_services.completion_outbox_dispatcher);
        assert!(!bootstrap.started_services.updater);

        (bootstrap, temp)
    });

    bootstrap.shutdown();
}
