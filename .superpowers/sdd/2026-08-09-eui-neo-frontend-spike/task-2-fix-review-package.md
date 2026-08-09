# Task 2 Fix Review Package
FIX_BASE: 8bac8d78bcdf7f189304fa714d068e2d73ddb541 HEAD: 1e92ed75da0702bc628b5f42e0af7fe5d48c7814
1e92ed75 fix(eui): avoid env writes on root re-pin
 src-tauri/codeg-eui-core/src/data_root.rs | 68 ++++++++++++++++++++++++++-----
 1 file changed, 58 insertions(+), 10 deletions(-)
diff --git a/src-tauri/codeg-eui-core/src/data_root.rs b/src-tauri/codeg-eui-core/src/data_root.rs
index c2de94a6..8f9d8ac0 100644
--- a/src-tauri/codeg-eui-core/src/data_root.rs
+++ b/src-tauri/codeg-eui-core/src/data_root.rs
@@ -4,10 +4,13 @@ use std::sync::OnceLock;
 
 use thiserror::Error;
 
 static STARTUP_WORKING_DIRECTORY: OnceLock<Result<PathBuf, String>> = OnceLock::new();
 static PINNED_EUI_DATA_ROOT: OnceLock<PathBuf> = OnceLock::new();
+#[cfg(test)]
+static ENVIRONMENT_WRITE_PHASES: std::sync::atomic::AtomicUsize =
+    std::sync::atomic::AtomicUsize::new(0);
 
 #[derive(Debug, Clone, PartialEq, Eq)]
 pub struct EuiRootInputs {
     pub codeg_eui_data_dir: Option<PathBuf>,
     pub xdg_data_home: Option<PathBuf>,
@@ -66,16 +69,20 @@ pub fn resolve_eui_data_root(input: &EuiRootInputs) -> Result<PathBuf, DataRootE
 pub fn pin_eui_data_root(root: PathBuf) -> Result<(), DataRootError> {
     let absolute = absolutize_without_requiring_existence(&root)?;
     if absolute.as_os_str().as_encoded_bytes().contains(&0) {
         return Err(DataRootError::EmbeddedNul);
     }
-    verify_or_set_process_pin(&absolute)?;
+    if !verify_or_set_process_pin(&absolute)? {
+        return Ok(());
+    }
 
     // This function is a startup-only trust-boundary operation. Callers must
     // invoke it before starting worker threads or environment-reading helpers.
     env::remove_var("CODEG_HOME");
     env::set_var("CODEG_DATA_DIR", &absolute);
+    #[cfg(test)]
+    ENVIRONMENT_WRITE_PHASES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
     Ok(())
 }
 
 pub(crate) fn absolutize_from(path: &Path, cwd: &Path) -> PathBuf {
     let absolute = if path.is_absolute() {
@@ -95,23 +102,27 @@ pub(crate) fn startup_working_directory() -> Result<PathBuf, DataRootError> {
 
 fn absolutize_without_requiring_existence(root: &Path) -> Result<PathBuf, DataRootError> {
     Ok(absolutize_from(root, &startup_working_directory()?))
 }
 
-fn verify_or_set_process_pin(requested: &PathBuf) -> Result<(), DataRootError> {
+fn verify_or_set_process_pin(requested: &PathBuf) -> Result<bool, DataRootError> {
     if let Some(pinned) = PINNED_EUI_DATA_ROOT.get() {
-        return roots_match(pinned, requested);
+        roots_match(pinned, requested)?;
+        return Ok(false);
     }
 
     match PINNED_EUI_DATA_ROOT.set(requested.clone()) {
-        Ok(()) => Ok(()),
-        Err(_) => roots_match(
-            PINNED_EUI_DATA_ROOT
-                .get()
-                .expect("EUI data root is set after a failed OnceLock set"),
-            requested,
-        ),
+        Ok(()) => Ok(true),
+        Err(_) => {
+            roots_match(
+                PINNED_EUI_DATA_ROOT
+                    .get()
+                    .expect("EUI data root is set after a failed OnceLock set"),
+                requested,
+            )?;
+            Ok(false)
+        }
     }
 }
 
 fn roots_match(pinned: &PathBuf, requested: &PathBuf) -> Result<(), DataRootError> {
     if pinned == requested {
@@ -137,5 +148,42 @@ fn lexically_normalize(path: &Path) -> PathBuf {
             Component::Normal(part) => normalized.push(part),
         }
     }
     normalized
 }
+
+#[cfg(test)]
+mod tests {
+    use super::ENVIRONMENT_WRITE_PHASES;
+    use crate::{
+        codeg_eui_begin_shutdown, codeg_eui_init, codeg_eui_poll, codeg_eui_shutdown,
+        CodegEuiFrame, CODEG_EUI_OK,
+    };
+    use std::sync::atomic::Ordering;
+
+    #[test]
+    fn same_root_abi_restart_does_not_repeat_environment_write_phase() {
+        let temp = tempfile::tempdir().expect("tempdir");
+        let root = temp.path().to_str().expect("UTF-8 temp path").as_bytes();
+
+        assert_eq!(ENVIRONMENT_WRITE_PHASES.load(Ordering::SeqCst), 0);
+        assert_eq!(codeg_eui_init(root.as_ptr(), root.len()), CODEG_EUI_OK);
+        assert_eq!(ENVIRONMENT_WRITE_PHASES.load(Ordering::SeqCst), 1);
+        complete_shutdown();
+
+        assert_eq!(codeg_eui_init(root.as_ptr(), root.len()), CODEG_EUI_OK);
+        assert_eq!(
+            ENVIRONMENT_WRITE_PHASES.load(Ordering::SeqCst),
+            1,
+            "same-root restart must verify the pin without rewriting process env"
+        );
+        complete_shutdown();
+    }
+
+    fn complete_shutdown() {
+        assert_eq!(codeg_eui_begin_shutdown(), CODEG_EUI_OK);
+        let mut frame = CodegEuiFrame::default();
+        assert_eq!(codeg_eui_poll(&mut frame), CODEG_EUI_OK);
+        assert_eq!(frame.shutdown_ready, 1);
+        assert_eq!(codeg_eui_shutdown(), CODEG_EUI_OK);
+    }
+}
