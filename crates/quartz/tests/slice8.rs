use quartz_kernel::{
    ComponentSpec, ComponentTree, CompositionPatch, Error, EventGrant, FiberState, Limits, Runtime,
    SnapshotGrant, TraceEvent, WorkspaceGrant,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const BEFORE: &[u8] = b"alpha\n";
const CANDIDATE: &[u8] = b"alpha reviewed by Quartz\n";
const EXTERNAL: &[u8] = b"external concurrent edit\n";
static NEXT_CASE: AtomicU64 = AtomicU64::new(1);

#[test]
fn payload_reads_are_bounded_and_require_declared_committed_authority() {
    let case = TempCase::new("payload-authority");
    append_candidate(&case);

    let mut runtime = persistent_runtime(&case);
    runtime
        .apply_tree(single("probe", spec("event-payload-probe").with_config(0)))
        .unwrap();
    assert_eq!(runtime.fiber_state("probe"), Some(FiberState::Active));
    assert_eq!(
        runtime.state_value("probe", 820),
        Some(CANDIDATE.len() as u64)
    );
    assert_eq!(
        runtime.state_value("probe", 821),
        Some(u64::from(CANDIDATE[0]))
    );
    assert_eq!(runtime.state_value("probe", 822), Some(4));
    runtime.shutdown_persistent().unwrap();
    assert!(runtime.is_observationally_clean());

    let mut undeclared = persistent_runtime(&case);
    undeclared
        .apply_tree(single(
            "probe",
            spec("event-payload-undeclared").with_config(0),
        ))
        .unwrap();
    assert!(matches!(
        undeclared.fiber_state("probe"),
        Some(FiberState::Failed(message)) if message.contains("guest returned status 2")
    ));
    undeclared.shutdown_persistent().unwrap();

    let mut unbound = Runtime::new(Limits::default()).unwrap();
    unbound
        .apply_tree(single("probe", spec("event-payload-probe").with_config(0)))
        .unwrap();
    assert_eq!(unbound.fiber_state("probe"), Some(FiberState::Inactive));
    unbound.apply_tree(ComponentTree::default()).unwrap();
    assert!(unbound.is_observationally_clean());

    let scalar = TempCase::new("payload-free");
    append_scalar(&scalar);
    let mut payload_free = persistent_runtime(&scalar);
    payload_free
        .apply_tree(single("probe", spec("event-payload-probe").with_config(0)))
        .unwrap();
    assert!(matches!(
        payload_free.fiber_state("probe"),
        Some(FiberState::Failed(message)) if message.contains("guest returned status 3")
    ));
    payload_free.shutdown_persistent().unwrap();
}

#[test]
fn reviewed_candidate_requires_explicit_exact_approval() {
    let case = TempCase::new("approval");
    fs::write(case.source(), BEFORE).unwrap();
    append_candidate(&case);

    let edit = Edit::new("proposal-editor-a", 8_001, CANDIDATE);
    let mut denied = persistent_runtime(&case);
    denied
        .apply_tree(reviewed_tree(&case, edit, 8_000))
        .unwrap();
    assert!(matches!(
        denied.fiber_state("root/editor"),
        Some(FiberState::Failed(message)) if message.contains("guest returned status 7")
    ));
    assert_eq!(fs::read(case.source()).unwrap(), BEFORE);
    denied.shutdown_persistent().unwrap();

    let wrong_turn = Edit::new("proposal-editor-a", 8_001, CANDIDATE).with_turn(2);
    let mut unselected = persistent_runtime(&case);
    unselected
        .apply_tree(reviewed_tree(&case, wrong_turn, 8_001))
        .unwrap();
    assert!(matches!(
        unselected.fiber_state("root/editor"),
        Some(FiberState::Failed(message)) if message.contains("guest returned status 3")
    ));
    assert_eq!(fs::read(case.source()).unwrap(), BEFORE);
    unselected.shutdown_persistent().unwrap();

    let wrong = Edit::new("proposal-editor-a", 8_001, b"different reviewed bytes\n");
    let mut mismatched = persistent_runtime(&case);
    mismatched
        .apply_tree(reviewed_tree(&case, wrong, 8_001))
        .unwrap();
    assert!(matches!(
        mismatched.fiber_state("root/editor"),
        Some(FiberState::Failed(_))
    ));
    assert_eq!(fs::read(case.source()).unwrap(), BEFORE);
    mismatched.shutdown_persistent().unwrap();

    let mut approved = persistent_runtime(&case);
    approved
        .apply_tree(reviewed_tree(&case, edit, 8_001))
        .unwrap();
    assert_eq!(
        approved.fiber_state("root/editor"),
        Some(FiberState::Active)
    );
    assert_eq!(fs::read(case.source()).unwrap(), CANDIDATE);
    approved.shutdown_persistent().unwrap();
    assert_eq!(fs::read(case.source()).unwrap(), BEFORE);
    assert!(approved.is_observationally_clean());

    let drift_edit = Edit::new("proposal-editor-b", 8_002, CANDIDATE);
    let mut drifted = persistent_runtime(&case);
    drifted
        .apply_tree(reviewed_tree(&case, drift_edit, 8_002))
        .unwrap();
    assert_eq!(fs::read(case.source()).unwrap(), CANDIDATE);
    fs::write(case.source(), EXTERNAL).unwrap();
    let canonical_source = fs::canonicalize(case.source()).unwrap();
    let error = drifted.shutdown_persistent().unwrap_err();
    assert!(
        matches!(
            &error,
            Error::MutationAmbiguous(path) if path == &canonical_source
        ),
        "drift recovery error: {error:?}"
    );
    assert_eq!(fs::read(case.source()).unwrap(), EXTERNAL);
}

#[test]
fn reviewed_candidate_survives_restart_and_governed_editor_replacement() {
    let case = TempCase::new("replacement");
    fs::write(case.source(), BEFORE).unwrap();
    append_candidate(&case);

    let mut runtime = persistent_runtime(&case);
    assert_eq!(runtime.events().len(), 1);
    assert_eq!(
        runtime.events()[0].payload.as_ref().unwrap().bytes,
        CANDIDATE
    );
    let base_revision = runtime.composition_revision() + 1;
    runtime
        .apply_tree(replacement_tree(&case, base_revision))
        .unwrap();
    assert_eq!(fs::read(case.source()).unwrap(), CANDIDATE);

    let (old, new) = runtime
        .trace()
        .iter()
        .find_map(|event| match event {
            TraceEvent::ReplacementCommitted { old, new, path } if path == "root/editor" => {
                Some((*old, *new))
            }
            _ => None,
        })
        .expect("governed editor replacement");
    let old_recovery = runtime
        .trace()
        .iter()
        .position(|event| {
            matches!(
                event,
                TraceEvent::EffectRecovered { fiber, kind, .. }
                    if *fiber == old && kind == "workspace-publication"
            )
        })
        .expect("old editor publication recovery");
    let new_publication = runtime
        .trace()
        .iter()
        .position(|event| {
            matches!(
                event,
                TraceEvent::EffectApplied { fiber, kind, .. }
                    if *fiber == new && kind == "workspace-publication"
            )
        })
        .expect("replacement editor publication");
    assert!(old_recovery < new_publication);

    runtime.shutdown_persistent().unwrap();
    assert_eq!(fs::read(case.source()).unwrap(), BEFORE);
    assert!(runtime.is_observationally_clean());
}

#[derive(Clone, Copy)]
struct Edit<'a> {
    module: &'a str,
    turn: u64,
    operation: u64,
    result: &'a [u8],
}

