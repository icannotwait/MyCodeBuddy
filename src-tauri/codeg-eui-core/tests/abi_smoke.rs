use codeg_eui_core::{
    codeg_eui_api_version, codeg_eui_begin_shutdown, codeg_eui_init, codeg_eui_poll,
    codeg_eui_shutdown, CodegEuiFrame, CODEG_EUI_API_VERSION, CODEG_EUI_ERR_INVALID_STATE,
    CODEG_EUI_ERR_NULL_POINTER, CODEG_EUI_OK,
};
use std::time::Duration;

#[test]
fn abi_version_and_null_poll_are_stable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let data_dir = temp.path().to_str().expect("UTF-8 temp path").as_bytes();

    assert_eq!(codeg_eui_api_version(), CODEG_EUI_API_VERSION);
    assert_eq!(CODEG_EUI_API_VERSION, 1);
    assert_eq!(
        codeg_eui_poll(std::ptr::null_mut::<CodegEuiFrame>()),
        CODEG_EUI_ERR_NULL_POINTER
    );

    assert_eq!(
        codeg_eui_init(data_dir.as_ptr(), data_dir.len()),
        CODEG_EUI_OK
    );
    assert_eq!(codeg_eui_shutdown(), CODEG_EUI_ERR_INVALID_STATE);
    assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);

    let frame = drain_until_ready();
    assert_eq!(frame.api_version, CODEG_EUI_API_VERSION);
    assert_eq!(frame.shutdown_ready, 1);
    assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
}

fn drain_until_ready() -> CodegEuiFrame {
    for _ in 0..200 {
        let mut frame = CodegEuiFrame::default();
        assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
        if frame.shutdown_ready == 1 {
            return frame;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("shutdown did not become ready");
}
