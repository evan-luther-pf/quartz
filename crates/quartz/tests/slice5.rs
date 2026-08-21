use quartz_kernel::{
    ComponentSpec, ComponentTree, Error, EventGrant, FiberState, Limits, Runtime, SnapshotGrant,
};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_CASE: AtomicU64 = AtomicU64::new(1);
const FACT_KIND_SHIFT: u64 = 56;
const FACT_TURN_SHIFT: u64 = 48;
const FACT_INVOCATION_SHIFT: u64 = 32;

#[test]
fn repository_turns_survive_restarts_and_inspector_replacement() {
    let case = TempCase::new("complete");
    let source_a = case.source("repository-a.txt", b"Quartz repository snapshot A\n");
    let source_b = case.source("repository-b.txt", b"Quartz repository snapshot B\n");
    let mut runtime = persistent_runtime(&case, Limits::default()).unwrap();
    runtime
        .apply_tree(agent_tree(&source_a, &source_b, Inspector::A, false))
        .unwrap();
    assert_facts(&runtime, &[(1, 1, 0, 1)]);
    drop(runtime);

    let turn_one = [
        (2, 1, 17, 1),
        (3, 1, 18, 1),
        (4, 1, 18, 5001),
        (2, 1, 19, 5001),
        (5, 1, 0, 6001),
        (6, 1, 0, 1),
        (7, 1, 0, 1),
    ];
    runtime = advance(&case, turn_one.len(), Limits::default());
    assert_facts(
        &runtime,
        &std::iter::once((1, 1, 0, 1))
            .chain(turn_one)
            .collect::<Vec<_>>(),
    );
    let events = runtime.events();
    let payload = events[3].payload.as_ref().unwrap();
    assert_eq!(payload.bytes, fs::read(&source_a).unwrap());
    assert_eq!(payload.provenance, source_a.display().to_string());
    assert_eq!(payload.sha256, sha256(&payload.bytes));
    assert!(
        runtime.events().iter().enumerate().all(
            |(index, event)| (event.sequence, event.id) == (index as u64 + 1, index as u64 + 1)
        )
    );
    let first_transcript = runtime.events().to_vec();

    let base_revision = runtime.composition_revision();
    runtime
        .apply_tree(replacement_tree(&source_a, &source_b, base_revision, false))
        .unwrap();
    assert_eq!(runtime.events(), first_transcript);
    runtime
        .apply_tree(replacement_tree(&source_a, &source_b, base_revision, true))
        .unwrap();
    assert_eq!(
        &runtime.events()[..first_transcript.len()],
        first_transcript
    );
    assert_eq!(decode(runtime.events().last().unwrap().value), (1, 2, 0, 2));
    drop(runtime);

    let turn_two = [
        (2, 2, 33, 1),
        (3, 2, 34, 1),
        (4, 2, 34, 5002),
        (2, 2, 35, 5002),
        (5, 2, 0, 6002),
        (6, 2, 0, 1),
        (7, 2, 0, 1),
    ];
    runtime = advance(&case, turn_two.len(), Limits::default());
    assert_facts(
        &runtime,
        &first_transcript
            .iter()
            .map(|event| decode(event.value))
            .chain(std::iter::once((1, 2, 0, 2)))
            .chain(turn_two)
            .collect::<Vec<_>>(),
    );
    let events = runtime.events();
    let second_payload = events[11].payload.as_ref().unwrap();
    assert_eq!(second_payload.bytes, fs::read(&source_b).unwrap());
    assert_eq!(second_payload.provenance, source_b.display().to_string());
    assert_eq!(second_payload.sha256, sha256(&second_payload.bytes));
    assert_eq!(
        runtime
            .events()
            .iter()
            .filter(|event| event.payload.is_some())
            .count(),
        2
    );

    runtime.apply_tree(ComponentTree::default()).unwrap();
    runtime.shutdown_persistent().unwrap();
    assert!(runtime.is_observationally_clean());
}

