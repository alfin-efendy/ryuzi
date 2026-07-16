use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const MAX_TOOL_SCHEMA_BYTES: usize = 256 * 1024;
pub const MAX_TOOL_DESCRIPTION_BYTES: usize = 16 * 1024;
pub const MAX_NORMALIZATION_METADATA_ENTRIES: usize = 16;
pub const MAX_NORMALIZATION_METADATA_KEY_BYTES: usize = 64;
pub const MAX_NORMALIZATION_METADATA_VALUE_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolEffect {
    ReadOnly,
    Mutating,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceScopeHint {
    None,
    Workspace,
    External,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FacadePriority {
    Preferred,
    Normal,
    Deferred,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub canonical_name: String,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub kind: String,
    pub effect: ToolEffect,
    pub idempotent: bool,
    pub interactive: bool,
    pub sequential_barrier: bool,
    pub resource_scope: ResourceScopeHint,
    pub result_limit_bytes: u64,
    pub facade_priority: FacadePriority,
    pub policy_aliases: Vec<String>,
    pub v2_only: bool,
    pub v1_only: bool,
    pub allow_lossless_coercions: bool,
}

impl ToolDescriptor {
    pub fn conservative(
        canonical_name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        kind: impl Into<String>,
    ) -> Self {
        let kind = kind.into();
        let read_only = matches!(kind.as_str(), "read" | "search" | "fetch");
        let resource_scope = match kind.as_str() {
            "read" | "search" => ResourceScopeHint::Workspace,
            "fetch" => ResourceScopeHint::External,
            _ => ResourceScopeHint::Unknown,
        };
        Self {
            canonical_name: canonical_name.into(),
            description: description.into(),
            input_schema,
            output_schema: None,
            kind,
            effect: if read_only {
                ToolEffect::ReadOnly
            } else {
                ToolEffect::Unknown
            },
            idempotent: read_only,
            interactive: false,
            sequential_barrier: !read_only,
            resource_scope,
            result_limit_bytes: 50_000,
            facade_priority: FacadePriority::Normal,
            policy_aliases: Vec::new(),
            v2_only: false,
            v1_only: false,
            allow_lossless_coercions: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorCategory {
    Caller,
    Precondition,
    Conflict,
    Permission,
    Transient,
    Timeout,
    Cancelled,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolError {
    pub category: ToolErrorCategory,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl ToolError {
    pub fn new(
        category: ToolErrorCategory,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            category,
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    pub fn caller(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ToolErrorCategory::Caller, code, message)
    }

    pub fn precondition(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ToolErrorCategory::Precondition, code, message)
    }

    pub fn transient(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ToolErrorCategory::Transient, code, message)
    }

    pub fn internal(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(ToolErrorCategory::Internal, code, message)
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ToolError {}

pub struct ToolInputCtx<'a> {
    pub work_dir: &'a Path,
    pub attachments_dir: Option<&'a Path>,
    pub extra_skill_dirs: &'a [PathBuf],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedInput {
    pub value: Value,
    pub normalized: bool,
    metadata: BTreeMap<String, String>,
}

impl NormalizedInput {
    pub fn unchanged(value: Value) -> Self {
        Self {
            value,
            normalized: false,
            metadata: BTreeMap::new(),
        }
    }

    pub fn changed(value: Value) -> Self {
        Self {
            value,
            normalized: true,
            metadata: BTreeMap::new(),
        }
    }

    pub fn metadata(&self) -> &BTreeMap<String, String> {
        &self.metadata
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        if self.metadata.len() >= MAX_NORMALIZATION_METADATA_ENTRIES {
            return self;
        }
        let mut key = key.into();
        let mut value = value.into();
        truncate_utf8(&mut key, MAX_NORMALIZATION_METADATA_KEY_BYTES);
        truncate_utf8(&mut value, MAX_NORMALIZATION_METADATA_VALUE_BYTES);
        self.metadata.insert(key, value);
        self
    }
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightMeta {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AvailabilityProbe {
    Available,
    Unavailable { code: String, transient: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaCompileError {
    pub code: String,
    pub message: String,
}

impl SchemaCompileError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SchemaCompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SchemaCompileError {}

pub fn compile_canonical_schema(mut schema: Value) -> Value {
    close_object_shapes(&mut schema);
    schema
}

fn close_object_shapes(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                close_object_shapes(value);
            }
        }
        Value::Object(object) => {
            let object_shape = has_object_semantics(object);
            if object_shape && !object.contains_key("additionalProperties") {
                object.insert("additionalProperties".into(), Value::Bool(false));
            }
            for value in object.values_mut() {
                close_object_shapes(value);
            }
        }
        _ => {}
    }
}

fn type_includes(value: &Option<&Value>, expected: &str) -> bool {
    match value {
        Some(Value::String(kind)) => kind == expected,
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind.as_str() == Some(expected)),
        _ => false,
    }
}

pub fn compile_openai_strict_schema(canonical: &Value) -> Result<Value, SchemaCompileError> {
    let mut wire = compile_canonical_schema(canonical.clone());
    if !wire
        .as_object()
        .is_some_and(|root| root.get("type").and_then(Value::as_str) == Some("object"))
    {
        return Err(SchemaCompileError::new(
            "strict_root_not_object",
            "Strict tool schemas require an object root",
        ));
    }
    compile_strict_node(&mut wire)?;
    Ok(wire)
}

fn compile_strict_node(value: &mut Value) -> Result<(), SchemaCompileError> {
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };

    reject_unsupported_keywords(object)?;

    if let Some(one_of) = object.remove("oneOf") {
        let Value::Array(branches) = one_of else {
            return Err(SchemaCompileError::new(
                "invalid_tool_schema",
                "Tool schema is invalid",
            ));
        };
        if !one_of_is_provably_disjoint(&branches) {
            return Err(SchemaCompileError::new(
                "non_disjoint_one_of",
                "Strict schema union branches are not provably disjoint",
            ));
        }
        object.insert("anyOf".into(), Value::Array(branches));
    }

    if is_object_schema(object) {
        match object.get("additionalProperties") {
            Some(Value::Bool(false)) => {}
            _ => {
                return Err(SchemaCompileError::new(
                    "unsupported_open_object_schema",
                    "Strict tool schemas require closed object shapes",
                ))
            }
        }

        let canonical_required = required_names(object)?;
        let Some(Value::Object(properties)) = object.get_mut("properties") else {
            object.insert("required".into(), Value::Array(Vec::new()));
            return compile_non_property_children(object);
        };
        let property_names = properties.keys().cloned().collect::<Vec<_>>();
        for name in &property_names {
            let property = properties
                .get_mut(name)
                .expect("property key came from map");
            let optional = !canonical_required.contains(name);
            if optional && schema_accepts_null(property) {
                return Err(SchemaCompileError::new(
                    "ambiguous_optional_null",
                    "Strict schema cannot distinguish an omitted optional from a null value",
                ));
            }
            compile_strict_node(property)?;
            if optional {
                let original = std::mem::take(property);
                *property = json!({
                    "anyOf": [original, {"type": "null"}]
                });
            }
        }
        let mut required = property_names;
        required.sort();
        object.insert(
            "required".into(),
            Value::Array(required.into_iter().map(Value::String).collect()),
        );
    }

    compile_non_property_children(object)
}

fn compile_non_property_children(
    object: &mut Map<String, Value>,
) -> Result<(), SchemaCompileError> {
    for key in ["items"] {
        if let Some(child) = object.get_mut(key) {
            compile_strict_node(child)?;
        }
    }
    for key in ["anyOf"] {
        if let Some(Value::Array(children)) = object.get_mut(key) {
            for child in children {
                compile_strict_node(child)?;
            }
        }
    }
    if let Some(Value::Object(definitions)) = object.get_mut("$defs") {
        for definition in definitions.values_mut() {
            compile_strict_node(definition)?;
        }
    }
    Ok(())
}

fn required_names(object: &Map<String, Value>) -> Result<BTreeSet<String>, SchemaCompileError> {
    let Some(required) = object.get("required") else {
        return Ok(BTreeSet::new());
    };
    let Some(required) = required.as_array() else {
        return Err(SchemaCompileError::new(
            "invalid_tool_schema",
            "Tool schema is invalid",
        ));
    };
    required
        .iter()
        .map(|name| {
            name.as_str().map(str::to_owned).ok_or_else(|| {
                SchemaCompileError::new("invalid_tool_schema", "Tool schema is invalid")
            })
        })
        .collect()
}

fn is_object_schema(object: &Map<String, Value>) -> bool {
    has_object_semantics(object)
}

fn has_object_semantics(object: &Map<String, Value>) -> bool {
    const OBJECT_KEYWORDS: &[&str] = &[
        "additionalProperties",
        "dependentRequired",
        "dependentSchemas",
        "maxProperties",
        "minProperties",
        "patternProperties",
        "properties",
        "propertyNames",
        "required",
        "unevaluatedProperties",
    ];
    type_includes(&object.get("type"), "object")
        || OBJECT_KEYWORDS
            .iter()
            .any(|keyword| object.contains_key(*keyword))
}

fn schema_accepts_null(schema: &Value) -> bool {
    let Some(object) = schema.as_object() else {
        return schema.is_null();
    };
    if object.contains_key("type") {
        return type_includes(&object.get("type"), "null");
    }
    if let Some(constant) = object.get("const") {
        return constant.is_null();
    }
    if let Some(values) = object.get("enum").and_then(Value::as_array) {
        return values.iter().any(Value::is_null);
    }
    let union_accepts_null = ["anyOf", "oneOf"]
        .iter()
        .filter_map(|key| object.get(*key).and_then(Value::as_array))
        .flatten()
        .any(schema_accepts_null);
    union_accepts_null || !object.contains_key("anyOf") && !object.contains_key("oneOf")
}

fn one_of_is_provably_disjoint(branches: &[Value]) -> bool {
    if branches.len() < 2 {
        return false;
    }
    let Some(first) = branches.first().and_then(Value::as_object) else {
        return false;
    };
    let Some(first_properties) = first.get("properties").and_then(Value::as_object) else {
        return false;
    };

    first_properties.keys().any(|candidate| {
        let mut seen = Vec::with_capacity(branches.len());
        for branch in branches {
            let Some(branch) = branch.as_object() else {
                return false;
            };
            let required = branch
                .get("required")
                .and_then(Value::as_array)
                .is_some_and(|required| {
                    required
                        .iter()
                        .any(|name| name.as_str() == Some(candidate.as_str()))
                });
            if !required {
                return false;
            }
            let Some(discriminator) = branch
                .get("properties")
                .and_then(Value::as_object)
                .and_then(|properties| properties.get(candidate))
                .and_then(singleton_schema_value)
            else {
                return false;
            };
            if seen.contains(&discriminator) {
                return false;
            }
            seen.push(discriminator);
        }
        true
    })
}

fn singleton_schema_value(schema: &Value) -> Option<Value> {
    let object = schema.as_object()?;
    if let Some(value) = object.get("const") {
        return Some(value.clone());
    }
    let values = object.get("enum")?.as_array()?;
    (values.len() == 1).then(|| values[0].clone())
}

fn reject_unsupported_keywords(object: &Map<String, Value>) -> Result<(), SchemaCompileError> {
    const ALLOWED: &[&str] = &[
        "$defs",
        "$ref",
        "additionalProperties",
        "anyOf",
        "const",
        "description",
        "enum",
        "exclusiveMaximum",
        "exclusiveMinimum",
        "format",
        "items",
        "maxItems",
        "maximum",
        "minItems",
        "minimum",
        "multipleOf",
        "oneOf",
        "pattern",
        "properties",
        "required",
        "title",
        "type",
    ];
    if object.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
        return Err(SchemaCompileError::new(
            "unsupported_strict_schema",
            "Tool schema uses a construct unsupported by strict mode",
        ));
    }
    Ok(())
}

pub(crate) fn explicit_open_object_schema(schema: &Value) -> bool {
    match schema {
        Value::Array(values) => values.iter().any(explicit_open_object_schema),
        Value::Object(object) => {
            let explicitly_open = is_object_schema(object)
                && object
                    .get("additionalProperties")
                    .is_some_and(|value| value != &Value::Bool(false));
            explicitly_open || object.values().any(explicit_open_object_schema)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compile_canonical_schema, compile_openai_strict_schema, FacadePriority, ResourceScopeHint,
        ToolDescriptor, ToolEffect, ToolErrorCategory,
    };
    use serde_json::json;

    #[test]
    fn canonical_schema_closes_every_object_shape() {
        let schema = compile_canonical_schema(json!({
            "type": "object",
            "properties": {
                "batch": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {"text": {"type": "string"}}
                    }
                }
            }
        }));

        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["properties"]["batch"]["items"]["additionalProperties"],
            false
        );
    }

    #[test]
    fn openai_strict_wire_schema_requires_nullable_canonical_optionals() {
        let canonical = json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "offset": {"type": "integer"}
            },
            "required": ["path"]
        });

        let wire = compile_openai_strict_schema(&canonical).unwrap();

        assert_eq!(wire["required"], json!(["offset", "path"]));
        assert_eq!(wire["additionalProperties"], false);
        assert_eq!(
            wire["properties"]["offset"]["anyOf"][1],
            json!({"type": "null"})
        );
    }

    #[test]
    fn canonical_schema_preserves_explicit_open_object_semantics() {
        let open = compile_canonical_schema(json!({
            "type": "object",
            "additionalProperties": true
        }));
        let schema_valued = compile_canonical_schema(json!({
            "type": "object",
            "additionalProperties": {"type": "string"}
        }));

        assert_eq!(open["additionalProperties"], true);
        assert_eq!(
            schema_valued["additionalProperties"],
            json!({"type": "string"})
        );
    }

    #[test]
    fn canonical_schema_closes_object_keywords_without_an_explicit_type() {
        let schema = compile_canonical_schema(json!({
            "required": ["value"]
        }));

        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn strict_schema_rejects_ambiguous_optional_null() {
        let error = compile_openai_strict_schema(&json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "secret_optional_marker": {"type": ["string", "null"]}
            }
        }))
        .unwrap_err();

        assert_eq!(error.code, "ambiguous_optional_null");
        assert!(!error.message.contains("secret_optional_marker"));
    }

    #[test]
    fn strict_schema_rejects_unconstrained_optional_as_ambiguous_null() {
        let error = compile_openai_strict_schema(&json!({
            "type": "object",
            "properties": {"optional": {}}
        }))
        .unwrap_err();

        assert_eq!(error.code, "ambiguous_optional_null");
    }

    #[test]
    fn strict_schema_rejects_a_nullable_object_root() {
        let error = compile_openai_strict_schema(&json!({
            "type": ["object", "null"],
            "properties": {}
        }))
        .unwrap_err();

        assert_eq!(error.code, "strict_root_not_object");
    }

    #[test]
    fn strict_schema_rejects_non_disjoint_one_of() {
        let error = compile_openai_strict_schema(&json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "value": {
                    "oneOf": [
                        {"type": "string"},
                        {"minLength": 1}
                    ]
                }
            },
            "required": ["value"]
        }))
        .unwrap_err();

        assert_eq!(error.code, "non_disjoint_one_of");
    }

    #[test]
    fn strict_schema_translates_provably_disjoint_one_of() {
        let wire = compile_openai_strict_schema(&json!({
            "type": "object",
            "properties": {
                "operation": {
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": {"action": {"enum": ["read"]}},
                            "required": ["action"]
                        },
                        {
                            "type": "object",
                            "properties": {"action": {"enum": ["write"]}},
                            "required": ["action"]
                        }
                    ]
                }
            },
            "required": ["operation"]
        }))
        .unwrap();

        assert!(wire["properties"]["operation"].get("oneOf").is_none());
        assert_eq!(
            wire["properties"]["operation"]["anyOf"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn descriptor_and_error_categories_have_stable_serialization() {
        let descriptor = ToolDescriptor {
            canonical_name: "read".into(),
            description: "Read a file".into(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
            kind: "read".into(),
            effect: ToolEffect::ReadOnly,
            idempotent: true,
            interactive: false,
            sequential_barrier: false,
            resource_scope: ResourceScopeHint::Workspace,
            result_limit_bytes: 50_000,
            facade_priority: FacadePriority::Normal,
            policy_aliases: vec![],
            v2_only: false,
            v1_only: false,
            allow_lossless_coercions: false,
        };

        let serialized = serde_json::to_value(&descriptor).unwrap();
        assert_eq!(serialized["effect"], "read_only");
        assert_eq!(serialized["resource_scope"], "workspace");
        assert_eq!(serialized["facade_priority"], "normal");

        let categories = [
            ToolErrorCategory::Caller,
            ToolErrorCategory::Precondition,
            ToolErrorCategory::Conflict,
            ToolErrorCategory::Permission,
            ToolErrorCategory::Transient,
            ToolErrorCategory::Timeout,
            ToolErrorCategory::Cancelled,
            ToolErrorCategory::Internal,
        ];
        assert_eq!(
            serde_json::to_value(categories).unwrap(),
            json!([
                "caller",
                "precondition",
                "conflict",
                "permission",
                "transient",
                "timeout",
                "cancelled",
                "internal"
            ])
        );
    }
}
