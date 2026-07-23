//! Reusable provider-conformance harness (plan Task 16, Step 1).
//!
//! A provider component reaches the LLM router through the generic
//! [`crate::plugins::wasm_provider`] seam. Before a real provider is migrated
//! onto that seam, it must prove — against MOCKED, allowlisted HTTP — that it
//! behaves like a provider: it lists models, completes in order, never leaks a
//! host/forged credential onto the wire, maps upstream HTTP errors onto the
//! right `provider-error`, and lets the host budget catch a stalled upstream
//! instead of hanging.
//!
//! This module is that proof harness, and it is DECOUPLED from any one
//! fixture's wire format or expected values: [`ProviderConformance`] is
//! parameterized by a [`ConformanceFixture`] — a compiled component artifact
//! + provider id, the mock upstream's per-endpoint response bodies (whatever
//! bytes THIS component's own `list-models`/`complete` parses; the harness
//! never parses them itself), and a [`ProviderExpectations`] struct describing
//! what the six checks should observe. Each check stands up a mock HTTP
//! upstream ([`MockUpstream`]) seeded with the fixture's wire bodies, points a
//! real [`WasmProviderTransport`] at it (via
//! [`crate::plugins::wasm_provider::build_test_transport_with_grants`],
//! granting the `ryuzi:http`/`ryuzi:storage` capabilities and seeding the
//! mock's base URL into the component's own storage slice — the generic
//! "endpoint override" channel a real provider component would read too), and
//! drives the actual host seam. The synthetic `component-provider-http`
//! fixture below is exactly ONE instantiation of a [`ConformanceFixture`];
//! later per-provider slices build one per REAL provider component (its own
//! JSON/SSE wire bodies + expected models/text) and call the SAME six checks.
//!
//! Everything here is `#[cfg(test)]` lib-test code (the integration-test build
//! OOMs on the dev box); the module is gated behind `#[cfg(test)]` in
//! `plugins/mod.rs`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::Router;

use crate::plugins::wasm_provider::{
    build_test_transport_with_grants, TestTransportGrants, WasmCompletionRequest, WasmModelInfo,
    WasmProviderRuntime, WasmProviderTransport, WasmTokenUsage,
};

/// The single loopback host every mock upstream binds to and every
/// conformance run allowlists (a bare host: the allowlist matches on host,
/// not port).
const LOOPBACK_HOST: &str = "127.0.0.1";

/// Storage key the harness seeds with the mock upstream's base URL; a
/// fixture's guest component reads it back through `ryuzi:storage` (the
/// `component-provider-http` fixture does — see its `src/lib.rs`).
const BASE_URL_STORAGE_KEY: &str = "conformance-base-url";

/// How the mock `/complete` endpoint should respond, per conformance
/// scenario. `Body`/`Status` bodies are caller-supplied (see
/// [`ConformanceFixture::wire`] / [`ProviderExpectations::http_error_cases`])
/// — the harness never hardcodes wire-level content.
#[derive(Clone)]
enum CompleteBehavior {
    /// `200 OK` with this body.
    Body(String),
    /// This HTTP status with a short error body (drives error mapping).
    Status(u16),
    /// Accept the request then stall far past any test budget (drives the
    /// host timeout budget catching a slow upstream).
    Stall,
}

/// A mock HTTP upstream the conformance harness points a provider transport
/// at. Serves `/models` (a caller-supplied success body) and `/complete` (per
/// [`CompleteBehavior`]), and records every `Authorization` header value it
/// receives so the auth-absence check can assert it saw none. Wire-agnostic:
/// it serves whatever bytes the caller hands it and never parses them.
struct MockUpstream {
    base_url: String,
    seen_authorization: Arc<Mutex<Vec<String>>>,
}