#[test]
fn snapshot_admission_rejects_missing_changed_and_undeclared_paths() {
    for changed in [false, true] {
        let case = TempCase::new(if changed { "changed" } else { "missing" });
        let source = case.source("repository.txt", b"admitted bytes\n");
        let grant = snapshot_grant(&source);
        if changed {
            fs::write(&source, b"changed bytes\n").unwrap();
        } else {
            fs::remove_file(&source).unwrap();
        }
        let mut runtime = persistent_runtime(&case, Limits::default()).unwrap();
        let error = runtime
            .apply_tree(single_inspector(
                "repo-inspector-a",
                fnv1a(b"admitted bytes\n"),
                vec![grant],
            ))
            .unwrap_err();
        assert!(matches!(
            error,
            Error::SnapshotIo { .. } | Error::SnapshotDigestMismatch { .. }
        ));
        runtime.shutdown_persistent().unwrap();
    }

    let case = TempCase::new("undeclared-index");
    let source = case.source("repository.txt", b"only one grant\n");
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    runtime
        .apply_tree(single_inspector(
            "repo-inspector-b",
            fnv1a(b"only one grant\n"),
            vec![snapshot_grant(&source)],
        ))
        .unwrap();
    assert!(matches!(
        runtime.fiber_state("repo-inspector-b"),
        Some(FiberState::Failed(_))
    ));
    assert!(runtime.events().is_empty());
    runtime.apply_tree(ComponentTree::default()).unwrap();
    assert!(runtime.is_observationally_clean());

    for (limits, expected_limit) in [
        (
            Limits {
                max_snapshot_grants: 0,
                ..Limits::default()
            },
            "grant",
        ),
        (
            Limits {
                max_snapshot_bytes: 4,
                ..Limits::default()
            },
            "bytes",
        ),
    ] {
        let case = TempCase::new(expected_limit);
        let source = case.source("repository.txt", b"bounded snapshot\n");
        let mut runtime = Runtime::new(limits).unwrap();
        let error = runtime
            .apply_tree(single_inspector(
                "repo-inspector-a",
                fnv1a(b"bounded snapshot\n"),
                vec![snapshot_grant(&source)],
            ))
            .unwrap_err();
        assert!(matches!(
            (expected_limit, error),
            ("grant", Error::SnapshotGrantLimit { .. })
                | ("bytes", Error::SnapshotBytesLimit { .. })
        ));
        assert!(runtime.is_observationally_clean());
    }
}

#[test]
fn payload_bounds_and_corruption_fail_closed() {
    for (name, limits) in [
        (
            "count-limit",
            Limits {
                max_payload_records: 0,
                ..Limits::default()
            },
        ),
        (
            "record-limit",
            Limits {
                max_payload_bytes: 4,
                ..Limits::default()
            },
        ),
        (
            "total-limit",
            Limits {
                max_payload_total_bytes: 4,
                ..Limits::default()
            },
        ),
    ] {
        let case = TempCase::new(name);
        let source_a = case.source("repository-a.txt", b"payload exceeds limit\n");
        let source_b = case.source("repository-b.txt", b"unused\n");
        let mut runtime = persistent_runtime(&case, limits).unwrap();
        runtime
            .apply_tree(agent_tree(&source_a, &source_b, Inspector::A, false))
            .unwrap();
        drop(runtime);
        for _ in 0..2 {
            runtime = persistent_runtime(&case, limits).unwrap();
            drop(runtime);
        }
        runtime = persistent_runtime(&case, limits).unwrap();
        assert!(matches!(
            runtime.fiber_state("a-loop"),
            Some(FiberState::Failed(_))
        ));
        assert_eq!(runtime.events().len(), 3);
        assert!(runtime.events().iter().all(|event| event.payload.is_none()));
        runtime.apply_tree(ComponentTree::default()).unwrap();
        runtime.shutdown_persistent().unwrap();
        assert!(runtime.is_observationally_clean());
    }

    let case = TempCase::new("corrupt-payload");
    let source_a = case.source("repository-a.txt", b"unique durable evidence bytes\n");
    let source_b = case.source("repository-b.txt", b"unused\n");
    let mut runtime = persistent_runtime(&case, Limits::default()).unwrap();
    runtime
        .apply_tree(agent_tree(&source_a, &source_b, Inspector::A, false))
        .unwrap();
    drop(runtime);
    runtime = advance(&case, 3, Limits::default());
    assert!(runtime.events()[3].payload.is_some());
    drop(runtime);
    let event_path = case.path("events.qe");
    let mut torn_frame = Vec::new();
    torn_frame.extend_from_slice(&5_u64.to_le_bytes());
    torn_frame.extend_from_slice(&16_u32.to_le_bytes());
    torn_frame.extend_from_slice(b"torn");
    OpenOptions::new()
        .append(true)
        .open(&event_path)
        .unwrap()
        .write_all(&torn_frame)
        .unwrap();
    runtime = persistent_runtime(&case, Limits::default()).unwrap();
    assert_eq!(
        runtime.events()[3].payload.as_ref().unwrap().bytes,
        fs::read(&source_a).unwrap()
    );
    drop(runtime);
    let mut bytes = fs::read(&event_path).unwrap();
    let mut cursor = 8;
    let mut last_payload = None;
    while cursor < bytes.len() {
        let length =
            u32::from_le_bytes(bytes[cursor + 8..cursor + 12].try_into().unwrap()) as usize;
        let payload = cursor + 12;
        last_payload = Some(payload);
        cursor = payload + length + 32;
    }
    bytes[last_payload.expect("event frame")] ^= 0xff;
    fs::write(&event_path, bytes).unwrap();
    assert!(matches!(
        persistent_runtime(&case, Limits::default()).err().unwrap(),
        Error::EventCorrupt(_)
    ));
}

