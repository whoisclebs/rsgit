use std::process::Command;

#[test]
fn binary_rejects_invalid_bind_address() {
    let bin = env!("CARGO_BIN_EXE_rsgit");
    let output = Command::new(bin)
        .env("RSGIT_ADDR", "not-a-socket")
        .output()
        .expect("run rsgit binary");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid RSGIT_ADDR"),
        "stderr was: {stderr}"
    );
}