impl MockUpstream {
    /// Bind a fresh loopback listener, serve `/models` (always `models_body`)
    /// and `/complete` (per `complete`), and return the running upstream.
    async fn start(models_body: String, complete: CompleteBehavior) -> Self {
        let seen_authorization: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let models_seen = seen_authorization.clone();
        let models_route = get(move |headers: HeaderMap| {
            let seen = models_seen.clone();
            let body = models_body.clone();
            async move {
                record_authorization(&headers, &seen);
                (StatusCode::OK, body)
            }
        });

        let complete_seen = seen_authorization.clone();
        let complete_route = post(move |headers: HeaderMap| {
            let seen = complete_seen.clone();
            let behavior = complete.clone();
            async move {
                record_authorization(&headers, &seen);
                match behavior {
                    CompleteBehavior::Body(body) => (StatusCode::OK, body),
                    CompleteBehavior::Status(status) => (
                        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                        format!("upstream error {status}"),
                    ),
                    CompleteBehavior::Stall => {
                        // Far longer than any conformance budget: the host must
                        // catch this, not wait it out.
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        (StatusCode::OK, "too late".to_string())
                    }
                }
            }
        });

        let app = Router::new()
            .route("/models", models_route)
            .route("/complete", complete_route);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener should bind");
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        MockUpstream {
            base_url: format!("http://{LOOPBACK_HOST}:{port}"),
            seen_authorization,
        }
    }

    /// Every `Authorization` header value this upstream has received so far.
    fn authorization_headers_seen(&self) -> Vec<String> {
        self.seen_authorization.lock().unwrap().clone()
    }
}

/// Record any `Authorization` header on an incoming mock request.
fn record_authorization(headers: &HeaderMap, seen: &Arc<Mutex<Vec<String>>>) {
    if let Some(value) = headers.get(axum::http::header::AUTHORIZATION) {
        seen.lock()
            .unwrap()
            .push(value.to_str().unwrap_or("<non-utf8>").to_string());
    }
}

/// The mock upstream's wire-level response bodies for one fixture: whatever
/// format THIS provider component's own `list-models`/`complete` parses. The
/// harness serves these bytes verbatim over HTTP and never interprets them —
/// a real provider's config carries that provider's actual JSON/SSE bodies
/// instead of the synthetic fixture's tab-separated tables.
pub(crate) struct MockWireBodies {
    /// Body the mock's `GET /models` serves on success.
    pub models_body: String,
    /// Body the mock's `POST /complete` serves on success.
    pub complete_success_body: String,
}

/// One HTTP-error scenario the conformance battery drives: an upstream status
/// this provider component's own error-mapping logic must turn into a
/// `provider-error` whose rendered `Err(String)` contains `expected_substring`.
pub(crate) struct HttpErrorCase {
    pub status: u16,
    pub expected_substring: &'static str,
}

/// What the six-point battery expects THIS provider component to produce over
/// the same mocked host seam. One instance is one fixture's worth of expected
/// values; the checks themselves stay fixture-agnostic.
pub(crate) struct ProviderExpectations {
    /// Expected `list-models` result, in order.
    pub models: Vec<WasmModelInfo>,
    /// Expected `complete` chunk texts, in the EXACT order the upstream
    /// served them. Anti-tautology: a fixture's wire body should serve these
    /// NOT already in alphabetical/sorted order, so a harness that only
    /// checked set membership (rather than order) would still fail.
    pub chunk_texts: Vec<&'static str>,
    /// Expected concatenation of `chunk_texts` — the final completion text.
    pub final_text: &'static str,
    /// Expected usage carried by the terminal (last) chunk, if any.
    pub terminal_usage: Option<WasmTokenUsage>,
    /// HTTP-error scenarios to drive in turn (status -> expected substring),
    /// exercising this component's own upstream-status -> `provider-error`
    /// mapping.
    pub http_error_cases: Vec<HttpErrorCase>,
    /// Substrings acceptable in the mapped error when the upstream stalls
    /// past the host's timeout budget (providers may phrase this
    /// differently — rate-limited/unavailable/failed/timeout wording all
    /// count as "caught it, didn't hang").
    pub timeout_error_substrings: Vec<&'static str>,
    /// A literal the auth-absence check additionally asserts never leaks into
    /// the completion output, IF this fixture's guest forges one onto its
    /// requests. The substantive assertion — the mock never receives an
    /// `Authorization` header at all — always runs regardless of this field.
    pub guest_forged_secret: Option<&'static str>,
}

