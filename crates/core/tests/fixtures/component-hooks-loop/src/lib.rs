// A component fixture exporting `ryuzi:hooks/hooks@0.1.0` whose `handle`
// ALWAYS loops forever, ignoring the payload. Paired with `component-hooks`
// (spinreject branch) in the IMP-1 epoch-isolation regression: this component
// is given a short timeout so it traps and calls the host's
// `engine.increment_epoch()`, which — with per-component engines — must NOT
// trip the concurrently-executing deny component's epoch deadline.

wit_bindgen::generate!({
    path: "wit",
    world: "hooks-loop-fixture",
    generate_all,
});

use exports::ryuzi::hooks::hooks::{Guest, HookError, HookEvent, HookResult};

struct Fixture;

impl Guest for Fixture {
    fn handle(_event: HookEvent) -> Result<HookResult, HookError> {
        // `black_box` keeps the optimizer from eliding this otherwise
        // side-effect-free loop, so the host's fuel/epoch budget really fires.
        let mut counter: u64 = 0;
        loop {
            counter = counter.wrapping_add(1);
            std::hint::black_box(counter);
        }
    }
}

export!(Fixture);
