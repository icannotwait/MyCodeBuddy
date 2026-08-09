use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use codeg_eui_core::{
    codeg_eui_begin_shutdown, codeg_eui_init, codeg_eui_poll, codeg_eui_shutdown,
    pin_eui_data_root, resolve_eui_data_root, CodegEuiFrame, DataRootError, EuiBootstrap,
    EuiRootInputs, CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_ERR_INVALID_UTF8,
    CODEG_EUI_ERR_TOO_LARGE, CODEG_EUI_OK,
};
use tempfile::TempDir;

static PROCESS_ENV: Mutex<()> = Mutex::new(());
const CHILD_CASE_ENV: &str = "CODEG_EUI_DATA_ROOT_TEST_CASE";

#[test]
fn ambient_main_data_dir_and_codeg_home_never_choose_the_eui_root() {
    let inputs = EuiRootInputs {
        codeg_eui_data_dir: None,
        xdg_data_home: Some(PathBuf::from("/tmp/xdg")),
        home: Some(PathBuf::from("/home/tester")),
        cwd: PathBuf::from("/work"),
    };

    assert_eq!(
        resolve_eui_data_root(&inputs).unwrap(),
        PathBuf::from("/tmp/xdg/codeg-eui")
    );
}

#[test]
fn explicit_eui_root_is_absolutized() {
    let inputs = EuiRootInputs {
        codeg_eui_data_dir: Some(PathBuf::from("relative-eui")),
        xdg_data_home: Some(PathBuf::from("/tmp/ignored")),
        home: Some(PathBuf::from("/home/tester")),
        cwd: PathBuf::from("/work"),
    };

    assert_eq!(
        resolve_eui_data_root(&inputs).unwrap(),
        PathBuf::from("/work/relative-eui")
    );
}

#[test]
fn empty_eui_root_falls_back_to_home_when_xdg_is_unavailable() {
    let inputs = EuiRootInputs {
        codeg_eui_data_dir: Some(PathBuf::new()),
        xdg_data_home: None,
        home: Some(PathBuf::from("/home/tester")),
        cwd: PathBuf::from("/work"),
    };

    assert_eq!(
        resolve_eui_data_root(&inputs).unwrap(),
        PathBuf::from("/home/tester/.local/share/codeg-eui")
    );
}

#[test]
fn missing_eui_xdg_and_home_roots_is_an_error() {
    let inputs = EuiRootInputs {
        codeg_eui_data_dir: None,
        xdg_data_home: None,
        home: None,
        cwd: PathBuf::from("/work"),
    };

    assert_eq!(
        resolve_eui_data_root(&inputs),
        Err(DataRootError::HomeUnavailable)
    );
}

#[test]
fn bootstrap_ignores_ambient_main_app_roots() {
    let fixture = IsolationFixture::new();

    run_child_case("bootstrap_from_environment", &fixture);

    assert!(fixture.eui_root.join("codeg.db").is_file());
    assert!(fixture.eui_root.join("logs").is_dir());
    assert!(!fixture.main_data_root.join("codeg.db").exists());
    assert!(!fixture.main_home_root.join("logs").exists());
}

#[test]
fn abi_argument_root_overrides_eui_environment_and_remains_pinned() {
    let fixture = IsolationFixture::new();

    run_child_case("bootstrap_from_abi_argument", &fixture);

    assert!(fixture.argument_root.join("codeg.db").is_file());
    assert!(fixture.argument_root.join("logs").is_dir());
    assert!(!fixture.eui_root.join("codeg.db").exists());
    assert!(!fixture.main_data_root.join("codeg.db").exists());
    assert!(!fixture.main_home_root.join("logs").exists());
    assert!(!fixture.different_root.join("codeg.db").exists());
}