/// Everything [`ProviderConformance`] needs for one battery run: the compiled
/// component + provider id under test, the mock's wire-level response
/// bodies, and the expected outputs the six checks assert against. The
/// synthetic `component-provider-http` fixture
/// ([`synthetic_fixture_conformance`]) is exactly one instance of this; a
/// later slice builds one per REAL provider component instead.
pub(crate) struct ConformanceFixture {
    pub artifact: PathBuf,
    pub provider_id: String,
    /// The model id put on every `complete` request (the mock ignores it —
    /// scenario selection is driven by [`CompleteBehavior`] — but a real
    /// provider component may route on it, so it stays a genuine field).
    pub request_model: String,
    pub wire: MockWireBodies,
    pub expect: ProviderExpectations,
}

/// The reusable provider conformance battery: parameterized by a
/// [`ConformanceFixture`]. Later per-provider slices construct one per real
/// provider component and call [`Self::run_full_battery`] (or an individual
/// check) against it.
pub(crate) struct ProviderConformance {
    fixture: ConformanceFixture,
}

impl ProviderConformance {
    pub(crate) fn new(fixture: ConformanceFixture) -> Self {
        Self { fixture }
    }

    /// Build a real [`WasmProviderTransport`] over the component under test,
    /// granting `ryuzi:http` + `ryuzi:storage`, allowlisting the loopback
    /// mock, seeding the mock base URL into the component's storage slice,
    /// and bounding every call (and the host's own HTTP budget) by `timeout`.
    /// Delegates the actual bundle/context/policy wiring to
    /// [`build_test_transport_with_grants`] — the same builder
    /// `wasm_provider`'s own tests use — so that ~80 lines of boilerplate
    /// exists exactly once.
    async fn transport(
        &self,
        mock: &MockUpstream,
        timeout: Duration,
    ) -> (Arc<WasmProviderTransport>, tempfile::NamedTempFile) {
        build_test_transport_with_grants(
            self.fixture.artifact.clone(),
            &self.fixture.provider_id,
            timeout,
            TestTransportGrants {
                network_allowlist: vec![LOOPBACK_HOST.to_string()],
                allow_storage: true,
                storage_seed: vec![(
                    BASE_URL_STORAGE_KEY.to_string(),
                    mock.base_url.as_bytes().to_vec(),
                )],
            },
        )
        .await
    }

    /// The request every check drives `complete` with (a real prompt; the
    /// fixture reads its upstream from storage, not from the prompt, so this
    /// stays a genuine provider request regardless of which fixture is under
    /// test).
    fn completion_request(&self) -> WasmCompletionRequest {
        WasmCompletionRequest {
            model: self.fixture.request_model.clone(),
            prompt: "hello".to_string(),
            max_tokens: Some(64),
            temperature: Some(0.2),
        }
    }

    /// Run the whole six-point battery in sequence.
    pub(crate) async fn run_full_battery(&self) {
        self.assert_lists_models().await;
        self.assert_completes_in_order().await;
        self.assert_strips_guest_authorization().await;
        self.assert_maps_http_errors().await;
        self.assert_maps_timeouts().await;
    }

    /// (1) Model listing: `list-models` returns exactly the models the mock
    /// `/models` endpoint served, in order.
    pub(crate) async fn assert_lists_models(&self) {
        let mock = MockUpstream::start(
            self.fixture.wire.models_body.clone(),
            CompleteBehavior::Body(self.fixture.wire.complete_success_body.clone()),
        )
        .await;
        let (transport, _tmp) = self.transport(&mock, Duration::from_secs(10)).await;

        let models = transport
            .list_models()
            .await
            .expect("list-models over mocked HTTP must succeed");

        assert_eq!(
            models, self.fixture.expect.models,
            "list-models must return the models the mock /models endpoint served",
        );
    }

