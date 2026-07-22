//! wasm32-only guest glue: wires [`crate::logic`] to the host-managed
//! `ryuzi:oauth/oauth` egress and exports `ryuzi:connector/connector`.
//!
//! Kept deliberately thin — no API decisions live here, only effect
//! orchestration and WIT type mapping. Every request is built by
//! [`logic::plan_request`] (so an unconfirmed mutation never reaches this
//! module) and sent through `oauth.authorized-request("atlassian-cloud", ..)`,
//! which injects the bearer host-side; this component never sees a token.
//! Responses are handed straight back to [`logic::parse_response`].

use crate::logic;

wit_bindgen::generate!({
    path: "wit",
    world: "atlassian",
    generate_all,
});

use exports::ryuzi::connector::connector::{
    ConnectorError, Guest, ToolArgument, ToolCall, ToolDefinition, ToolParameter, ToolResult,
    ToolValue,
};
use ryuzi::oauth::oauth;

struct Atlassian;

impl Guest for Atlassian {
    fn list_tools() -> Result<Vec<ToolDefinition>, ConnectorError> {
        // Static catalogue; no host effect required.
        Ok(logic::tool_definitions().into_iter().map(map_def).collect())
    }

    fn invoke(call: ToolCall) -> Result<ToolResult, ConnectorError> {
        let ToolCall {
            call_id,
            name,
            arguments,
        } = call;
        let args: Vec<logic::Arg> = arguments.into_iter().map(map_arg).collect();

        // Plan first: the confirmation gate for a mutating tool fires here,
        // returning an error BEFORE any request is built (see plan_request).
        let planned = logic::plan_request(&name, &args).map_err(map_error)?;

        let response = match send(&planned) {
            Ok(response) => response,
            Err(error) => {
                // `auth_status` is a connection probe: a missing (`denied`) or
                // expired token is reported as "not connected", never an error.
                if name == "auth_status" && is_disconnected(&error) {
                    return Ok(ToolResult {
                        call_id,
                        values: map_values(logic::not_connected()),
                    });
                }
                return Err(oauth_error_to_connector(&name, error));
            }
        };

        let values =
            logic::parse_response(&name, response.status, &response.body).map_err(map_error)?;
        Ok(ToolResult {
            call_id,
            values: map_values(values),
        })
    }
}

/// Send one planned request through the host-managed OAuth egress for the
/// `atlassian-cloud` profile. The host injects the bearer and strips any
/// component-set `authorization` (never present here); the component never
/// sees the token.
fn send(planned: &logic::PlannedRequest) -> Result<oauth::AuthorizedResponse, oauth::OauthError> {
    let request = oauth::OauthRequest {
        method: planned.method.clone(),
        url: planned.url.clone(),
        headers: planned
            .headers
            .iter()
            .map(|(name, value)| oauth::Header {
                name: name.clone(),
                value: value.clone(),
            })
            .collect(),
        body: planned.body.clone(),
    };
    oauth::authorized_request(logic::OAUTH_PROFILE, &request)
}

fn is_disconnected(error: &oauth::OauthError) -> bool {
    matches!(
        error,
        oauth::OauthError::Denied | oauth::OauthError::Expired
    )
}

fn oauth_error_to_connector(tool: &str, error: oauth::OauthError) -> ConnectorError {
    match error {
        oauth::OauthError::Denied => ConnectorError::Failed(format!(
            "Atlassian is not connected. Connect it via Cockpit's Plugins screen before calling '{tool}'."
        )),
        oauth::OauthError::Expired => ConnectorError::Failed(
            "Atlassian authorization expired. Reconnect it via Cockpit's Plugins screen."
                .to_string(),
        ),
        oauth::OauthError::InvalidRequest(message) => ConnectorError::InvalidCall(message),
        oauth::OauthError::Failed(message) => {
            ConnectorError::Failed(format!("Atlassian request failed: {message}"))
        }
    }
}

fn map_def(def: logic::ToolDef) -> ToolDefinition {
    ToolDefinition {
        name: def.name,
        description: def.description,
        parameters: def
            .parameters
            .into_iter()
            .map(|param| ToolParameter {
                name: param.name,
                value_type: param.value_type,
                required: param.required,
            })
            .collect(),
    }
}

fn map_arg(arg: ToolArgument) -> logic::Arg {
    logic::Arg {
        name: arg.name,
        value: match arg.value {
            ToolValue::Text(text) => logic::ArgValue::Text(text),
            ToolValue::Integer(integer) => logic::ArgValue::Integer(integer),
            ToolValue::Decimal(decimal) => logic::ArgValue::Decimal(decimal),
            ToolValue::Boolean(boolean) => logic::ArgValue::Boolean(boolean),
        },
    }
}

fn map_values(values: Vec<logic::ToolValueOut>) -> Vec<ToolValue> {
    values.into_iter().map(map_value).collect()
}

fn map_value(value: logic::ToolValueOut) -> ToolValue {
    match value {
        logic::ToolValueOut::Text(text) => ToolValue::Text(text),
        logic::ToolValueOut::Integer(integer) => ToolValue::Integer(integer),
        logic::ToolValueOut::Decimal(decimal) => ToolValue::Decimal(decimal),
        logic::ToolValueOut::Boolean(boolean) => ToolValue::Boolean(boolean),
    }
}

fn map_error(error: logic::ToolError) -> ConnectorError {
    match error {
        logic::ToolError::NotFound => ConnectorError::NotFound,
        logic::ToolError::InvalidCall(message) => ConnectorError::InvalidCall(message),
        logic::ToolError::Unavailable => ConnectorError::Unavailable,
        logic::ToolError::Failed(message) => ConnectorError::Failed(message),
        // No WIT variant for confirmation — surface it as an invalid call so the
        // model is told to re-invoke with confirm=true.
        logic::ToolError::ConfirmationRequired(message) => ConnectorError::InvalidCall(message),
    }
}

export!(Atlassian);
