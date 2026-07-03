//! Telemetry seam ported from the retired TS `observability` layer
//! (`packages/core/src/observability/{types,console}.ts`).
//!
//! `Telemetry` is the seam `ControlPlane` instruments against. `NoopTelemetry`
//! is the default (used by [`crate::control::ControlPlane::new`]); daemon
//! wiring later picks `ConsoleTelemetry` (or an OTLP impl) based on config.
//! Every implementation MUST be infallible — telemetry must never panic or
//! otherwise interrupt core control-flow.

use serde_json::Value;

/// Ordered key/value attributes attached to a span/count/record event.
///
/// A `Vec` (not a map) because attribute keys are small, static, call-site
/// literals — insertion order is preserved into the emitted JSON, and a
/// `Vec` avoids pulling in a map type for what's almost always 0-3 entries.
pub type Attrs = Vec<(&'static str, String)>;

/// A single in-flight span. Attributes may be added any time before `end()`;
/// `set_error` marks the span as failed without ending it. `end` consumes the
/// span (by `Box`) so it can only be closed once.
pub trait Span: Send {
    fn set_attribute(&mut self, key: &'static str, value: String);
    fn set_error(&mut self, message: &str);
    fn end(self: Box<Self>);
}

/// The telemetry seam `ControlPlane` (and later the daemon) instruments
/// against. All methods are infallible — implementations must never panic or
/// propagate an error into core control-flow.
pub trait Telemetry: Send + Sync {
    fn start_span(&self, name: &'static str, attrs: Attrs) -> Box<dyn Span>;
    fn count(&self, name: &'static str, attrs: Attrs);
    fn record(&self, name: &'static str, value: f64, attrs: Attrs);
    /// Flush/close any underlying exporter. Default no-op; only exporting
    /// implementations (e.g. OTLP) need to override this.
    fn shutdown(&self) {}
    /// Debug-only identifier for the concrete backend (e.g. `"console"`,
    /// `"otel"`, `"noop"`). Never used by production logic — it exists so
    /// tests (notably `select_telemetry`'s) can probe which backend was
    /// chosen without downcasting the trait object.
    fn backend_name(&self) -> &'static str {
        "unknown"
    }
}

fn attrs_to_json(attrs: &Attrs) -> Value {
    Value::Object(
        attrs
            .iter()
            .map(|(k, v)| ((*k).to_string(), Value::String(v.clone())))
            .collect(),
    )
}

struct NoopSpan;

impl Span for NoopSpan {
    fn set_attribute(&mut self, _key: &'static str, _value: String) {}
    fn set_error(&mut self, _message: &str) {}
    fn end(self: Box<Self>) {}
}

/// A `Telemetry` that observes nothing — the default when no telemetry
/// backend is configured.
pub struct NoopTelemetry;

impl Telemetry for NoopTelemetry {
    fn start_span(&self, _name: &'static str, _attrs: Attrs) -> Box<dyn Span> {
        Box::new(NoopSpan)
    }
    fn count(&self, _name: &'static str, _attrs: Attrs) {}
    fn record(&self, _name: &'static str, _value: f64, _attrs: Attrs) {}
    fn backend_name(&self) -> &'static str {
        "noop"
    }
}

/// A `Telemetry` that renders each event as one JSON line via `sink`,
/// mirroring the TS `ConsoleTelemetry` (`observability/console.ts`) shapes:
/// - span: `{"kind":"span","name":..,"attrs":{..},"durationMs":N[,"error":msg]}`
/// - count: `{"kind":"count","name":..,"attrs":{..}}`
/// - record: `{"kind":"record","name":..,"value":N,"attrs":{..}}`
pub struct ConsoleTelemetry {
    sink: std::sync::Arc<dyn Fn(&str) + Send + Sync>,
    clock: fn() -> i64,
}

impl ConsoleTelemetry {
    /// Default console telemetry: writes each line to stderr, uses the
    /// wall-clock (`crate::paths::now_ms`) for span durations.
    pub fn new() -> Self {
        Self {
            sink: std::sync::Arc::new(|line: &str| eprintln!("{line}")),
            clock: crate::paths::now_ms,
        }
    }

    /// Inject a sink + clock — used by tests to capture emitted lines and to
    /// control span durations deterministically.
    pub fn with_sink(sink: impl Fn(&str) + Send + Sync + 'static, clock: fn() -> i64) -> Self {
        Self {
            sink: std::sync::Arc::new(sink),
            clock,
        }
    }
}

impl Default for ConsoleTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

struct ConsoleSpan {
    name: &'static str,
    attrs: Attrs,
    error: Option<String>,
    start: i64,
    sink: std::sync::Arc<dyn Fn(&str) + Send + Sync>,
    clock: fn() -> i64,
}

impl Span for ConsoleSpan {
    fn set_attribute(&mut self, key: &'static str, value: String) {
        self.attrs.push((key, value));
    }

    fn set_error(&mut self, message: &str) {
        self.error = Some(message.to_string());
    }

    fn end(self: Box<Self>) {
        let duration_ms = (self.clock)() - self.start;
        let mut line = serde_json::json!({
            "kind": "span",
            "name": self.name,
            "attrs": attrs_to_json(&self.attrs),
            "durationMs": duration_ms,
        });
        if let Some(err) = &self.error {
            line.as_object_mut()
                .expect("span line is always a JSON object")
                .insert("error".to_string(), Value::String(err.clone()));
        }
        (self.sink)(&line.to_string());
    }
}

impl Telemetry for ConsoleTelemetry {
    fn start_span(&self, name: &'static str, attrs: Attrs) -> Box<dyn Span> {
        Box::new(ConsoleSpan {
            name,
            attrs,
            error: None,
            start: (self.clock)(),
            sink: self.sink.clone(),
            clock: self.clock,
        })
    }

    fn count(&self, name: &'static str, attrs: Attrs) {
        let line = serde_json::json!({
            "kind": "count",
            "name": name,
            "attrs": attrs_to_json(&attrs),
        });
        (self.sink)(&line.to_string());
    }

    fn record(&self, name: &'static str, value: f64, attrs: Attrs) {
        let line = serde_json::json!({
            "kind": "record",
            "name": name,
            "value": value,
            "attrs": attrs_to_json(&attrs),
        });
        (self.sink)(&line.to_string());
    }

    fn backend_name(&self) -> &'static str {
        "console"
    }
}

/// OTLP/HTTP telemetry backend, ported from the retired TS
/// `observability/otel.ts` adapter: batched span export to
/// `{endpoint}/v1/traces`, periodic metric export to `{endpoint}/v1/metrics`,
/// service name `ryuzi`. Every `Telemetry`/`Span` method is infallible —
/// failures are swallowed so telemetry can never interrupt core
/// control-flow. (Construction can fail — see [`OtelTelemetry::new`] — so
/// callers keep a `ConsoleTelemetry` fallback for a bad/unreachable
/// endpoint.)
#[cfg(feature = "otel")]
pub struct OtelTelemetry {
    tracer_provider: opentelemetry_sdk::trace::SdkTracerProvider,
    meter_provider: opentelemetry_sdk::metrics::SdkMeterProvider,
    tracer: opentelemetry_sdk::trace::SdkTracer,
    meter: opentelemetry::metrics::Meter,
    counters: std::sync::Mutex<
        std::collections::HashMap<&'static str, opentelemetry::metrics::Counter<u64>>,
    >,
    histograms: std::sync::Mutex<
        std::collections::HashMap<&'static str, opentelemetry::metrics::Histogram<f64>>,
    >,
}

/// A minimal `std::error::Error` wrapper so `Span::set_error`'s `&str`
/// message can be handed to `opentelemetry::trace::Span::record_error`
/// (which requires `&dyn Error`).
#[cfg(feature = "otel")]
#[derive(Debug)]
struct OtelErrorMessage(String);

#[cfg(feature = "otel")]
impl std::fmt::Display for OtelErrorMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(feature = "otel")]
impl std::error::Error for OtelErrorMessage {}

#[cfg(feature = "otel")]
fn attrs_to_kvs(attrs: &Attrs) -> Vec<opentelemetry::KeyValue> {
    attrs
        .iter()
        .map(|(k, v)| opentelemetry::KeyValue::new(*k, v.clone()))
        .collect()
}

#[cfg(feature = "otel")]
impl OtelTelemetry {
    /// Build the OTLP/HTTP exporters + providers against `endpoint`. Spans
    /// are exported to `{endpoint}/v1/traces`, metrics to
    /// `{endpoint}/v1/metrics` — the otlp-http exporter appends the
    /// signal-specific path to `endpoint` automatically. Service name is
    /// fixed at `ryuzi` (TS parity).
    pub fn new(endpoint: &str) -> anyhow::Result<Self> {
        use opentelemetry::trace::TracerProvider as _;
        use opentelemetry_otlp::WithExportConfig;

        let resource = opentelemetry_sdk::Resource::builder()
            .with_service_name("ryuzi")
            .build();

        let span_exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint(endpoint)
            .build()?;
        let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_resource(resource.clone())
            .with_batch_exporter(span_exporter)
            .build();

        let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_endpoint(endpoint)
            .build()?;
        let reader = opentelemetry_sdk::metrics::PeriodicReader::builder(metric_exporter).build();
        let meter_provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
            .with_resource(resource)
            .with_reader(reader)
            .build();

        let tracer = tracer_provider.tracer("ryuzi");
        let meter = {
            use opentelemetry::metrics::MeterProvider as _;
            meter_provider.meter("ryuzi")
        };

        Ok(Self {
            tracer_provider,
            meter_provider,
            tracer,
            meter,
            counters: std::sync::Mutex::new(std::collections::HashMap::new()),
            histograms: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }
}

#[cfg(feature = "otel")]
struct OtelSpan(opentelemetry_sdk::trace::Span);

#[cfg(feature = "otel")]
impl Span for OtelSpan {
    fn set_attribute(&mut self, key: &'static str, value: String) {
        use opentelemetry::trace::Span as _;
        self.0
            .set_attribute(opentelemetry::KeyValue::new(key, value));
    }

    fn set_error(&mut self, message: &str) {
        use opentelemetry::trace::Span as _;
        self.0.record_error(&OtelErrorMessage(message.to_string()));
        self.0
            .set_status(opentelemetry::trace::Status::error(message.to_string()));
    }

    fn end(mut self: Box<Self>) {
        use opentelemetry::trace::Span as _;
        self.0.end();
    }
}

#[cfg(feature = "otel")]
impl Telemetry for OtelTelemetry {
    fn start_span(&self, name: &'static str, attrs: Attrs) -> Box<dyn Span> {
        use opentelemetry::trace::{Span as _, Tracer as _};
        let mut span = self.tracer.start(name);
        for kv in attrs_to_kvs(&attrs) {
            span.set_attribute(kv);
        }
        Box::new(OtelSpan(span))
    }

    fn count(&self, name: &'static str, attrs: Attrs) {
        let Ok(mut counters) = self.counters.lock() else {
            return;
        };
        let counter = counters
            .entry(name)
            .or_insert_with(|| self.meter.u64_counter(name).build());
        counter.add(1, &attrs_to_kvs(&attrs));
    }

    fn record(&self, name: &'static str, value: f64, attrs: Attrs) {
        let Ok(mut histograms) = self.histograms.lock() else {
            return;
        };
        let histogram = histograms
            .entry(name)
            .or_insert_with(|| self.meter.f64_histogram(name).build());
        histogram.record(value, &attrs_to_kvs(&attrs));
    }

    fn shutdown(&self) {
        let _ = self.tracer_provider.shutdown();
        let _ = self.meter_provider.shutdown();
    }

    fn backend_name(&self) -> &'static str {
        "otel"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A sink that appends every emitted line to a shared `Vec`, plus the
    /// handle tests use to inspect what was captured.
    fn capturing_sink() -> (
        Arc<Mutex<Vec<String>>>,
        impl Fn(&str) + Send + Sync + 'static,
    ) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let captured = lines.clone();
        (lines, move |line: &str| {
            captured.lock().unwrap().push(line.to_string())
        })
    }

    /// Parse every captured line as JSON (never string-compare — the shape,
    /// not incidental formatting, is the contract).
    fn parse_lines(lines: &Arc<Mutex<Vec<String>>>) -> Vec<Value> {
        lines
            .lock()
            .unwrap()
            .iter()
            .map(|l| serde_json::from_str(l).expect("sink line must be valid JSON"))
            .collect()
    }

    fn fixed_clock() -> i64 {
        1_000
    }

    #[test]
    fn span_end_emits_span_shape_with_attrs_and_duration() {
        let (lines, sink) = capturing_sink();
        let telemetry = ConsoleTelemetry::with_sink(sink, fixed_clock);

        let mut span = telemetry.start_span("harness.run", vec![("session_pk", "abc".to_string())]);
        span.set_attribute("extra", "1".to_string());
        span.end();

        let parsed = parse_lines(&lines);
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0],
            serde_json::json!({
                "kind": "span",
                "name": "harness.run",
                "attrs": { "session_pk": "abc", "extra": "1" },
                "durationMs": 0,
            })
        );
    }

    #[test]
    fn span_end_includes_error_when_set() {
        let (lines, sink) = capturing_sink();
        let telemetry = ConsoleTelemetry::with_sink(sink, fixed_clock);

        let mut span = telemetry.start_span("harness.run", vec![]);
        span.set_error("boom");
        span.end();

        let parsed = parse_lines(&lines);
        assert_eq!(
            parsed[0],
            serde_json::json!({
                "kind": "span",
                "name": "harness.run",
                "attrs": {},
                "durationMs": 0,
                "error": "boom",
            })
        );
    }

    #[test]
    fn span_duration_reflects_clock_delta() {
        // A clock local to THIS test (its own static) so parallel test
        // threads never share the counter with another test's clock.
        fn clock() -> i64 {
            use std::sync::atomic::{AtomicI64, Ordering};
            static TICK: AtomicI64 = AtomicI64::new(0);
            TICK.fetch_add(25, Ordering::SeqCst)
        }
        let (lines, sink) = capturing_sink();
        let telemetry = ConsoleTelemetry::with_sink(sink, clock);

        let span = telemetry.start_span("x", vec![]);
        span.end();

        let parsed = parse_lines(&lines);
        assert_eq!(parsed[0]["durationMs"], 25);
    }

    #[test]
    fn count_emits_count_shape() {
        let (lines, sink) = capturing_sink();
        let telemetry = ConsoleTelemetry::with_sink(sink, fixed_clock);

        telemetry.count("session.run", vec![]);
        telemetry.count("approval.allow", vec![("tool", "bash".to_string())]);

        let parsed = parse_lines(&lines);
        assert_eq!(
            parsed[0],
            serde_json::json!({ "kind": "count", "name": "session.run", "attrs": {} })
        );
        assert_eq!(
            parsed[1],
            serde_json::json!({
                "kind": "count",
                "name": "approval.allow",
                "attrs": { "tool": "bash" },
            })
        );
    }

    #[test]
    fn record_emits_record_shape() {
        let (lines, sink) = capturing_sink();
        let telemetry = ConsoleTelemetry::with_sink(sink, fixed_clock);

        telemetry.record("latency_ms", 42.5, vec![("op", "x".to_string())]);

        let parsed = parse_lines(&lines);
        assert_eq!(
            parsed[0],
            serde_json::json!({
                "kind": "record",
                "name": "latency_ms",
                "value": 42.5,
                "attrs": { "op": "x" },
            })
        );
    }

    #[test]
    fn noop_telemetry_never_panics_and_emits_nothing() {
        let telemetry = NoopTelemetry;
        let mut span = telemetry.start_span("x", vec![("k", "v".to_string())]);
        span.set_attribute("a", "b".to_string());
        span.set_error("err");
        span.end();
        telemetry.count("c", vec![]);
        telemetry.record("r", 1.0, vec![]);
        telemetry.shutdown();
    }

    #[cfg(feature = "otel")]
    #[test]
    fn otel_telemetry_constructs_against_dummy_endpoint_without_panicking() {
        // Port 9 ("discard") is never a live OTLP collector; construction
        // (building exporters/providers) must still succeed — export only
        // happens async, later, off the batch/periodic timers.
        let telemetry =
            OtelTelemetry::new("http://localhost:9").expect("construction must not fail");

        let mut span = telemetry.start_span("harness.run", vec![("k", "v".to_string())]);
        span.set_attribute("extra", "1".to_string());
        span.set_error("boom");
        span.end();

        telemetry.count("session.run", vec![]);
        telemetry.record("latency_ms", 42.5, vec![]);
    }

    #[cfg(feature = "otel")]
    #[test]
    fn otel_telemetry_shutdown_is_infallible() {
        let telemetry = OtelTelemetry::new("http://localhost:9").expect("construction");
        // `shutdown` returns nothing (not a Result) — the call itself
        // succeeding (no panic) is the assertion.
        telemetry.shutdown();
        // Calling it twice must also not panic (providers already shut down).
        telemetry.shutdown();
    }

    // `build_daemon` (the real call site) is async and already runs inside a
    // tokio runtime — the otlp-http exporter's blocking reqwest client must
    // not panic ("cannot start a runtime from within a runtime") when built
    // from that context.
    #[cfg(feature = "otel")]
    #[tokio::test]
    async fn otel_telemetry_constructs_inside_a_tokio_runtime_without_panicking() {
        let telemetry = OtelTelemetry::new("http://localhost:9").expect("construction");
        telemetry.shutdown();
    }
}
