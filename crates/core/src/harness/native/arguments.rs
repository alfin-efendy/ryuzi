use super::tool_contract::{PreflightMeta, ToolError, ToolFieldError, ToolInputCtx, ToolMetadata};
use super::tool_plan::PlannedTool;
use super::tools::Tool;
use serde_json::Value;
use std::sync::Arc;

pub const MAX_RAW_ARGUMENT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentRepairKind {
    TrailingComma,
    MissingClosingDelimiter,
    TrailingCommaAndMissingClosingDelimiter,
}

impl ArgumentRepairKind {
    pub const fn metric_label(self) -> &'static str {
        match self {
            Self::TrailingComma => "trailing_comma",
            Self::MissingClosingDelimiter => "missing_closing_delimiter",
            Self::TrailingCommaAndMissingClosingDelimiter => {
                "trailing_comma_and_missing_closing_delimiter"
            }
        }
    }
}

pub struct DecodedArguments {
    pub input: Value,
    pub repair: Option<ArgumentRepairKind>,
}

enum WireArguments {
    Start(Value),
    Streamed { raw: String, overflowed: bool },
}

pub struct WireToolCall {
    pub id: String,
    pub name: String,
    arguments: Option<WireArguments>,
}

impl WireToolCall {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        start_input: Value,
        streamed_json: &str,
        overflowed: bool,
    ) -> Self {
        let arguments = if streamed_json.is_empty() && !overflowed {
            WireArguments::Start(start_input)
        } else {
            let exceeds_cap = overflowed || streamed_json.len() > MAX_RAW_ARGUMENT_BYTES;
            WireArguments::Streamed {
                raw: if exceeds_cap {
                    String::new()
                } else {
                    streamed_json.to_owned()
                },
                overflowed: exceeds_cap,
            }
        };
        Self {
            id: id.into(),
            name: name.into(),
            arguments: Some(arguments),
        }
    }

    pub fn from_owned(
        id: String,
        name: String,
        start_input: Value,
        streamed_json: String,
        overflowed: bool,
    ) -> Self {
        if overflowed || streamed_json.len() > MAX_RAW_ARGUMENT_BYTES {
            return Self {
                id,
                name,
                arguments: Some(WireArguments::Streamed {
                    raw: String::new(),
                    overflowed: true,
                }),
            };
        }
        let arguments = if streamed_json.is_empty() {
            WireArguments::Start(start_input)
        } else {
            WireArguments::Streamed {
                raw: streamed_json,
                overflowed: false,
            }
        };
        Self {
            id,
            name,
            arguments: Some(arguments),
        }
    }

    pub fn discard_arguments(&mut self) {
        self.arguments = None;
    }

    fn decode(&mut self) -> Result<DecodedArguments, ToolError> {
        match self.arguments.take() {
            Some(WireArguments::Start(input)) => {
                if match serde_json::to_vec(&input) {
                    Ok(serialized) => serialized.len() > MAX_RAW_ARGUMENT_BYTES,
                    Err(_) => true,
                } {
                    return Err(invalid_arguments());
                }
                object_arguments(input, None)
            }
            Some(WireArguments::Streamed { raw, overflowed }) => {
                decode_wire_arguments(&raw, overflowed)
            }
            None => Err(ToolError::precondition(
                "invalid_persisted_tool_plan",
                "Tool arguments were already consumed",
            )),
        }
    }
}

impl std::fmt::Debug for WireToolCall {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WireToolCall")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("arguments", &"<redacted>")
            .finish()
    }
}

pub struct ValidatedToolCall {
    pub wire: WireToolCall,
    pub canonical_name: String,
    pub tool: Arc<dyn Tool>,
    pub input: Value,
    pub repair: Option<ArgumentRepairKind>,
    pub normalization: ToolMetadata,
}

impl std::fmt::Debug for ValidatedToolCall {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ValidatedToolCall")
            .field("wire", &self.wire)
            .field("canonical_name", &self.canonical_name)
            .field("input", &"<redacted>")
            .field("repair", &self.repair)
            .field("normalization", &self.normalization)
            .finish_non_exhaustive()
    }
}

pub struct PreparedToolCall {
    pub validated: ValidatedToolCall,
    pub preflight: PreflightMeta,
}

pub struct RejectedToolCall {
    pub wire: WireToolCall,
    pub canonical_name: Option<String>,
    pub error: Box<ToolError>,
}

