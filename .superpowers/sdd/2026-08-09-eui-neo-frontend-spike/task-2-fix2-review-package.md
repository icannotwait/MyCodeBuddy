# Task 2 Fix2 Review Package
FIX_BASE: 1e92ed75da0702bc628b5f42e0af7fe5d48c7814 HEAD: be8b41cf8545470694e2d0b490ec5b6f6cb1a227
be8b41cf fix(eui): make root pin initialization atomic
 src-tauri/codeg-eui-core/src/data_root.rs | 120 +++++++++++++++++++++---------
 1 file changed, 85 insertions(+), 35 deletions(-)
diff --git a/src-tauri/codeg-eui-core/src/data_root.rs b/src-tauri/codeg-eui-core/src/data_root.rs
index 8f9d8ac0..f1b08e67 100644
--- a/src-tauri/codeg-eui-core/src/data_root.rs
+++ b/src-tauri/codeg-eui-core/src/data_root.rs
@@ -7,10 +7,18 @@ use thiserror::Error;
 static STARTUP_WORKING_DIRECTORY: OnceLock<Result<PathBuf, String>> = OnceLock::new();
 static PINNED_EUI_DATA_ROOT: OnceLock<PathBuf> = OnceLock::new();
 #[cfg(test)]
 static ENVIRONMENT_WRITE_PHASES: std::sync::atomic::AtomicUsize =
     std::sync::atomic::AtomicUsize::new(0);
