//! Per-session, per-model token tally persisted inside the `session_context`
//! JSON payload under the `"models"` key. Stores TOKENS only; dollar figures
//! are computed on demand from the current price table.

use crate::domain::ModelCost;
use crate::llm_router::model_meta::ModelMeta;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// One model's accumulated billed token buckets.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Buckets {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

/// Per-model token tally. `BTreeMap` for deterministic ordering.
#[derive(Debug, Default)]
pub struct Tally(BTreeMap<String, Buckets>);

impl Tally {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, model: &str) -> Option<&Buckets> {
        self.0.get(model)
    }

    /// Add one committed response's buckets under `model`.
    pub fn add(
        &mut self,
        model: &str,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_creation: u64,
    ) {
        let b = self.0.entry(model.to_string()).or_default();
        b.input += input;
        b.output += output;
        b.cache_read += cache_read;
        b.cache_creation += cache_creation;
    }

    /// Parse the `"models"` object out of a `session_context` payload.
    pub fn from_payload(payload: &Value) -> Tally {
        let mut t = Tally::default();
        if let Some(obj) = payload.get("models").and_then(|m| m.as_object()) {
            for (model, v) in obj {
                t.0.insert(
                    model.clone(),
                    Buckets {
                        input: v["input"].as_u64().unwrap_or(0),
                        output: v["output"].as_u64().unwrap_or(0),
                        cache_read: v["cache_read"].as_u64().unwrap_or(0),
                        cache_creation: v["cache_creation"].as_u64().unwrap_or(0),
                    },
                );
            }
        }
        t
    }

    /// The `"models"` object value for persisting back into the payload.
    pub fn to_payload_value(&self) -> Value {
        let mut obj = serde_json::Map::new();
        for (model, b) in &self.0 {
            obj.insert(
                model.clone(),
                json!({
                    "input": b.input,
                    "output": b.output,
                    "cache_read": b.cache_read,
                    "cache_creation": b.cache_creation,
                }),
            );
        }
        Value::Object(obj)
    }

    /// Price every model via `price(model_id) -> ModelMeta`; returns
    /// `(total_usd, rows)`.
    pub fn to_model_costs(&self, price: impl Fn(&str) -> ModelMeta) -> (f64, Vec<ModelCost>) {
        let mut total = 0.0;
        let mut rows = Vec::new();
        for (model, b) in &self.0 {
            let usd = price(model).cost_usd(b.input, b.output, b.cache_read, b.cache_creation);
            total += usd;
            rows.push(ModelCost {
                model: model.clone(),
                input: b.input,
                output: b.output,
                cache_read: b.cache_read,
                cache_creation: b.cache_creation,
                usd,
            });
        }
        (total, rows)
    }

    /// The set of models with accumulated tokens (used to resolve pricing
    /// metadata for each one, asynchronously, before calling
    /// [`Tally::to_model_costs`]).
    pub fn model_ids(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_router::model_meta::ModelMeta;
    use serde_json::json;

    fn priced(input: f64, output: f64) -> ModelMeta {
        ModelMeta {
            context_window: 200_000,
            max_output_tokens: 8_192,
            supports_prompt_cache: true,
            supports_reasoning: false,
            cost_input: input,
            cost_output: output,
            cost_cache_read: 0.0,
            cost_cache_write: 0.0,
            ..crate::llm_router::model_meta::FALLBACK.clone()
        }
    }

    #[test]
    fn add_accumulates_across_turns_per_model() {
        let mut t = Tally::default();
        t.add("m1", 100, 40, 0, 0);
        t.add("m1", 50, 10, 0, 0);
        t.add("m2", 200, 0, 0, 0);
        let b = t.get("m1").unwrap();
        assert_eq!((b.input, b.output), (150, 50));
        assert_eq!(t.get("m2").unwrap().input, 200);
    }

    #[test]
    fn payload_round_trips_and_merges_with_context_snapshot() {
        let mut t = Tally::default();
        t.add("m1", 100, 40, 5, 2);
        // to_payload_value returns just the models object.
        let models = t.to_payload_value();
        let reloaded = Tally::from_payload(&json!({ "active_tokens": 9, "models": models }));
        assert_eq!(reloaded.get("m1").unwrap().output, 40);
        assert_eq!(reloaded.get("m1").unwrap().cache_creation, 2);
    }

    #[test]
    fn from_payload_without_models_is_empty() {
        let t = Tally::from_payload(&json!({ "active_tokens": 9 }));
        assert!(t.is_empty());
    }

    #[test]
    fn to_model_costs_prices_each_model_and_sums() {
        let mut t = Tally::default();
        t.add("m1", 1_000_000, 0, 0, 0); // $3 at rate 3.0
        t.add("m2", 0, 1_000_000, 0, 0); // $15 at output rate 15.0
        let (total, mut rows) = t.to_model_costs(|id| match id {
            "m1" => priced(3.0, 0.0),
            _ => priced(0.0, 15.0),
        });
        rows.sort_by(|a, b| a.model.cmp(&b.model));
        assert!((total - 18.0).abs() < 1e-9, "total {total}");
        assert!((rows[0].usd - 3.0).abs() < 1e-9);
        assert!((rows[1].usd - 15.0).abs() < 1e-9);
    }
}
