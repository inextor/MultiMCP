use std::collections::BTreeMap;
use std::time::Duration;

use rmcp::model::JsonObject;
use serde_json::Value;

use crate::config::{CommandSpec, ParamType, check_value_type};

/// Validate call args against the spec: apply defaults, reject unknown or
/// mistyped values, enforce required/enum/range. Returns final values.
pub fn resolve_args(
    spec: &CommandSpec,
    provided: Option<&JsonObject>,
) -> Result<BTreeMap<String, Value>, String> {
    if let Some(args) = provided {
        for key in args.keys() {
            if !spec.params.contains_key(key) {
                return Err(format!("unknown argument {key:?} for tool {:?}", spec.name));
            }
        }
    }
    let mut out = BTreeMap::new();
    for (name, param) in &spec.params {
        let value = provided
            .and_then(|args| args.get(name))
            .or(param.default.as_ref());
        let Some(value) = value else {
            if param.required {
                return Err(format!(
                    "missing required argument {name:?} for tool {:?}",
                    spec.name
                ));
            }
            continue;
        };
        check_value_type(&spec.name, name, param.kind, value, param.values.as_deref())
            .map_err(|e| e.to_string())?;
        if matches!(param.kind, ParamType::Integer | ParamType::Number)
            && let Some(n) = value.as_f64()
        {
            if let Some(min) = param.minimum
                && n < min
            {
                return Err(format!(
                    "argument {name:?} for tool {:?}: {n} < minimum {min}",
                    spec.name
                ));
            }
            if let Some(max) = param.maximum
                && n > max
            {
                return Err(format!(
                    "argument {name:?} for tool {:?}: {n} > maximum {max}",
                    spec.name
                ));
            }
        }
        out.insert(name.clone(), value.clone());
    }
    Ok(out)
}

fn render(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Substitute `{name}` placeholders in each argv token. `{{` / `}}` are
/// literal-brace escapes. Unknown placeholders are an error.
pub fn substitute(
    argv: &[String],
    values: &BTreeMap<String, Value>,
) -> Result<Vec<String>, String> {
    argv.iter()
        .map(|token| substitute_token(token, values))
        .collect()
}

fn substitute_token(token: &str, values: &BTreeMap<String, Value>) -> Result<String, String> {
    let mut out = String::with_capacity(token.len());
    let bytes = token.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                if bytes.get(i + 1) == Some(&b'{') {
                    out.push('{');
                    i += 2;
                } else if let Some(end) = token[i..].find('}') {
                    let name = &token[i + 1..i + end];
                    match values.get(name) {
                        Some(v) => out.push_str(&render(v)),
                        None => return Err(format!("no value for placeholder {{{name}}}")),
                    }
                    i += end + 1;
                } else {
                    out.push('{');
                    i += 1;
                }
            }
            b'}' => {
                if bytes.get(i + 1) == Some(&b'}') {
                    out.push('}');
                    i += 2;
                } else {
                    out.push('}');
                    i += 1;
                }
            }
            _ => {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
    }
    Ok(out)
}

pub struct ExecOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Run argv with no shell, enforcing a timeout. Pipes are drained
/// concurrently with the wait so large outputs cannot deadlock.
pub async fn run(argv: &[String], timeout_secs: u64) -> Result<ExecOutput, String> {
    use tokio::io::AsyncReadExt;

    let mut child = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot execute {:?}: {e}", argv[0]))?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        match stdout.as_mut() {
            Some(pipe) => pipe.read_to_end(&mut buf).await.map(|_| buf),
            None => Ok(buf),
        }
    });
    let err_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        match stderr.as_mut() {
            Some(pipe) => pipe.read_to_end(&mut buf).await.map(|_| buf),
            None => Ok(buf),
        }
    });
    let timeout = Duration::from_secs(timeout_secs.max(1));
    // The future is dropped on timeout, releasing the `child` borrow.
    let waited = tokio::time::timeout(timeout, async {
        let status = child.wait().await?;
        let stdout = out_task
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))??;
        let stderr = err_task
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))??;
        std::io::Result::Ok((status, stdout, stderr))
    })
    .await;
    match waited {
        Ok(Ok((status, stdout, stderr))) => Ok(ExecOutput {
            code: status.code(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            timed_out: false,
        }),
        Ok(Err(e)) => Err(format!("failed running {:?}: {e}", argv[0])),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Ok(ExecOutput {
                code: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: true,
            })
        }
    }
}

