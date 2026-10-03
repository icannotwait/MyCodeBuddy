use quote::ToTokens;
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
};
use syn::{
    spanned::Spanned,
    visit::{self, Visit},
    ItemFn, ItemMod,
};
struct Scan {
    file: PathBuf,
    dir: PathBuf,
    modules: Vec<String>,
    cfg: Vec<String>,
    tests: Vec<Value>,
    gates: Vec<Value>,
    gated: bool,
    warnings: Vec<String>,
    visited: BTreeSet<PathBuf>,
}
fn attrs(attrs: &[syn::Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("cfg"))
        .map(|a| a.meta.to_token_stream().to_string())
        .collect()
}
fn is_test(f: &ItemFn) -> bool {
    f.attrs.iter().any(|a| {
        let p = a.path().to_token_stream().to_string();
        p == "test" || p == "tokio :: test"
    })
}
fn contains_test(items: &[syn::Item]) -> bool {
    items.iter().any(|i| match i {
        syn::Item::Fn(f) => is_test(f),
        syn::Item::Mod(m) => m.content.as_ref().is_some_and(|(_, i)| contains_test(i)),
        _ => false,
    })
}
fn test_only(m: &ItemMod) -> bool {
    m.attrs.iter().any(|a| {
        let s = a.meta.to_token_stream().to_string();
        s == "cfg (test)" || s.starts_with("cfg (all (test ,")
    })
}
impl Scan {
    fn load(&mut self, p: PathBuf, dir: PathBuf) {
        self.visited.insert(p.clone());
        let source = fs::read_to_string(&p).unwrap();
        let syntax = syn::parse_file(&source).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        let old_file = std::mem::replace(&mut self.file, p);
        let old_dir = std::mem::replace(&mut self.dir, dir);
        let cfg_len = self.cfg.len();
        self.cfg.extend(attrs(&syntax.attrs));
        self.visit_file(&syntax);
        self.cfg.truncate(cfg_len);
        self.file = old_file;
        self.dir = old_dir;
    }
    fn gate(&mut self, span: proc_macro2::Span, kind: &str, name: String) {
        self.gates.push(json!({"file":self.file,"line":span.start().line,"column":span.start().column,"kind":kind,"name":name}));
    }
}
impl<'ast> Visit<'ast> for Scan {
    fn visit_item_mod(&mut self, m: &'ast ItemMod) {
        let old = self.gated;
        let cfg_len = self.cfg.len();
        self.cfg.extend(attrs(&m.attrs));
        let gate = !old
            && (test_only(m)
                || (self.file.ends_with("acp/shared_session/tests.rs") && m.ident == "tests"))
            && m.content
                .as_ref()
                .is_some_and(|(_, items)| contains_test(items));
        if gate {
            self.gate(m.span(), "module", m.ident.to_string());
            self.gated = true;
        }
        self.modules.push(m.ident.to_string());
        if let Some((_, items)) = &m.content {
            let old_dir = self.dir.clone();
            self.dir.push(m.ident.to_string());
            for i in items {
                self.visit_item(i);
            }
            self.dir = old_dir;
        } else {
            let a = self.dir.join(format!("{}.rs", m.ident));
            let b = self.dir.join(m.ident.to_string()).join("mod.rs");
            let p = if a.exists() { a } else { b };
            self.load(p, self.dir.join(m.ident.to_string()));
        }
        self.modules.pop();
        self.gated = old;
        self.cfg.truncate(cfg_len);
    }
    fn visit_item_fn(&mut self, f: &'ast ItemFn) {
        if is_test(f) {
            let mut p = self.modules.clone();
            p.push(f.sig.ident.to_string());
            let mut cfg = self.cfg.clone();
            cfg.extend(attrs(&f.attrs));
            self.tests.push(json!({"file":self.file,"name":p.join("::"),"line":f.span().start().line,"end_line":f.span().end().line,"cfg":cfg,"attrs":f.attrs.iter().map(|a|a.meta.to_token_stream().to_string()).collect::<Vec<_>>()}));
            if !self.gated {
                self.gate(f.span(), "function", f.sig.ident.to_string());
            }
        }
        visit::visit_item_fn(self, f);
    }
    fn visit_item_macro(&mut self, m: &'ast syn::ItemMacro) {
        let s = m.mac.tokens.to_string();
        if s.contains("# [test]") || s.contains("# [tokio :: test") {
            self.warnings.push(format!(
                "{}: macro {} at {} contains test attributes",
                self.file.display(),
                m.mac.path.to_token_stream(),
                m.span().start().line
            ));
        }
        if m.mac.path.is_ident("include") {
            let p: syn::LitStr = syn::parse2(m.mac.tokens.clone()).expect("literal include path");
            let cfg_len = self.cfg.len();
            self.cfg.extend(attrs(&m.attrs));
            self.load(
                self.file.parent().unwrap().join(p.value()),
                self.dir.clone(),
            );
            self.cfg.truncate(cfg_len);
        }
        visit::visit_item_macro(self, m);
    }
}
fn orphan_test_files(root: &Path, visited: &BTreeSet<PathBuf>) -> Vec<String> {
    fn walk(
        root: &Path,
        directory: &Path,
        visited: &BTreeSet<PathBuf>,
        warnings: &mut Vec<String>,
    ) {
        let mut paths = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            // These are this package's separate binary roots, outside --lib.
            if path == root.join("bin")
                || path == root.join("server_bin")
                || path == root.join("main.rs")
            {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, visited, warnings);
            } else if path.extension().is_some_and(|extension| extension == "rs")
                && !visited.contains(&path)
            {
                let source = fs::read_to_string(&path).unwrap();
                let syntax = syn::parse_file(&source)
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                if contains_test(&syntax.items) {
                    warnings.push(format!(
                        "{} contains tests but is not registered in the library module tree",
                        path.display()
                    ));
                }
            }
        }
    }
    let mut warnings = vec![];
    walk(root, root, visited, &mut warnings);
    warnings
}

#[cfg(test)]
mod inventory_tests {
    use super::*;

    #[test]
    fn orphan_tests_are_reported_but_binary_roots_are_excluded() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("codeg-inventory-{}-{unique}", std::process::id()));
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("lib.rs"), "#[test] fn registered() {}\n").unwrap();
        fs::write(
            root.join("orphan.rs"),
            "mod tests { #[tokio::test] async fn forgotten() {} }\n",
        )
        .unwrap();
        fs::write(root.join("bin/main.rs"), "#[test] fn binary_test() {}\n").unwrap();
        let visited = BTreeSet::from([root.join("lib.rs")]);
        let warnings = orphan_test_files(&root, &visited);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("orphan.rs"));
        fs::remove_dir_all(root).unwrap();
    }
}

fn main() {
    let src = PathBuf::from(env::args().nth(1).expect("source directory"));
    let mut s = Scan {
        file: PathBuf::new(),
        dir: src.clone(),
        modules: vec![],
        cfg: vec![],
        tests: vec![],
        gates: vec![],
        gated: false,
        warnings: vec![],
        visited: BTreeSet::new(),
    };
    s.load(src.join("lib.rs"), src.clone());
    s.warnings.extend(orphan_test_files(&src, &s.visited));
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"tests":s.tests,"gates":s.gates,"warnings":s.warnings})
        )
        .unwrap()
    );
}
