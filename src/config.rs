use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const CONFIG_DIR_NAME: &str = "MultiMCP";
pub const CONFIG_EXTENSION: &str = "json";

/// Resolve `<name>` to `<config_dir>/MultiMCP/<name>.json`.
///
/// A trailing `.json` suffix is accepted and normalized. Anything looking
/// like a path (slashes, parent refs, absolute paths) is rejected: the file
/// always comes from the MultiMCP config directory.
pub fn resolve_config_path(name: &str) -> Result<PathBuf> {
    let stem = name.strip_suffix(".json").unwrap_or(name);
    if stem.is_empty()
        || stem.contains('/')
        || stem.contains('\\')
        || stem.contains("..")
        || stem.starts_with('.')
    {
        bail!("invalid server name {name:?}: expected a plain name like \"backup\"");
    }
    let base = dirs::config_dir().context("cannot determine user config directory")?;
    Ok(base
        .join(CONFIG_DIR_NAME)
        .join(format!("{stem}.{CONFIG_EXTENSION}")))
}

/// Top-level content of one `<name>.json` server file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
}

/// One command exposed as an MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    pub name: String,
    pub description: String,
    /// argv template; `{param}` placeholders are substituted per call. No shell.
    pub argv: Vec<String>,
    #[serde(default)]
    pub params: BTreeMap<String, ParamSpec>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub read_only: bool,
}

fn default_timeout() -> u64 {
    60
}

/// One typed tool parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamSpec {
    #[serde(rename = "type")]
    pub kind: ParamType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "default_required")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
    /// Allowed values for `enum` params.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
}

fn default_required() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    String,
    Integer,
    Number,
    Boolean,
    Enum,
}

pub fn is_valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
}

impl ServerDefinition {
    pub fn validate(&self) -> Result<()> {
        for cmd in &self.commands {
            cmd.validate()?;
        }
        let mut seen = std::collections::HashSet::new();
        for cmd in &self.commands {
            if !seen.insert(cmd.name.as_str()) {
                bail!("duplicate command name {:?}", cmd.name);
            }
        }
        Ok(())
    }
}

impl CommandSpec {
    pub fn validate(&self) -> Result<()> {
        if !is_valid_tool_name(&self.name) {
            bail!("invalid command name {:?}", self.name);
        }
        if self.argv.is_empty() {
            bail!("command {:?} has an empty argv", self.name);
        }
        for (param_name, spec) in &self.params {
            if !is_valid_param_name(param_name) {
                bail!(
                    "command {:?} has invalid param name {param_name:?}",
                    self.name
                );
            }
            spec.validate(&self.name, param_name)?;
        }
        for token in &self.argv {
            for placeholder in find_placeholders(token) {
                if !self.params.contains_key(&placeholder) {
                    bail!(
                        "command {:?} uses {{{placeholder}}} which is not a declared param",
                        self.name
                    );
                }
            }
        }
        if self.timeout_secs == 0 {
            bail!("command {:?} has timeout_secs = 0", self.name);
        }
        Ok(())
    }
}

fn is_valid_param_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl ParamSpec {
    fn validate(&self, cmd: &str, param: &str) -> Result<()> {
        if self.kind == ParamType::Enum {
            match &self.values {
                Some(v) if !v.is_empty() => {}
                _ => {
                    bail!("command {cmd:?} param {param:?}: enum needs a non-empty \"values\" list")
                }
            }
        } else if self.values.is_some() {
            bail!("command {cmd:?} param {param:?}: \"values\" is only valid for enum params");
        }
        if matches!(self.kind, ParamType::Integer | ParamType::Number) {
            if let (Some(min), Some(max)) = (self.minimum, self.maximum)
                && min > max
            {
                bail!("command {cmd:?} param {param:?}: minimum > maximum");
            }
        } else if self.minimum.is_some() || self.maximum.is_some() {
            bail!("command {cmd:?} param {param:?}: minimum/maximum only valid for numeric params");
        }
        if let Some(default) = &self.default {
            check_value_type(cmd, param, self.kind, default, self.values.as_deref())?;
        }
        Ok(())
    }
}

