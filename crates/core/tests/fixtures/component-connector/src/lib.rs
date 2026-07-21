// A component fixture exporting `ryuzi:connector/connector@0.1.0` with three
// in-process tools that exercise the Task 9 connector adapter:
//   - `echo`    — returns its `message` argument, for enumeration + invocation
//                 + synthesized-schema tests.
//   - `slow`    — loops forever, for the timeout-isolation test (a trapping
//                 invoke must be caught and must not crash the host).
//   - `explode` — returns a `connector-error`, for the guest-error path.

wit_bindgen::generate!({
    path: "wit",
    world: "connector-fixture",
    generate_all,
});

use exports::ryuzi::connector::connector::{
    ConnectorError, Guest, ToolCall, ToolDefinition, ToolParameter, ToolResult, ToolValue,
};

struct Fixture;

impl Guest for Fixture {
    fn list_tools() -> Result<Vec<ToolDefinition>, ConnectorError> {
        Ok(vec![
            ToolDefinition {
                name: "echo".to_string(),
                description: "Echo the message argument back".to_string(),
                parameters: vec![ToolParameter {
                    name: "message".to_string(),
                    value_type: "string".to_string(),
                    required: true,
                }],
            },
            ToolDefinition {
                name: "slow".to_string(),
                description: "Loops forever to exercise timeout isolation".to_string(),
                parameters: vec![],
            },
            ToolDefinition {
                name: "explode".to_string(),
                description: "Always returns a connector error".to_string(),
                parameters: vec![],
            },
        ])
    }

    fn invoke(call: ToolCall) -> Result<ToolResult, ConnectorError> {
        match call.name.as_str() {
            "echo" => {
                let value = call
                    .arguments
                    .into_iter()
                    .find(|argument| argument.name == "message")
                    .map(|argument| argument.value)
                    .unwrap_or(ToolValue::Text(String::new()));
                Ok(ToolResult {
                    call_id: call.call_id,
                    values: vec![value],
                })
            }
            // `black_box` keeps the optimizer from eliding this otherwise
            // side-effect-free loop, so the host's fuel/epoch budget really
            // fires.
            "slow" => {
                let mut counter: u64 = 0;
                loop {
                    counter = counter.wrapping_add(1);
                    std::hint::black_box(counter);
                }
            }
            "explode" => Err(ConnectorError::Failed(
                "intentional connector failure".to_string(),
            )),
            other => Err(ConnectorError::InvalidCall(format!("unknown tool: {other}"))),
        }
    }
}

export!(Fixture);
