use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[cfg(test)]
thread_local! {
    static CANONICAL_COMPILATION_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub const MAX_TOOL_SCHEMA_BYTES: usize = 256 * 1024;
pub const MAX_TOOL_DESCRIPTION_BYTES: usize = 16 * 1024;
pub const MAX_TOOL_METADATA_ENTRIES: usize = 8;
const MAX_TOOL_METADATA_TOKEN_BYTES: usize = 32;
const INVALID_TOOL_METADATA_TOKEN: &str = "Tool metadata token is invalid";
const INVALID_TOOL_METADATA_ENTRY: &str = "Tool metadata entry is invalid";
const TOOL_METADATA_LIMIT: &str = "tool metadata exceeds the bounded fact limit";

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolMetadataToken {
    Workspace,
    Attachments,
    SkillDirectory,
    RelativePath,
    AbsolutePath,
    LosslessInteger,
    LosslessBoolean,
    Native,
}

impl ToolMetadataToken {
    pub fn parse(value: &str) -> Result<Self, ToolError> {
        if value.len() > MAX_TOOL_METADATA_TOKEN_BYTES || !value.is_ascii() {
            return Err(ToolError::caller(
                "invalid_tool_metadata_token",
                INVALID_TOOL_METADATA_TOKEN,
            ));
        }
        match value {
            "workspace" => Ok(Self::Workspace),
            "attachments" => Ok(Self::Attachments),
            "skill_directory" => Ok(Self::SkillDirectory),
            "relative_path" => Ok(Self::RelativePath),
            "absolute_path" => Ok(Self::AbsolutePath),
            "lossless_integer" => Ok(Self::LosslessInteger),
            "lossless_boolean" => Ok(Self::LosslessBoolean),
            "native" => Ok(Self::Native),
            _ => Err(ToolError::caller(
                "invalid_tool_metadata_token",
                INVALID_TOOL_METADATA_TOKEN,
            )),
        }
    }
}

impl<'de> Deserialize<'de> for ToolMetadataToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct TokenVisitor;

        impl serde::de::Visitor<'_> for TokenVisitor {
            type Value = ToolMetadataToken;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a stable tool metadata token")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                ToolMetadataToken::parse(value).map_err(|_| E::custom(INVALID_TOOL_METADATA_TOKEN))
            }
        }

        deserializer.deserialize_str(TokenVisitor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ToolMetadataEntry {
    WorkspaceResolution(ToolMetadataToken),
    AttachmentResolution(ToolMetadataToken),
    SkillResolution(ToolMetadataToken),
    Coercion(ToolMetadataToken),
    ResourceCount(u64),
    CandidateCount(u64),
    MatchCount(u64),
    CacheHit(bool),
    Truncated(bool),
}

#[derive(Debug, Clone, Copy)]
enum ToolMetadataKind {
    WorkspaceResolution,
    AttachmentResolution,
    SkillResolution,
    Coercion,
    ResourceCount,
    CandidateCount,
    MatchCount,
    CacheHit,
    Truncated,
}

impl<'de> Deserialize<'de> for ToolMetadataKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct KindVisitor;

        impl serde::de::Visitor<'_> for KindVisitor {
            type Value = ToolMetadataKind;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a stable tool metadata kind")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value.len() > MAX_TOOL_METADATA_TOKEN_BYTES || !value.is_ascii() {
                    return Err(E::custom(INVALID_TOOL_METADATA_ENTRY));
                }
                match value {
                    "workspace_resolution" => Ok(ToolMetadataKind::WorkspaceResolution),
                    "attachment_resolution" => Ok(ToolMetadataKind::AttachmentResolution),
                    "skill_resolution" => Ok(ToolMetadataKind::SkillResolution),
                    "coercion" => Ok(ToolMetadataKind::Coercion),
                    "resource_count" => Ok(ToolMetadataKind::ResourceCount),
                    "candidate_count" => Ok(ToolMetadataKind::CandidateCount),
                    "match_count" => Ok(ToolMetadataKind::MatchCount),
                    "cache_hit" => Ok(ToolMetadataKind::CacheHit),
                    "truncated" => Ok(ToolMetadataKind::Truncated),
                    _ => Err(E::custom(INVALID_TOOL_METADATA_ENTRY)),
                }
            }
        }

        deserializer.deserialize_str(KindVisitor)
    }
}

