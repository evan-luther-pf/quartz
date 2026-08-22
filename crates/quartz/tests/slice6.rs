use quartz_kernel::{
    ComponentSpec, ComponentTree, CompositionPatch, Error, EventGrant, ExchangeAdapter,
    ExchangeFailure, ExchangeGrant, ExchangeResponse, FiberState, Limits, Runtime, SnapshotGrant,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

static NEXT_CASE: AtomicU64 = AtomicU64::new(1);

#[test]
fn production_response_is_bounded_durable_and_replaceable() {
    let case = TempCase::new("success");
    let prompt = case.source("prompt.txt", b"Answer with one short sentence.");
    let adapter = Arc::new(FakeAdapter::success(b"Quartz model answer", 7));
    let mut runtime = persistent_runtime(&case, adapter.clone(), Limits::default()).unwrap();
    runtime
        .apply_tree(production_tree(&case, &prompt, 64 * 1024, 64 * 1024, 1_000))
        .unwrap();
    assert_eq!(runtime.events().len(), 1);
    assert_eq!(
        runtime.events()[0].payload.as_ref().unwrap().bytes,
        fs::read(&prompt).unwrap()
    );
    drop(runtime);

    runtime = persistent_runtime(&case, adapter.clone(), Limits::default()).unwrap();
    assert_eq!(adapter.calls(), 0);
    assert_eq!(
        runtime.events().len(),
        2,
        "loop={:?} provider={:?} trace={:?}",
        runtime.fiber_state("a-loop"),
        runtime.fiber_state("c-provider"),
        runtime.trace()
    );
    drop(runtime);

    runtime = persistent_runtime(&case, adapter.clone(), Limits::default()).unwrap();
    assert_eq!(
        adapter.calls(),
        1,
        "loop={:?} provider={:?} events={:?} trace={:?}",
        runtime.fiber_state("a-loop"),
        runtime.fiber_state("c-provider"),
        runtime
            .events()
            .iter()
            .map(|event| event.value >> 56)
            .collect::<Vec<_>>(),
        runtime.trace()
    );
    assert_eq!(runtime.events().len(), 3);
    let response = runtime.events()[2].payload.clone().unwrap();
    assert_eq!(response.bytes, b"Quartz model answer");
    assert_eq!(response.provenance, "fake:model-response");
    assert_eq!(response.sha256, sha256(b"Quartz model answer"));
    drop(runtime);

    runtime = advance(&case, adapter.clone(), 2);
    assert_eq!(adapter.calls(), 1);
    assert_eq!(
        runtime
            .events()
            .iter()
            .map(|event| event.value >> 56)
            .collect::<Vec<_>>(),
        vec![1, 2, 5, 6, 7]
    );
    assert_eq!(runtime.events()[2].payload.as_ref(), Some(&response));

    let base_revision = runtime.composition_revision();
    runtime
        .apply_tree(governed_provider_tree(&case, &prompt, base_revision, false))
        .unwrap();
    runtime
        .apply_tree(governed_provider_tree(&case, &prompt, base_revision, true))
        .unwrap();
    assert!(matches!(
        runtime.fiber_state("c-provider"),
        Some(FiberState::Active)
    ));
    assert_eq!(adapter.calls(), 1);
    assert_eq!(runtime.events()[2].payload.as_ref(), Some(&response));
    assert_eq!(runtime.observation().exchange_registrations, 0);

    let mut restore_tree = governed_provider_tree(&case, &prompt, base_revision, true);
    restore_tree.roots.pop();
    runtime.apply_tree(restore_tree).unwrap();
    assert!(matches!(
        runtime.fiber_state("c-provider"),
        Some(FiberState::Active)
    ));
    assert_eq!(runtime.observation().exchange_registrations, 1);
    assert_eq!(adapter.calls(), 1);
    runtime.shutdown_persistent().unwrap();
    assert!(runtime.is_observationally_clean());
}

#[test]
fn missing_network_authority_and_adapter_fail_closed() {
    let case = TempCase::new("denied");
    let prompt = case.source("prompt.txt", b"bounded prompt");
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    let error = runtime
        .apply_tree(ComponentTree {
            roots: vec![spec("provider", "production-agent-provider")],
        })
        .unwrap_err();
    assert!(matches!(error, Error::Manifest(message) if message.contains("exchange authority")));

    let mut runtime = Runtime::open_persistent(
        Limits::default(),
        spec("event-store", "event-store")
            .with_journal_paths(vec![case.path("composition.qj")])
            .with_event_stream_paths(vec![case.path("events.qe")]),
    )
    .unwrap();
    runtime
        .apply_tree(production_tree(&case, &prompt, 1024, 1024, 1_000))
        .unwrap();
    assert!(matches!(
        runtime.fiber_state("c-provider"),
        Some(FiberState::Failed(message)) if message.contains("guest returned status 7")
    ));
    assert!(!case.path("exchange.qx").exists());
}

#[test]
fn request_and_response_limits_prevent_unbounded_exchange() {
    let request_case = TempCase::new("request-limit");
    let prompt = request_case.source("prompt.txt", b"request exceeds four bytes");
    let adapter = Arc::new(FakeAdapter::success(b"ok", 1));
    let mut runtime =
        persistent_runtime(&request_case, adapter.clone(), Limits::default()).unwrap();
    runtime
        .apply_tree(production_tree(&request_case, &prompt, 4, 1024, 1_000))
        .unwrap();
    drop(runtime);
    runtime = persistent_runtime(&request_case, adapter.clone(), Limits::default()).unwrap();
    drop(runtime);
    runtime = persistent_runtime(&request_case, adapter.clone(), Limits::default()).unwrap();
    assert!(matches!(
        runtime.fiber_state("a-loop"),
        Some(FiberState::Failed(_))
    ));
    assert_eq!(adapter.calls(), 0);

    let response_case = TempCase::new("response-limit");
    let prompt = response_case.source("prompt.txt", b"small");
    let adapter = Arc::new(FakeAdapter::success(b"response exceeds four bytes", 1));
    let mut runtime =
        persistent_runtime(&response_case, adapter.clone(), Limits::default()).unwrap();
    runtime
        .apply_tree(production_tree(&response_case, &prompt, 1024, 4, 1_000))
        .unwrap();
    drop(runtime);
    runtime = persistent_runtime(&response_case, adapter.clone(), Limits::default()).unwrap();
    drop(runtime);
    runtime = persistent_runtime(&response_case, adapter.clone(), Limits::default()).unwrap();
    assert!(matches!(
        runtime.fiber_state("a-loop"),
        Some(FiberState::Active)
    ));
    assert_eq!(adapter.calls(), 1);
    assert_eq!(
        runtime
            .events()
            .iter()
            .map(|event| event.value >> 56)
            .collect::<Vec<_>>(),
        vec![1, 2, 8]
    );
    drop(runtime);
    runtime = persistent_runtime(&response_case, adapter.clone(), Limits::default()).unwrap();
    assert_eq!(adapter.calls(), 1);
    assert_eq!(
        runtime
            .events()
            .iter()
            .map(|event| event.value >> 56)
            .collect::<Vec<_>>(),
        vec![1, 2, 8, 7]
    );
}

#[test]
fn timed_out_exchange_is_ambiguous_and_never_retried() {
    let case = TempCase::new("timeout");
    let prompt = case.source("prompt.txt", b"timeout prompt");
    let adapter = Arc::new(FakeAdapter::delayed(
        Duration::from_millis(500),
        b"late response",
    ));
    let mut runtime = persistent_runtime(&case, adapter.clone(), Limits::default()).unwrap();
    runtime
        .apply_tree(production_tree(&case, &prompt, 1024, 1024, 10))
        .unwrap();
    drop(runtime);
    runtime = persistent_runtime(&case, adapter.clone(), Limits::default()).unwrap();
    assert_eq!(adapter.calls(), 0);
    drop(runtime);

    runtime = persistent_runtime(&case, adapter.clone(), Limits::default()).unwrap();
    assert!(matches!(
        runtime.fiber_state("a-loop"),
        Some(FiberState::Active)
    ));
    assert_eq!(adapter.calls(), 1);
    assert_eq!(
        runtime
            .events()
            .iter()
            .map(|event| event.value >> 56)
            .collect::<Vec<_>>(),
        vec![1, 2, 8]
    );
    drop(runtime);

    runtime = persistent_runtime(&case, adapter.clone(), Limits::default()).unwrap();
    assert_eq!(adapter.calls(), 1);
    assert_eq!(
        runtime
            .events()
            .iter()
            .map(|event| event.value >> 56)
            .collect::<Vec<_>>(),
        vec![1, 2, 8, 7]
    );

    let recovery_case = TempCase::new("timeout-recovery");
    let prompt = recovery_case.source("prompt.txt", b"timeout recovery prompt");
    let recovery_adapter = Arc::new(FakeAdapter::delayed(
        Duration::from_millis(200),
        b"late response",
    ));
    let mut recovery_runtime =
        persistent_runtime(&recovery_case, recovery_adapter.clone(), Limits::default()).unwrap();
    recovery_runtime
        .apply_tree(production_tree(&recovery_case, &prompt, 1024, 1024, 10))
        .unwrap();
    drop(recovery_runtime);
    recovery_runtime =
        persistent_runtime(&recovery_case, recovery_adapter.clone(), Limits::default()).unwrap();
    drop(recovery_runtime);
    recovery_runtime =
        persistent_runtime(&recovery_case, recovery_adapter, Limits::default()).unwrap();
    assert_eq!(recovery_runtime.observation().exchange_workers, 1);
    let recovery_started = Instant::now();
    recovery_runtime.shutdown_persistent().unwrap();
    assert!(recovery_started.elapsed() >= Duration::from_millis(100));
    assert!(recovery_runtime.is_observationally_clean());
}

#[test]
fn started_exchange_without_terminal_blocks_restart_retry() {
    let case = TempCase::new("started");
    let prompt = case.source("prompt.txt", b"crash boundary prompt");
    write_started_exchange(&case.path("exchange.qx"), 17, &fs::read(&prompt).unwrap());
    let adapter = Arc::new(FakeAdapter::success(b"must not run", 1));
    let mut runtime = persistent_runtime(&case, adapter.clone(), Limits::default()).unwrap();
    runtime
        .apply_tree(production_tree(&case, &prompt, 1024, 1024, 1_000))
        .unwrap();
    assert_eq!(runtime.events().len(), 1);
    drop(runtime);
    runtime = persistent_runtime(&case, adapter.clone(), Limits::default()).unwrap();
    assert_eq!(adapter.calls(), 0);
    drop(runtime);

    runtime = persistent_runtime(&case, adapter.clone(), Limits::default()).unwrap();
    assert!(matches!(
        runtime.fiber_state("a-loop"),
        Some(FiberState::Active)
    ));
    assert_eq!(adapter.calls(), 0);
    assert_eq!(
        runtime
            .events()
            .iter()
            .map(|event| event.value >> 56)
            .collect::<Vec<_>>(),
        vec![1, 2, 8]
    );
    drop(runtime);

    runtime = persistent_runtime(&case, adapter.clone(), Limits::default()).unwrap();
    assert_eq!(adapter.calls(), 0);
    assert_eq!(
        runtime
            .events()
            .iter()
            .map(|event| event.value >> 56)
            .collect::<Vec<_>>(),
        vec![1, 2, 8, 7]
    );
}

#[test]
fn cached_success_replays_and_invocation_collision_fails_closed() {
    let cached = TempCase::new("cached");
    let prompt = cached.source("prompt.txt", b"cached prompt");
    write_succeeded_exchange(
        &cached.path("exchange.qx"),
        17,
        &fs::read(&prompt).unwrap(),
        b"cached response",
        9,
    );
    let adapter = Arc::new(FakeAdapter::success(b"must not run", 1));
    let mut runtime = persistent_runtime(&cached, adapter.clone(), Limits::default()).unwrap();
    runtime
        .apply_tree(production_tree(&cached, &prompt, 1024, 1024, 1_000))
        .unwrap();
    drop(runtime);
    runtime = persistent_runtime(&cached, adapter.clone(), Limits::default()).unwrap();
    drop(runtime);
    runtime = persistent_runtime(&cached, adapter.clone(), Limits::default()).unwrap();
    assert_eq!(adapter.calls(), 0);
    let events = runtime.events();
    let response = events[2].payload.as_ref().unwrap();
    assert_eq!(response.bytes, b"cached response");
    assert_eq!(response.provenance, "cached:response");
    assert_eq!(response.sha256, sha256(b"cached response"));
    drop(runtime);

    let collision = TempCase::new("collision");
    let prompt = collision.source("prompt.txt", b"actual prompt");
    write_started_exchange(&collision.path("exchange.qx"), 17, b"different prompt");
    let adapter = Arc::new(FakeAdapter::success(b"must not run", 1));
    let mut runtime = persistent_runtime(&collision, adapter.clone(), Limits::default()).unwrap();
    runtime
        .apply_tree(production_tree(&collision, &prompt, 1024, 1024, 1_000))
        .unwrap();
    drop(runtime);
    runtime = persistent_runtime(&collision, adapter.clone(), Limits::default()).unwrap();
    drop(runtime);
    runtime = persistent_runtime(&collision, adapter.clone(), Limits::default()).unwrap();
    assert!(matches!(
        runtime.fiber_state("a-loop"),
        Some(FiberState::Failed(_))
    ));
    assert_eq!(adapter.calls(), 0);
    assert_eq!(runtime.events().len(), 2);
}

fn persistent_runtime(
    case: &TempCase,
    adapter: Arc<dyn ExchangeAdapter>,
    limits: Limits,
) -> quartz_kernel::Result<Runtime> {
    Runtime::open_persistent_with_exchange(
        limits,
        spec("event-store", "event-store")
            .with_journal_paths(vec![case.path("composition.qj")])
            .with_event_stream_paths(vec![case.path("events.qe")]),
        adapter,
    )
}

fn advance(case: &TempCase, adapter: Arc<dyn ExchangeAdapter>, generations: usize) -> Runtime {
    let mut runtime = persistent_runtime(case, adapter.clone(), Limits::default()).unwrap();
    for _ in 1..generations {
        drop(runtime);
        runtime = persistent_runtime(case, adapter.clone(), Limits::default()).unwrap();
    }
    runtime
}

fn production_tree(
    case: &TempCase,
    prompt: &Path,
    max_request_bytes: usize,
    max_response_bytes: usize,
    timeout_ms: u64,
) -> ComponentTree {
    let event_grant = || EventGrant::new("quartz.agent", "repository-turn", 2);
    ComponentTree {
        roots: vec![
            spec("a-loop", "agent-loop")
                .with_config(1)
                .with_event_grants(vec![event_grant()]),
            spec("b-gateway", "agent-gateway"),
            spec("c-provider", "production-agent-provider").with_exchange_grants(vec![
                ExchangeGrant::new(
                    "fake-adapter",
                    case.path("exchange.qx"),
                    max_request_bytes,
                    max_response_bytes,
                    timeout_ms,
                ),
            ]),
            spec("d-tool", "agent-tool-a"),
            spec("z-client", "production-agent-client")
                .with_config(1)
                .with_event_grants(vec![event_grant()])
                .with_snapshot_grants(vec![snapshot_grant(prompt)]),
        ],
    }
}
fn governed_provider_tree(
    case: &TempCase,
    prompt: &Path,
    base_revision: u64,
    replaced: bool,
) -> ComponentTree {
    let mut tree = production_tree(case, prompt, 64 * 1024, 64 * 1024, 1_000);
    let replacement = spec("c-provider", "agent-provider");
    if replaced {
        tree.roots[2] = replacement.clone();
    }
    tree.roots.push(spec("x-governor", "governor"));
    tree.roots.push(
        spec("y-controller", "durable-controller")
            .with_config((base_revision + 1) << 32)
            .with_patches(vec![CompositionPatch::replace("c-provider", replacement)]),
    );
    tree
}

fn snapshot_grant(path: &Path) -> SnapshotGrant {
    SnapshotGrant::from_file(path, path.display().to_string()).unwrap()
}

fn write_started_exchange(path: &Path, invocation: u64, request: &[u8]) {
    write_exchange_records(
        path,
        vec![serde_json::json!({
            "schema": 3,
            "invocation": invocation,
            "request_sha256": sha256(request),
            "outcome": {"kind": "started"}
        })],
    );
}

fn write_succeeded_exchange(
    path: &Path,
    invocation: u64,
    request: &[u8],
    response: &[u8],
    usage: u64,
) {
    let request_sha256 = sha256(request);
    write_exchange_records(
        path,
        vec![
            serde_json::json!({
                "schema": 3,
                "invocation": invocation,
                "request_sha256": request_sha256,
                "outcome": {"kind": "started"}
            }),
            serde_json::json!({
                "schema": 3,
                "invocation": invocation,
                "request_sha256": request_sha256,
                "outcome": {
                    "kind": "succeeded",
                    "payload": {
                        "provenance": "cached:response",
                        "sha256": sha256(response),
                        "bytes": response
                    },
                    "usage": usage
                }
            }),
        ],
    );
}

fn write_exchange_records(path: &Path, records: Vec<serde_json::Value>) {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"QUARTZX3");
    for (index, record) in records.into_iter().enumerate() {
        let payload = serde_json::to_vec(&record).unwrap();
        let sequence = index as u64 + 1;
        let length = payload.len() as u32;
        bytes.extend_from_slice(&sequence.to_le_bytes());
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend_from_slice(&payload);
        let mut digest = Sha256::new();
        digest.update(sequence.to_le_bytes());
        digest.update(length.to_le_bytes());
        digest.update(&payload);
        bytes.extend_from_slice(&digest.finalize());
    }
    fs::write(path, bytes).unwrap();
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").unwrap();
    }
    output
}

