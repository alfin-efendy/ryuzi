// A component fixture exporting `ryuzi:hooks/hooks@0.1.0` for the Task 9 hook
// adapter. The host encodes the JSON hook payload as a single `text` hook
// value; this fixture branches on that text so a single component covers the
// three behaviours the adapter tests need:
//   - payload containing "boom" — loops forever, exercising the `tool.before`
//     timeout → documented fail-OPEN (allow) path.
//   - payload containing "deny" — returns `hook-error::rejected`, the gating
//     deny signal.
//   - otherwise — returns an allowing `hook-result`.

wit_bindgen::generate!({
    path: "wit",
    world: "hooks-fixture",
    generate_all,
});

use exports::ryuzi::hooks::hooks::{Guest, HookError, HookEvent, HookResult, HookValue};

struct Fixture;

impl Guest for Fixture {
    fn handle(event: HookEvent) -> Result<HookResult, HookError> {
        let payload = event
            .values
            .iter()
            .find_map(|value| match value {
                HookValue::Text(text) => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();

        // `black_box` keeps the optimizer from eliding this otherwise
        // side-effect-free loop, so the host's fuel/epoch budget really fires.
        if payload.contains("boom") {
            let mut counter: u64 = 0;
            loop {
                counter = counter.wrapping_add(1);
                std::hint::black_box(counter);
            }
        }

        if payload.contains("deny") {
            return Err(HookError::Rejected);
        }

        Ok(HookResult {
            handled: true,
            values: vec![],
        })
    }
}

export!(Fixture);