impl<'a> Edit<'a> {
    fn new(module: &'a str, operation: u64, result: &'a [u8]) -> Self {
        Self {
            module,
            turn: 1,
            operation,
            result,
        }
    }

    fn with_turn(mut self, turn: u64) -> Self {
        self.turn = turn;
        self
    }
}

fn append_candidate(case: &TempCase) {
    fs::write(case.candidate(), CANDIDATE).unwrap();
    let mut runtime = persistent_runtime(case);
    runtime
        .apply_tree(single(
            "candidate",
            spec("candidate-appender")
                .with_event_grants(vec![event_grant()])
                .with_snapshot_grants(vec![snapshot_grant(&case.candidate())]),
        ))
        .unwrap();
    assert_eq!(runtime.events().len(), 1);
    assert_eq!(
        runtime.events()[0].payload.as_ref().unwrap().bytes,
        CANDIDATE
    );
    runtime.shutdown_persistent().unwrap();
}

fn append_scalar(case: &TempCase) {
    let mut runtime = persistent_runtime(case);
    runtime
        .apply_tree(single(
            "scalar",
            spec("event-appender")
                .with_config(42)
                .with_event_grants(vec![event_grant()]),
        ))
        .unwrap();
    assert_eq!(runtime.events().len(), 1);
    assert!(runtime.events()[0].payload.is_none());
    runtime.shutdown_persistent().unwrap();
}

