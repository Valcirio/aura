use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_DEPTH: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Read,
    LocalWrite,
    ExternalWrite,
    Destructive,
    System,
}

impl Effect {
    pub fn requires_approval(self) -> bool {
        matches!(
            self,
            Effect::ExternalWrite | Effect::Destructive | Effect::System
        )
    }
    pub fn label(self) -> &'static str {
        match self {
            Effect::Read => "Leitura",
            Effect::LocalWrite => "Escrita local",
            Effect::ExternalWrite => "Escrita externa",
            Effect::Destructive => "Destrutiva",
            Effect::System => "Sistema",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionField {
    pub label: String,
    pub value: String,
}

impl ActionField {
    pub fn new(label: &str, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub label: String,
    pub description: String,
    pub parameters: Value,
    pub effect: Effect,
    pub integration: String,
}

impl ToolSpec {
    pub fn new(
        integration: &str,
        name: &str,
        label: &str,
        description: &str,
        effect: Effect,
        parameters: Value,
    ) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            description: description.into(),
            parameters,
            effect,
            integration: integration.into(),
        }
    }
    pub fn definition(&self) -> Value {
        json!({"type":"function","function":{"name":self.name,"description":self.description,"parameters":self.parameters}})
    }
    pub fn validate(&self, args: &Value) -> Result<()> {
        validate_schema(&self.parameters, args)
    }
}

pub fn validate_schema(schema: &Value, value: &Value) -> Result<()> {
    check(schema, value, "$", 0)
}

fn err(path: &str, detail: impl AsRef<str>) -> AppError {
    AppError::new(
        "TOOL_ARGS_INVALID",
        format!("Argumento inválido em {path}: {}", detail.as_ref()),
    )
}

fn type_ok(t: &str, v: &Value) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        _ => true, // unknown type name: ignored
    }
}

