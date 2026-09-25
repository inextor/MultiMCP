use std::path::PathBuf;
use std::process::Stdio;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

fn fixture_config() -> Value {
    json!({
        "name": "e2e",
        "instructions": "e2e test server",
        "commands": [
            {
                "name": "greet",
                "description": "Print a greeting",
                "argv": ["echo", "hello {who}"],
                "params": {
                    "who": {"type": "string", "required": false, "default": "world"}
                },
                "timeout_secs": 10,
                "read_only": true
            },
            {
                "name": "failer",
                "description": "Always fails",
                "argv": ["sh", "-c", "echo oops >&2; exit 3"],
                "params": {},
                "timeout_secs": 10,
                "read_only": true
            }
        ]
    })
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    lines: tokio::io::Lines<BufReader<ChildStdout>>,
    next_id: i64,
}

impl Client {
    async fn spawn(xdg: &PathBuf) -> Self {
        let bin = env!("CARGO_BIN_EXE_multimcp");
        let mut child = Command::new(bin)
            .arg("e2e")
            .env("XDG_CONFIG_HOME", xdg)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next_id: 1,
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.stdin
            .write_all(format!("{msg}\n").as_bytes())
            .await
            .unwrap();
        loop {
            let line =
                tokio::time::timeout(std::time::Duration::from_secs(15), self.lines.next_line())
                    .await
                    .expect("response timeout")
                    .unwrap()
                    .expect("server closed stdout");
            let value: Value = serde_json::from_str(&line).unwrap();
            // Skip server notifications (no id).
            if value.get("id").and_then(|i| i.as_i64()) == Some(id) {
                return value;
            }
        }
    }

    async fn notify(&mut self, method: &str) {
        let msg = json!({"jsonrpc": "2.0", "method": method});
        self.stdin
            .write_all(format!("{msg}\n").as_bytes())
            .await
            .unwrap();
    }

    async fn shutdown(mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

fn setup_xdg(test_name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("multimcp-e2e-{}-{}", test_name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cfg_dir = dir.join("MultiMCP");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("e2e.json"),
        serde_json::to_string_pretty(&fixture_config()).unwrap(),
    )
    .unwrap();
    dir
}

fn tool_names(list_result: &Value) -> Vec<String> {
    list_result["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

fn first_text(call_result: &Value) -> String {
    call_result["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn stdio_list_call_and_errors() {
    let xdg = setup_xdg("basic");
    let mut client = Client::spawn(&xdg).await;

    let init = client
        .request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "e2e", "version": "0.0.0"},
            }),
        )
        .await;
    assert_eq!(init["result"]["serverInfo"]["name"], json!("e2e"));
    client.notify("notifications/initialized").await;

    let list = client.request("tools/list", json!({})).await;
    let names = tool_names(&list);
    assert!(names.contains(&"greet".to_string()));
    assert!(names.contains(&"failer".to_string()));
    assert!(names.contains(&"register_command".to_string()));

    // greet schema: who is optional with default.
    let greet = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "greet")
        .unwrap();
    assert_eq!(greet["inputSchema"]["required"], json!([]));
    assert_eq!(
        greet["inputSchema"]["properties"]["who"]["default"],
        json!("world")
    );

    // Call with explicit arg.
    let call = client
        .request(
            "tools/call",
            json!({"name": "greet", "arguments": {"who": "bob"}}),
        )
        .await;
    assert!(first_text(&call).contains("hello bob"));
    assert_ne!(call["result"]["isError"], json!(true));

    // Call with defaults applied.
    let call = client
        .request("tools/call", json!({"name": "greet", "arguments": {}}))
        .await;
    assert!(first_text(&call).contains("hello world"));

    // Failing command -> tool error result, not a crash.
    let call = client
        .request("tools/call", json!({"name": "failer", "arguments": {}}))
        .await;
    assert_eq!(call["result"]["isError"], json!(true));
    assert!(first_text(&call).contains("exit code: 3"));

    // Unknown tool -> protocol error.
    let call = client
        .request("tools/call", json!({"name": "nope", "arguments": {}}))
        .await;
    assert_eq!(call["error"]["code"], json!(-32602));

    // Unknown argument -> protocol error.
    let call = client
        .request(
            "tools/call",
            json!({"name": "greet", "arguments": {"bogus": 1}}),
        )
        .await;
    assert_eq!(call["error"]["code"], json!(-32602));

    client.shutdown().await;
    std::fs::remove_dir_all(&xdg).ok();
}

#[tokio::test]
async fn stdio_register_persists_and_survives_restart() {
    let xdg = setup_xdg("register");
    let cfg_file = xdg.join("MultiMCP").join("e2e.json");
    let mut client = Client::spawn(&xdg).await;

    let init = client
        .request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "e2e", "version": "0.0.0"},
            }),
        )
        .await;
    assert!(init.get("result").is_some());
    client.notify("notifications/initialized").await;

    let reg = client
        .request(
            "tools/call",
            json!({
                "name": "register_command",
                "arguments": {
                    "name": "shout",
                    "description": "Echo loudly",
                    "binary": "echo",
                    "args": ["LOUD {word}"],
                    "params": {"word": {"type": "string", "description": "Word"}},
                    "timeout_secs": 10,
                    "read_only": true,
                }
            }),
        )
        .await;
    assert_ne!(reg["result"]["isError"], json!(true));
    assert!(first_text(&reg).contains("shout"));

    // Available immediately.
    let list = client.request("tools/list", json!({})).await;
    assert!(tool_names(&list).contains(&"shout".to_string()));

    // Callable immediately.
    let call = client
        .request(
            "tools/call",
            json!({"name": "shout", "arguments": {"word": "hi"}}),
        )
        .await;
    assert!(first_text(&call).contains("LOUD hi"));

    // Duplicate registration rejected.
    let dup = client
        .request(
            "tools/call",
            json!({
                "name": "register_command",
                "arguments": {
                    "name": "shout",
                    "description": "dup",
                    "binary": "echo",
                    "args": ["x"],
                    "params": {},
                }
            }),
        )
        .await;
    assert_eq!(dup["result"]["isError"], json!(true));

    client.shutdown().await;

    // Persisted to the server file.
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&cfg_file).unwrap()).unwrap();
    assert!(
        saved["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "shout")
    );

    // Survives restart.
    let mut client2 = Client::spawn(&xdg).await;
    let init = client2
        .request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "e2e", "version": "0.0.0"},
            }),
        )
        .await;
    assert!(init.get("result").is_some());
    client2.notify("notifications/initialized").await;
    let list = client2.request("tools/list", json!({})).await;
    assert!(tool_names(&list).contains(&"shout".to_string()));
    client2.shutdown().await;

    std::fs::remove_dir_all(&xdg).ok();
}
