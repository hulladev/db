#![cfg(feature = "server")]

use std::{fs, path::PathBuf, process::Command};

#[test]
fn authoring_rejects_wrong_table_ids_fields_query_writes_and_mutation_watches() {
    let dependencies = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/ui");
    let output =
        std::env::temp_dir().join(format!("hulladb-compile-contracts-{}", std::process::id()));
    fs::create_dir_all(&output).unwrap();
    let probe = output.join("server_probe.rs");
    fs::write(&probe, "use hulladb::QueryContext; fn main() {}\n").unwrap();
    let mut libraries: Vec<_> = fs::read_dir(&dependencies)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|file| {
            file.file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("libhulladb-") && name.ends_with(".rlib"))
        })
        .collect();
    libraries.sort_by_key(|file| std::cmp::Reverse(file.metadata().unwrap().modified().unwrap()));
    // Client-only and server artifacts coexist in the same target directory.
    // Select the newest server artifact instead of relying on build order.
    let library = libraries
        .into_iter()
        .find(|file| {
            Command::new("rustc")
                .args(["--edition=2024", "--emit=metadata", "--extern"])
                .arg(format!("hulladb={}", file.path().display()))
                .arg("-L")
                .arg(format!("dependency={}", dependencies.display()))
                .arg("--out-dir")
                .arg(&output)
                .arg(&probe)
                .output()
                .unwrap()
                .status
                .success()
        })
        .expect("compiled hulladb server library")
        .path();
    for (case, diagnostic) in [
        ("query_write", "no method named `insert`"),
        ("wrong_id", "expected `Id<Note>`, found `Id<Other>`"),
        ("wrong_field", "expected `String`, found `bool`"),
        ("mutation_watch", "cannot find function `watch`"),
        ("unsupported_input", "supported scalars are"),
        (
            "conflicting_reference",
            "already declares its reference target",
        ),
    ] {
        let result = Command::new("rustc")
            .arg("--edition=2024")
            .arg("--emit=metadata")
            .arg("--extern")
            .arg(format!("hulladb={}", library.display()))
            .arg("-L")
            .arg(format!("dependency={}", dependencies.display()))
            .arg("--out-dir")
            .arg(&output)
            .arg(fixtures.join(format!("{case}.rs")))
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success(), "{case} unexpectedly compiled");
        assert!(
            stderr.contains(diagnostic),
            "{case} failed for another reason:\n{stderr}"
        );
    }
    fs::remove_dir_all(output).unwrap();
}