+#[cfg(test)]
+static ENVIRONMENT_WRITE_PAUSE: OnceLock<EnvironmentWritePause> = OnceLock::new();
+
+#[cfg(test)]
+struct EnvironmentWritePause {
+    entered: std::sync::Barrier,
+    release: std::sync::Barrier,
+}
 
 #[derive(Debug, Clone, PartialEq, Eq)]
 pub struct EuiRootInputs {
     pub codeg_eui_data_dir: Option<PathBuf>,
     pub xdg_data_home: Option<PathBuf>,
@@ -69,21 +77,27 @@ pub fn resolve_eui_data_root(input: &EuiRootInputs) -> Result<PathBuf, DataRootE
 pub fn pin_eui_data_root(root: PathBuf) -> Result<(), DataRootError> {
     let absolute = absolutize_without_requiring_existence(&root)?;
     if absolute.as_os_str().as_encoded_bytes().contains(&0) {
         return Err(DataRootError::EmbeddedNul);
     }
-    if !verify_or_set_process_pin(&absolute)? {
-        return Ok(());
-    }
-
-    // This function is a startup-only trust-boundary operation. Callers must
-    // invoke it before starting worker threads or environment-reading helpers.
-    env::remove_var("CODEG_HOME");
-    env::set_var("CODEG_DATA_DIR", &absolute);
-    #[cfg(test)]
-    ENVIRONMENT_WRITE_PHASES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
-    Ok(())
+    let pinned = PINNED_EUI_DATA_ROOT.get_or_init(|| {
+        // OnceLock publishes the root only after this startup-only
+        // trust-boundary phase completes, so equal callers cannot proceed
+        // while ambient environment values are still effective.
+        #[cfg(test)]
+        if let Some(pause) = ENVIRONMENT_WRITE_PAUSE.get() {
+            pause.entered.wait();
+            pause.release.wait();
+        }
+        env::remove_var("CODEG_HOME");
+        env::set_var("CODEG_DATA_DIR", &absolute);
+        #[cfg(test)]
+        ENVIRONMENT_WRITE_PHASES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
+        absolute.clone()
+    });
+
+    roots_match(pinned, &absolute)
 }
 
 pub(crate) fn absolutize_from(path: &Path, cwd: &Path) -> PathBuf {
     let absolute = if path.is_absolute() {
         path.to_path_buf()
@@ -102,30 +116,10 @@ pub(crate) fn startup_working_directory() -> Result<PathBuf, DataRootError> {
 
 fn absolutize_without_requiring_existence(root: &Path) -> Result<PathBuf, DataRootError> {
     Ok(absolutize_from(root, &startup_working_directory()?))
 }
 
-fn verify_or_set_process_pin(requested: &PathBuf) -> Result<bool, DataRootError> {
-    if let Some(pinned) = PINNED_EUI_DATA_ROOT.get() {
-        roots_match(pinned, requested)?;
-        return Ok(false);
-    }
-
-    match PINNED_EUI_DATA_ROOT.set(requested.clone()) {
-        Ok(()) => Ok(true),
-        Err(_) => {
-            roots_match(
-                PINNED_EUI_DATA_ROOT
-                    .get()
-                    .expect("EUI data root is set after a failed OnceLock set"),
-                requested,
-            )?;
-            Ok(false)
-        }
-    }
-}
-
 fn roots_match(pinned: &PathBuf, requested: &PathBuf) -> Result<(), DataRootError> {
     if pinned == requested {
         Ok(())
     } else {
         Err(DataRootError::AlreadyPinned {
@@ -151,23 +145,79 @@ fn lexically_normalize(path: &Path) -> PathBuf {
     normalized
 }
 
 #[cfg(test)]
 mod tests {
-    use super::ENVIRONMENT_WRITE_PHASES;
+    use super::{
+        pin_eui_data_root, EnvironmentWritePause, ENVIRONMENT_WRITE_PAUSE,
+        ENVIRONMENT_WRITE_PHASES, PINNED_EUI_DATA_ROOT,
+    };
     use crate::{
         codeg_eui_begin_shutdown, codeg_eui_init, codeg_eui_poll, codeg_eui_shutdown,
         CodegEuiFrame, CODEG_EUI_OK,
     };
     use std::sync::atomic::Ordering;
+    use std::sync::{mpsc, Arc, Barrier};
+    use std::time::Duration;
 
     #[test]
-    fn same_root_abi_restart_does_not_repeat_environment_write_phase() {
+    fn pin_lifecycle_publishes_only_after_environment_write_phase() {
         let temp = tempfile::tempdir().expect("tempdir");
-        let root = temp.path().to_str().expect("UTF-8 temp path").as_bytes();
+        let root_path = temp.path().to_path_buf();
+        let pause = EnvironmentWritePause {
+            entered: Barrier::new(2),
+            release: Barrier::new(2),
+        };
+        assert!(ENVIRONMENT_WRITE_PAUSE.set(pause).is_ok());
+
+        let first_root = root_path.clone();
+        let first = std::thread::spawn(move || pin_eui_data_root(first_root));
+        let pause = ENVIRONMENT_WRITE_PAUSE.get().expect("pause installed");
+        pause.entered.wait();
+        let published_early = PINNED_EUI_DATA_ROOT.get().is_some();
+
+        let second_started = Arc::new(Barrier::new(2));
+        let second_started_in_thread = Arc::clone(&second_started);
+        let (second_done_tx, second_done_rx) = mpsc::channel();
+        let second_root = root_path.clone();
+        let second = std::thread::spawn(move || {
+            second_started_in_thread.wait();
+            second_done_tx
+                .send(pin_eui_data_root(second_root))
+                .expect("send second pin result");
+        });
+        second_started.wait();
+
+        let early_result = second_done_rx.recv_timeout(Duration::from_millis(250));
+        pause.release.wait();
+        assert_eq!(first.join().expect("first pin thread"), Ok(()));
+        let (returned_early, second_result) = match early_result {
+            Ok(result) => (true, result),
+            Err(mpsc::RecvTimeoutError::Timeout) => (
+                false,
+                second_done_rx
+                    .recv_timeout(Duration::from_secs(2))
+                    .expect("equal pin returns after first pin completes"),
+            ),
+            Err(mpsc::RecvTimeoutError::Disconnected) => {
+                panic!("equal pin result channel disconnected")
+            }
+        };
+        second.join().expect("second pin thread");
+
+        assert!(
+            !published_early,
+            "root published before env write completed"
+        );
+        assert!(
+            !returned_early,
+            "equal pin returned before env write completed"
+        );
+        assert_eq!(second_result, Ok(()));
+        assert_eq!(ENVIRONMENT_WRITE_PHASES.load(Ordering::SeqCst), 1);
 
-        assert_eq!(ENVIRONMENT_WRITE_PHASES.load(Ordering::SeqCst), 0);
+        let root = root_path.to_str().expect("UTF-8 temp path").as_bytes();
         assert_eq!(codeg_eui_init(root.as_ptr(), root.len()), CODEG_EUI_OK);
         assert_eq!(ENVIRONMENT_WRITE_PHASES.load(Ordering::SeqCst), 1);
         complete_shutdown();
 
         assert_eq!(codeg_eui_init(root.as_ptr(), root.len()), CODEG_EUI_OK);
