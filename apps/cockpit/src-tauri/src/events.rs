use ryuzi_core::CoreEvent;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri_specta::Event;

#[derive(Debug, Clone, Serialize, Deserialize, Type, Event)]
pub struct CoreEventMsg {
    pub event: CoreEvent,
}