    /// (2) Non-stream completion + (3) stream ordering: `complete` returns the
    /// served chunks in their exact served order, the concatenation is the
    /// expected final text, and the terminal chunk carries `finished` + usage.
    pub(crate) async fn assert_completes_in_order(&self) {
        let mock = MockUpstream::start(
            self.fixture.wire.models_body.clone(),
            CompleteBehavior::Body(self.fixture.wire.complete_success_body.clone()),
        )
        .await;
        let (transport, _tmp) = self.transport(&mock, Duration::from_secs(10)).await;

        let chunks = transport
            .complete(self.completion_request())
            .await
            .expect("complete over mocked HTTP must succeed");

        let texts: Vec<&str> = chunks.iter().map(|chunk| chunk.text.as_str()).collect();
        assert_eq!(
            texts, self.fixture.expect.chunk_texts,
            "chunk order must be preserved exactly as the upstream served it",
        );

        let final_text: String = chunks.iter().map(|chunk| chunk.text.as_str()).collect();
        assert_eq!(
            final_text.as_str(),
            self.fixture.expect.final_text,
            "the concatenated completion must be the expected final text",
        );

        let (last, rest) = chunks
            .split_last()
            .expect("a conformance fixture must serve at least one chunk");
        assert!(
            rest.iter().all(|chunk| !chunk.finished),
            "only the terminal chunk may be finished",
        );
        assert!(last.finished, "the terminal chunk must be finished");
        assert_eq!(
            last.usage, self.fixture.expect.terminal_usage,
            "the terminal chunk must carry the expected token usage",
        );
    }

    /// (4) Auth absence: the guest cannot get a forged/host credential onto
    /// the wire; the host must strip any `Authorization` it sends, so the
    /// mock upstream must never see one, and the guest must never surface one.
    pub(crate) async fn assert_strips_guest_authorization(&self) {
        let mock = MockUpstream::start(
            self.fixture.wire.models_body.clone(),
            CompleteBehavior::Body(self.fixture.wire.complete_success_body.clone()),
        )
        .await;
        let (transport, _tmp) = self.transport(&mock, Duration::from_secs(10)).await;

        let chunks = transport
            .complete(self.completion_request())
            .await
            .expect("complete over mocked HTTP must succeed");

        let seen = mock.authorization_headers_seen();
        assert!(
            seen.is_empty(),
            "the guest's Authorization must be stripped before it reaches \
             the upstream, but the mock saw: {seen:?}",
        );

        if let Some(secret) = self.fixture.expect.guest_forged_secret {
            let final_text: String = chunks.iter().map(|chunk| chunk.text.as_str()).collect();
            assert!(
                !final_text.contains(secret),
                "the guest must not surface a forged/host secret in its output",
            );
        }
    }

    /// (5) HTTP error mapping: each configured upstream status maps to a
    /// `provider-error` surfaced as `Err(String)` containing the expected
    /// substring (e.g. a `429` to rate-limited, a `5xx` to unavailable).
    pub(crate) async fn assert_maps_http_errors(&self) {
        for case in &self.fixture.expect.http_error_cases {
            let mock = MockUpstream::start(
                self.fixture.wire.models_body.clone(),
                CompleteBehavior::Status(case.status),
            )
            .await;
            let (transport, _tmp) = self.transport(&mock, Duration::from_secs(10)).await;
            let error = transport
                .complete(self.completion_request())
                .await
                .expect_err(&format!(
                    "a {} upstream must surface as Err, not a chunk list",
                    case.status
                ));
            assert!(
                error.contains(case.expected_substring),
                "a {} upstream must map to an error containing {:?}, got: {error}",
                case.status,
                case.expected_substring,
            );
        }
    }