enum PendingMetadataValue {
    Token(ToolMetadataToken),
    Count(u64),
    Flag(bool),
}

impl<'de> Deserialize<'de> for PendingMetadataValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ValueVisitor;

        impl serde::de::Visitor<'_> for ValueVisitor {
            type Value = PendingMetadataValue;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a bounded typed tool metadata value")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                ToolMetadataToken::parse(value)
                    .map(PendingMetadataValue::Token)
                    .map_err(|_| E::custom(INVALID_TOOL_METADATA_TOKEN))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(PendingMetadataValue::Count(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                u64::try_from(value)
                    .map(PendingMetadataValue::Count)
                    .map_err(|_| E::custom(INVALID_TOOL_METADATA_ENTRY))
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(PendingMetadataValue::Flag(value))
            }
        }

        deserializer.deserialize_any(ValueVisitor)
    }
}

#[derive(Debug, Clone, Copy)]
enum ToolMetadataField {
    Kind,
    Value,
}

impl<'de> Deserialize<'de> for ToolMetadataField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct FieldVisitor;

        impl serde::de::Visitor<'_> for FieldVisitor {
            type Value = ToolMetadataField;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a tool metadata entry field")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                match value {
                    "kind" => Ok(ToolMetadataField::Kind),
                    "value" => Ok(ToolMetadataField::Value),
                    _ => Err(E::custom(INVALID_TOOL_METADATA_ENTRY)),
                }
            }
        }

        deserializer.deserialize_identifier(FieldVisitor)
    }
}

