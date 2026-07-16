use serde::{Deserialize, Serialize};

pub const CAPABILITY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeToolsVersion {
    V1,
    V2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireProtocol {
    AnthropicMessages,
    OpenAiChat,
    OpenAiResponses,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolInteractionMode {
    DirectFunctions,
    CodeOrchestrator,
    Hybrid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySource {
    TransportDefault,
    ExplicitOverride,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportToolCapabilities {
    pub wire_protocol: WireProtocol,
    pub supports_function_tools: bool,
    pub supports_custom_freeform_tools: bool,
    pub supports_parallel_tool_calls: bool,
    pub supports_strict_function_schema: bool,
    pub supports_tool_output_schema: bool,
    pub schema_budget_tokens: u32,
}

impl TransportToolCapabilities {
    pub const fn function_only(wire_protocol: WireProtocol) -> Self {
        Self {
            wire_protocol,
            supports_function_tools: true,
            supports_custom_freeform_tools: false,
            supports_parallel_tool_calls: false,
            supports_strict_function_schema: false,
            supports_tool_output_schema: false,
            schema_budget_tokens: 16_000,
        }
    }

    pub fn intersection(
        capabilities: impl IntoIterator<Item = Self>,
    ) -> Result<Self, CapabilityResolutionError> {
        let mut capabilities = capabilities.into_iter();
        let Some(mut intersection) = capabilities.next() else {
            return Err(CapabilityResolutionError::unavailable(
                "no eligible transport targets",
            ));
        };
        for next in capabilities {
            if intersection.wire_protocol != next.wire_protocol {
                intersection.wire_protocol = WireProtocol::Mixed;
            }
            intersection.supports_function_tools &= next.supports_function_tools;
            intersection.supports_custom_freeform_tools &= next.supports_custom_freeform_tools;
            intersection.supports_parallel_tool_calls &= next.supports_parallel_tool_calls;
            intersection.supports_strict_function_schema &= next.supports_strict_function_schema;
            intersection.supports_tool_output_schema &= next.supports_tool_output_schema;
            intersection.schema_budget_tokens = intersection
                .schema_budget_tokens
                .min(next.schema_budget_tokens);
        }
        Ok(intersection)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeToolSurfaces {
    pub direct_functions: bool,
    pub code_orchestrator: bool,
}

impl RuntimeToolSurfaces {
    pub const fn direct_only() -> Self {
        Self {
            direct_functions: true,
            code_orchestrator: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCapabilityProfile {
    pub interaction_mode: ToolInteractionMode,
    pub wire_protocol: WireProtocol,
    pub supports_custom_freeform_tools: bool,
    pub supports_parallel_tool_calls: bool,
    pub supports_strict_function_schema: bool,
    pub supports_tool_output_schema: bool,
    pub schema_budget_tokens: u32,
    pub supports_prompt_cache: bool,
    pub capability_source: CapabilitySource,
    pub capability_schema_version: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityInputs {
    pub transport: TransportToolCapabilities,
    pub runtime: RuntimeToolSurfaces,
    pub override_mode: Option<ToolInteractionMode>,
    pub supports_prompt_cache: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityResolutionError {
    pub code: &'static str,
    pub message: String,
}

impl CapabilityResolutionError {
    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            code: "capability_unavailable",
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CapabilityResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CapabilityResolutionError {}

pub struct CapabilityResolver;

impl CapabilityResolver {
    pub fn resolve(
        inputs: CapabilityInputs,
    ) -> Result<ToolCapabilityProfile, CapabilityResolutionError> {
        let direct = inputs.transport.supports_function_tools && inputs.runtime.direct_functions;
        let code =
            inputs.transport.supports_custom_freeform_tools && inputs.runtime.code_orchestrator;
        let supported = |mode| match mode {
            ToolInteractionMode::DirectFunctions => direct,
            ToolInteractionMode::CodeOrchestrator => code,
            ToolInteractionMode::Hybrid => direct && code,
        };
        let interaction_mode = match inputs.override_mode {
            Some(mode) if supported(mode) => mode,
            Some(mode) => {
                return Err(CapabilityResolutionError::unavailable(format!(
                    "requested interaction mode {mode:?} is unavailable"
                )))
            }
            None if direct && code => ToolInteractionMode::Hybrid,
            None if direct => ToolInteractionMode::DirectFunctions,
            None if code => ToolInteractionMode::CodeOrchestrator,
            None => {
                return Err(CapabilityResolutionError::unavailable(
                    "transport and runtime share no tool interaction mode",
                ))
            }
        };

        Ok(ToolCapabilityProfile {
            interaction_mode,
            wire_protocol: inputs.transport.wire_protocol,
            supports_custom_freeform_tools: inputs.transport.supports_custom_freeform_tools,
            supports_parallel_tool_calls: inputs.transport.supports_parallel_tool_calls,
            supports_strict_function_schema: inputs.transport.supports_strict_function_schema,
            supports_tool_output_schema: inputs.transport.supports_tool_output_schema,
            schema_budget_tokens: inputs.transport.schema_budget_tokens,
            supports_prompt_cache: inputs.supports_prompt_cache,
            capability_source: if inputs.override_mode.is_some() {
                CapabilitySource::ExplicitOverride
            } else {
                CapabilitySource::TransportDefault
            },
            capability_schema_version: CAPABILITY_SCHEMA_VERSION,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conservative_function_transport_resolves_direct_without_model_identity() {
        let profile = CapabilityResolver::resolve(CapabilityInputs {
            transport: TransportToolCapabilities {
                wire_protocol: WireProtocol::AnthropicMessages,
                supports_function_tools: true,
                supports_custom_freeform_tools: false,
                supports_parallel_tool_calls: false,
                supports_strict_function_schema: false,
                supports_tool_output_schema: false,
                schema_budget_tokens: 16_000,
            },
            runtime: RuntimeToolSurfaces {
                direct_functions: true,
                code_orchestrator: false,
            },
            override_mode: None,
            supports_prompt_cache: true,
        })
        .unwrap();

        assert_eq!(
            profile.interaction_mode,
            ToolInteractionMode::DirectFunctions
        );
        assert_eq!(
            profile.capability_source,
            CapabilitySource::TransportDefault
        );
    }

    #[test]
    fn unavailable_code_override_is_rejected_instead_of_silently_downgraded() {
        let error = CapabilityResolver::resolve(CapabilityInputs {
            transport: TransportToolCapabilities::function_only(WireProtocol::AnthropicMessages),
            runtime: RuntimeToolSurfaces::direct_only(),
            override_mode: Some(ToolInteractionMode::CodeOrchestrator),
            supports_prompt_cache: false,
        })
        .unwrap_err();

        assert_eq!(error.code, "capability_unavailable");
    }

    #[test]
    fn intersection_keeps_only_capabilities_shared_by_every_target() {
        let profile = TransportToolCapabilities::intersection([
            TransportToolCapabilities {
                wire_protocol: WireProtocol::OpenAiResponses,
                supports_function_tools: true,
                supports_custom_freeform_tools: true,
                supports_parallel_tool_calls: true,
                supports_strict_function_schema: true,
                supports_tool_output_schema: true,
                schema_budget_tokens: 32_000,
            },
            TransportToolCapabilities {
                wire_protocol: WireProtocol::OpenAiChat,
                supports_function_tools: true,
                supports_custom_freeform_tools: false,
                supports_parallel_tool_calls: false,
                supports_strict_function_schema: false,
                supports_tool_output_schema: false,
                schema_budget_tokens: 16_000,
            },
        ])
        .unwrap();

        assert_eq!(profile.wire_protocol, WireProtocol::Mixed);
        assert!(profile.supports_function_tools);
        assert!(!profile.supports_custom_freeform_tools);
        assert!(!profile.supports_parallel_tool_calls);
        assert!(!profile.supports_strict_function_schema);
        assert!(!profile.supports_tool_output_schema);
        assert_eq!(profile.schema_budget_tokens, 16_000);
    }

    #[test]
    fn stable_capability_enums_use_snake_case_strings() {
        assert_eq!(
            serde_json::to_string(&NativeToolsVersion::V2).unwrap(),
            r#""v2""#
        );
        assert_eq!(
            serde_json::to_string(&ToolInteractionMode::CodeOrchestrator).unwrap(),
            r#""code_orchestrator""#
        );
        assert_eq!(
            serde_json::to_string(&WireProtocol::OpenAiResponses).unwrap(),
            r#""open_ai_responses""#
        );
    }
}