/// Format execution output as the tool result text.
pub fn format_output(spec_name: &str, argv: &[String], out: &ExecOutput) -> String {
    if out.timed_out {
        return format!("tool {spec_name:?} timed out");
    }
    let mut text = format!(
        "exit code: {}\ncommand: {}\n",
        code_str(out.code),
        argv.join(" ")
    );
    text.push_str(&format!("--- stdout ---\n{}", none_if_empty(&out.stdout)));
    text.push_str(&format!("--- stderr ---\n{}", none_if_empty(&out.stderr)));
    text
}

fn code_str(code: Option<i32>) -> String {
    code.map_or_else(|| "terminated by signal".to_string(), |c| c.to_string())
}

fn none_if_empty(s: &str) -> std::borrow::Cow<'_, str> {
    if s.trim().is_empty() {
        std::borrow::Cow::Borrowed("(empty)\n")
    } else if s.ends_with('\n') {
        std::borrow::Cow::Borrowed(s)
    } else {
        std::borrow::Cow::Owned(format!("{s}\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_with(params_json: serde_json::Value) -> CommandSpec {
        serde_json::from_value(serde_json::json!({
            "name": "cmd",
            "description": "d",
            "argv": ["echo"],
            "params": params_json,
        }))
        .unwrap()
    }

    fn obj(json: serde_json::Value) -> JsonObject {
        match json {
            Value::Object(map) => map,
            _ => panic!("not an object"),
        }
    }

    #[test]
    fn resolve_applies_defaults_and_rejects_unknown() {
        let spec = spec_with(serde_json::json!({
            "a": {"type": "string"},
            "b": {"type": "integer", "required": false, "default": 3},
        }));
        let args = obj(serde_json::json!({"a": "x"}));
        let out = resolve_args(&spec, Some(&args)).unwrap();
        assert_eq!(out["a"], Value::String("x".to_string()));
        assert_eq!(out["b"], Value::from(3));

        let missing = obj(serde_json::json!({}));
        assert!(resolve_args(&spec, Some(&missing)).is_err());

        let unknown = obj(serde_json::json!({"a": "x", "zzz": 1}));
        assert!(resolve_args(&spec, Some(&unknown)).is_err());

        let wrong_type = obj(serde_json::json!({"a": "x", "b": "three"}));
        assert!(resolve_args(&spec, Some(&wrong_type)).is_err());
    }

    #[test]
    fn resolve_enforces_enum_and_range() {
        let spec = spec_with(serde_json::json!({
            "e": {"type": "enum", "values": ["a", "b"]},
            "n": {"type": "integer", "minimum": 1.0, "maximum": 5.0},
        }));
        let ok = obj(serde_json::json!({"e": "a", "n": 4}));
        resolve_args(&spec, Some(&ok)).unwrap();
        let bad_enum = obj(serde_json::json!({"e": "z", "n": 4}));
        assert!(resolve_args(&spec, Some(&bad_enum)).is_err());
        let bad_range = obj(serde_json::json!({"e": "a", "n": 99}));
        assert!(resolve_args(&spec, Some(&bad_range)).is_err());
    }

    #[test]
    fn substitute_partial_tokens_and_escapes() {
        let spec = spec_with(serde_json::json!({
            "who": {"type": "string"},
            "n": {"type": "integer"},
        }));
        let args = obj(serde_json::json!({"who": "bob", "n": 7}));
        let values = resolve_args(&spec, Some(&args)).unwrap();
        let argv = substitute(
            &[
                "echo".to_string(),
                "hi {who} #{n}".to_string(),
                "{{who}}".to_string(),
            ],
            &values,
        )
        .unwrap();
        assert_eq!(argv, vec!["echo", "hi bob #7", "{who}"]);
    }

    #[tokio::test]
    async fn run_echo_and_failure() {
        let out = run(&["echo".to_string(), "hi".to_string()], 10)
            .await
            .unwrap();
        assert!(!out.timed_out);
        assert_eq!(out.code, Some(0));
        assert_eq!(out.stdout, "hi\n");

        let out = run(
            &["sh".to_string(), "-c".to_string(), "exit 3".to_string()],
            10,
        )
        .await
        .unwrap();
        assert_eq!(out.code, Some(3));
    }

    #[tokio::test]
    async fn run_timeout_kills() {
        let out = run(&["sleep".to_string(), "30".to_string()], 1)
            .await
            .unwrap();
        assert!(out.timed_out);
    }
}