impl std::fmt::Debug for RejectedToolCall {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RejectedToolCall")
            .field("wire", &self.wire)
            .field("canonical_name", &self.canonical_name)
            .field("error", &self.error)
            .finish()
    }
}

pub struct ArgumentGateway;

impl ArgumentGateway {
    pub fn validate(
        mut wire: WireToolCall,
        planned: &PlannedTool,
        tool: Arc<dyn Tool>,
        context: &ToolInputCtx<'_>,
    ) -> Result<ValidatedToolCall, RejectedToolCall> {
        let result = Self::validate_inner(&mut wire, planned, tool.clone(), context);
        match result {
            Ok((input, repair, normalization)) => Ok(ValidatedToolCall {
                wire,
                canonical_name: planned.canonical_name.clone(),
                tool,
                input,
                repair,
                normalization,
            }),
            Err(error) => Err(RejectedToolCall {
                wire,
                canonical_name: Some(planned.canonical_name.clone()),
                error: Box::new(error),
            }),
        }
    }

    fn validate_inner(
        wire: &mut WireToolCall,
        planned: &PlannedTool,
        tool: Arc<dyn Tool>,
        context: &ToolInputCtx<'_>,
    ) -> Result<(Value, Option<ArgumentRepairKind>, ToolMetadata), ToolError> {
        let decoded = wire.decode()?;
        validate_schema(&planned.wire_schema, &decoded.input)?;

        let mut canonical_input = decoded.input;
        if planned.strict {
            remove_optional_null_sentinels(
                &mut canonical_input,
                &planned.canonical_schema,
                &planned.canonical_schema,
            )?;
        }
        validate_schema(&planned.canonical_schema, &canonical_input)?;
        let normalized = tool.normalize_input(context, canonical_input)?;
        validate_schema(&planned.canonical_schema, &normalized.value)?;
        let normalization = normalized.metadata().clone();
        Ok((normalized.value, decoded.repair, normalization))
    }
}

impl std::fmt::Debug for DecodedArguments {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DecodedArguments")
            .field("input", &"<redacted>")
            .field("repair", &self.repair)
            .finish()
    }
}

pub fn decode_wire_arguments(raw: &str, overflowed: bool) -> Result<DecodedArguments, ToolError> {
    if overflowed || raw.len() > MAX_RAW_ARGUMENT_BYTES {
        return Err(invalid_arguments());
    }
    let trimmed = raw.trim();
    if let Ok(input) = serde_json::from_str::<Value>(trimmed) {
        return object_arguments(input, None);
    }

    let (repaired, repair) = repair_delimiters(trimmed).ok_or_else(invalid_arguments)?;
    let input = serde_json::from_str::<Value>(&repaired).map_err(|_| invalid_arguments())?;
    object_arguments(input, Some(repair))
}

fn object_arguments(
    input: Value,
    repair: Option<ArgumentRepairKind>,
) -> Result<DecodedArguments, ToolError> {
    if !input.is_object() {
        return Err(invalid_arguments());
    }
    Ok(DecodedArguments { input, repair })
}

fn repair_delimiters(raw: &str) -> Option<(String, ArgumentRepairKind)> {
    let mut repaired = String::with_capacity(raw.len().saturating_add(16));
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut removed_comma = false;

    let chars = raw.char_indices().collect::<Vec<_>>();
    for (position, (_, character)) in chars.iter().copied().enumerate() {
        if in_string {
            repaired.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }

        match character {
            '"' => {
                in_string = true;
                repaired.push(character);
            }
            '{' => {
                stack.push('}');
                repaired.push(character);
            }
            '[' => {
                stack.push(']');
                repaired.push(character);
            }
            '}' | ']' => {
                if stack.pop() != Some(character) {
                    return None;
                }
                repaired.push(character);
            }
            ',' => {
                let next = chars[position + 1..]
                    .iter()
                    .map(|(_, next)| *next)
                    .find(|next| !next.is_whitespace());
                if matches!(next, Some('}' | ']')) {
                    removed_comma = true;
                } else {
                    repaired.push(character);
                }
            }
            _ => repaired.push(character),
        }
    }

    if in_string || escaped {
        return None;
    }
    let appended_closer = !stack.is_empty();
    while let Some(delimiter) = stack.pop() {
        repaired.push(delimiter);
    }
    let repair = match (removed_comma, appended_closer) {
        (true, true) => ArgumentRepairKind::TrailingCommaAndMissingClosingDelimiter,
        (true, false) => ArgumentRepairKind::TrailingComma,
        (false, true) => ArgumentRepairKind::MissingClosingDelimiter,
        (false, false) => return None,
    };
    Some((repaired, repair))
}

