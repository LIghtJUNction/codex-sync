//! Codex conversation synchronization library.

pub mod cc_switch;
pub mod cli;
pub mod config;
pub mod discovery;
pub mod history;
pub mod sync;
pub mod web;

#[cfg(test)]
mod architecture_tests {
    use std::{fs, path::Path};
    use walkdir::WalkDir;

    #[test]
    fn modules_use_named_files_instead_of_mod_rs() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

        let legacy_modules = WalkDir::new(&source)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file() && entry.file_name() == "mod.rs")
            .map(|entry| entry.path().display().to_string())
            .collect::<Vec<_>>();
        assert!(
            legacy_modules.is_empty(),
            "禁止使用 mod.rs：{}",
            legacy_modules.join(", ")
        );

        for entry in fs::read_dir(&source).expect("read src") {
            let path = entry.expect("read src entry").path();
            if !path.is_dir() {
                continue;
            }
            let module = path.file_name().expect("module directory name");
            let entry_file = source.join(module).with_extension("rs");
            assert!(
                entry_file.is_file(),
                "模块目录 {} 缺少同名入口文件 {}",
                path.display(),
                entry_file.display()
            );
        }
    }
}