fn check(schema: &Value, value: &Value, path: &str, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(err(path, "profundidade máxima excedida"));
    }
    let Some(s) = schema.as_object() else {
        return Ok(());
    };
    match s.get("type") {
        Some(Value::String(t)) if !type_ok(t, value) => {
            return Err(err(path, format!("tipo esperado {t}")));
        }
        Some(Value::Array(ts)) => {
            let names: Vec<&str> = ts.iter().filter_map(Value::as_str).collect();
            if !names.is_empty() && !names.iter().any(|t| type_ok(t, value)) {
                return Err(err(path, format!("tipo esperado {}", names.join(" ou "))));
            }
        }
        _ => {}
    }
    if let Some(Value::Array(options)) = s.get("enum") {
        if !options.contains(value) {
            return Err(err(path, "valor fora das opções permitidas"));
        }
    }
    if let (Some(n), Some(min)) = (value.as_f64(), s.get("minimum").and_then(Value::as_f64)) {
        if n < min {
            return Err(err(path, format!("valor mínimo {min}")));
        }
    }
    if let (Some(n), Some(max)) = (value.as_f64(), s.get("maximum").and_then(Value::as_f64)) {
        if n > max {
            return Err(err(path, format!("valor máximo {max}")));
        }
    }
    if let Some(text) = value.as_str() {
        let len = text.chars().count() as u64;
        if let Some(min) = s.get("minLength").and_then(Value::as_u64) {
            if len < min {
                return Err(err(path, format!("mínimo de {min} caracteres")));
            }
        }
        if let Some(max) = s.get("maxLength").and_then(Value::as_u64) {
            if len > max {
                return Err(err(path, format!("máximo de {max} caracteres")));
            }
        }
    }
    if let Some(items) = value.as_array() {
        let len = items.len() as u64;
        if let Some(min) = s.get("minItems").and_then(Value::as_u64) {
            if len < min {
                return Err(err(path, format!("mínimo de {min} itens")));
            }
        }
        if let Some(max) = s.get("maxItems").and_then(Value::as_u64) {
            if len > max {
                return Err(err(path, format!("máximo de {max} itens")));
            }
        }
        if let Some(item_schema) = s.get("items") {
            for (i, item) in items.iter().enumerate() {
                check(item_schema, item, &format!("{path}[{i}]"), depth + 1)?;
            }
        }
    }
    if let Some(obj) = value.as_object() {
        if let Some(Value::Array(req)) = s.get("required") {
            for key in req.iter().filter_map(Value::as_str) {
                if !obj.contains_key(key) {
                    return Err(err(&format!("{path}.{key}"), "campo obrigatório ausente"));
                }
            }
        }
        let props = s.get("properties").and_then(Value::as_object);
        if s.get("additionalProperties") == Some(&Value::Bool(false)) {
            for key in obj.keys() {
                if !props.is_some_and(|p| p.contains_key(key)) {
                    return Err(err(&format!("{path}.{key}"), "campo desconhecido"));
                }
            }
        }
        if let Some(props) = props {
            for (key, sub) in props {
                if let Some(v) = obj.get(key) {
                    check(sub, v, &format!("{path}.{key}"), depth + 1)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person_schema() -> Value {
        json!({
            "type": "object",
            "properties": {"name": {"type": "string"}, "n": {"type": "integer"}},
            "required": ["name"]
        })
    }

    #[test]
    fn valid_object() {
        assert!(validate_schema(&person_schema(), &json!({"name": "a", "n": 3})).is_ok());
    }

    #[test]
    fn missing_required() {
        let app_error = validate_schema(&person_schema(), &json!({})).unwrap_err();
        assert_eq!(app_error.code, "TOOL_ARGS_INVALID");
        assert!(app_error.message.contains("$.name"));
    }

    #[test]
    fn unknown_key_rejected() {
        let strict = json!({"type": "object", "properties": {"name": {"type": "string"}}, "additionalProperties": false});
        let app_error = validate_schema(&strict, &json!({"x": 1})).unwrap_err();
        assert_eq!(app_error.code, "TOOL_ARGS_INVALID");
        let loose = json!({"type": "object", "properties": {"name": {"type": "string"}}});
        assert!(validate_schema(&loose, &json!({"x": 1})).is_ok());
    }

    #[test]
    fn nested_array_type_path() {
        let schema = json!({"type":"object","properties":{"calendar_ids":{"type":"array","items":{"type":"string"}}}});
        let app_error =
            validate_schema(&schema, &json!({"calendar_ids": ["a", "b", 3]})).unwrap_err();
        assert_eq!(app_error.code, "TOOL_ARGS_INVALID");
        assert_eq!(
            app_error.message,
            "Argumento inválido em $.calendar_ids[2]: tipo esperado string"
        );
    }

    #[test]
    fn enum_mismatch() {
        let schema = json!({"enum": ["a", "b"]});
        let app_error = validate_schema(&schema, &json!("c")).unwrap_err();
        assert_eq!(app_error.code, "TOOL_ARGS_INVALID");
        assert!(validate_schema(&schema, &json!("a")).is_ok());
    }

    #[test]
    fn max_items() {
        let schema =
            json!({"type": "array", "items": {"type": "integer"}, "maxItems": 2, "minItems": 1});
        let app_error = validate_schema(&schema, &json!([1, 2, 3])).unwrap_err();
        assert_eq!(app_error.code, "TOOL_ARGS_INVALID");
        let app_error = validate_schema(&schema, &json!([])).unwrap_err();
        assert_eq!(app_error.code, "TOOL_ARGS_INVALID");
    }

    #[test]
    fn string_length_counts_chars() {
        let schema = json!({"type": "string", "minLength": 2, "maxLength": 3});
        let app_error = validate_schema(&schema, &json!("a")).unwrap_err();
        assert_eq!(app_error.code, "TOOL_ARGS_INVALID");
        let app_error = validate_schema(&schema, &json!("abcd")).unwrap_err();
        assert_eq!(app_error.code, "TOOL_ARGS_INVALID");
        assert_eq!("çã".len(), 4);
        assert!(validate_schema(&schema, &json!("çã")).is_ok());
    }

    #[test]
    fn integer_rules() {
        let schema = json!({"type": "integer"});
        let app_error = validate_schema(&schema, &json!(1.5)).unwrap_err();
        assert_eq!(app_error.code, "TOOL_ARGS_INVALID");
        assert!(validate_schema(&schema, &json!(3)).is_ok());
        let bounded = json!({"type": "integer", "minimum": 2, "maximum": 5});
        let app_error = validate_schema(&bounded, &json!(1)).unwrap_err();
        assert_eq!(app_error.code, "TOOL_ARGS_INVALID");
        let app_error = validate_schema(&bounded, &json!(6)).unwrap_err();
        assert_eq!(app_error.code, "TOOL_ARGS_INVALID");
        assert!(validate_schema(&bounded, &json!(3)).is_ok());
    }

    #[test]
    fn type_array_with_null() {
        let schema = json!({"type": ["string", "null"]});
        assert!(validate_schema(&schema, &json!(null)).is_ok());
        assert!(validate_schema(&schema, &json!("x")).is_ok());
        let app_error = validate_schema(&schema, &json!(1)).unwrap_err();
        assert_eq!(app_error.code, "TOOL_ARGS_INVALID");
    }

    #[test]
    fn empty_args_ok() {
        let schema = json!({"type": "object", "properties": {}});
        assert!(validate_schema(&schema, &json!({})).is_ok());
    }

    #[test]
    fn depth_cap() {
        let mut schema = json!({"type": "string"});
        let mut value = json!("x");
        for _ in 0..40 {
            schema = json!({"type": "array", "items": schema});
            value = json!([value]);
        }
        let app_error = validate_schema(&schema, &value).unwrap_err();
        assert_eq!(app_error.code, "TOOL_ARGS_INVALID");
    }

    #[test]
    fn effect_approval() {
        assert!(!Effect::Read.requires_approval());
        assert!(!Effect::LocalWrite.requires_approval());
        assert!(Effect::ExternalWrite.requires_approval());
        assert!(Effect::Destructive.requires_approval());
        assert!(Effect::System.requires_approval());
        assert_eq!(Effect::LocalWrite.label(), "Escrita local");
        assert_eq!(
            serde_json::to_value(Effect::ExternalWrite).unwrap(),
            json!("external_write")
        );
    }

    #[test]
    fn definition_shape() {
        let spec = ToolSpec::new(
            "cal",
            "calendar_list",
            "Listando",
            "desc",
            Effect::Read,
            json!({"type": "object", "properties": {}}),
        );
        let d = spec.definition();
        assert_eq!(d["type"], "function");
        assert_eq!(d["function"]["name"], "calendar_list");
        assert_eq!(d["function"]["description"], "desc");
        assert_eq!(
            d["function"]["parameters"],
            json!({"type": "object", "properties": {}})
        );
    }

    #[test]
    fn google_calendar_schemas() {
        let parameters = &json!({"type":"object","properties":{"calendar_id":{"type":"string"},"start":{"type":"string"},"end":{"type":"string"},"query":{"type":"string"},"page_token":{"type":"string"}},"required":["calendar_id","start","end"],"additionalProperties":false});
        let args = json!({
            "calendar_id": "primary",
            "start": "2026-11-03T00:00:00-03:00",
            "end": "2026-11-04T00:00:00-03:00"
        });
        assert!(validate_schema(parameters, &args).is_ok());
        let mut extra = args.clone();
        extra["extra"] = json!(1);
        let app_error = validate_schema(parameters, &extra).unwrap_err();
        assert_eq!(app_error.code, "TOOL_ARGS_INVALID");
        let mut missing = args.clone();
        missing.as_object_mut().unwrap().remove("end");
        let app_error = validate_schema(parameters, &missing).unwrap_err();
        assert_eq!(app_error.code, "TOOL_ARGS_INVALID");
    }
}