fn invalid_arguments() -> ToolError {
    ToolError::caller("invalid_arguments", "Tool arguments are invalid")
}

fn validate_schema(schema: &Value, input: &Value) -> Result<(), ToolError> {
    let validator = jsonschema::validator_for(schema).map_err(|_| {
        ToolError::precondition(
            "invalid_persisted_tool_plan",
            "The frozen tool schema is invalid",
        )
    })?;
    let mut validation_error = invalid_arguments();
    for error in validator.iter_errors(input) {
        if validation_error.field_errors.len() >= super::tool_contract::MAX_TOOL_ERROR_FIELD_ERRORS
        {
            break;
        }
        let path = error.instance_path().to_string();
        match error.kind() {
            jsonschema::error::ValidationErrorKind::Required { property } => {
                let property = property.as_str().unwrap_or("field");
                validation_error = validation_error.with_field_error(ToolFieldError::new(
                    child_pointer(&path, property),
                    "missing_required",
                    "Missing required field",
                ));
            }
            jsonschema::error::ValidationErrorKind::AdditionalProperties { unexpected }
            | jsonschema::error::ValidationErrorKind::UnevaluatedProperties { unexpected } => {
                for _ in unexpected {
                    validation_error = validation_error.with_field_error(ToolFieldError::new(
                        child_pointer(&path, "field"),
                        "unexpected_field",
                        "Unexpected field",
                    ));
                }
            }
            kind => {
                let (code, message) = fixed_validation_error(kind);
                validation_error = validation_error.with_field_error(ToolFieldError::new(
                    pointer_or_root(&path),
                    code,
                    message,
                ));
            }
        }
    }
    if validation_error.field_errors.is_empty() {
        Ok(())
    } else {
        Err(validation_error)
    }
}

fn fixed_validation_error(
    kind: &jsonschema::error::ValidationErrorKind,
) -> (&'static str, &'static str) {
    use jsonschema::error::ValidationErrorKind as Kind;
    match kind {
        Kind::Type { .. } => ("wrong_type", "Wrong type"),
        Kind::Enum { .. } | Kind::Constant { .. } => ("disallowed_option", "Disallowed option"),
        Kind::MinItems { .. } => ("array_too_short", "Array too short"),
        Kind::MaxItems { .. } | Kind::AdditionalItems { .. } => {
            ("array_too_long", "Array too long")
        }
        Kind::AnyOf { .. } | Kind::OneOfNotValid { .. } => {
            ("no_matching_union", "Schema constraint failed")
        }
        Kind::OneOfMultipleValid { .. } => ("ambiguous_union", "Ambiguous union"),
        _ => ("schema_constraint", "Schema constraint failed"),
    }
}

fn pointer_or_root(path: &str) -> String {
    if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    }
}

fn child_pointer(path: &str, child: &str) -> String {
    let escaped = child.replace('~', "~0").replace('/', "~1");
    if path.is_empty() {
        format!("/{escaped}")
    } else {
        format!("{path}/{escaped}")
    }
}

fn remove_optional_null_sentinels(
    input: &mut Value,
    schema: &Value,
    root_schema: &Value,
) -> Result<(), ToolError> {
    let schema = resolve_local_ref(schema, root_schema)?;
    let Some(object_schema) = schema.as_object() else {
        return Ok(());
    };

    if let Some(branches) = object_schema
        .get("anyOf")
        .or_else(|| object_schema.get("oneOf"))
        .and_then(Value::as_array)
    {
        let mut matching = Vec::new();
        for branch in branches {
            let mut candidate = input.clone();
            match remove_optional_null_sentinels(&mut candidate, branch, root_schema) {
                Ok(()) if branch_accepts_candidate(branch, root_schema, &candidate)? => {
                    matching.push(candidate);
                }
                Ok(()) => {}
                Err(error) if error.code == "invalid_arguments" => {}
                Err(error) => return Err(error),
            }
        }
        return match matching.as_slice() {
            [candidate] => {
                *input = candidate.clone();
                Ok(())
            }
            [] => Ok(()),
            _ => Err(invalid_arguments().with_field_error(ToolFieldError::new(
                "/",
                "ambiguous_union",
                "Ambiguous union",
            ))),
        };
    }

    if let (Some(input_object), Some(properties)) = (
        input.as_object_mut(),
        object_schema.get("properties").and_then(Value::as_object),
    ) {
        let required = object_schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        let names = input_object.keys().cloned().collect::<Vec<_>>();
        for name in names {
            let Some(property_schema) = properties.get(&name) else {
                continue;
            };
            if input_object.get(&name).is_some_and(Value::is_null)
                && !required.contains(name.as_str())
            {
                input_object.remove(&name);
            } else if let Some(value) = input_object.get_mut(&name) {
                remove_optional_null_sentinels(value, property_schema, root_schema)?;
            }
        }
    } else if let (Some(items), Some(item_schema)) =
        (input.as_array_mut(), object_schema.get("items"))
    {
        for item in items {
            remove_optional_null_sentinels(item, item_schema, root_schema)?;
        }
    }
    Ok(())
}

