use std::process::Stdio;

use tokio::process::Command;

/// `init` creates a servable file; a second `init` refuses to overwrite.
#[tokio::test]
async fn cli_init_creates_server_file() {
    let xdg = std::env::temp_dir().join(format!("multimcp-cli-init-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&xdg);
    let bin = env!("CARGO_BIN_EXE_multimcp");

    let out = Command::new(bin)
        .args(["init", "demo"])
        .env("XDG_CONFIG_HOME", &xdg)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let file = xdg.join("MultiMCP").join("demo.json");
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved["name"], serde_json::json!("demo"));
    assert!(
        saved["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "greet")
    );

    // Refuses to overwrite.
    let out = Command::new(bin)
        .args(["init", "demo"])
        .env("XDG_CONFIG_HOME", &xdg)
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());

    std::fs::remove_dir_all(&xdg).ok();
}