#[test]
fn isolated_process_case() {
    let Ok(case) = std::env::var(CHILD_CASE_ENV) else {
        return;
    };
    let _env_guard = PROCESS_ENV
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    match case.as_str() {
        "bootstrap_from_environment" => {
            let eui_root = path_from_env("CODEG_EUI_DATA_DIR");
            let bootstrap = EuiBootstrap::start().expect("bootstrap from EUI environment root");

            assert_eq!(bootstrap.state.data_dir, eui_root);
            assert_eq!(
                std::env::var_os("CODEG_DATA_DIR"),
                Some(eui_root.into_os_string())
            );
            assert!(std::env::var_os("CODEG_HOME").is_none());
            bootstrap.shutdown();
        }
        "bootstrap_from_abi_argument" => {
            let argument_root = path_from_env("CODEG_EUI_ARGUMENT_ROOT");
            let different_root = path_from_env("CODEG_EUI_DIFFERENT_ROOT");
            let argument = argument_root.to_str().expect("UTF-8 temp path").as_bytes();

            assert_eq!(
                pin_eui_data_root(PathBuf::from(String::from("invalid\0root"))),
                Err(DataRootError::EmbeddedNul),
                "an invalid environment value must not poison the process pin"
            );

            let invalid_utf8 = [0xff];
            assert_eq!(
                codeg_eui_init(invalid_utf8.as_ptr(), invalid_utf8.len()),
                CODEG_EUI_ERR_INVALID_UTF8
            );
            let oversized = vec![b'x'; 32_769];
            assert_eq!(
                codeg_eui_init(oversized.as_ptr(), oversized.len()),
                CODEG_EUI_ERR_TOO_LARGE
            );
            let embedded_nul = b"invalid\0root";
            assert_eq!(
                codeg_eui_init(embedded_nul.as_ptr(), embedded_nul.len()),
                CODEG_EUI_ERR_INVALID_STATE
            );

            assert_eq!(
                codeg_eui_init(argument.as_ptr(), argument.len()),
                CODEG_EUI_OK
            );
            assert_eq!(
                std::env::var_os("CODEG_DATA_DIR"),
                Some(argument_root.clone().into_os_string())
            );
            assert!(std::env::var_os("CODEG_HOME").is_none());
            complete_abi_shutdown();

            assert_eq!(
                codeg_eui_init(argument.as_ptr(), argument.len()),
                CODEG_EUI_OK,
                "re-init with the same normalized root must remain legal"
            );
            complete_abi_shutdown();

            let different = different_root.to_str().expect("UTF-8 temp path").as_bytes();
            assert_eq!(
                codeg_eui_init(different.as_ptr(), different.len()),
                CODEG_EUI_ERR_INVALID_STATE,
                "a different root must return a stable init error"
            );
        }
        other => panic!("unknown child case: {other}"),
    }
}

struct IsolationFixture {
    _temp: TempDir,
    eui_root: PathBuf,
    argument_root: PathBuf,
    different_root: PathBuf,
    main_data_root: PathBuf,
    main_home_root: PathBuf,
}

impl IsolationFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        Self {
            eui_root: temp.path().join("eui"),
            argument_root: temp.path().join("argument-eui"),
            different_root: temp.path().join("different-eui"),
            main_data_root: temp.path().join("main-data"),
            main_home_root: temp.path().join("main-home"),
            _temp: temp,
        }
    }
}

fn run_child_case(case: &str, fixture: &IsolationFixture) {
    let status = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", "isolated_process_case", "--nocapture"])
        .env(CHILD_CASE_ENV, case)
        .env("CODEG_DATA_DIR", &fixture.main_data_root)
        .env("CODEG_HOME", &fixture.main_home_root)
        .env("CODEG_EUI_DATA_DIR", &fixture.eui_root)
        .env("CODEG_EUI_ARGUMENT_ROOT", &fixture.argument_root)
        .env("CODEG_EUI_DIFFERENT_ROOT", &fixture.different_root)
        .status()
        .expect("run isolated child test process");

    assert!(status.success(), "child case {case} failed with {status}");
}

fn path_from_env(name: &str) -> PathBuf {
    Path::new(&std::env::var_os(name).unwrap_or_else(|| panic!("{name} is set"))).to_path_buf()
}

fn complete_abi_shutdown() {
    assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
    for _ in 0..200 {
        let mut frame = CodegEuiFrame::default();
        assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
        if frame.shutdown_ready == 1 {
            assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("shutdown did not become ready");
}