    /// (6) Timeout mapping: a stalled upstream is caught by the host's per-call
    /// budget (its HTTP timeout, and the epoch budget behind it) and surfaces
    /// promptly as `Err` — never a hang or panic.
    pub(crate) async fn assert_maps_timeouts(&self) {
        let mock = MockUpstream::start(
            self.fixture.wire.models_body.clone(),
            CompleteBehavior::Stall,
        )
        .await;
        let budget = Duration::from_millis(600);
        let (transport, _tmp) = self.transport(&mock, budget).await;

        let started = Instant::now();
        let error = transport
            .complete(self.completion_request())
            .await
            .expect_err("a stalled upstream must be caught by the host budget, not hang");
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(10),
            "the host budget must catch the stall promptly (well under the mock's \
             30s stall), but the call took {elapsed:?}",
        );
        assert!(
            self.fixture
                .expect
                .timeout_error_substrings
                .iter()
                .any(|substring| error.contains(substring)),
            "a caught stall must read as a timeout/failure provider-error, got: {error}",
        );
    }
}

/// The model table the synthetic fixture's mock `/models` endpoint serves
/// (`id\tdisplay\tcontext` per line). Two models in a fixed order so listing +
/// order are both checked. Fixture-specific data — see
/// [`synthetic_fixture_conformance`], NOT the generic harness above.
const MODELS_BODY: &str = "fixture-model\tFixture Model\t8192\nfixture-mini\tFixture Mini\t4096";

/// The completion table the synthetic fixture's mock `/complete` endpoint
/// serves in the success scenario (`text\tfinished[\tinput\toutput]` per
/// line). Deliberately NOT in alphabetical order, so a harness that only
/// checked set membership (rather than order) would still pass — the order
/// assertion must pin `Zeta, Alpha, Mu` exactly. Fixture-specific data.
const OK_CHUNKS_BODY: &str = "Zeta\tfalse\nAlpha\tfalse\nMu\ttrue\t11\t3";

/// The `Authorization` value the synthetic fixture forges onto every request
/// (see the fixture's own `src/lib.rs`); the host must strip it, so it must
/// NEVER reach the upstream, and the completion output must never contain it.
const FORBIDDEN_AUTHORIZATION: &str = "Bearer guest-forbidden-secret";

/// The prebuilt `component-provider-http` fixture artifact (built on demand by
/// [`crate::plugins::build_fixture_components_once`]).
fn provider_http_fixture_artifact() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/component-provider-http/target/wasm32-wasip2/release")
        .join("ryuzi_component_provider_http_fixture.wasm")
}

/// The synthetic `component-provider-http` fixture's own conformance config:
/// exactly what its tab-separated wire format and hand-written guest logic
/// produce. A later slice builds a [`ConformanceFixture`] per REAL provider
/// component instead — same six checks, different wire bodies + expectations.
fn synthetic_fixture_conformance() -> ConformanceFixture {
    ConformanceFixture {
        artifact: provider_http_fixture_artifact(),
        provider_id: "wasm-prov-conformance".to_string(),
        request_model: "fixture-model".to_string(),
        wire: MockWireBodies {
            models_body: MODELS_BODY.to_string(),
            complete_success_body: OK_CHUNKS_BODY.to_string(),
        },
        expect: ProviderExpectations {
            models: vec![
                WasmModelInfo {
                    id: "fixture-model".to_string(),
                    display_name: "Fixture Model".to_string(),
                    context_window: 8192,
                },
                WasmModelInfo {
                    id: "fixture-mini".to_string(),
                    display_name: "Fixture Mini".to_string(),
                    context_window: 4096,
                },
            ],
            chunk_texts: vec!["Zeta", "Alpha", "Mu"],
            final_text: "ZetaAlphaMu",
            terminal_usage: Some(WasmTokenUsage {
                input: 11,
                output: 3,
            }),
            http_error_cases: vec![
                HttpErrorCase {
                    status: 429,
                    expected_substring: "rate limited",
                },
                HttpErrorCase {
                    status: 503,
                    expected_substring: "unavailable",
                },
            ],
            timeout_error_substrings: vec![
                "failed",
                "timeout",
                "timed out",
                "budget",
                "unavailable",
            ],
            guest_forged_secret: Some(FORBIDDEN_AUTHORIZATION),
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_component_passes_the_full_conformance_battery() {
    crate::plugins::build_fixture_components_once();
    let harness = ProviderConformance::new(synthetic_fixture_conformance());
    harness.run_full_battery().await;
}
