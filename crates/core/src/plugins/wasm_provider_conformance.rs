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
//! upstream ([`MockUpstream`]) on the fixture's own endpoint paths and seeded
//! with its wire bodies, points a real [`WasmProviderTransport`] at it (via
//! [`crate::plugins::wasm_provider::build_test_transport_with_grants`],
//! granting the `ryuzi:http`/`ryuzi:storage` capabilities — plus
//! `ryuzi:provider-auth` and a stored user credential for a fixture that
//! declares one — and seeding the mock's base URL into the component's own
//! storage slice under the fixture's override key, the generic "endpoint
//! override" channel a real provider component would read too), and drives the
//! actual host seam.
//!
//! Two fixtures live below and share the SAME six checks: the synthetic
//! `component-provider-http` fixture (plain `ryuzi:http`, tab-separated wire
//! format) and the real `plugins/openai` component (host-mediated
//! `ryuzi:provider-auth`, OpenAI JSON). Later per-provider slices add one more
//! [`ConformanceFixture`] each — never another copy of the checks.
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
/// at. Serves the fixture's model-list path (a caller-supplied success body)
/// and its completion path (per [`CompleteBehavior`]), and records every
/// `Authorization` header value it receives so the auth check can assert on
/// exactly what reached the wire. Wire-agnostic: it serves whatever bytes the
/// caller hands it, on whatever paths, and never parses them.
struct MockUpstream {
    base_url: String,
    seen_authorization: Arc<Mutex<Vec<String>>>,
}