fn reviewed_tree(case: &TempCase, edit: Edit<'_>, authority_maximum: u64) -> ComponentTree {
    ComponentTree {
        roots: vec![
            ComponentSpec::new("root", artifact("root"))
                .with_config(2)
                .with_children(vec![
                    ComponentSpec::new("authority", artifact("mutation-authority"))
                        .with_config(authority_maximum),
                    editor_spec(case, edit),
                ]),
        ],
    }
}

fn replacement_tree(case: &TempCase, base_revision: u64) -> ComponentTree {
    let editor_a = Edit::new("proposal-editor-a", 8_001, CANDIDATE);
    let editor_b = Edit::new("proposal-editor-b", 8_002, CANDIDATE);
    ComponentTree {
        roots: vec![
            ComponentSpec::new("root", artifact("root"))
                .with_config(3)
                .with_children(vec![
                    ComponentSpec::new("authority", artifact("mutation-authority"))
                        .with_config(8_002),
                    editor_spec(case, editor_a),
                    ComponentSpec::new("governor", artifact("governor")),
                ]),
            ComponentSpec::new("zz-controller", artifact("durable-controller"))
                .with_config(base_revision << 32)
                .with_patches(vec![CompositionPatch::replace(
                    "root/editor",
                    editor_spec(case, editor_b),
                )]),
        ],
    }
}

fn editor_spec(case: &TempCase, edit: Edit<'_>) -> ComponentSpec {
    ComponentSpec::new("editor", artifact(edit.module))
        .with_config(edit.turn)
        .with_workspace_grants(vec![
            WorkspaceGrant::new(
                case.source(),
                case.mutation(),
                edit.operation,
                "reviewed candidate turn 1",
                sha256(BEFORE),
                sha256(edit.result),
                64 * 1024,
            )
            .unwrap(),
        ])
}

fn persistent_runtime(case: &TempCase) -> Runtime {
    Runtime::open_persistent(
        Limits::default(),
        ComponentSpec::new("event-store", artifact("event-store"))
            .with_journal_paths(vec![case.journal()])
            .with_event_stream_paths(vec![case.events()]),
    )
    .unwrap()
}

fn event_grant() -> EventGrant {
    EventGrant::new("quartz.agent", "repository-turn", 2)
}

fn snapshot_grant(path: &Path) -> SnapshotGrant {
    SnapshotGrant::from_file(path, "slice8 durable candidate").unwrap()
}

fn single(entry: &str, component: ComponentSpec) -> ComponentTree {
    ComponentTree {
        roots: vec![ComponentSpec {
            entry: entry.into(),
            ..component
        }],
    }
}

fn spec(name: &str) -> ComponentSpec {
    ComponentSpec::new(name, artifact(name))
}

fn artifact(name: &str) -> PathBuf {
    PathBuf::from(env!("QUARTZ_FIXTURE_DIR"))
        .join(name)
        .with_extension("wasm")
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

struct TempCase {
    root: PathBuf,
}

impl TempCase {
    fn new(name: &str) -> Self {
        let id = NEXT_CASE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("quartz-slice8-{name}-{}-{id}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn source(&self) -> PathBuf {
        self.path("source.txt")
    }

    fn candidate(&self) -> PathBuf {
        self.path("candidate.txt")
    }

    fn journal(&self) -> PathBuf {
        self.path("composition.qj")
    }

    fn events(&self) -> PathBuf {
        self.path("events.qe")
    }

    fn mutation(&self) -> PathBuf {
        self.path("mutations.qm")
    }
}

impl Drop for TempCase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
