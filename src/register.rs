use std::collections::BTreeMap;

use rmcp::model::JsonObject;
use serde_json::Value;

use crate::config::{CommandSpec, ParamSpec, is_valid_tool_name};

pub const REGISTER_TOOL_NAME: &str = "register_command";

/// Build and validate a `CommandSpec` from `register_command` arguments.
pub fn build_spec(values: &JsonObject) -> Result<CommandSpec, String> {
    let name = get_string(values, "name")?;
    let description = get_string(values, "description")?;
    let binary = get_string(values, "binary")?;
    let args = get_string_array(values, "args")?;
    let params = get_params(values)?;
    let timeout_secs = get_optional_u64(values, "timeout_secs")?.unwrap_or(60);
    let read_only = get_optional_bool(values, "read_only")?.unwrap_or(false);

    if !is_valid_tool_name(&name) {
        return Err(format!("invalid tool name {name:?}"));
    }
    if name == REGISTER_TOOL_NAME {
        return Err(format!("{REGISTER_TOOL_NAME:?} is reserved"));
    }
    check_binary(&binary)?;

    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(binary);
    argv.extend(args);

    let spec = CommandSpec {
        name,
        description,
        argv,
        params,
        timeout_secs,
        read_only,
    };
    spec.validate().map_err(|e| e.to_string())?;
    Ok(spec)
}

fn get_string(values: &JsonObject, key: &str) -> Result<String, String> {
    match values.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        _ => Err(format!("{key:?} must be a non-empty string")),
    }
}

fn get_string_array(values: &JsonObject, key: &str) -> Result<Vec<String>, String> {
    match values.get(key) {
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(s) => out.push(s.clone()),
                    _ => return Err(format!("{key:?} must be an array of strings")),
                }
            }
            Ok(out)
        }
        _ => Err(format!("{key:?} must be an array of strings")),
    }
}

fn get_params(values: &JsonObject) -> Result<BTreeMap<String, ParamSpec>, String> {
    match values.get("params") {
        Some(Value::Object(map)) => {
            let mut out = BTreeMap::new();
            for (name, spec) in map {
                let parsed: ParamSpec = serde_json::from_value(spec.clone())
                    .map_err(|e| format!("param {name:?} is invalid: {e}"))?;
                out.insert(name.clone(), parsed);
            }
            Ok(out)
        }
        _ => Err("\"params\" must be an object".to_string()),
    }
}

fn get_optional_u64(values: &JsonObject, key: &str) -> Result<Option<u64>, String> {
    match values.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_u64()
            .filter(|v| *v > 0)
            .map(Some)
            .ok_or_else(|| format!("{key:?} must be a positive integer")),
        _ => Err(format!("{key:?} must be a positive integer")),
    }
}

fn get_optional_bool(values: &JsonObject, key: &str) -> Result<Option<bool>, String> {
    match values.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        _ => Err(format!("{key:?} must be a boolean")),
    }
}

fn check_binary(binary: &str) -> Result<(), String> {
    if binary.contains('/') {
        let path = std::path::Path::new(binary);
        if !path.is_file() {
            return Err(format!("binary {binary:?} does not exist"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path)
                .map_err(|e| format!("cannot stat {binary:?}: {e}"))?
                .permissions()
                .mode();
            if mode & 0o111 == 0 {
                return Err(format!("binary {binary:?} is not executable"));
            }
        }
        Ok(())
    } else {
        which::which(binary)
            .map(|_| ())
            .map_err(|_| format!("binary {binary:?} not found in PATH"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(json: serde_json::Value) -> JsonObject {
        match json {
            Value::Object(map) => map,
            _ => panic!("not an object"),
        }
    }

    fn valid() -> JsonObject {
        args(serde_json::json!({
            "name": "hi",
            "description": "say hi",
            "binary": "echo",
            "args": ["hello {who}"],
            "params": {"who": {"type": "string", "required": false, "default": "w"}},
        }))
    }

    #[test]
    fn build_ok_with_defaults() {
        let spec = build_spec(&valid()).unwrap();
        assert_eq!(
            spec.argv,
            vec!["echo".to_string(), "hello {who}".to_string()]
        );
        assert_eq!(spec.timeout_secs, 60);
        assert!(!spec.read_only);
    }

    #[test]
    fn build_rejects_bad_input() {
        // reserved name
        let mut v = valid();
        v.insert(
            "name".to_string(),
            Value::String(REGISTER_TOOL_NAME.to_string()),
        );
        assert!(build_spec(&v).is_err());
        // bad name
        let mut v = valid();
        v.insert("name".to_string(), Value::String("has space".to_string()));
        assert!(build_spec(&v).is_err());
        // missing binary
        let mut v = valid();
        v.insert(
            "binary".to_string(),
            Value::String("definitely-not-a-real-binary-xyz".to_string()),
        );
        assert!(build_spec(&v).is_err());
        // nonexistent absolute path
        let mut v = valid();
        v.insert(
            "binary".to_string(),
            Value::String("/nonexistent/bin".to_string()),
        );
        assert!(build_spec(&v).is_err());
        // placeholder without param
        let mut v = valid();
        v.insert(
            "args".to_string(),
            Value::Array(vec![Value::String("{ghost}".to_string())]),
        );
        assert!(build_spec(&v).is_err());
        // invalid param spec
        let mut v = valid();
        v.insert(
            "params".to_string(),
            serde_json::json!({"p": {"type": "nonsense"}}),
        );
        assert!(build_spec(&v).is_err());
    }
}
