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
//! Every fixture below shares the SAME six checks: the synthetic
//! `component-provider-http` fixture (plain `ryuzi:http`, tab-separated wire
//! format), plus one per real OpenAI-chat provider component (`plugins/openai`,
//! `plugins/groq`, … — host-mediated `ryuzi:provider-auth`, OpenAI JSON), built
//! through [`OpenAiFormatFixture`]. A later per-provider slice adds one more
//! [`ConformanceFixture`] — never another copy of the checks.
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
/// and its completion path (per [`CompleteBehavior`]), and records every value
/// it receives for the fixture's own CREDENTIAL header
/// ([`ConformanceFixture::credential_header`] — `authorization` for a
/// bearer-scheme provider, `x-api-key` for an Anthropic-scheme one) so the auth
/// check can assert on exactly what reached the wire. Wire-agnostic: it serves
/// whatever bytes the caller hands it, on whatever paths, and never parses them.
struct MockUpstream {
    base_url: String,
    seen_credential: Arc<Mutex<Vec<String>>>,
}

impl MockUpstream {
    /// Bind a fresh loopback listener, serve the fixture's model-list path
    /// (always `wire.models_body`) and its completion path (per `complete`),
    /// and return the running upstream. The paths come from the fixture
    /// because they are this provider's REAL endpoint paths relative to its
    /// base URL (`/models` + `/chat/completions` for an OpenAI-format
    /// provider, `/models` + `/messages` for Anthropic) — the component builds
    /// its own URLs, so the mock has to meet it where it actually knocks.
    ///
    /// `credential_header` is the header name whose values this upstream
    /// records for the auth-absence check: the credential scheme is descriptor
    /// DATA, so a bearer provider records `authorization` and an `x-api-key`
    /// provider records `x-api-key`, without the checks below caring which.
    async fn start(
        wire: &MockWireBodies,
        complete: CompleteBehavior,
        credential_header: &str,
    ) -> Self {
        let models_body = wire.models_body.clone();
        let seen_credential: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let models_seen = seen_credential.clone();
        let models_header = credential_header.to_string();
        let models_route = get(move |headers: HeaderMap| {
            let seen = models_seen.clone();
            let name = models_header.clone();
            let body = models_body.clone();
            async move {
                record_credential(&headers, &name, &seen);
                (StatusCode::OK, body)
            }
        });

        let complete_seen = seen_credential.clone();
        let complete_header = credential_header.to_string();
        let complete_route = post(move |headers: HeaderMap| {
            let seen = complete_seen.clone();
            let name = complete_header.clone();
            let behavior = complete.clone();
            async move {
                record_credential(&headers, &name, &seen);
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
            seen_credential,
        }
    }

    /// Every value this upstream has received for the fixture's credential
    /// header so far.
    fn credential_headers_seen(&self) -> Vec<String> {
        self.seen_credential.lock().unwrap().clone()
    }
}

/// Record any occurrence of the credential header `name` (case-insensitive) on
/// an incoming mock request.
fn record_credential(headers: &HeaderMap, name: &str, seen: &Arc<Mutex<Vec<String>>>) {
    if let Some(value) = headers.get(name) {
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
    /// The ONLY value the mock upstream may observe for the fixture's
    /// credential header ([`ConformanceFixture::credential_header`] —
    /// `authorization` for a bearer provider, `x-api-key` for an
    /// Anthropic-scheme one).
    ///
    /// `None` — the component authenticates through plain `ryuzi:http`, so no
    /// credential header may reach the upstream at all: the host strips whatever
    /// the guest sets, and the check asserts the mock saw none.
    ///
    /// `Some(value)` — the component authenticates through
    /// `ryuzi:provider-auth`, so the HOST puts the user's stored credential on
    /// the wire (a bearer provider's value is `Bearer <key>`; an `x-api-key`
    /// provider's is the bare key). The check then asserts every observed value
    /// equals exactly this host-injected one, which is the same guarantee
    /// stated the other way round: nothing a guest could contribute ever
    /// appears.
    pub expected_authorization: Option<&'static str>,
}

/// How an OAuth provider fixture authenticates: the `[[oauth]]` profile the
/// bundle declares (== the router provider id and the id the guest passes to
/// `authorized-request`) and the access token seeded for it. The host injects
/// that token as `Authorization: Bearer <token>`; the component never sees it.
/// `None` on a fixture that reaches its upstream through plain `ryuzi:http` or
/// host-mediated `ryuzi:provider-auth` instead.
pub(crate) struct OAuthProfileSeed {
    pub profile_id: &'static str,
    pub access_token: &'static str,
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
    /// The request header the mock upstream watches for this component's
    /// host-injected credential — `authorization` for a bearer-scheme provider,
    /// `x-api-key` for an Anthropic-scheme one. Derived from the descriptor's
    /// `AuthScheme`, the same DATA the host keys its injection off, so the
    /// auth-absence check need not know which provider it is running.
    pub credential_header: String,
    /// For an OAuth provider: the `[[oauth]]` profile + seeded token that grants
    /// `ryuzi:oauth` and lets `authorized-request` inject a host-managed bearer.
    /// `None` for an API-key/http provider (whose auth is `stored_api_key` /
    /// plain http instead). Exactly one of `stored_api_key`/`oauth_profile` is
    /// `Some` for any credentialed fixture.
    pub oauth_profile: Option<OAuthProfileSeed>,
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
        // An OAuth fixture grants `ryuzi:oauth` + a seeded profile token instead
        // of `ryuzi:provider-auth` + a stored API key. The two are mutually
        // exclusive (a fixture is one or the other), so this reuses the SAME
        // builder and the SAME six checks — only the grant + seeded credential
        // differ.
        let (oauth_profile_ids, oauth_tokens) = match &self.fixture.oauth_profile {
            Some(seed) => (
                vec![seed.profile_id.to_string()],
                vec![(seed.profile_id.to_string(), seed.access_token.to_string())],
            ),
            None => (vec![], vec![]),
        };
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
                oauth_profile_ids,
                oauth_tokens,
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
            &self.fixture.credential_header,
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
            &self.fixture.credential_header,
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
    /// upstream sees no credential header at all (the host strips the one the
    /// guest forges); for a `ryuzi:provider-auth` component it means the only
    /// value the mock ever sees for the fixture's credential header is the
    /// host-injected one ([`ProviderExpectations::expected_authorization`]).
    /// Either way the guest must never surface a credential in its output.
    pub(crate) async fn assert_strips_guest_authorization(&self) {
        let mock = MockUpstream::start(
            &self.fixture.wire,
            CompleteBehavior::Body(self.fixture.wire.complete_success_body.clone()),
            &self.fixture.credential_header,
        )
        .await;
        let (transport, _tmp) = self.transport(&mock, Duration::from_secs(10)).await;

        let chunks = transport
            .complete(self.completion_request())
            .await
            .expect("complete over mocked HTTP must succeed");

        let seen = mock.credential_headers_seen();
        match self.fixture.expect.expected_authorization {
            None => assert!(
                seen.is_empty(),
                "the guest's credential header must be stripped before it reaches \
                 the upstream, but the mock saw: {seen:?}",
            ),
            Some(expected) => {
                assert!(
                    !seen.is_empty(),
                    "the host must have injected the stored credential, but the \
                     mock saw no credential header at all",
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
            let mock = MockUpstream::start(
                &self.fixture.wire,
                CompleteBehavior::Status(case.status),
                &self.fixture.credential_header,
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
            &self.fixture.wire,
            CompleteBehavior::Stall,
            &self.fixture.credential_header,
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
        base_url_storage_key: "conformance-base-url".to_string(),
        // Plain `ryuzi:http` egress: no provider-auth grant, no stored key.
        stored_api_key: None,
        // No credential is injected at all, so the header watched here is
        // immaterial — `authorization` matches what a guest would forge.
        credential_header: "authorization".to_string(),
        // Plain `ryuzi:http`, not OAuth.
        oauth_profile: None,
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
// The REAL OpenAI-CHAT provider components (plan Task 16, Steps 2 + 3)
//
// Same six checks, real components, and OpenAI's actual wire format. Nothing
// below touches the harness itself — each provider is one more
// [`ConformanceFixture`], which is the point of the Step 1 parameterization.
//
// Every component here is built on the shared `ryuzi-openai-format` crate, so
// their endpoint paths, storage override key and error mapping are properties
// of the FORMAT rather than of any one provider. [`OpenAiFormatFixture`]
// therefore carries only what genuinely differs — the provider id, the model
// it is asked for, the stored credential, and the literal wire bodies its mock
// upstream serves — and fills the format-level fields in itself.
// ---------------------------------------------------------------------------

/// Model-discovery path every OpenAI-format component GETs (its descriptor's
/// `has_models_endpoint` is `true` and none of them override the default).
const OPENAI_FORMAT_MODELS_PATH: &str = "/models";

/// Chat-generation path every OpenAI-format component POSTs to (every
/// descriptor here leaves `chat_path` at `None`).
const OPENAI_FORMAT_CHAT_PATH: &str = "/chat/completions";

/// The base-URL override key these components read from their own storage
/// slice (`ryuzi_openai_format::BASE_URL_STORAGE_KEY`) — the real product-level
/// proxy override, which the harness reuses to aim them at its mock.
const OPENAI_FORMAT_BASE_URL_KEY: &str = "base-url";

/// One real OpenAI-format provider component's conformance data.
///
/// Deliberately NOT a generator of its own expectations: the wire bodies are
/// literal JSON and the expected models/text/usage are literal values, so the
/// battery still compares what the COMPONENT parsed against what a human wrote
/// down, not one derivation against another.
struct OpenAiFormatFixture {
    /// Router provider id — also the `plugins/<dir>` name, the manifest
    /// `provider-ids` entry, and (via `ryuzi_plugin_<id>`) the built artifact's
    /// stem. A drift in any of those surfaces as a load or `denied` failure.
    provider_id: &'static str,
    /// The model id put on every `complete` request.
    request_model: &'static str,
    /// The user API key this run stores through the real `provider_connections`
    /// path. `ryuzi:provider-auth` resolves it host-side and injects it as
    /// `Authorization: Bearer …` (every descriptor here declares
    /// `AuthScheme::Bearer`), so the component itself never sees this value.
    stored_api_key: &'static str,
    /// Exactly what the mock upstream must therefore observe — and nothing else.
    expected_authorization: &'static str,
    /// Literal `GET /models` body the mock serves.
    models_body: &'static str,
    /// `(id, context_window)` per model, in served order. The display name is
    /// asserted to equal the id, which is a property of the format: an
    /// OpenAI-shaped `/models` response carries no display name.
    expected_models: &'static [(&'static str, u32)],
    /// Literal `POST /chat/completions` (non-stream) body the mock serves.
    completion_body: &'static str,
    /// The whole completion — the flat ABI collapses the response to ONE
    /// terminal chunk, so this is both the only chunk's text and the final text.
    expected_text: &'static str,
    /// Usage the terminal chunk must carry, from `completion_body`'s `usage`.
    expected_usage: WasmTokenUsage,
}

impl OpenAiFormatFixture {
    /// The built component artifact. Each `plugins/<id>` is a standalone
    /// workspace crate (not a `tests/fixtures/*` fixture), and cargo names its
    /// output after the crate with `-` replaced by `_`.
    fn artifact(&self) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins")
            .join(self.provider_id)
            .join("target/wasm32-wasip2/release")
            .join(format!(
                "ryuzi_plugin_{}.wasm",
                self.provider_id.replace('-', "_")
            ))
    }

    /// Expand into a full [`ConformanceFixture`], supplying the format-level
    /// fields (paths, storage key, error mapping) every OpenAI-format component
    /// shares by construction.
    fn into_conformance(self) -> ConformanceFixture {
        ConformanceFixture {
            artifact: self.artifact(),
            provider_id: self.provider_id.to_string(),
            request_model: self.request_model.to_string(),
            base_url_storage_key: OPENAI_FORMAT_BASE_URL_KEY.to_string(),
            stored_api_key: Some(self.stored_api_key),
            // Every OpenAI-format descriptor here declares `AuthScheme::Bearer`,
            // so the host injects the credential as `Authorization: Bearer …`.
            credential_header: "authorization".to_string(),
            // API-key providers, host-mediated via `ryuzi:provider-auth`.
            oauth_profile: None,
            wire: MockWireBodies {
                models_path: OPENAI_FORMAT_MODELS_PATH.to_string(),
                models_body: self.models_body.to_string(),
                complete_path: OPENAI_FORMAT_CHAT_PATH.to_string(),
                complete_success_body: self.completion_body.to_string(),
            },
            expect: ProviderExpectations {
                models: self
                    .expected_models
                    .iter()
                    .map(|(id, context_window)| WasmModelInfo {
                        id: (*id).to_string(),
                        display_name: (*id).to_string(),
                        context_window: *context_window,
                    })
                    .collect(),
                // Flat-text ABI + a buffered upstream: one terminal chunk.
                chunk_texts: vec![self.expected_text],
                final_text: self.expected_text,
                terminal_usage: Some(self.expected_usage),
                http_error_cases: vec![
                    HttpErrorCase {
                        status: 429,
                        expected_substring: "rate limited",
                    },
                    HttpErrorCase {
                        status: 503,
                        expected_substring: "unavailable",
                    },
                    // A 4xx that is NOT a model-not-found stays an
                    // invalid-request carrying only the status — never the
                    // upstream message.
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
                // These components set no credential header of their own — they
                // have no `ryuzi:http` import to set one with — so there is no
                // forged secret to look for.
                guest_forged_secret: None,
                // ...and the ONLY credential on the wire is the host-injected one.
                expected_authorization: Some(self.expected_authorization),
            },
        }
    }
}

/// Build the component under test, then run the whole six-point battery against
/// it. One line per provider below.
async fn run_openai_format_battery(fixture: OpenAiFormatFixture) {
    crate::plugins::build_provider_component_once(fixture.provider_id);
    ProviderConformance::new(fixture.into_conformance())
        .run_full_battery()
        .await;
}

/// A two-model `/models` body for a provider whose descriptor seeds NO model
/// list. The ids are synthetic on purpose — inventing real-looking ones would
/// assert a catalog this repo has no source for — and the component treats a
/// `/models` id opaquely anyway. They are served in NON-alphabetical order so
/// the battery's order assertion is not satisfied by an accidental sort, and
/// they embed the provider id so a cross-wired fixture (running provider A's
/// component against provider B's expectations) fails loudly.
macro_rules! synthetic_models_body {
    ($id:literal) => {
        concat!(
            r#"{"object":"list","data":[{"id":""#,
            $id,
            r#"-zeta","object":"model"},{"id":""#,
            $id,
            r#"-alpha","object":"model"}]}"#
        )
    };
}

/// The matching expectations for [`synthetic_models_body!`]. Both windows are
/// the shared conservative default (`ryuzi_openai_format::DEFAULT_CONTEXT_WINDOW`):
/// these providers ship an EMPTY static context-window table, because their
/// `/models` responses carry no context length and their descriptors pin no
/// per-model values. That the two windows coincide here is therefore the
/// behaviour under test, not the blind spot M1 fixed for `openai` — `openai` is
/// the one component with a real table, and its fixture below still asserts two
/// DIFFERENT windows.
macro_rules! synthetic_models_expected {
    ($id:literal) => {
        &[
            (concat!($id, "-zeta"), 128_000),
            (concat!($id, "-alpha"), 128_000),
        ]
    };
}

/// A non-stream chat-completion body. `text` is the whole completion (the flat
/// ABI collapses the response to one terminal chunk) and the usage counts are
/// per-provider so a cross-wired fixture cannot pass.
macro_rules! completion_body {
    ($text:literal, $input:literal, $output:literal) => {
        concat!(
            r#"{"id":"chatcmpl-conformance","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":""#,
            $text,
            r#""},"finish_reason":"stop"}],"usage":{"prompt_tokens":"#,
            $input,
            r#","completion_tokens":"#,
            $output,
            r#"}}"#
        )
    };
}

/// The real `openai` component. The ONLY fixture here with a populated static
/// context-window table, so its two expected windows deliberately DIFFER:
/// `gpt-5.2` is unknown to the table and takes the conservative default,
/// `gpt-3.5-turbo` is a genuine table hit at its published 16_385. A pair that
/// coincided could not tell a lookup from a fallback.
const OPENAI_FIXTURE: OpenAiFormatFixture = OpenAiFormatFixture {
    provider_id: "openai",
    request_model: "gpt-5.2",
    stored_api_key: "sk-conformance-openai-key",
    expected_authorization: "Bearer sk-conformance-openai-key",
    models_body: r#"{"object":"list","data":[
      {"id":"gpt-5.2","object":"model","created":1,"owned_by":"openai"},
      {"id":"gpt-3.5-turbo","object":"model","created":2,"owned_by":"openai"}
    ]}"#,
    expected_models: &[("gpt-5.2", 128_000), ("gpt-3.5-turbo", 16_385)],
    completion_body: completion_body!("Zeta Alpha Mu", 11, 3),
    expected_text: "Zeta Alpha Mu",
    expected_usage: WasmTokenUsage {
        input: 11,
        output: 3,
    },
};

const OPENROUTER_FIXTURE: OpenAiFormatFixture = OpenAiFormatFixture {
    provider_id: "openrouter",
    request_model: "openrouter-zeta",
    stored_api_key: "sk-or-conformance-key",
    expected_authorization: "Bearer sk-or-conformance-key",
    models_body: synthetic_models_body!("openrouter"),
    expected_models: synthetic_models_expected!("openrouter"),
    completion_body: completion_body!("routed reply", 21, 5),
    expected_text: "routed reply",
    expected_usage: WasmTokenUsage {
        input: 21,
        output: 5,
    },
};

const GROQ_FIXTURE: OpenAiFormatFixture = OpenAiFormatFixture {
    provider_id: "groq",
    request_model: "groq-zeta",
    stored_api_key: "gsk-conformance-key",
    expected_authorization: "Bearer gsk-conformance-key",
    models_body: synthetic_models_body!("groq"),
    expected_models: synthetic_models_expected!("groq"),
    completion_body: completion_body!("fast reply", 13, 7),
    expected_text: "fast reply",
    expected_usage: WasmTokenUsage {
        input: 13,
        output: 7,
    },
};

/// `deepseek` is the one non-`openai` descriptor here that SEEDS a model list
/// (`["deepseek-chat", "deepseek-reasoner"]`), so its fixture uses those real
/// ids instead of synthetic ones — served reasoner-first, i.e. not sorted.
const DEEPSEEK_FIXTURE: OpenAiFormatFixture = OpenAiFormatFixture {
    provider_id: "deepseek",
    request_model: "deepseek-chat",
    stored_api_key: "sk-ds-conformance-key",
    expected_authorization: "Bearer sk-ds-conformance-key",
    models_body: r#"{"object":"list","data":[
      {"id":"deepseek-reasoner","object":"model"},
      {"id":"deepseek-chat","object":"model"}
    ]}"#,
    expected_models: &[("deepseek-reasoner", 128_000), ("deepseek-chat", 128_000)],
    completion_body: completion_body!("reasoned reply", 17, 9),
    expected_text: "reasoned reply",
    expected_usage: WasmTokenUsage {
        input: 17,
        output: 9,
    },
};

const MISTRAL_FIXTURE: OpenAiFormatFixture = OpenAiFormatFixture {
    provider_id: "mistral",
    request_model: "mistral-zeta",
    stored_api_key: "mi-conformance-key",
    expected_authorization: "Bearer mi-conformance-key",
    models_body: synthetic_models_body!("mistral"),
    expected_models: synthetic_models_expected!("mistral"),
    completion_body: completion_body!("le reply", 23, 11),
    expected_text: "le reply",
    expected_usage: WasmTokenUsage {
        input: 23,
        output: 11,
    },
};

const XAI_FIXTURE: OpenAiFormatFixture = OpenAiFormatFixture {
    provider_id: "xai",
    request_model: "xai-zeta",
    stored_api_key: "xai-conformance-key",
    expected_authorization: "Bearer xai-conformance-key",
    models_body: synthetic_models_body!("xai"),
    expected_models: synthetic_models_expected!("xai"),
    completion_body: completion_body!("witty reply", 29, 13),
    expected_text: "witty reply",
    expected_usage: WasmTokenUsage {
        input: 29,
        output: 13,
    },
};

const NVIDIA_FIXTURE: OpenAiFormatFixture = OpenAiFormatFixture {
    provider_id: "nvidia",
    request_model: "nvidia-zeta",
    stored_api_key: "nvapi-conformance-key",
    expected_authorization: "Bearer nvapi-conformance-key",
    models_body: synthetic_models_body!("nvidia"),
    expected_models: synthetic_models_expected!("nvidia"),
    completion_body: completion_body!("accelerated reply", 31, 17),
    expected_text: "accelerated reply",
    expected_usage: WasmTokenUsage {
        input: 31,
        output: 17,
    },
};

/// `google` points at Gemini's OpenAI-COMPATIBILITY endpoint, and its
/// descriptor seeds a model list (`["gemini-3.0-pro", "gemini-3.0-flash"]`), so
/// its fixture uses those real ids instead of synthetic ones — served
/// flash-first, i.e. not in the descriptor's own order.
const GOOGLE_FIXTURE: OpenAiFormatFixture = OpenAiFormatFixture {
    provider_id: "google",
    request_model: "gemini-3.0-pro",
    stored_api_key: "AIza-conformance-key",
    expected_authorization: "Bearer AIza-conformance-key",
    models_body: r#"{"object":"list","data":[
      {"id":"gemini-3.0-flash","object":"model"},
      {"id":"gemini-3.0-pro","object":"model"}
    ]}"#,
    expected_models: &[("gemini-3.0-flash", 128_000), ("gemini-3.0-pro", 128_000)],
    completion_body: completion_body!("grounded reply", 41, 23),
    expected_text: "grounded reply",
    expected_usage: WasmTokenUsage {
        input: 41,
        output: 23,
    },
};

const HUGGINGFACE_FIXTURE: OpenAiFormatFixture = OpenAiFormatFixture {
    provider_id: "huggingface",
    request_model: "huggingface-zeta",
    stored_api_key: "hf-conformance-key",
    expected_authorization: "Bearer hf-conformance-key",
    models_body: synthetic_models_body!("huggingface"),
    expected_models: synthetic_models_expected!("huggingface"),
    completion_body: completion_body!("routed hub reply", 37, 19),
    expected_text: "routed hub reply",
    expected_usage: WasmTokenUsage {
        input: 37,
        output: 19,
    },
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openai_component_passes_the_full_conformance_battery() {
    run_openai_format_battery(OPENAI_FIXTURE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openrouter_component_passes_the_full_conformance_battery() {
    run_openai_format_battery(OPENROUTER_FIXTURE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn groq_component_passes_the_full_conformance_battery() {
    run_openai_format_battery(GROQ_FIXTURE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deepseek_component_passes_the_full_conformance_battery() {
    run_openai_format_battery(DEEPSEEK_FIXTURE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mistral_component_passes_the_full_conformance_battery() {
    run_openai_format_battery(MISTRAL_FIXTURE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn xai_component_passes_the_full_conformance_battery() {
    run_openai_format_battery(XAI_FIXTURE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nvidia_component_passes_the_full_conformance_battery() {
    run_openai_format_battery(NVIDIA_FIXTURE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn huggingface_component_passes_the_full_conformance_battery() {
    run_openai_format_battery(HUGGINGFACE_FIXTURE).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn google_component_passes_the_full_conformance_battery() {
    run_openai_format_battery(GOOGLE_FIXTURE).await;
}

/// Every OpenAI-format provider bundle ported so far, for the manifest audit
/// below. Kept next to the fixtures so a new provider is added in one place.
///
/// HAND-MAINTAINED, and that is its one blind spot: the audit iterates this
/// list, not `plugins/`, so a bundle added under `plugins/<id>/` WITHOUT a
/// fixture here is audited by nothing — its manifest, allowlist and base URL go
/// unchecked. Adding a provider means adding it here in the same change.
const OPENAI_FORMAT_FIXTURES: &[&OpenAiFormatFixture] = &[
    &OPENAI_FIXTURE,
    &OPENROUTER_FIXTURE,
    &GROQ_FIXTURE,
    &DEEPSEEK_FIXTURE,
    &MISTRAL_FIXTURE,
    &XAI_FIXTURE,
    &NVIDIA_FIXTURE,
    &HUGGINGFACE_FIXTURE,
    &GOOGLE_FIXTURE,
];

/// The conformance battery proves each component BEHAVES like its provider.
/// This proves each bundle is DECLARED like one — which is what decides whether
/// the host will hand it the user's credential at all, and is invisible to a
/// battery that grants capabilities itself.
///
/// For every ported provider, the committed `ryuzi-plugin.toml` must:
/// - parse and validate as a `PluginBundleManifest`;
/// - declare `provider-ids` EXPLICITLY (the `[id]` fallback does not authorize
///   `ryuzi:provider-auth`) and name exactly this provider;
/// - allowlist exactly one host, and that host must be the host of the
///   provider's OWN `ProviderDescriptor::base_url` — so the user's key can only
///   travel to the endpoint the router itself would have used.
///
/// ...and the component's own `src/logic.rs` must carry the descriptor's FULL
/// `base_url`, PATH included. The allowlist check above matches on host only,
/// and the conformance battery always overrides the base URL through storage to
/// reach its mock, so a mistranscribed path (dropping groq's `/openai` segment,
/// say) would otherwise satisfy every other gate in this file and only fail
/// against the live upstream.
///
/// It also re-checks the descriptor facts this whole slice assumes: an API-key
/// provider, with the expected auth scheme, and a live `/models` endpoint. A
/// descriptor that drifts away from those makes its component wrong, and this
/// fails first.
fn assert_bundle_is_declared_like_its_provider(
    id: &str,
    expected_auth: crate::llm_router::registry::AuthScheme,
) {
    use crate::llm_router::registry::{self, ProviderCategory};
    use ryuzi_plugin_sdk::PluginBundleManifest;

    let descriptor =
        registry::descriptor(id).unwrap_or_else(|| panic!("{id} must exist in the router catalog"));
    let base_url = descriptor
        .base_url
        .unwrap_or_else(|| panic!("{id} must declare a base_url to allowlist"));
    let expected_host = url::Url::parse(base_url)
        .unwrap_or_else(|e| panic!("{id} base_url {base_url}: {e}"))
        .host_str()
        .unwrap_or_else(|| panic!("{id} base_url {base_url} has no host"))
        .to_string();

    // The descriptor facts this component was built from.
    assert_eq!(
        descriptor.category,
        ProviderCategory::ApiKey,
        "{id} category"
    );
    assert_eq!(descriptor.auth, expected_auth, "{id} auth scheme");
    assert!(descriptor.has_models_endpoint, "{id} has_models_endpoint");
    assert_eq!(descriptor.chat_path, None, "{id} chat_path");

    let bundle_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins")
        .join(id);
    let path = bundle_dir.join("ryuzi-plugin.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let manifest: PluginBundleManifest =
        toml::from_str(&text).unwrap_or_else(|e| panic!("{id} manifest: {e}"));
    manifest
        .validate()
        .unwrap_or_else(|e| panic!("{id} manifest is invalid: {e}"));

    assert_eq!(manifest.id, id, "bundle id");
    assert_eq!(
        manifest.provider_ids,
        vec![id.to_string()],
        "{id} must declare provider-ids EXPLICITLY — the [id] fallback does \
         not grant ryuzi:provider-auth",
    );
    let hosts: Vec<&str> = manifest
        .permissions
        .network
        .iter()
        .map(|host| host.0.as_str())
        .collect();
    assert_eq!(
        hosts,
        vec![expected_host.as_str()],
        "{id} must allowlist exactly the host of its descriptor's base_url \
         ({base_url}) — a wider allowlist widens where the user's key may go",
    );

    // The full base URL — scheme, host AND path — as the component transcribed
    // it. Read out of the source text rather than through the config constant
    // because each `plugins/<id>` is a separate workspace this crate cannot
    // link. Deliberately the assigned VALUE of `default_base_url`, not merely
    // an occurrence somewhere in the file: every one of these logic modules
    // also quotes its base URL in a doc comment, so a `contains` check would
    // pass on a component whose real config had drifted.
    let logic_path = bundle_dir.join("src/logic.rs");
    let logic = std::fs::read_to_string(&logic_path)
        .unwrap_or_else(|e| panic!("{}: {e}", logic_path.display()));
    let configured = configured_default_base_url(&logic).unwrap_or_else(|| {
        panic!(
            "{} must assign a string literal to `default_base_url`",
            logic_path.display()
        )
    });
    assert_eq!(
        configured,
        base_url,
        "{} configures base URL {configured:?} but the {id} descriptor says \
         {base_url:?} — the allowlist check matches on HOST only and the \
         conformance battery overrides the base URL through storage, so a wrong \
         PATH escapes every other gate in this file",
        logic_path.display(),
    );
}

/// The string literal a provider component's `logic.rs` assigns to
/// `default_base_url`, if any — the base URL that component will really send
/// to when the user has set no override. Source-text extraction, because these
/// bundles are separate workspaces the engine crate cannot link against.
///
/// Matches the ASSIGNMENT — a `default_base_url:` directly followed by a string
/// literal — not the struct FIELD DEFINITION (`default_base_url: &'static str`),
/// which the Anthropic component declares inline in the same file. The
/// assignment is the first `default_base_url:` whose next non-space character
/// opens a quoted string.
fn configured_default_base_url(logic: &str) -> Option<&str> {
    logic
        .split("default_base_url:")
        .skip(1)
        .find_map(|segment| {
            let literal = segment.trim_start().strip_prefix('"')?;
            let (value, _) = literal.split_once('"')?;
            Some(value)
        })
}

#[test]
fn every_ported_provider_bundle_declares_provider_auth_and_only_its_own_host() {
    use crate::llm_router::registry::AuthScheme;

    for fixture in OPENAI_FORMAT_FIXTURES {
        // Every OpenAI-format descriptor ported so far declares Bearer auth.
        assert_bundle_is_declared_like_its_provider(fixture.provider_id, AuthScheme::Bearer);
    }

    // Anthropic speaks a different wire format (`ApiFormat::Anthropic`) but is
    // held to the identical bundle-DECLARATION contract — explicit
    // `provider-ids`, a one-host allowlist matching its descriptor base URL, and
    // that full base URL (path included) in its `logic.rs`. Its only difference
    // here is the auth scheme: `x-api-key`, not bearer.
    assert_bundle_is_declared_like_its_provider("anthropic", AuthScheme::XApiKey);
}

// ---------------------------------------------------------------------------
// The `anthropic` provider component (plan Task 16c4, Step C)
//
// The SAME six checks, driven through the SAME `ProviderConformance` harness —
// but Anthropic's `/messages` wire format, not OpenAI-chat. That is the point
// of the Step 1 parameterization: a different format is a different
// `ConformanceFixture` (different wire bodies, `/messages` path, `x-api-key`
// credential header), never another copy of the checks.
// ---------------------------------------------------------------------------

/// The user API key this run stores for `anthropic`. `ryuzi:provider-auth`
/// resolves it host-side and injects it as `x-api-key` (the `anthropic`
/// descriptor declares `AuthScheme::XApiKey`), so the component itself never
/// sees this value — and, unlike a bearer provider, the header the mock watches
/// carries the BARE key, not a `Bearer …` wrapper.
const ANTHROPIC_STORED_API_KEY: &str = "sk-ant-conformance-key";

/// The literal `GET /models` body Anthropic's endpoint serves: typed `model`
/// entries with a `display_name` the component must PREFER over the id (the
/// OpenAI-format listing has no such field). Served opus-first, i.e. not sorted,
/// so the battery's order assertion is not satisfied by an accidental sort.
const ANTHROPIC_MODELS_BODY: &str = r#"{"data":[
  {"type":"model","id":"claude-opus-4-5","display_name":"Claude Opus 4.5","created_at":"2025-01-01T00:00:00Z"},
  {"type":"model","id":"claude-haiku-4-5","display_name":"Claude Haiku 4.5","created_at":"2025-01-01T00:00:00Z"}
],"has_more":false}"#;

/// The literal `POST /messages` (non-stream) body Anthropic serves: a `content`
/// array of typed blocks (NOT `choices[]`) and `usage.input_tokens`/
/// `output_tokens` (NOT `prompt_`/`completion_tokens`). A leading `thinking`
/// block is included to prove the component skips it and never surfaces private
/// reasoning as the completion.
const ANTHROPIC_MESSAGE_BODY: &str = r#"{
  "id":"msg_conformance",
  "type":"message",
  "role":"assistant",
  "model":"claude-opus-4-5",
  "content":[
    {"type":"thinking","thinking":"private chain of thought"},
    {"type":"text","text":"considered reply"}
  ],
  "stop_reason":"end_turn",
  "usage":{"input_tokens":43,"output_tokens":29}
}"#;

/// The `anthropic` component's built artifact. Like every `plugins/<id>`, a
/// standalone workspace crate whose cargo output is the crate name with `-`
/// replaced by `_`.
fn anthropic_artifact() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/anthropic/target/wasm32-wasip2/release")
        .join("ryuzi_plugin_anthropic.wasm")
}

/// The `anthropic` conformance fixture: Anthropic-Messages wire bodies, the
/// `/models` + `/messages` endpoint paths, the shared base-URL override key, and
/// the `x-api-key` credential header the host injects for this scheme.
fn anthropic_conformance() -> ConformanceFixture {
    ConformanceFixture {
        artifact: anthropic_artifact(),
        provider_id: "anthropic".to_string(),
        request_model: "claude-opus-4-5".to_string(),
        base_url_storage_key: OPENAI_FORMAT_BASE_URL_KEY.to_string(),
        stored_api_key: Some(ANTHROPIC_STORED_API_KEY),
        // `AuthScheme::XApiKey`: the host injects the bare key as `x-api-key`,
        // so that — not `authorization` — is the header the mock watches.
        credential_header: "x-api-key".to_string(),
        // The x-api-key sibling authenticates via `ryuzi:provider-auth`, not OAuth.
        oauth_profile: None,
        wire: MockWireBodies {
            models_path: "/models".to_string(),
            models_body: ANTHROPIC_MODELS_BODY.to_string(),
            // ApiFormat::Anthropic generates at `/messages`, not
            // `/chat/completions`.
            complete_path: "/messages".to_string(),
            complete_success_body: ANTHROPIC_MESSAGE_BODY.to_string(),
        },
        expect: ProviderExpectations {
            // Display names come from the `/models` body, NOT the id — the one
            // place Anthropic's listing carries more than the OpenAI shape.
            models: vec![
                WasmModelInfo {
                    id: "claude-opus-4-5".to_string(),
                    display_name: "Claude Opus 4.5".to_string(),
                    context_window: 128_000,
                },
                WasmModelInfo {
                    id: "claude-haiku-4-5".to_string(),
                    display_name: "Claude Haiku 4.5".to_string(),
                    context_window: 128_000,
                },
            ],
            // Flat-text ABI + a buffered upstream: one terminal chunk, carrying
            // only the first text block (the thinking block is dropped).
            chunk_texts: vec!["considered reply"],
            final_text: "considered reply",
            terminal_usage: Some(WasmTokenUsage {
                input: 43,
                output: 29,
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
                // A 4xx that is NOT a not-found stays an invalid-request
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
            // `ryuzi:http` import to set one with — so there is no forged secret
            // to look for.
            guest_forged_secret: None,
            // ...and the ONLY value the mock may see for `x-api-key` is the
            // host-injected bare key.
            expected_authorization: Some(ANTHROPIC_STORED_API_KEY),
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anthropic_component_passes_the_full_conformance_battery() {
    crate::plugins::build_provider_component_once("anthropic");
    ProviderConformance::new(anthropic_conformance())
        .run_full_battery()
        .await;
}

// ---------------------------------------------------------------------------
// The `anthropic-oauth` provider component (plan Task 16c5, Step C)
//
// The SAME six checks, driven through the SAME `ProviderConformance` harness —
// but the OAUTH egress path, not `ryuzi:provider-auth`. That is the point of
// the Step 1 parameterization AND the Step-C harness extension: an OAuth
// provider is a `ConformanceFixture` whose `oauth_profile` is `Some` (granting
// `ryuzi:oauth` + a seeded profile token) instead of `stored_api_key`, never
// another copy of the checks. Anthropic-Messages wire format, shared with the
// x-api-key sibling; the ONE behavioural difference the harness observes is who
// puts the bearer on the wire — the HOST, from the seeded OAuth token, never
// the guest.
// ---------------------------------------------------------------------------

/// The OAuth access token this run seeds for the `anthropic-oauth` profile.
/// `ryuzi:oauth` resolves it host-side and injects it as `Authorization: Bearer
/// <token>`, so the component itself never sees this value — and the auth-absence
/// check asserts the mock upstream saw EXACTLY this, i.e. nothing the guest
/// forged (the guest has no `ryuzi:http` to forge one with anyway).
const ANTHROPIC_OAUTH_SEEDED_TOKEN: &str = "oauth-access-token-conformance";

/// The literal `GET /models` body the OAuth endpoint serves. Uses model ids and
/// display names distinct from the x-api-key sibling's body so a cross-wired
/// fixture (running one component against the other's expectations) fails loudly.
/// Served opus-first — i.e. NOT alphabetical (`haiku` would sort first) — so the
/// battery's order assertion is not satisfied by an accidental sort.
const ANTHROPIC_OAUTH_MODELS_BODY: &str = r#"{"data":[
  {"type":"model","id":"claude-opus-4-8","display_name":"Claude Opus 4.8","created_at":"2026-01-01T00:00:00Z"},
  {"type":"model","id":"claude-haiku-4-5-20251001","display_name":"Claude Haiku 4.5","created_at":"2025-10-01T00:00:00Z"}
],"has_more":false}"#;

/// The literal `POST /messages` (non-stream) body the OAuth endpoint serves: a
/// `content` array of typed blocks with a leading `thinking` block the component
/// must skip, and `usage.input_tokens`/`output_tokens` distinct from the
/// sibling's so a cross-wired fixture cannot pass.
const ANTHROPIC_OAUTH_MESSAGE_BODY: &str = r#"{
  "id":"msg_oauth_conformance",
  "type":"message",
  "role":"assistant",
  "model":"claude-opus-4-8",
  "content":[
    {"type":"thinking","thinking":"private chain of thought"},
    {"type":"text","text":"subscription reply"}
  ],
  "stop_reason":"end_turn",
  "usage":{"input_tokens":51,"output_tokens":37}
}"#;

/// The `anthropic-oauth` component's built artifact — a standalone workspace
/// crate whose cargo output is the crate name with `-` replaced by `_`.
fn anthropic_oauth_artifact() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/anthropic-oauth/target/wasm32-wasip2/release")
        .join("ryuzi_plugin_anthropic_oauth.wasm")
}

/// The `anthropic-oauth` conformance fixture: Anthropic-Messages wire bodies on
/// the `/models` + `/messages` paths, the shared base-URL override key, and —
/// the Step-C addition — an `oauth_profile` that grants `ryuzi:oauth` and seeds
/// a profile token, so the host injects `Authorization: Bearer <token>` and the
/// component never touches a credential.
fn anthropic_oauth_conformance() -> ConformanceFixture {
    ConformanceFixture {
        artifact: anthropic_oauth_artifact(),
        provider_id: "anthropic-oauth".to_string(),
        request_model: "claude-opus-4-8".to_string(),
        base_url_storage_key: OPENAI_FORMAT_BASE_URL_KEY.to_string(),
        // OAuth, not `ryuzi:provider-auth`: no stored API key.
        stored_api_key: None,
        // The descriptor declares `AuthScheme::Bearer`, and the host injects the
        // OAuth token as `Authorization: Bearer …`, so `authorization` — not
        // `x-api-key` — is the header the mock watches.
        credential_header: "authorization".to_string(),
        oauth_profile: Some(OAuthProfileSeed {
            // Matches the manifest `[[oauth]]` id, the router provider id, and
            // `logic::OAUTH_PROFILE` — the string the guest passes to
            // `authorized-request`. A drift is a hard `denied`.
            profile_id: "anthropic-oauth",
            access_token: ANTHROPIC_OAUTH_SEEDED_TOKEN,
        }),
        wire: MockWireBodies {
            models_path: "/models".to_string(),
            models_body: ANTHROPIC_OAUTH_MODELS_BODY.to_string(),
            // ApiFormat::Anthropic generates at `/messages`; the OAuth guest
            // appends `?beta=true`, which axum ignores for path routing so this
            // `/messages` route still matches.
            complete_path: "/messages".to_string(),
            complete_success_body: ANTHROPIC_OAUTH_MESSAGE_BODY.to_string(),
        },
        expect: ProviderExpectations {
            models: vec![
                WasmModelInfo {
                    id: "claude-opus-4-8".to_string(),
                    display_name: "Claude Opus 4.8".to_string(),
                    context_window: 128_000,
                },
                WasmModelInfo {
                    id: "claude-haiku-4-5-20251001".to_string(),
                    display_name: "Claude Haiku 4.5".to_string(),
                    context_window: 128_000,
                },
            ],
            // Flat-text ABI + buffered upstream: one terminal chunk carrying only
            // the first text block (the thinking block is dropped).
            chunk_texts: vec!["subscription reply"],
            final_text: "subscription reply",
            terminal_usage: Some(WasmTokenUsage {
                input: 51,
                output: 37,
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
                // A 4xx that is NOT a not-found stays an invalid-request carrying
                // only the status — never the upstream message. The host passes a
                // non-2xx upstream status straight through `authorized-request`,
                // so this exercises the SAME `classify_error` mapping as the
                // x-api-key path.
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
            // The component sets no credential header of its own — no
            // `ryuzi:http`, no `ryuzi:provider-auth`, only host-mediated
            // `ryuzi:oauth` — so there is no forged secret to look for.
            guest_forged_secret: None,
            // ...and the ONLY value the mock may see for `authorization` is the
            // HOST-injected bearer built from the seeded OAuth token. This is the
            // auth-absence guarantee for the OAuth path: the guest never sets or
            // sees the bearer; the mock receives only what the host injected.
            expected_authorization: Some("Bearer oauth-access-token-conformance"),
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anthropic_oauth_component_passes_the_full_conformance_battery() {
    crate::plugins::build_provider_component_once("anthropic-oauth");
    ProviderConformance::new(anthropic_oauth_conformance())
        .run_full_battery()
        .await;
}

/// The battery proves the `anthropic-oauth` component BEHAVES like its provider.
/// This proves its bundle is DECLARED like one — the OAuth analogue of
/// [`assert_bundle_is_declared_like_its_provider`], which only covers API-key
/// (`ryuzi:provider-auth`) bundles. What decides whether the host will inject
/// the Claude-subscription bearer at all is invisible to a battery that grants
/// `ryuzi:oauth` itself, so it is checked here against the committed manifest.
#[test]
fn anthropic_oauth_bundle_is_declared_like_its_oauth_provider() {
    use crate::llm_router::registry::{self, ProviderCategory};
    use ryuzi_plugin_sdk::PluginBundleManifest;

    let id = "anthropic-oauth";
    let descriptor =
        registry::descriptor(id).unwrap_or_else(|| panic!("{id} must exist in the router catalog"));

    // The descriptor facts this component was built from: an OAuth provider,
    // Anthropic wire format, with a live `/models` endpoint.
    assert_eq!(
        descriptor.category,
        ProviderCategory::OAuth,
        "{id} category"
    );
    assert_eq!(
        descriptor.format,
        registry::ApiFormat::Anthropic,
        "{id} wire format"
    );
    assert!(descriptor.has_models_endpoint, "{id} has_models_endpoint");
    let oauth = descriptor
        .oauth
        .as_ref()
        .unwrap_or_else(|| panic!("{id} must declare an OAuthConfig"));

    // The hosts the manifest may allowlist: the API base host plus the OAuth
    // authorize/token hosts the profile authenticates against — nothing wider.
    let host_of = |raw: &str| {
        url::Url::parse(raw)
            .unwrap_or_else(|e| panic!("{id} url {raw}: {e}"))
            .host_str()
            .unwrap_or_else(|| panic!("{id} url {raw} has no host"))
            .to_string()
    };
    let base_url = descriptor
        .base_url
        .unwrap_or_else(|| panic!("{id} must declare a base_url to allowlist"));
    let mut expected_hosts = std::collections::BTreeSet::new();
    expected_hosts.insert(host_of(base_url));
    expected_hosts.insert(host_of(oauth.authorize_url));
    expected_hosts.insert(host_of(oauth.token_url));

    let bundle_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins")
        .join(id);
    let path = bundle_dir.join("ryuzi-plugin.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let manifest: PluginBundleManifest =
        toml::from_str(&text).unwrap_or_else(|e| panic!("{id} manifest: {e}"));
    manifest
        .validate()
        .unwrap_or_else(|e| panic!("{id} manifest is invalid: {e}"));

    assert_eq!(manifest.id, id, "bundle id");
    assert_eq!(
        manifest.provider_ids,
        vec![id.to_string()],
        "{id} must declare provider-ids EXPLICITLY so the router registers its \
         transport under that id",
    );
    // Exactly one `[[oauth]]` profile, whose id equals the string the component
    // passes to `authorized-request` (and the router provider id).
    assert_eq!(manifest.oauth.len(), 1, "{id} declares one oauth profile");
    assert_eq!(
        manifest.oauth[0].id, id,
        "{id} oauth profile id must equal the provider id the guest requests",
    );
    let hosts: std::collections::BTreeSet<String> = manifest
        .permissions
        .network
        .iter()
        .map(|host| host.0.clone())
        .collect();
    assert_eq!(
        hosts, expected_hosts,
        "{id} must allowlist exactly the API host plus the OAuth authorize/token \
         hosts its profile authenticates against ({expected_hosts:?}) — a wider \
         allowlist widens where a request may travel",
    );

    // The full base URL — scheme, host AND path — as the component transcribed
    // it into its own `logic.rs`, read from source text because the bundle is a
    // separate workspace this crate cannot link. The allowlist check matches on
    // HOST only and the battery overrides the base URL through storage, so a
    // wrong PATH escapes every other gate here.
    let logic_path = bundle_dir.join("src/logic.rs");
    let logic = std::fs::read_to_string(&logic_path)
        .unwrap_or_else(|e| panic!("{}: {e}", logic_path.display()));
    let configured = configured_default_base_url(&logic).unwrap_or_else(|| {
        panic!(
            "{} must assign a string literal to `default_base_url`",
            logic_path.display()
        )
    });
    assert_eq!(
        configured,
        base_url,
        "{} configures base URL {configured:?} but the {id} descriptor says \
         {base_url:?}",
        logic_path.display(),
    );
}