fn branch_accepts_candidate(
    branch: &Value,
    root_schema: &Value,
    candidate: &Value,
) -> Result<bool, ToolError> {
    let branch = resolve_local_ref(branch, root_schema)?;
    let mut rooted_branch = branch.clone();
    if let (Some(branch_object), Some(root_object)) =
        (rooted_branch.as_object_mut(), root_schema.as_object())
    {
        for keyword in ["$schema", "$defs", "definitions"] {
            if !branch_object.contains_key(keyword) {
                if let Some(value) = root_object.get(keyword) {
                    branch_object.insert(keyword.to_string(), value.clone());
                }
            }
        }
    }
    let validator = jsonschema::validator_for(&rooted_branch).map_err(|_| {
        ToolError::precondition(
            "invalid_persisted_tool_plan",
            "The frozen tool schema is invalid",
        )
    })?;
    Ok(validator.is_valid(candidate))
}

const MAX_LOCAL_REF_DEPTH: usize = 64;

fn resolve_local_ref<'a>(schema: &'a Value, root: &'a Value) -> Result<&'a Value, ToolError> {
    let mut current = schema;
    let mut visited_pointers = std::collections::BTreeSet::new();
    for _ in 0..MAX_LOCAL_REF_DEPTH {
        let Some(reference) = current
            .as_object()
            .and_then(|object| object.get("$ref"))
            .and_then(Value::as_str)
        else {
            return Ok(current);
        };
        let Some(pointer) = reference.strip_prefix('#') else {
            return Err(ToolError::precondition(
                "invalid_persisted_tool_plan",
                "The frozen tool schema contains an unsupported reference",
            ));
        };
        if !visited_pointers.insert(pointer) {
            return Err(ToolError::precondition(
                "invalid_persisted_tool_plan",
                "The frozen tool schema contains a reference cycle",
            ));
        }
        current = root.pointer(pointer).ok_or_else(|| {
            ToolError::precondition(
                "invalid_persisted_tool_plan",
                "The frozen tool schema contains an invalid reference",
            )
        })?;
    }
    Err(ToolError::precondition(
        "invalid_persisted_tool_plan",
        "The frozen tool schema reference chain is too deep",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::native::tool_contract::{
        compile_canonical_schema, compile_openai_strict_schema, NormalizedInput, ToolDescriptor,
        ToolErrorCategory, ToolInputCtx,
    };
    use crate::harness::native::tool_plan::PlannedTool;
    use crate::harness::native::tools::read::Read;
    use crate::harness::native::tools::{PermissionSpec, Tool, ToolCtx, ToolOutput};
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Arc;

    struct SchemaTool {
        schema: Value,
        invalidating_normalization: bool,
    }

    #[async_trait]
    impl Tool for SchemaTool {
        fn name(&self) -> &str {
            "schema_test"
        }

        fn description(&self) -> &str {
            "schema test"
        }

        fn input_schema(&self) -> Value {
            self.schema.clone()
        }

        fn kind(&self) -> &'static str {
            "other"
        }

        fn normalize_input(
            &self,
            _ctx: &ToolInputCtx<'_>,
            mut input: Value,
        ) -> Result<NormalizedInput, ToolError> {
            if self.invalidating_normalization {
                input["path"] = json!(42);
                Ok(NormalizedInput::changed(input))
            } else {
                Ok(NormalizedInput::unchanged(input))
            }
        }

        fn permission(&self, _input: &Value) -> PermissionSpec {
            PermissionSpec::new("schema-test", "schema test")
        }

        async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> anyhow::Result<ToolOutput> {
            unreachable!("argument tests never execute handlers")
        }
    }

    fn planned(canonical_schema: Value, strict: bool) -> PlannedTool {
        let wire_schema = if strict {
            compile_openai_strict_schema(&canonical_schema).unwrap()
        } else {
            canonical_schema.clone()
        };
        planned_with_wire(canonical_schema, wire_schema, strict)
    }

    fn planned_with_wire(canonical_schema: Value, wire_schema: Value, strict: bool) -> PlannedTool {
        PlannedTool {
            canonical_name: "schema_test".into(),
            descriptor: ToolDescriptor::conservative(
                "schema_test",
                "schema test",
                canonical_schema.clone(),
                "other",
            ),
            canonical_schema,
            wire_schema,
            strict,
            contract_hash: "frozen".into(),
        }
    }

    #[test]
    fn file_reference_normalization_passes_final_canonical_revalidation() {
        let tool = Arc::new(Read);
        let canonical_schema = compile_canonical_schema(tool.input_schema());
        let planned = planned(canonical_schema, false);
        let validated = validate(
            r#"{"path":"missing.rs:12"}"#,
            json!({"path": "missing.rs:12"}),
            &planned,
            tool,
        )
        .unwrap();

        assert_eq!(validated.input["path"], "missing.rs");
        assert_eq!(validated.input["offset"], 12);
        assert!(!validated.normalization.is_empty());
    }

    fn validate(
        raw: &str,
        start_input: Value,
        planned: &PlannedTool,
        tool: Arc<dyn Tool>,
    ) -> Result<ValidatedToolCall, RejectedToolCall> {
        let dir = tempfile::tempdir().unwrap();
        let extra = Vec::new();
        let context = ToolInputCtx {
            work_dir: dir.path(),
            attachments_dir: None,
            extra_skill_dirs: &extra,
        };
        ArgumentGateway::validate(
            WireToolCall::new("call", "schema_test", start_input, raw, false),
            planned,
            tool,
            &context,
        )
    }

    #[test]
    fn accepted_repairs_report_only_fixed_kinds() {
        let cases = [
            ("  {\"path\":\"safe\"}  ", None),
            (
                "{\"path\":\"safe\",}",
                Some(ArgumentRepairKind::TrailingComma),
            ),
            (
                "{\"items\":[1,2,],}",
                Some(ArgumentRepairKind::TrailingComma),
            ),
            (
                "{\"path\":\"safe\"",
                Some(ArgumentRepairKind::MissingClosingDelimiter),
            ),
        ];

        for (raw, expected_repair) in cases {
            let decoded = decode_wire_arguments(raw, false).unwrap();
            assert!(decoded.input.is_object());
            assert_eq!(decoded.repair, expected_repair, "raw input case failed");
            let rendered = format!("{decoded:?}");
            assert!(!rendered.contains(raw));
            assert!(!rendered.contains("safe"));
        }
    }

    #[test]
    fn ambiguous_broken_and_non_object_payloads_reject_without_raw_text() {
        for (raw, secret) in [
            (r#"{"open":"secret-token"#, Some("secret-token")),
            ("{]", None),
            ("[]", None),
            ("null", None),
            ("path=private-token", Some("private-token")),
        ] {
            let error = decode_wire_arguments(raw, false).unwrap_err();
            assert_eq!(error.code, "invalid_arguments");
            let rendered = format!("{error:?}");
            if let Some(secret) = secret {
                assert!(!rendered.contains(secret));
            }
        }
    }

    #[test]
    fn raw_cap_accepts_exact_boundary_and_rejects_one_byte_more() {
        let prefix = r#"{"pad":""#;
        let suffix = r#""}"#;
        let exact = format!(
            "{prefix}{}{suffix}",
            "x".repeat(MAX_RAW_ARGUMENT_BYTES - prefix.len() - suffix.len())
        );
        assert_eq!(exact.len(), MAX_RAW_ARGUMENT_BYTES);
        assert!(decode_wire_arguments(&exact, false).is_ok());

        let over = format!("{exact}x");
        let error = decode_wire_arguments(&over, false).unwrap_err();
        assert_eq!(error.code, "invalid_arguments");
        assert!(!format!("{error:?}").contains(&over));
        assert!(decode_wire_arguments("{}", true).is_err());
    }

    #[test]
    fn schema_failures_are_sanitized_pointer_errors_capped_at_eight() {
        let mut properties = serde_json::Map::new();
        for index in 0..10 {
            properties.insert(format!("required_{index}"), json!({"type": "string"}));
        }
        properties.insert("path".into(), json!({"type": "string"}));
        properties.insert("mode".into(), json!({"type": "string", "enum": ["safe"]}));
        properties.insert(
            "names".into(),
            json!({"type": "array", "items": {"type": "string"}, "minItems": 1}),
        );
        let required = (0..10)
            .map(|index| Value::String(format!("required_{index}")))
            .chain([json!("path"), json!("mode"), json!("names")])
            .collect::<Vec<_>>();
        let schema = json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        });
        let plan = planned(schema, false);
        let tool: Arc<dyn Tool> = Arc::new(SchemaTool {
            schema: plan.canonical_schema.clone(),
            invalidating_normalization: false,
        });
        let error = validate(
            r#"{"path":1,"mode":"unsafe","names":[],"surprise":"raw-secret"}"#,
            json!({}),
            &plan,
            tool,
        )
        .unwrap_err()
        .error;
        assert_eq!(error.code, "invalid_arguments");
        assert_eq!(error.field_errors.len(), 8);
        let serialized = serde_json::to_string(&error).unwrap();
        assert!(!serialized.contains("raw-secret"));
        assert!(!serialized.contains("unsafe"));
        assert!(error
            .field_errors
            .iter()
            .all(|field| field.field.starts_with('/')));
        let codes = error
            .field_errors
            .iter()
            .map(|field| field.code.as_str())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"missing_required"));

        let focused_schema = json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "mode": {"type": "string", "enum": ["safe"]},
                "names": {"type": "array", "items": {"type": "string"}, "minItems": 1}
            },
            "required": ["path", "mode", "names"],
            "additionalProperties": false
        });
        let focused_plan = planned(focused_schema.clone(), false);
        let focused_tool: Arc<dyn Tool> = Arc::new(SchemaTool {
            schema: focused_schema,
            invalidating_normalization: false,
        });
        let cases = [
            (r#"{"mode":"safe","names":["x"]}"#, "missing_required"),
            (r#"{"path":1,"mode":"safe","names":["x"]}"#, "wrong_type"),
            (
                r#"{"path":"p","mode":"unsafe","names":["x"]}"#,
                "disallowed_option",
            ),
            (
                r#"{"path":"p","mode":"safe","names":[]}"#,
                "array_too_short",
            ),
            (
                r#"{"path":"p","mode":"safe","names":["x"],"extra":1}"#,
                "unexpected_field",
            ),
        ];
        for (raw, expected_code) in cases {
            let error = validate(raw, json!({}), &focused_plan, focused_tool.clone())
                .unwrap_err()
                .error;
            assert!(
                error
                    .field_errors
                    .iter()
                    .any(|field| field.code == expected_code),
                "missing sanitized code {expected_code} for {raw}"
            );
        }
    }

    #[test]
    fn strict_optional_nulls_are_removed_recursively() {
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "offset": {"type": "integer"},
                "nested": {
                    "type": "object",
                    "properties": {"note": {"type": "string"}},
                    "additionalProperties": false
                },
                "rows": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {"tag": {"type": "string"}},
                        "additionalProperties": false
                    }
                }
            },
            "required": ["path", "nested", "rows"],
            "additionalProperties": false
        });
        let plan = planned(schema.clone(), true);
        let tool: Arc<dyn Tool> = Arc::new(SchemaTool {
            schema,
            invalidating_normalization: false,
        });
        let valid = validate(
            r#"{"path":"p","offset":null,"nested":{"note":null},"rows":[{"tag":null}]}"#,
            json!({}),
            &plan,
            tool.clone(),
        )
        .unwrap();
        assert_eq!(valid.input, json!({"path":"p","nested":{},"rows":[{}]}));

        for raw in [
            r#"{"path":null,"offset":null,"nested":{"note":null},"rows":[]}"#,
            r#"{"path":"p","offset":null,"nested":{"unknown":null},"rows":[]}"#,
        ] {
            assert_eq!(
                validate(raw, json!({}), &plan, tool.clone())
                    .unwrap_err()
                    .error
                    .code,
                "invalid_arguments"
            );
        }
    }

    #[test]
    fn strict_ambiguous_union_rejects_instead_of_guessing() {
        let schema = json!({
            "type": "object",
            "properties": {
                "choice": {"anyOf": [
                    {"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false},
                    {"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false}
                ]}
            },
            "required": ["choice"],
            "additionalProperties": false
        });
        let plan = planned(schema.clone(), true);
        let tool: Arc<dyn Tool> = Arc::new(SchemaTool {
            schema,
            invalidating_normalization: false,
        });
        assert_eq!(
            validate(r#"{"choice":{"value":"x"}}"#, json!({}), &plan, tool,)
                .unwrap_err()
                .error
                .code,
            "invalid_arguments"
        );
    }

    #[test]
    fn strict_discriminator_union_strips_optional_null_in_the_selected_branch() {
        let schema = json!({
            "type": "object",
            "properties": {
                "choice": {"oneOf": [
                    {
                        "type": "object",
                        "properties": {
                            "kind": {"const": "a"},
                            "note": {"type": "string"}
                        },
                        "required": ["kind"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "properties": {
                            "kind": {"const": "b"},
                            "count": {"type": "integer"}
                        },
                        "required": ["kind"],
                        "additionalProperties": false
                    }
                ]}
            },
            "required": ["choice"],
            "additionalProperties": false
        });
        let plan = planned(schema.clone(), true);
        let tool: Arc<dyn Tool> = Arc::new(SchemaTool {
            schema,
            invalidating_normalization: false,
        });

        let valid = validate(
            r#"{"choice":{"kind":"a","note":null}}"#,
            json!({}),
            &plan,
            tool,
        )
        .unwrap();

        assert_eq!(valid.input, json!({"choice":{"kind":"a"}}));
    }

    #[test]
    fn strict_union_branch_local_refs_resolve_against_the_frozen_root() {
        let schema = json!({
            "$defs": {
                "branch_a": {
                    "type": "object",
                    "properties": {
                        "kind": {"const": "a"},
                        "note": {"type": "string"}
                    },
                    "required": ["kind"],
                    "additionalProperties": false
                },
                "branch_b": {
                    "type": "object",
                    "properties": {
                        "kind": {"const": "b"},
                        "count": {"type": "integer"}
                    },
                    "required": ["kind"],
                    "additionalProperties": false
                }
            },
            "type": "object",
            "properties": {
                "choice": {"oneOf": [
                    {"$ref": "#/$defs/branch_a"},
                    {"$ref": "#/$defs/branch_b"}
                ]}
            },
            "required": ["choice"],
            "additionalProperties": false
        });
        let wire_schema = json!({
            "$defs": {
                "branch_a": {
                    "type": "object",
                    "properties": {
                        "kind": {"enum": ["a"]},
                        "note": {"anyOf": [{"type": "string"}, {"type": "null"}]}
                    },
                    "required": ["kind", "note"],
                    "additionalProperties": false
                },
                "branch_b": {
                    "type": "object",
                    "properties": {
                        "kind": {"enum": ["b"]},
                        "count": {"anyOf": [{"type": "integer"}, {"type": "null"}]}
                    },
                    "required": ["kind", "count"],
                    "additionalProperties": false
                }
            },
            "type": "object",
            "properties": {
                "choice": {"anyOf": [
                    {"$ref": "#/$defs/branch_a"},
                    {"$ref": "#/$defs/branch_b"}
                ]}
            },
            "required": ["choice"],
            "additionalProperties": false
        });
        let plan = planned_with_wire(schema.clone(), wire_schema, true);
        let tool: Arc<dyn Tool> = Arc::new(SchemaTool {
            schema,
            invalidating_normalization: false,
        });

        let valid = validate(
            r#"{"choice":{"kind":"a","note":null}}"#,
            json!({}),
            &plan,
            tool,
        )
        .unwrap();

        assert_eq!(valid.input, json!({"choice":{"kind":"a"}}));
    }

    #[test]
    fn strict_chained_local_refs_remove_optional_null_sentinels() {
        let schema = json!({
            "$defs": {
                "choice_alias": {"$ref": "#/$defs/choice_shape"},
                "choice_shape": {
                    "type": "object",
                    "properties": {
                        "kind": {"const": "a"},
                        "note": {"type": "string"}
                    },
                    "required": ["kind"],
                    "additionalProperties": false
                }
            },
            "type": "object",
            "properties": {
                "choice": {"$ref": "#/$defs/choice_alias"}
            },
            "required": ["choice"],
            "additionalProperties": false
        });
        let wire_schema = json!({
            "$defs": {
                "choice_alias": {"$ref": "#/$defs/choice_shape"},
                "choice_shape": {
                    "type": "object",
                    "properties": {
                        "kind": {"enum": ["a"]},
                        "note": {"anyOf": [{"type": "string"}, {"type": "null"}]}
                    },
                    "required": ["kind", "note"],
                    "additionalProperties": false
                }
            },
            "type": "object",
            "properties": {
                "choice": {"$ref": "#/$defs/choice_alias"}
            },
            "required": ["choice"],
            "additionalProperties": false
        });
        let plan = planned_with_wire(schema.clone(), wire_schema, true);
        let tool: Arc<dyn Tool> = Arc::new(SchemaTool {
            schema,
            invalidating_normalization: false,
        });

        let valid = validate(
            r#"{"choice":{"kind":"a","note":null}}"#,
            json!({}),
            &plan,
            tool,
        )
        .unwrap();

        assert_eq!(valid.input, json!({"choice":{"kind":"a"}}));
    }

    #[test]
    fn cyclic_local_refs_are_rejected_deterministically() {
        let schema = json!({
            "$defs": {
                "first": {"$ref": "#/$defs/second"},
                "second": {"$ref": "#/$defs/first"}
            }
        });

        let error = resolve_local_ref(&schema["$defs"]["first"], &schema).unwrap_err();

        assert_eq!(error.category, ToolErrorCategory::Precondition);
        assert_eq!(error.code, "invalid_persisted_tool_plan");
    }

    #[test]
    fn zero_match_any_of_is_not_reported_as_ambiguous() {
        let schema = json!({
            "type": "object",
            "properties": {
                "value": {"anyOf": [
                    {"type": "string"},
                    {"type": "integer"}
                ]}
            },
            "required": ["value"],
            "additionalProperties": false
        });
        let plan = planned(schema.clone(), false);
        let tool: Arc<dyn Tool> = Arc::new(SchemaTool {
            schema,
            invalidating_normalization: false,
        });

        let error = validate(r#"{"value":true}"#, json!({}), &plan, tool)
            .unwrap_err()
            .error;

        assert!(error
            .field_errors
            .iter()
            .any(|field| field.code == "no_matching_union"));
        assert!(error
            .field_errors
            .iter()
            .all(|field| field.code != "ambiguous_union"));
    }

    #[test]
    fn non_strict_omission_uses_frozen_schema_not_current_descriptor() {
        let frozen = json!({
            "type":"object",
            "properties":{"path":{"type":"string"},"offset":{"type":"integer"}},
            "required":["path"],
            "additionalProperties":false
        });
        let current = json!({
            "type":"object",
            "properties":{"count":{"type":"integer"}},
            "required":["count"],
            "additionalProperties":false
        });
        let plan = planned(frozen, false);
        let tool: Arc<dyn Tool> = Arc::new(SchemaTool {
            schema: current,
            invalidating_normalization: false,
        });
        let valid = validate(r#"{"path":"p"}"#, json!({}), &plan, tool).unwrap();
        assert_eq!(valid.input, json!({"path":"p"}));
    }

    #[test]
    fn malformed_stream_never_falls_back_and_invalid_normalization_rejects() {
        let schema = json!({
            "type":"object",
            "properties":{"path":{"type":"string"}},
            "required":["path"],
            "additionalProperties":false
        });
        let plan = planned(schema.clone(), false);
        let normal: Arc<dyn Tool> = Arc::new(SchemaTool {
            schema: schema.clone(),
            invalidating_normalization: false,
        });
        assert_eq!(
            validate("{]", json!({"path":"fallback"}), &plan, normal)
                .unwrap_err()
                .error
                .code,
            "invalid_arguments"
        );
        let whitespace: Arc<dyn Tool> = Arc::new(SchemaTool {
            schema: schema.clone(),
            invalidating_normalization: false,
        });
        assert_eq!(
            validate("   ", json!({"path":"fallback"}), &plan, whitespace)
                .unwrap_err()
                .error
                .code,
            "invalid_arguments"
        );

        let invalidating: Arc<dyn Tool> = Arc::new(SchemaTool {
            schema,
            invalidating_normalization: true,
        });
        assert_eq!(
            validate(
                r#"{"path":"valid-before-normalization"}"#,
                json!({}),
                &plan,
                invalidating,
            )
            .unwrap_err()
            .error
            .code,
            "invalid_arguments"
        );
    }
}