impl<'de> Deserialize<'de> for ToolMetadataEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct EntryVisitor;

        impl<'de> serde::de::Visitor<'de> for EntryVisitor {
            type Value = ToolMetadataEntry;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a bounded typed tool metadata entry")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut kind = None;
                let mut value = None;
                while let Some(field) = map.next_key::<ToolMetadataField>()? {
                    match field {
                        ToolMetadataField::Kind => {
                            if kind.is_some() {
                                return Err(serde::de::Error::custom(INVALID_TOOL_METADATA_ENTRY));
                            }
                            kind = Some(map.next_value::<ToolMetadataKind>()?);
                        }
                        ToolMetadataField::Value => {
                            if value.is_some() {
                                return Err(serde::de::Error::custom(INVALID_TOOL_METADATA_ENTRY));
                            }
                            value = Some(map.next_value::<PendingMetadataValue>()?);
                        }
                    }
                }

                let kind =
                    kind.ok_or_else(|| serde::de::Error::custom(INVALID_TOOL_METADATA_ENTRY))?;
                let value =
                    value.ok_or_else(|| serde::de::Error::custom(INVALID_TOOL_METADATA_ENTRY))?;
                match (kind, value) {
                    (ToolMetadataKind::WorkspaceResolution, PendingMetadataValue::Token(value)) => {
                        Ok(ToolMetadataEntry::WorkspaceResolution(value))
                    }
                    (
                        ToolMetadataKind::AttachmentResolution,
                        PendingMetadataValue::Token(value),
                    ) => Ok(ToolMetadataEntry::AttachmentResolution(value)),
                    (ToolMetadataKind::SkillResolution, PendingMetadataValue::Token(value)) => {
                        Ok(ToolMetadataEntry::SkillResolution(value))
                    }
                    (ToolMetadataKind::Coercion, PendingMetadataValue::Token(value)) => {
                        Ok(ToolMetadataEntry::Coercion(value))
                    }
                    (ToolMetadataKind::ResourceCount, PendingMetadataValue::Count(value)) => {
                        Ok(ToolMetadataEntry::ResourceCount(value))
                    }
                    (ToolMetadataKind::CandidateCount, PendingMetadataValue::Count(value)) => {
                        Ok(ToolMetadataEntry::CandidateCount(value))
                    }
                    (ToolMetadataKind::MatchCount, PendingMetadataValue::Count(value)) => {
                        Ok(ToolMetadataEntry::MatchCount(value))
                    }
                    (ToolMetadataKind::CacheHit, PendingMetadataValue::Flag(value)) => {
                        Ok(ToolMetadataEntry::CacheHit(value))
                    }
                    (ToolMetadataKind::Truncated, PendingMetadataValue::Flag(value)) => {
                        Ok(ToolMetadataEntry::Truncated(value))
                    }
                    _ => Err(serde::de::Error::custom(INVALID_TOOL_METADATA_ENTRY)),
                }
            }
        }

        deserializer.deserialize_map(EntryVisitor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ToolMetadataKey {
    WorkspaceResolution,
    AttachmentResolution,
    SkillResolution,
    Coercion,
    ResourceCount,
    CandidateCount,
    MatchCount,
    CacheHit,
    Truncated,
}

impl ToolMetadataEntry {
    fn key(&self) -> ToolMetadataKey {
        match self {
            Self::WorkspaceResolution(_) => ToolMetadataKey::WorkspaceResolution,
            Self::AttachmentResolution(_) => ToolMetadataKey::AttachmentResolution,
            Self::SkillResolution(_) => ToolMetadataKey::SkillResolution,
            Self::Coercion(_) => ToolMetadataKey::Coercion,
            Self::ResourceCount(_) => ToolMetadataKey::ResourceCount,
            Self::CandidateCount(_) => ToolMetadataKey::CandidateCount,
            Self::MatchCount(_) => ToolMetadataKey::MatchCount,
            Self::CacheHit(_) => ToolMetadataKey::CacheHit,
            Self::Truncated(_) => ToolMetadataKey::Truncated,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolMetadata {
    entries: BTreeMap<ToolMetadataKey, ToolMetadataEntry>,
}

impl ToolMetadata {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> impl ExactSizeIterator<Item = &ToolMetadataEntry> {
        self.entries.values()
    }

    pub fn insert(&mut self, entry: ToolMetadataEntry) -> Result<(), ToolError> {
        let key = entry.key();
        if self.entries.contains_key(&key) {
            return Err(ToolError::caller(
                "duplicate_tool_metadata",
                "Tool metadata contains a duplicate fact",
            ));
        }
        if self.entries.len() >= MAX_TOOL_METADATA_ENTRIES {
            return Err(ToolError::caller(
                "tool_metadata_limit",
                "Tool metadata exceeds the bounded fact limit",
            ));
        }
        self.entries.insert(key, entry);
        Ok(())
    }
}

impl Serialize for ToolMetadata {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeSeq;

        let mut sequence = serializer.serialize_seq(Some(self.entries.len()))?;
        for entry in self.entries.values() {
            sequence.serialize_element(entry)?;
        }
        sequence.end()
    }
}

impl<'de> Deserialize<'de> for ToolMetadata {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct MetadataVisitor;

        impl<'de> serde::de::Visitor<'de> for MetadataVisitor {
            type Value = ToolMetadata;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a bounded sequence of typed tool metadata entries")
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                if sequence
                    .size_hint()
                    .is_some_and(|size| size > MAX_TOOL_METADATA_ENTRIES)
                {
                    return Err(serde::de::Error::custom(TOOL_METADATA_LIMIT));
                }

                let mut metadata = ToolMetadata::default();
                for _ in 0..MAX_TOOL_METADATA_ENTRIES {
                    let Some(entry) = sequence.next_element::<ToolMetadataEntry>()? else {
                        return Ok(metadata);
                    };
                    metadata.insert(entry).map_err(serde::de::Error::custom)?;
                }
                if sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(serde::de::Error::custom(TOOL_METADATA_LIMIT));
                }
                Ok(metadata)
            }
        }

        deserializer.deserialize_seq(MetadataVisitor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedInput {
    pub value: Value,
    pub normalized: bool,
    metadata: ToolMetadata,
}

impl NormalizedInput {
    pub fn unchanged(value: Value) -> Self {
        Self {
            value,
            normalized: false,
            metadata: ToolMetadata::default(),
        }
    }

    pub fn changed(value: Value) -> Self {
        Self {
            value,
            normalized: true,
            metadata: ToolMetadata::default(),
        }
    }

    pub fn metadata(&self) -> &ToolMetadata {
        &self.metadata
    }

    pub fn with_metadata(mut self, entry: ToolMetadataEntry) -> Result<Self, ToolError> {
        self.metadata.insert(entry)?;
        Ok(self)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightMeta {
    #[serde(default, skip_serializing_if = "ToolMetadata::is_empty")]
    metadata: ToolMetadata,
}

impl PreflightMeta {
    pub fn metadata(&self) -> &ToolMetadata {
        &self.metadata
    }

    pub fn with_metadata(mut self, entry: ToolMetadataEntry) -> Result<Self, ToolError> {
        self.metadata.insert(entry)?;
        Ok(self)
    }
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
    #[cfg(test)]
    CANONICAL_COMPILATION_COUNT.with(|count| count.set(count.get() + 1));
    close_object_shapes(&mut schema);
    schema
}

#[cfg(test)]
pub(crate) fn canonical_compilation_count() -> usize {
    CANONICAL_COMPILATION_COUNT.with(std::cell::Cell::get)
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
            visit_schema_children_mut(object, close_object_shapes);
        }
        _ => {}
    }
}

const SINGLE_SCHEMA_KEYWORDS: &[&str] = &[
    "additionalItems",
    "additionalProperties",
    "contains",
    "contentSchema",
    "else",
    "if",
    "items",
    "not",
    "propertyNames",
    "then",
    "unevaluatedItems",
    "unevaluatedProperties",
];
const SCHEMA_ARRAY_KEYWORDS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems"];
const SCHEMA_MAP_KEYWORDS: &[&str] = &[
    "$defs",
    "definitions",
    "dependentSchemas",
    "patternProperties",
    "properties",
];

fn visit_schema_children_mut(object: &mut Map<String, Value>, mut visit: impl FnMut(&mut Value)) {
    for keyword in SINGLE_SCHEMA_KEYWORDS {
        if let Some(schema) = object.get_mut(*keyword) {
            visit(schema);
        }
    }
    for keyword in SCHEMA_ARRAY_KEYWORDS {
        if let Some(Value::Array(schemas)) = object.get_mut(*keyword) {
            for schema in schemas {
                visit(schema);
            }
        }
    }
    for keyword in SCHEMA_MAP_KEYWORDS {
        if let Some(Value::Object(schemas)) = object.get_mut(*keyword) {
            for schema in schemas.values_mut() {
                visit(schema);
            }
        }
    }
    if let Some(Value::Object(dependencies)) = object.get_mut("dependencies") {
        for schema in dependencies.values_mut() {
            if schema.is_object() || schema.is_boolean() {
                visit(schema);
            }
        }
    }
}

fn visit_schema_children(
    object: &Map<String, Value>,
    mut visit: impl FnMut(&Value) -> bool,
) -> bool {
    SINGLE_SCHEMA_KEYWORDS
        .iter()
        .filter_map(|keyword| object.get(*keyword))
        .any(&mut visit)
        || SCHEMA_ARRAY_KEYWORDS
            .iter()
            .filter_map(|keyword| object.get(*keyword).and_then(Value::as_array))
            .flatten()
            .any(&mut visit)
        || SCHEMA_MAP_KEYWORDS
            .iter()
            .filter_map(|keyword| object.get(*keyword).and_then(Value::as_object))
            .flat_map(Map::values)
            .any(&mut visit)
        || object
            .get("dependencies")
            .and_then(Value::as_object)
            .is_some_and(|dependencies| {
                dependencies
                    .values()
                    .filter(|schema| schema.is_object() || schema.is_boolean())
                    .any(visit)
            })
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
    if wire
        .as_object()
        .is_none_or(|root| root.get("type").and_then(Value::as_str) != Some("object"))
    {
        return Err(SchemaCompileError::new(
            "strict_root_not_object",
            "Strict tool schemas require an object root",
        ));
    }
    rewrite_schema_consts(&mut wire)?;
    compile_strict_node(&mut wire)?;
    Ok(wire)
}

fn rewrite_schema_consts(value: &mut Value) -> Result<(), SchemaCompileError> {
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    if let Some(constant) = object.remove("const") {
        if object.contains_key("enum") {
            return Err(SchemaCompileError::new(
                "unsupported_strict_schema",
                "Tool schema uses a construct unsupported by strict mode",
            ));
        }
        object.insert("enum".into(), Value::Array(vec![constant]));
    }

    let mut result = Ok(());
    visit_schema_children_mut(object, |schema| {
        if result.is_ok() {
            result = rewrite_schema_consts(schema);
        }
    });
    result
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
        "dependencies",
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProvenDiscriminator {
    String(String),
    Boolean(bool),
    Null,
}

fn singleton_schema_value(schema: &Value) -> Option<ProvenDiscriminator> {
    let object = schema.as_object()?;
    let values = object.get("enum")?.as_array()?;
    if values.len() != 1 {
        return None;
    }
    match &values[0] {
        Value::String(value) => Some(ProvenDiscriminator::String(value.clone())),
        Value::Bool(value) => Some(ProvenDiscriminator::Boolean(*value)),
        Value::Null => Some(ProvenDiscriminator::Null),
        _ => None,
    }
}

fn reject_unsupported_keywords(object: &Map<String, Value>) -> Result<(), SchemaCompileError> {
    const ALLOWED: &[&str] = &[
        "$defs",
        "$ref",
        "additionalProperties",
        "anyOf",
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
            explicitly_open || visit_schema_children(object, explicit_open_object_schema)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compile_canonical_schema, compile_openai_strict_schema, explicit_open_object_schema,
        FacadePriority, NormalizedInput, PreflightMeta, ResourceScopeHint, ToolDescriptor,
        ToolEffect, ToolErrorCategory, ToolMetadata, ToolMetadataEntry, ToolMetadataToken,
        MAX_TOOL_METADATA_ENTRIES,
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
    fn canonical_schema_traverses_only_schema_valued_keyword_positions() {
        let schema = compile_canonical_schema(json!({
            "type": "object",
            "properties": {
                "required": {"type": "string"},
                "additionalProperties": {"type": "boolean"},
                "nested": {
                    "anyOf": [
                        {"type": "array", "items": {"type": "object"}},
                        {"$ref": "#/$defs/payload"}
                    ]
                }
            },
            "$defs": {
                "payload": {"type": "object", "properties": {"id": {"type": "string"}}}
            },
            "const": {"required": []},
            "enum": [{"required": []}],
            "default": {"required": []},
            "examples": [{"additionalProperties": true}]
        }));

        assert!(schema["properties"].get("additionalProperties").is_some());
        assert!(schema["properties"].get("properties").is_none());
        assert_eq!(schema["const"], json!({"required": []}));
        assert_eq!(schema["enum"], json!([{"required": []}]));
        assert_eq!(schema["default"], json!({"required": []}));
        assert_eq!(schema["examples"], json!([{"additionalProperties": true}]));
        assert_eq!(
            schema["properties"]["nested"]["anyOf"][0]["items"]["additionalProperties"],
            false
        );
        assert_eq!(schema["$defs"]["payload"]["additionalProperties"], false);
        assert!(!explicit_open_object_schema(&schema));
    }

    #[test]
    fn open_schema_detection_checks_only_genuine_schema_positions() {
        let instance_data_only = json!({
            "type": "object",
            "properties": {
                "additionalProperties": {"type": "boolean"}
            },
            "const": {"additionalProperties": true},
            "enum": [{"additionalProperties": true}]
        });
        let genuinely_open = json!({
            "type": "object",
            "properties": {
                "nested": {
                    "type": "array",
                    "items": {"type": "object", "additionalProperties": true}
                }
            }
        });

        assert!(!explicit_open_object_schema(&instance_data_only));
        assert!(explicit_open_object_schema(&genuinely_open));
    }

    #[test]
    fn canonical_schema_traverses_legacy_draft_schema_positions() {
        let schema = compile_canonical_schema(json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {
                "tuple": {
                    "type": "array",
                    "items": [{"type": "string"}],
                    "additionalItems": {
                        "type": "object",
                        "properties": {"id": {"type": "string"}}
                    }
                }
            },
            "dependencies": {
                "payload": {
                    "dependencies": {"name": ["id"]}
                },
                "typed_payload": {
                    "type": "object",
                    "properties": {"name": {"type": "string"}}
                },
                "disabled": false,
                "related": ["payload", "additionalProperties"]
            }
        }));

        jsonschema::validator_for(&schema).unwrap();
        assert_eq!(
            schema["properties"]["tuple"]["additionalItems"]["additionalProperties"],
            false
        );
        assert_eq!(
            schema["dependencies"]["payload"]["additionalProperties"],
            false
        );
        assert_eq!(
            schema["dependencies"]["payload"]["dependencies"]["name"],
            json!(["id"])
        );
        assert_eq!(
            schema["dependencies"]["typed_payload"]["additionalProperties"],
            false
        );
        assert_eq!(schema["dependencies"]["disabled"], false);
        assert_eq!(
            schema["dependencies"]["related"],
            json!(["payload", "additionalProperties"])
        );
        assert!(!explicit_open_object_schema(&schema));
    }

    #[test]
    fn open_schema_detection_checks_legacy_schema_values_but_not_dependency_names() {
        let dependency_names_only = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "dependencies": {
                "related": ["additionalProperties"]
            }
        });
        let open_additional_items = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "array",
            "items": [{"type": "string"}],
            "additionalItems": {"type": "object", "additionalProperties": true}
        });
        let open_dependency_schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "dependencies": {
                "payload": {"type": "object", "additionalProperties": true}
            }
        });

        assert!(!explicit_open_object_schema(&dependency_names_only));
        assert!(explicit_open_object_schema(&open_additional_items));
        assert!(explicit_open_object_schema(&open_dependency_schema));
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
    fn strict_schema_rewrites_schema_node_consts_to_singleton_enums() {
        fn assert_no_const(value: &serde_json::Value) {
            match value {
                serde_json::Value::Array(values) => values.iter().for_each(assert_no_const),
                serde_json::Value::Object(object) => {
                    assert!(!object.contains_key("const"), "wire schema contains const");
                    object.values().for_each(assert_no_const);
                }
                _ => {}
            }
        }

        let wire = compile_openai_strict_schema(&json!({
            "type": "object",
            "properties": {
                "mode": {"const": "safe"},
                "operation": {
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": {"action": {"const": "read"}},
                            "required": ["action"]
                        },
                        {
                            "type": "object",
                            "properties": {"action": {"const": "write"}},
                            "required": ["action"]
                        }
                    ]
                }
            },
            "required": ["mode", "operation"],
            "$defs": {"tag": {"const": true}}
        }))
        .unwrap();

        assert_no_const(&wire);
        assert_eq!(wire["properties"]["mode"]["enum"], json!(["safe"]));
        assert_eq!(wire["$defs"]["tag"]["enum"], json!([true]));
        assert_eq!(
            wire["properties"]["operation"]["anyOf"][0]["properties"]["action"]["enum"],
            json!(["read"])
        );
    }

    #[test]
    fn strict_schema_does_not_prove_numeric_singletons_disjoint() {
        let error = compile_openai_strict_schema(&json!({
            "type": "object",
            "properties": {
                "operation": {
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": {"action": {"enum": [1]}},
                            "required": ["action"]
                        },
                        {
                            "type": "object",
                            "properties": {"action": {"enum": [1.0]}},
                            "required": ["action"]
                        }
                    ]
                }
            },
            "required": ["operation"]
        }))
        .unwrap_err();

        assert_eq!(error.code, "non_disjoint_one_of");
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

    #[test]
    fn tool_metadata_is_typed_bounded_and_rejects_duplicates() {
        let mut metadata = ToolMetadata::default();
        let entries = [
            ToolMetadataEntry::WorkspaceResolution(ToolMetadataToken::Workspace),
            ToolMetadataEntry::AttachmentResolution(ToolMetadataToken::Attachments),
            ToolMetadataEntry::SkillResolution(ToolMetadataToken::SkillDirectory),
            ToolMetadataEntry::Coercion(ToolMetadataToken::LosslessInteger),
            ToolMetadataEntry::ResourceCount(1),
            ToolMetadataEntry::CandidateCount(2),
            ToolMetadataEntry::MatchCount(3),
            ToolMetadataEntry::CacheHit(true),
        ];
        assert_eq!(entries.len(), MAX_TOOL_METADATA_ENTRIES);
        for entry in entries {
            metadata.insert(entry).unwrap();
        }

        let limit = metadata
            .insert(ToolMetadataEntry::Truncated(false))
            .unwrap_err();
        assert_eq!(limit.code, "tool_metadata_limit");

        let mut duplicate = ToolMetadata::default();
        duplicate.insert(ToolMetadataEntry::CacheHit(true)).unwrap();
        let duplicate = duplicate
            .insert(ToolMetadataEntry::CacheHit(false))
            .unwrap_err();
        assert_eq!(duplicate.code, "duplicate_tool_metadata");

        let duplicate_serialized = json!([
            {"kind": "cache_hit", "value": true},
            {"kind": "cache_hit", "value": false}
        ]);
        assert!(serde_json::from_value::<ToolMetadata>(duplicate_serialized).is_err());
    }

    #[test]
    fn metadata_tokens_accept_only_stable_redaction_safe_vocabulary() {
        assert_eq!(
            ToolMetadataToken::parse("workspace").unwrap(),
            ToolMetadataToken::Workspace
        );
        for rejected in [
            "wørkspace",
            "../../secret.txt",
            "api_key",
            "bearer-token",
            "raw argument text",
        ] {
            let error = ToolMetadataToken::parse(rejected).unwrap_err();
            assert_eq!(error.code, "invalid_tool_metadata_token");
            assert!(!error.message.contains(rejected));
        }
    }

    #[test]
    fn metadata_deserialization_is_capped_and_redacts_rejected_content() {
        fn errors_for(metadata: serde_json::Value) -> [String; 2] {
            let direct = serde_json::from_value::<ToolMetadata>(metadata.clone())
                .unwrap_err()
                .to_string();
            let preflight = serde_json::from_value::<PreflightMeta>(json!({
                "metadata": metadata
            }))
            .unwrap_err()
            .to_string();
            [direct, preflight]
        }

        fn assert_redacted(metadata: serde_json::Value, marker: &str) {
            for error in errors_for(metadata) {
                assert!(!error.contains(marker), "metadata error leaked marker");
            }
        }

        let over_cap_marker = "OVER_CAP_SECRET_MARKER_4f7d";
        let mut over_cap = vec![
            json!({"kind": "workspace_resolution", "value": "workspace"}),
            json!({"kind": "attachment_resolution", "value": "attachments"}),
            json!({"kind": "skill_resolution", "value": "skill_directory"}),
            json!({"kind": "coercion", "value": "lossless_integer"}),
            json!({"kind": "resource_count", "value": 1}),
            json!({"kind": "candidate_count", "value": 2}),
            json!({"kind": "match_count", "value": 3}),
            json!({"kind": "cache_hit", "value": true}),
        ];
        over_cap.push(json!({"kind": over_cap_marker, "value": "raw"}));
        for error in errors_for(serde_json::Value::Array(over_cap)) {
            assert!(error.contains("tool metadata exceeds the bounded fact limit"));
            assert!(!error.contains(over_cap_marker));
        }

        let unknown_kind = "UNKNOWN_KIND_SECRET_MARKER_91c2";
        assert_redacted(json!([{"kind": unknown_kind, "value": true}]), unknown_kind);

        for token in [
            "UNKNOWN_TOKEN_SECRET_MARKER_a8e1".to_string(),
            "wørkspace_secret_marker".to_string(),
            "../../raw/secret-marker.txt".to_string(),
            "bearer-sensitive-secret-marker".to_string(),
            "x".repeat(512),
        ] {
            assert_redacted(
                json!([{"kind": "workspace_resolution", "value": &token}]),
                &token,
            );
        }

        for error in errors_for(json!([
            {"kind": "cache_hit", "value": true},
            {"kind": "cache_hit", "value": false}
        ])) {
            assert!(error.contains("duplicate_tool_metadata"));
        }
    }

    #[test]
    fn normalization_and_preflight_share_private_checked_metadata() {
        let normalized = NormalizedInput::changed(json!({"path": "safe"}))
            .with_metadata(ToolMetadataEntry::WorkspaceResolution(
                ToolMetadataToken::RelativePath,
            ))
            .unwrap();
        let preflight = PreflightMeta::default()
            .with_metadata(ToolMetadataEntry::ResourceCount(1))
            .unwrap();

        assert_eq!(normalized.metadata().len(), 1);
        assert_eq!(preflight.metadata().len(), 1);
        assert_eq!(
            serde_json::to_value(preflight).unwrap(),
            json!({"metadata": [{"kind": "resource_count", "value": 1}]})
        );
    }
}