fn spec(entry: &str, module: &str) -> ComponentSpec {
    ComponentSpec::new(entry, artifact(module))
}

fn artifact(module: &str) -> PathBuf {
    Path::new(env!("QUARTZ_FIXTURE_DIR"))
        .join(module)
        .with_extension("wasm")
}

struct FakeAdapter {
    calls: AtomicUsize,
    delay: Duration,
    bytes: Vec<u8>,
    usage: u64,
}

impl FakeAdapter {
    fn success(bytes: &[u8], usage: u64) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            delay: Duration::ZERO,
            bytes: bytes.to_vec(),
            usage,
        }
    }

    fn delayed(delay: Duration, bytes: &[u8]) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            delay,
            bytes: bytes.to_vec(),
            usage: 1,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ExchangeAdapter for FakeAdapter {
    fn identity(&self) -> &str {
        "fake-adapter"
    }

    fn exchange(
        &self,
        request: &[u8],
        _timeout: Duration,
        _max_response_bytes: usize,
    ) -> Result<ExchangeResponse, ExchangeFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(!request.is_empty());
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        Ok(ExchangeResponse {
            bytes: self.bytes.clone(),
            provenance: "fake:model-response".into(),
            usage: self.usage,
        })
    }
}

struct TempCase {
    root: PathBuf,
}

impl TempCase {
    fn new(name: &str) -> Self {
        let id = NEXT_CASE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("quartz-slice6-{name}-{}-{id}", std::process::id()));
        fs::create_dir(&root).unwrap();
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn source(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.path(name);
        fs::write(&path, bytes).unwrap();
        fs::canonicalize(path).unwrap()
    }
}

impl Drop for TempCase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