/// Check a JSON value against a param type (used for defaults and call args).
pub fn check_value_type(
    cmd: &str,
    param: &str,
    kind: ParamType,
    value: &serde_json::Value,
    values: Option<&[String]>,
) -> Result<()> {
    let ok = match kind {
        ParamType::String => value.is_string(),
        ParamType::Integer => value.as_i64().is_some(),
        ParamType::Number => value.as_f64().is_some(),
        ParamType::Boolean => value.is_boolean(),
        ParamType::Enum => value
            .as_str()
            .is_some_and(|s| values.is_some_and(|v| v.iter().any(|a| a == s))),
    };
    if !ok {
        bail!("command {cmd:?} param {param:?}: expected {kind:?}, got {value}");
    }
    Ok(())
}

/// Find `{name}` placeholders in a token. `{{` / `}}` are escapes, not placeholders.
pub fn find_placeholders(token: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = token.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                if bytes.get(i + 1) == Some(&b'{') {
                    i += 2;
                } else if let Some(end) = token[i..].find('}') {
                    let name = &token[i + 1..i + end];
                    if !name.is_empty() {
                        out.push(name.to_string());
                    }
                    i += end + 1;
                } else {
                    i += 1;
                }
            }
            b'}' => {
                i += if bytes.get(i + 1) == Some(&b'}') {
                    2
                } else {
                    1
                };
            }
            _ => i += 1,
        }
    }
    out
}

/// Load and validate a server file.
pub fn load_server_file(path: &std::path::Path) -> Result<ServerDefinition> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let def: ServerDefinition = serde_json::from_str(&text)
        .with_context(|| format!("invalid JSON in {}", path.display()))?;
    def.validate()?;
    Ok(def)
}

/// Atomically write a server file (temp file + rename).
pub fn save_server_file(path: &std::path::Path, def: &ServerDefinition) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(def)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

pub fn missing_file_hint(path: &std::path::Path) -> String {
    format!(
        "no such server file: {}\ncreate it with content like:\n{}",
        path.display(),
        serde_json::to_string_pretty(&serde_json::json!({
            "name": "example",
            "instructions": "Example server. Describe how to use these tools.",
            "commands": [{
                "name": "greet",
                "description": "Print a greeting",
                "argv": ["echo", "hello {who}"],
                "params": {
                    "who": {"type": "string", "description": "Who to greet",
                            "required": false, "default": "world"}
                },
                "timeout_secs": 30,
                "read_only": true
            }]
        }))
        .unwrap()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_accepts_plain_name_and_json_suffix() {
        let a = resolve_config_path("backup").unwrap();
        let b = resolve_config_path("backup.json").unwrap();
        assert_eq!(a, b);
        assert!(a.ends_with("MultiMCP/backup.json"));
    }

    #[test]
    fn resolve_rejects_paths_and_traversal() {
        for bad in ["", "../evil", "a/b", "a\\b", ".hidden", "..", "/abs"] {
            assert!(
                resolve_config_path(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn placeholders_and_escapes() {
        assert_eq!(
            find_placeholders("echo {who} {{literal}} {x}"),
            vec!["who".to_string(), "x".to_string()]
        );
        assert!(find_placeholders("no placeholders").is_empty());
        assert!(find_placeholders("{{only}} escapes").is_empty());
    }

    fn spec(argv: &[&str], params_json: serde_json::Value) -> CommandSpec {
        serde_json::from_value(serde_json::json!({
            "name": "cmd",
            "description": "d",
            "argv": argv,
            "params": params_json,
        }))
        .unwrap()
    }

    #[test]
    fn validate_ok() {
        let s = spec(
            &["echo", "{who}"],
            serde_json::json!({"who": {"type": "string", "required": false, "default": "w"}}),
        );
        s.validate().unwrap();
    }

    #[test]
    fn validate_rejects_undeclared_placeholder() {
        let s = spec(&["echo", "{nope}"], serde_json::json!({}));
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_rejects_bad_enum() {
        let s = spec(&["echo"], serde_json::json!({"e": {"type": "enum"}}));
        assert!(s.validate().is_err());
        let s = spec(
            &["echo"],
            serde_json::json!({"e": {"type": "enum", "values": ["a"]}}),
        );
        s.validate().unwrap();
    }

    #[test]
    fn validate_rejects_mistyped_default() {
        let s = spec(
            &["echo"],
            serde_json::json!({"n": {"type": "integer", "default": "ten"}}),
        );
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_rejects_duplicates() {
        let def = ServerDefinition {
            name: "s".to_string(),
            instructions: None,
            commands: vec![
                spec(&["echo"], serde_json::json!({})),
                spec(&["echo"], serde_json::json!({})),
            ],
        };
        assert!(def.validate().is_err());
    }
}