#[test]
fn restart_rejects_snapshot_drift_without_changing_durable_payload() {
    let case = TempCase::new("snapshot-drift");
    let source_a = case.source("repository-a.txt", b"stable snapshot\n");
    let source_b = case.source("repository-b.txt", b"unused\n");
    let mut runtime = persistent_runtime(&case, Limits::default()).unwrap();
    runtime
        .apply_tree(agent_tree(&source_a, &source_b, Inspector::A, false))
        .unwrap();
    drop(runtime);
    runtime = advance(&case, 3, Limits::default());
    let durable_payload = runtime.events()[3].payload.clone().unwrap();
    drop(runtime);
    fs::write(&source_a, b"drifted snapshot\n").unwrap();
    assert!(matches!(
        persistent_runtime(&case, Limits::default()).err().unwrap(),
        Error::SnapshotDigestMismatch { .. }
    ));
    assert_eq!(durable_payload.bytes, b"stable snapshot\n");
}

fn persistent_runtime(case: &TempCase, limits: Limits) -> quartz_kernel::Result<Runtime> {
    Runtime::open_persistent(
        limits,
        spec("event-store", "event-store")
            .with_journal_paths(vec![case.path("composition.qj")])
            .with_event_stream_paths(vec![case.path("events.qe")]),
    )
}

fn advance(case: &TempCase, generations: usize, limits: Limits) -> Runtime {
    let mut runtime = persistent_runtime(case, limits).unwrap();
    for _ in 1..generations {
        drop(runtime);
        runtime = persistent_runtime(case, limits).unwrap();
    }
    runtime
}

#[derive(Clone, Copy)]
enum Inspector {
    A,
    B,
}

fn agent_tree(
    source_a: &Path,
    source_b: &Path,
    inspector: Inspector,
    second_prompt: bool,
) -> ComponentTree {
    let event_grant = || EventGrant::new("quartz.agent", "repository-turn", 2);
    let (module, config, grants) = match inspector {
        Inspector::A => (
            "repo-inspector-a",
            fnv1a(&fs::read(source_a).unwrap()),
            vec![snapshot_grant(source_a)],
        ),
        Inspector::B => (
            "repo-inspector-b",
            fnv1a(&fs::read(source_b).unwrap()),
            vec![snapshot_grant(source_a), snapshot_grant(source_b)],
        ),
    };
    let mut roots = vec![
        spec("a-loop", "repo-agent-loop")
            .with_event_grants(vec![event_grant()])
            .with_snapshot_grants(vec![snapshot_grant(source_a), snapshot_grant(source_b)]),
        spec("b-gateway", "agent-gateway"),
        spec("c-provider", "repo-agent-provider"),
        spec("d-tool", module)
            .with_config(config)
            .with_snapshot_grants(grants),
        spec("e-client", "agent-client")
            .with_config(1)
            .with_event_grants(vec![event_grant()]),
    ];
    if second_prompt {
        roots.push(
            spec("f-client", "agent-client")
                .with_config(2)
                .with_event_grants(vec![event_grant()]),
        );
    }
    ComponentTree { roots }
}

fn replacement_tree(
    source_a: &Path,
    source_b: &Path,
    base_revision: u64,
    replaced: bool,
) -> ComponentTree {
    let mut tree = agent_tree(
        source_a,
        source_b,
        if replaced { Inspector::B } else { Inspector::A },
        replaced,
    );
    tree.roots.push(spec("x-governor", "governor"));
    tree.roots.push(
        spec("y-controller", "durable-controller")
            .with_config((base_revision + 1) << 32)
            .with_patches(vec![quartz_kernel::CompositionPatch::replace(
                "d-tool",
                spec("d-tool", "repo-inspector-b")
                    .with_config(fnv1a(&fs::read(source_b).unwrap()))
                    .with_snapshot_grants(vec![snapshot_grant(source_a), snapshot_grant(source_b)]),
            )]),
    );
    tree
}

fn single_inspector(module: &str, config: u64, grants: Vec<SnapshotGrant>) -> ComponentTree {
    ComponentTree {
        roots: vec![
            spec(module, module)
                .with_config(config)
                .with_snapshot_grants(grants),
        ],
    }
}

fn snapshot_grant(path: &Path) -> SnapshotGrant {
    SnapshotGrant::from_file(path, path.display().to_string()).unwrap()
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

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

fn assert_facts(runtime: &Runtime, expected: &[(u64, u64, u64, u64)]) {
    assert_eq!(
        runtime
            .events()
            .iter()
            .map(|event| decode(event.value))
            .collect::<Vec<_>>(),
        expected
    );
}

fn decode(value: u64) -> (u64, u64, u64, u64) {
    (
        value >> FACT_KIND_SHIFT,
        (value >> FACT_TURN_SHIFT) & 0xff,
        (value >> FACT_INVOCATION_SHIFT) & 0xffff,
        value & 0xffff_ffff,
    )
}

fn spec(entry: &str, module: &str) -> ComponentSpec {
    ComponentSpec::new(entry, artifact(module))
}

fn artifact(module: &str) -> PathBuf {
    Path::new(env!("QUARTZ_FIXTURE_DIR"))
        .join(module)
        .with_extension("wasm")
}

struct TempCase {
    root: PathBuf,
}

impl TempCase {
    fn new(name: &str) -> Self {
        let id = NEXT_CASE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("quartz-slice5-{name}-{}-{id}", std::process::id()));
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
