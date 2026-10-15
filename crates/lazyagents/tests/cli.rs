use std::process::Command;

#[test]
fn prints_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_lazyagents"))
        .arg("--version")
        .output()
        .expect("binary runs");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("lazyagents {}", env!("CARGO_PKG_VERSION"))
    );
}
