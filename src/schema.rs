use std::collections::BTreeMap;
use std::sync::Arc;

use rmcp::model::JsonObject;
use serde_json::{Map, Value};

use crate::config::{ParamSpec, ParamType};

/// Build a JSON Schema 2020-12 object schema for a command's params.
pub fn input_schema_for(params: &BTreeMap<String, ParamSpec>) -> Arc<JsonObject> {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for (name, spec) in params {
        properties.insert(name.clone(), param_schema(name, spec));
        if spec.required && spec.default.is_none() {
            required.push(Value::String(name.clone()));
        }
    }
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String("object".to_string()));
    schema.insert("properties".to_string(), Value::Object(properties));
    schema.insert("required".to_string(), Value::Array(required));
    Arc::new(schema)
}

fn param_schema(name: &str, spec: &ParamSpec) -> Value {
    let mut obj = Map::new();
    match spec.kind {
        ParamType::String => {
            obj.insert("type".to_string(), Value::String("string".to_string()));
        }
        ParamType::Integer => {
            obj.insert("type".to_string(), Value::String("integer".to_string()));
        }
        ParamType::Number => {
            obj.insert("type".to_string(), Value::String("number".to_string()));
        }
        ParamType::Boolean => {
            obj.insert("type".to_string(), Value::String("boolean".to_string()));
        }
        ParamType::Enum => {
            obj.insert("type".to_string(), Value::String("string".to_string()));
            if let Some(values) = &spec.values {
                obj.insert(
                    "enum".to_string(),
                    Value::Array(values.iter().map(|v| Value::String(v.clone())).collect()),
                );
            }
        }
    }
    obj.insert("title".to_string(), Value::String(humanize(name)));
    if let Some(desc) = &spec.description {
        obj.insert("description".to_string(), Value::String(desc.clone()));
    }
    if let Some(default) = &spec.default {
        obj.insert("default".to_string(), default.clone());
    }
    if let Some(min) = spec.minimum {
        obj.insert("minimum".to_string(), Value::from(min));
    }
    if let Some(max) = spec.maximum {
        obj.insert("maximum".to_string(), Value::from(max));
    }
    Value::Object(obj)
}

fn humanize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut capitalize = true;
    for ch in name.chars() {
        if ch == '_' || ch == '-' {
            out.push(' ');
            capitalize = true;
        } else if capitalize {
            out.extend(ch.to_uppercase());
            capitalize = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Static input schema for the built-in `register_command` tool.
pub fn register_command_schema() -> Arc<JsonObject> {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string", "title": "Name",
                "description": "Tool name (letters, digits, '_', '-', '.'). Must be unique in this server."
            },
            "description": {
                "type": "string", "title": "Description",
                "description": "What the command does. Shown to the model."
            },
            "binary": {
                "type": "string", "title": "Binary",
                "description": "Absolute path to the executable, or a bare name resolved via PATH."
            },
            "args": {
                "type": "array", "title": "Args",
                "description": "argv tokens after the binary. Tokens may contain {param} placeholders. No shell is used.",
                "items": {"type": "string"}
            },
            "params": {
                "type": "object", "title": "Params",
                "description": "Parameter definitions: name -> {type, description, required, default, minimum, maximum, values}. Types: string, integer, number, boolean, enum (enum needs values).",
                "additionalProperties": {
                    "type": "object",
                    "properties": {
                        "type": {"type": "string", "enum": ["string", "integer", "number", "boolean", "enum"]},
                        "description": {"type": "string"},
                        "required": {"type": "boolean", "default": true},
                        "default": {},
                        "minimum": {"type": "number"},
                        "maximum": {"type": "number"},
                        "values": {"type": "array", "items": {"type": "string"}}
                    },
                    "required": ["type"]
                }
            },
            "timeout_secs": {
                "type": "integer", "title": "Timeout Secs",
                "description": "Kill the command after this many seconds.",
                "default": 60, "minimum": 1
            },
            "read_only": {
                "type": "boolean", "title": "Read Only",
                "description": "Hint that the command does not modify anything.",
                "default": false
            }
        },
        "required": ["name", "description", "binary", "args", "params"]
    });
    match schema {
        Value::Object(map) => Arc::new(map),
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CommandSpec;

    fn params(json: serde_json::Value) -> BTreeMap<String, ParamSpec> {
        let spec: CommandSpec = serde_json::from_value(serde_json::json!({
            "name": "c", "description": "d", "argv": ["echo"], "params": json,
        }))
        .unwrap();
        spec.params
    }

    #[test]
    fn schema_marks_required_and_defaults() {
        let schema = input_schema_for(&params(serde_json::json!({
            "a": {"type": "string"},
            "b": {"type": "integer", "required": false, "default": 3},
            "e": {"type": "enum", "values": ["x", "y"], "description": "pick"},
            "n": {"type": "number", "minimum": 1.0, "maximum": 2.0},
        })));
        assert_eq!(schema["type"], Value::String("object".to_string()));
        assert_eq!(
            schema["required"],
            Value::Array(vec![
                Value::String("a".to_string()),
                Value::String("e".to_string()),
                Value::String("n".to_string()),
            ])
        );
        let props = schema["properties"].as_object().unwrap();
        assert_eq!(props["b"]["default"], Value::from(3));
        assert_eq!(props["e"]["enum"], serde_json::json!(["x", "y"]));
        assert_eq!(props["e"]["description"], Value::String("pick".to_string()));
        assert_eq!(props["n"]["minimum"], Value::from(1.0));
        assert_eq!(props["n"]["maximum"], Value::from(2.0));
        assert_eq!(props["a"]["title"], Value::String("A".to_string()));
    }

    #[test]
    fn register_schema_requires_core_fields() {
        let schema = register_command_schema();
        assert_eq!(schema["type"], Value::String("object".to_string()));
        let required = schema["required"].as_array().unwrap();
        for field in ["name", "description", "binary", "args", "params"] {
            assert!(required.contains(&Value::String(field.to_string())));
        }
    }
}