impl MockUpstream {
    /// Bind a fresh loopback listener, serve the fixture's model-list path
    /// (always `wire.models_body`) and its completion path (per `complete`),
    /// and return the running upstream. The paths come from the fixture
    /// because they are this provider's REAL endpoint paths relative to its
    /// base URL (`/models` + `/chat/completions` for an OpenAI-format
    /// provider) — the component builds its own URLs, so the mock has to meet
    /// it where it actually knocks.
    async fn start(wire: &MockWireBodies, complete: CompleteBehavior) -> Self {
        let models_body = wire.models_body.clone();
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
            .route(&wire.models_path, models_route)
            .route(&wire.complete_path, complete_route);

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

/// The mock upstream's wire surface for one fixture: the endpoint paths THIS
/// provider component actually requests, and the response bodies in whatever
/// format its own `list-models`/`complete` parses. The harness serves these
/// bytes verbatim over HTTP and never interprets them — a real provider's
/// config carries that provider's actual paths and JSON bodies instead of the
/// synthetic fixture's `/complete` + tab-separated tables.
pub(crate) struct MockWireBodies {
    /// Path the component GETs its model list from, relative to the base URL.
    pub models_path: String,
    /// Body the mock's model-list route serves on success.
    pub models_body: String,
    /// Path the component POSTs completions to, relative to the base URL.
    pub complete_path: String,
    /// Body the mock's completion route serves on success.
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
    /// requests. The substantive assertion — what the mock is allowed to see
    /// on the wire, per [`Self::expected_authorization`] — always runs
    /// regardless of this field.
    pub guest_forged_secret: Option<&'static str>,
    /// The ONLY `Authorization` value the mock upstream may observe.
    ///
    /// `None` — the component authenticates through plain `ryuzi:http`, so no
    /// `Authorization` may reach the upstream at all: the host strips whatever
    /// the guest sets, and the check asserts the mock saw none.
    ///
    /// `Some(value)` — the component authenticates through
    /// `ryuzi:provider-auth`, so the HOST puts the user's stored credential on
    /// the wire. The check then asserts every observed header equals exactly
    /// this host-injected value, which is the same guarantee stated the other
    /// way round: nothing a guest could contribute ever appears.
    pub expected_authorization: Option<&'static str>,
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
    /// Key in the component's own `ryuzi:storage` slice that the harness
    /// seeds with the mock's base URL. Fixture-owned because it is the
    /// component's OWN endpoint-override contract (the synthetic fixture reads
    /// `conformance-base-url`; the openai component reads `base-url`, its real
    /// product-level proxy override).
    pub base_url_storage_key: String,
    /// A user API key to store for [`Self::provider_id`], which also declares
    /// that id in the test bundle's manifest and so grants
    /// `ryuzi:provider-auth`. `Some` for a component that authenticates
    /// host-mediated (an API-key provider); `None` for one that reaches the
    /// upstream through plain `ryuzi:http`.
    pub stored_api_key: Option<&'static str>,
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
    /// granting `ryuzi:http` + `ryuzi:storage` (plus `ryuzi:provider-auth` and
    /// a stored user credential when the fixture declares one), allowlisting
    /// the loopback mock, seeding the mock base URL into the component's
    /// storage slice under the fixture's own override key, and bounding every
    /// call (and the host's own HTTP budget) by `timeout`. Delegates the
    /// actual bundle/context/policy wiring to
    /// [`build_test_transport_with_grants`] — the same builder
    /// `wasm_provider`'s own tests use — so that ~80 lines of boilerplate
    /// exists exactly once.
    async fn transport(
        &self,
        mock: &MockUpstream,
        timeout: Duration,
    ) -> (Arc<WasmProviderTransport>, tempfile::NamedTempFile) {
        let provider_auth = self
            .fixture
            .stored_api_key
            .map(|api_key| (self.fixture.provider_id.clone(), api_key.to_string()));
        build_test_transport_with_grants(
            self.fixture.artifact.clone(),
            &self.fixture.provider_id,
            timeout,
            TestTransportGrants {
                network_allowlist: vec![LOOPBACK_HOST.to_string()],
                allow_storage: true,
                storage_seed: vec![(
                    self.fixture.base_url_storage_key.clone(),
                    mock.base_url.as_bytes().to_vec(),
                )],
                provider_ids: provider_auth
                    .iter()
                    .map(|(provider, _)| provider.clone())
                    .collect(),
                provider_credentials: provider_auth.into_iter().collect(),
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
            &self.fixture.wire,
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
            &self.fixture.wire,
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

    /// (4) Auth absence: nothing a GUEST contributes can reach the wire as a
    /// credential. For a plain-`ryuzi:http` component that means the mock
    /// upstream sees no `Authorization` at all (the host strips the one the
    /// guest forges); for a `ryuzi:provider-auth` component it means the only
    /// value the mock ever sees is the host-injected one
    /// ([`ProviderExpectations::expected_authorization`]). Either way the guest
    /// must never surface a credential in its output.
    pub(crate) async fn assert_strips_guest_authorization(&self) {
        let mock = MockUpstream::start(
            &self.fixture.wire,
            CompleteBehavior::Body(self.fixture.wire.complete_success_body.clone()),
        )
        .await;
        let (transport, _tmp) = self.transport(&mock, Duration::from_secs(10)).await;

        let chunks = transport
            .complete(self.completion_request())
            .await
            .expect("complete over mocked HTTP must succeed");

        let seen = mock.authorization_headers_seen();
        match self.fixture.expect.expected_authorization {
            None => assert!(
                seen.is_empty(),
                "the guest's Authorization must be stripped before it reaches \
                 the upstream, but the mock saw: {seen:?}",
            ),
            Some(expected) => {
                assert!(
                    !seen.is_empty(),
                    "the host must have injected the stored credential, but the \
                     mock saw no Authorization at all",
                );
                assert!(
                    seen.iter().all(|value| value == expected),
                    "only the HOST-injected credential may reach the upstream \
                     (expected every value to be {expected:?}), but the mock saw: {seen:?}",
                );
            }
        }

        if let Some(secret) = self.fixture.expect.guest_forged_secret {
            assert!(
                !seen.iter().any(|value| value.contains(secret)),
                "a guest-forged credential must never reach the upstream, but \
                 the mock saw: {seen:?}",
            );
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
            let mock =
                MockUpstream::start(&self.fixture.wire, CompleteBehavior::Status(case.status))
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
        let mock = MockUpstream::start(&self.fixture.wire, CompleteBehavior::Stall).await;
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
        base_url_storage_key: "conformance-base-url".to_string(),
        // Plain `ryuzi:http` egress: no provider-auth grant, no stored key.
        stored_api_key: None,
        wire: MockWireBodies {
            models_path: "/models".to_string(),
            models_body: MODELS_BODY.to_string(),
            complete_path: "/complete".to_string(),
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
            // Plain `ryuzi:http`: the host strips the guest's forged header
            // and injects nothing, so the upstream must see NO Authorization.
            expected_authorization: None,
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_component_passes_the_full_conformance_battery() {
    crate::plugins::build_fixture_components_once();
    let harness = ProviderConformance::new(synthetic_fixture_conformance());
    harness.run_full_battery().await;
}

// ---------------------------------------------------------------------------
// The REAL `openai` provider component (plan Task 16, Step 2)
//
// Same six checks, a real component, and OpenAI's actual wire format. Nothing
// below touches the harness itself — it is one more [`ConformanceFixture`],
// which is the point of the Step 1 parameterization.
// ---------------------------------------------------------------------------

/// The user API key this run stores for `openai` through the real
/// `provider_connections` path. `ryuzi:provider-auth` resolves it host-side and
/// injects it as `Authorization: Bearer …` (the `openai` descriptor declares
/// `AuthScheme::Bearer`), so the component itself never sees this value.
const OPENAI_STORED_KEY: &str = "sk-conformance-openai-key";

/// Exactly what the mock upstream must therefore observe — and nothing else.
const OPENAI_EXPECTED_AUTHORIZATION: &str = "Bearer sk-conformance-openai-key";

/// An OpenAI `GET /v1/models` body. Two models, deliberately NOT in the order
/// the component's expectations would take if it sorted, and one id the
/// component's static context-window table does not know (so the conservative
/// default is exercised alongside a table hit).
///
/// `gpt-3.5-turbo` is the table hit ON PURPOSE: its published window (16_385)
/// DIFFERS from the component's default. A table entry whose value happened to
/// equal the default would make the assertion below unable to tell a real
/// lookup from the fallback, so the two expected windows must not coincide.
const OPENAI_MODELS_BODY: &str = r#"{"object":"list","data":[
  {"id":"gpt-5.2","object":"model","created":1,"owned_by":"openai"},
  {"id":"gpt-3.5-turbo","object":"model","created":2,"owned_by":"openai"}
]}"#;

/// An OpenAI `POST /v1/chat/completions` (non-stream) body. The flat provider
/// ABI collapses this to ONE terminal chunk, so the text below is the whole
/// completion.
const OPENAI_COMPLETION_BODY: &str = r#"{
  "id": "chatcmpl-conformance",
  "object": "chat.completion",
  "choices": [
    {"index": 0, "message": {"role": "assistant", "content": "Zeta Alpha Mu"}, "finish_reason": "stop"}
  ],
  "usage": {"prompt_tokens": 11, "completion_tokens": 3, "total_tokens": 14}
}"#;

/// The prebuilt `plugins/openai` component (built on demand by
/// [`crate::plugins::build_openai_component_once`] — it is a standalone
/// workspace crate, not a `tests/fixtures/*` fixture).
fn openai_component_artifact() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/openai/target/wasm32-wasip2/release")
        .join("ryuzi_plugin_openai.wasm")
}

/// The real `openai` component's conformance config: its actual endpoint paths
/// (`/models`, `/chat/completions`), its real `base-url` storage override key,
/// OpenAI-shaped JSON bodies, and the models/chunks/errors its own `logic`
/// module produces from them.
fn openai_component_conformance() -> ConformanceFixture {
    ConformanceFixture {
        artifact: openai_component_artifact(),
        // The router provider id the bundle declares — and therefore the id
        // whose stored credential `ryuzi:provider-auth` will inject.
        provider_id: "openai".to_string(),
        request_model: "gpt-5.2".to_string(),
        base_url_storage_key: "base-url".to_string(),
        stored_api_key: Some(OPENAI_STORED_KEY),
        wire: MockWireBodies {
            models_path: "/models".to_string(),
            models_body: OPENAI_MODELS_BODY.to_string(),
            complete_path: "/chat/completions".to_string(),
            complete_success_body: OPENAI_COMPLETION_BODY.to_string(),
        },
        expect: ProviderExpectations {
            models: vec![
                WasmModelInfo {
                    id: "gpt-5.2".to_string(),
                    // `/models` reports no display name, so the id doubles as
                    // one; an id outside the static table takes the
                    // conservative default window (128_000).
                    display_name: "gpt-5.2".to_string(),
                    context_window: 128_000,
                },
                WasmModelInfo {
                    id: "gpt-3.5-turbo".to_string(),
                    display_name: "gpt-3.5-turbo".to_string(),
                    // ...and this one is a genuine static-table HIT, whose
                    // value differs from the default above — so the pair
                    // distinguishes "looked the model up" from "fell back".
                    context_window: 16_385,
                },
            ],
            // Flat-text ABI + a buffered upstream: one terminal chunk.
            chunk_texts: vec!["Zeta Alpha Mu"],
            final_text: "Zeta Alpha Mu",
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
                // A 4xx that is NOT a model-not-found stays an invalid-request
                // carrying only the status — never the upstream message.
                HttpErrorCase {
                    status: 400,
                    expected_substring: "HTTP 400",
                },
            ],
            timeout_error_substrings: vec![
                "failed",
                "timeout",
                "timed out",
                "budget",
                "unavailable",
            ],
            // The component sets no credential header of its own — it has no
            // `ryuzi:http` import to set one with — so there is no forged
            // secret to look for.
            guest_forged_secret: None,
            // ...and the ONLY credential on the wire is the host-injected one.
            expected_authorization: Some(OPENAI_EXPECTED_AUTHORIZATION),
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openai_component_passes_the_full_conformance_battery() {
    crate::plugins::build_openai_component_once();
    let harness = ProviderConformance::new(openai_component_conformance());
    harness.run_full_battery().await;
}
