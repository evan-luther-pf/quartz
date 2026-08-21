use quartz_kernel::{
    ComponentSpec, ComponentTree, Error, EventGrant, FiberState, Limits, Runtime, SnapshotGrant,
    WorkspaceGrant,
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
const OPERATION: u64 = 9_001;
const DIRECT_OPERATION: u64 = 9_101;
const DIRECT_CANDIDATE: &[u8] = b"alpha\n!";
static NEXT_CASE: AtomicU64 = AtomicU64::new(1);

#[test]
fn promotion_requires_a_separate_exact_approver_identity() {
    let case = TempCase::new("approver");
    prepare_candidate(&case);

    let mut promoted = persistent_runtime(&case);
    promoted
        .apply_tree(promotion_tree(&case, "promotion-authority-a", OPERATION))
        .unwrap();
    assert_eq!(
        promoted.fiber_state("root/editor"),
        Some(FiberState::Active)
    );
    assert_eq!(fs::read(case.source()).unwrap(), CANDIDATE);
    assert_promotion_record(&case);
    promoted.shutdown_persistent().unwrap();
    assert_eq!(fs::read(case.source()).unwrap(), CANDIDATE);
    assert!(promoted.is_observationally_clean());

    let mut wrong_approver = persistent_runtime(&case);
    wrong_approver
        .apply_tree(promotion_tree(&case, "promotion-authority-b", OPERATION))
        .unwrap();
    assert!(matches!(
        wrong_approver.fiber_state("root/editor"),
        Some(FiberState::Failed(message)) if message.contains("guest returned status 6")
    ));
    assert_eq!(fs::read(case.source()).unwrap(), CANDIDATE);
    wrong_approver.shutdown_persistent().unwrap();
    assert_eq!(fs::read(case.source()).unwrap(), CANDIDATE);
    assert!(wrong_approver.is_observationally_clean());
}

#[test]
fn promotion_denial_and_cancellation_restore_the_original() {
    let denied_case = TempCase::new("denial");
    prepare_candidate(&denied_case);
    let mut denied = persistent_runtime(&denied_case);
    denied
        .apply_tree(promotion_tree(
            &denied_case,
            "promotion-authority-a",
            OPERATION - 1,
        ))
        .unwrap();
    assert!(matches!(
        denied.fiber_state("root/editor"),
        Some(FiberState::Failed(message)) if message.contains("guest returned status 7")
    ));
    assert_eq!(fs::read(denied_case.source()).unwrap(), BEFORE);
    denied.shutdown_persistent().unwrap();
    assert!(denied.is_observationally_clean());

    let cancelled_case = TempCase::new("cancellation");
    let mut cancelled = direct_runtime(&cancelled_case);
    cancelled
        .declare_tree(direct_promotion_tree(&cancelled_case))
        .unwrap();
    step_until_direct_publication(&mut cancelled, &cancelled_case);
    assert_eq!(
        cancelled.fiber_state("root/editor"),
        Some(FiberState::Activating)
    );
    cancelled.apply_tree(ComponentTree::default()).unwrap();
    assert_eq!(fs::read(cancelled_case.source()).unwrap(), BEFORE);
    assert!(cancelled.is_observationally_clean());
}

#[test]
fn process_loss_before_promotion_reconstructs_restoration_ownership() {
    let case = TempCase::new("precommit-crash");
    let mut crashed = direct_runtime(&case);
    crashed.declare_tree(direct_promotion_tree(&case)).unwrap();
    step_until_direct_publication(&mut crashed, &case);
    assert_eq!(fs::read(case.source()).unwrap(), DIRECT_CANDIDATE);
    std::mem::forget(crashed);

    let mut restarted = Runtime::new(Limits::default()).unwrap();
    restarted.apply_tree(direct_recovery_tree(&case)).unwrap();
    assert_eq!(
        restarted.fiber_state("root/editor"),
        Some(FiberState::Active)
    );
    assert_eq!(fs::read(case.source()).unwrap(), DIRECT_CANDIDATE);
    restarted.apply_tree(ComponentTree::default()).unwrap();
    assert_eq!(fs::read(case.source()).unwrap(), BEFORE);
    assert!(restarted.is_observationally_clean());
}

#[test]
fn process_loss_after_durable_promotion_preserves_the_candidate() {
    let case = TempCase::new("postcommit-crash");
    prepare_candidate(&case);
    let mut crashed = persistent_runtime(&case);
    crashed
        .apply_tree(promotion_tree(&case, "promotion-authority-a", OPERATION))
        .unwrap();
    assert_eq!(fs::read(case.source()).unwrap(), CANDIDATE);
    assert_promotion_record(&case);
    std::mem::forget(crashed);

    let mut restarted = persistent_runtime(&case);
    assert_eq!(
        restarted.fiber_state("root/editor"),
        Some(FiberState::Active)
    );
    assert_eq!(fs::read(case.source()).unwrap(), CANDIDATE);
    restarted.shutdown_persistent().unwrap();
    assert_eq!(fs::read(case.source()).unwrap(), CANDIDATE);
    assert!(restarted.is_observationally_clean());
}

#[test]
fn promoted_third_party_drift_is_ambiguous_and_never_overwritten() {
    let case = TempCase::new("drift");
    prepare_candidate(&case);
    let mut runtime = persistent_runtime(&case);
    runtime
        .apply_tree(promotion_tree(&case, "promotion-authority-a", OPERATION))
        .unwrap();
    fs::write(case.source(), EXTERNAL).unwrap();
    let canonical_source = fs::canonicalize(case.source()).unwrap();
    let error = runtime.shutdown_persistent().unwrap_err();
    assert!(
        matches!(
            &error,
            Error::MutationAmbiguous(path) if path == &canonical_source
        ),
        "drift shutdown error: {error:?}"
    );
    assert_eq!(fs::read(case.source()).unwrap(), EXTERNAL);
    drop(runtime);

    let mut restarted = persistent_runtime(&case);
    assert!(matches!(
        restarted.fiber_state("root/editor"),
        Some(FiberState::Failed(message)) if message.contains("repository mutation is ambiguous")
    ));
    assert_eq!(fs::read(case.source()).unwrap(), EXTERNAL);
    restarted.shutdown_persistent().unwrap();
    assert_eq!(fs::read(case.source()).unwrap(), EXTERNAL);
    assert!(restarted.is_observationally_clean());
}

fn prepare_candidate(case: &TempCase) {
    fs::write(case.source(), BEFORE).unwrap();
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
    assert_eq!(
        runtime.events()[0].payload.as_ref().unwrap().bytes,
        CANDIDATE
    );
    runtime.shutdown_persistent().unwrap();
}

fn direct_runtime(case: &TempCase) -> Runtime {
    fs::write(case.source(), BEFORE).unwrap();
    Runtime::new(Limits::default()).unwrap()
}

fn promotion_tree(case: &TempCase, promotion_authority: &str, maximum: u64) -> ComponentTree {
    ComponentTree {
        roots: vec![
            ComponentSpec::new("root", artifact("root"))
                .with_config(3)
                .with_children(vec![
                    ComponentSpec::new("mutation-authority", artifact("mutation-authority"))
                        .with_config(OPERATION),
                    ComponentSpec::new("promotion-authority", artifact(promotion_authority))
                        .with_config(maximum),
                    promotion_editor_spec(case, "promotion-editor-a"),
                ]),
        ],
    }
}

fn direct_promotion_tree(case: &TempCase) -> ComponentTree {
    ComponentTree {
        roots: vec![
            ComponentSpec::new("root", artifact("root"))
                .with_config(3)
                .with_children(vec![
                    ComponentSpec::new("mutation-authority", artifact("mutation-authority"))
                        .with_config(DIRECT_OPERATION),
                    ComponentSpec::new("promotion-authority", artifact("promotion-authority-a"))
                        .with_config(DIRECT_OPERATION),
                    direct_editor_spec(case, "promotion-direct-editor"),
                ]),
        ],
    }
}

fn direct_recovery_tree(case: &TempCase) -> ComponentTree {
    ComponentTree {
        roots: vec![
            ComponentSpec::new("root", artifact("root"))
                .with_config(2)
                .with_children(vec![
                    ComponentSpec::new("mutation-authority", artifact("mutation-authority"))
                        .with_config(DIRECT_OPERATION),
                    direct_editor_spec(case, "promotion-direct-publisher"),
                ]),
        ],
    }
}

fn direct_editor_spec(case: &TempCase, module: &str) -> ComponentSpec {
    ComponentSpec::new("editor", artifact(module))
        .with_config(u64::from(b'!'))
        .with_workspace_grants(vec![
            WorkspaceGrant::new(
                case.source(),
                case.mutation(),
                DIRECT_OPERATION,
                "direct promotion recovery",
                sha256(BEFORE),
                sha256(DIRECT_CANDIDATE),
                64 * 1024,
            )
            .unwrap(),
        ])
}

fn promotion_editor_spec(case: &TempCase, module: &str) -> ComponentSpec {
    ComponentSpec::new("editor", artifact(module))
        .with_config(1)
        .with_workspace_grants(vec![
            WorkspaceGrant::new(
                case.source(),
                case.mutation(),
                OPERATION,
                "promoted reviewed candidate turn 1",
                sha256(BEFORE),
                sha256(CANDIDATE),
                64 * 1024,
            )
            .unwrap(),
        ])
}

fn storage_spec(case: &TempCase) -> ComponentSpec {
    ComponentSpec::new("event-store", artifact("event-store"))
        .with_journal_paths(vec![case.journal()])
        .with_event_stream_paths(vec![case.events()])
}

fn persistent_runtime(case: &TempCase) -> Runtime {
    Runtime::open_persistent(Limits::default(), storage_spec(case)).unwrap()
}

fn step_until_direct_publication(runtime: &mut Runtime, case: &TempCase) {
    for _ in 0..32 {
        if fs::read(case.source()).unwrap() == DIRECT_CANDIDATE {
            return;
        }
        assert!(
            runtime.step().unwrap(),
            "runtime quiesced before publication"
        );
    }
    panic!("promotion editor did not publish within its activation bound");
}

fn assert_promotion_record(case: &TempCase) {
    let ledger = fs::read(case.mutation()).unwrap();
    let text = String::from_utf8_lossy(&ledger);
    let intent = text
        .find("promotion-intent")
        .expect("durable promotion intent");
    let promoted = text.rfind("promoted").expect("durable promotion commit");
    assert!(intent < promoted);
    assert!(text.contains("quartz.slice9.promotion-authority-a@0.1.0#"));
    assert!(text.contains(&format!("\"operation\":{OPERATION}")));
    assert!(text.contains(&sha256(BEFORE)));
    assert!(text.contains(&sha256(CANDIDATE)));
}

fn event_grant() -> EventGrant {
    EventGrant::new("quartz.agent", "repository-turn", 2)
}

fn snapshot_grant(path: &Path) -> SnapshotGrant {
    SnapshotGrant::from_file(path, "slice9 durable candidate").unwrap()
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
            std::env::temp_dir().join(format!("quartz-slice9-{name}-{}-{id}", std::process::id()));
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
