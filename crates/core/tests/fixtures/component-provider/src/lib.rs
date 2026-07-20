// A component fixture exporting `ryuzi:provider/provider@0.1.0` for the Task 10
// generic provider adapter. It exercises the router divert
// (`wasm_provider_stream`):
//   - `list-models` returns a single static model, for descriptor registration
//     + model discoverability.
//   - `complete` returns a TWO-chunk streaming completion IN ORDER ("Hello, "
//     then "world!"), so the adapter's chunk-order preservation is observable;
//     the final chunk is `finished` and carries a `token-usage`.
//   - a prompt containing "boom" loops forever, so a trapping/looping
//     completion is caught by the host fuel/epoch budget and surfaces as a
//     route-scoped error (never a daemon crash).
//   - a prompt containing "reject" returns a `provider-error`, the clean
//     guest-error path.

wit_bindgen::generate!({
    path: "wit",
    world: "provider-fixture",
    generate_all,
});

use exports::ryuzi::provider::provider::{
    CompletionChunk, CompletionRequest, Guest, ModelInfo, ProviderError, TokenUsage,
};

struct Fixture;

impl Guest for Fixture {
    fn list_models() -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(vec![ModelInfo {
            id: "fixture-model".to_string(),
            display_name: "Fixture Model".to_string(),
            context_window: 8192,
        }])
    }

    fn complete(request: CompletionRequest) -> Result<Vec<CompletionChunk>, ProviderError> {
        // `black_box` keeps the optimizer from eliding this otherwise
        // side-effect-free loop, so the host's fuel/epoch budget really fires.
        if request.prompt.contains("boom") {
            let mut counter: u64 = 0;
            loop {
                counter = counter.wrapping_add(1);
                std::hint::black_box(counter);
            }
        }
        if request.prompt.contains("reject") {
            return Err(ProviderError::Failed(
                "intentional provider failure".to_string(),
            ));
        }
        Ok(vec![
            CompletionChunk {
                text: "Hello, ".to_string(),
                finished: false,
                usage: None,
            },
            CompletionChunk {
                text: "world!".to_string(),
                finished: true,
                usage: Some(TokenUsage {
                    input: 7,
                    output: 3,
                }),
            },
        ])
    }
}

export!(Fixture);
