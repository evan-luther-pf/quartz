use quartz_kernel::{
    BindingKind, ComponentSpec, ComponentTree, CompositionPatch, Error, InterfaceId, Limits,
    Runtime,
};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_CASE: AtomicU64 = AtomicU64::new(1);

#[test]
fn committed_composition_reconstructs_after_unclean_drop() {
    let case = TempCase::new("cold-reconstruction");
    let journal = case.path("composition.qj");
    let mut runtime = persistent_runtime(&journal, Limits::default()).unwrap();
    runtime
        .apply_tree(governed_tree("provider-a", "provider-b"))
        .unwrap();

    assert_provider(&runtime, 2);
    assert_eq!(runtime.composition_revision(), 2);
    assert_eq!(runtime.journal_sequence(), Some(1));
    assert_eq!(runtime.observation().composition_effects, 1);
    drop(runtime);

    let runtime = persistent_runtime(&journal, Limits::default()).unwrap();
    assert_provider(&runtime, 2);
    assert_eq!(runtime.state_value("app/consumer", 900), Some(41));
    assert_eq!(runtime.composition_revision(), 2);
    assert_eq!(runtime.journal_sequence(), Some(1));
    assert_eq!(runtime.observation().composition_effects, 1);
}

#[test]
fn failed_candidate_does_not_append_a_patch_record() {
    let case = TempCase::new("failed-candidate");
    let journal = case.path("composition.qj");
    let mut runtime = persistent_runtime(&journal, Limits::default()).unwrap();
    runtime
        .apply_tree(governed_tree("provider-a", "provider-bad"))
        .unwrap();

    assert_provider(&runtime, 1);
    assert_eq!(runtime.journal_sequence(), Some(1));
    assert_eq!(runtime.observation().composition_effects, 0);
    drop(runtime);

    let runtime = persistent_runtime(&journal, Limits::default()).unwrap();
    assert_provider(&runtime, 1);
    assert_eq!(runtime.journal_sequence(), Some(1));
}

#[test]
fn recovered_patch_inverse_survives_another_restart() {
    let case = TempCase::new("inverse-recovery");
    let journal = case.path("composition.qj");
    let mut runtime = persistent_runtime(&journal, Limits::default()).unwrap();
    runtime
        .apply_tree(governed_tree("provider-a", "provider-b"))
        .unwrap();
    drop(runtime);

    let mut runtime = persistent_runtime(&journal, Limits::default()).unwrap();
    assert_eq!(runtime.observation().composition_effects, 1);
    runtime.apply_tree(application_tree("provider-b")).unwrap();
    assert_provider(&runtime, 1);
    assert_eq!(runtime.observation().composition_effects, 0);
    assert_eq!(runtime.journal_sequence(), Some(2));
    drop(runtime);

    let runtime = persistent_runtime(&journal, Limits::default()).unwrap();
    assert_provider(&runtime, 1);
    assert_eq!(runtime.observation().composition_effects, 0);
    assert_eq!(runtime.journal_sequence(), Some(2));
}

#[test]
fn torn_trailing_record_is_removed_on_open() {
    let case = TempCase::new("torn-tail");
    let journal = case.path("composition.qj");
    let mut runtime = persistent_runtime(&journal, Limits::default()).unwrap();
    runtime.apply_tree(application_tree("provider-a")).unwrap();
    drop(runtime);
    let committed_len = fs::metadata(&journal).unwrap().len();

    OpenOptions::new()
        .append(true)
        .open(&journal)
        .unwrap()
        .write_all(b"torn")
        .unwrap();
    assert_eq!(fs::metadata(&journal).unwrap().len(), committed_len + 4);

    let runtime = persistent_runtime(&journal, Limits::default()).unwrap();
    assert_provider(&runtime, 1);
    assert_eq!(runtime.journal_sequence(), Some(1));
    assert_eq!(fs::metadata(&journal).unwrap().len(), committed_len);
}

#[test]
fn interior_journal_corruption_fails_closed() {
    let case = TempCase::new("interior-corruption");
    let journal = case.path("composition.qj");
    let mut runtime = persistent_runtime(&journal, Limits::default()).unwrap();
    runtime.apply_tree(application_tree("provider-a")).unwrap();
    drop(runtime);

    let mut bytes = fs::read(&journal).unwrap();
    bytes[24] ^= 0x01;
    fs::write(&journal, bytes).unwrap();

    assert!(matches!(
        persistent_runtime(&journal, Limits::default()),
        Err(Error::JournalCorrupt(error)) if error.contains("checksum mismatch")
    ));
}

#[test]
fn artifact_digest_mismatch_is_rejected_during_replay() {
    let case = TempCase::new("artifact-digest");
    let journal = case.path("composition.qj");
    let copied_provider = case.path("provider.wasm");
    fs::copy(artifact("provider-a"), &copied_provider).unwrap();

    let mut runtime = persistent_runtime(&journal, Limits::default()).unwrap();
    runtime
        .apply_tree(application_tree_with_provider(ComponentSpec::new(
            "provider",
            &copied_provider,
        )))
        .unwrap();
    drop(runtime);

    OpenOptions::new()
        .append(true)
        .open(&copied_provider)
        .unwrap()
        .write_all(&[0])
        .unwrap();

    match persistent_runtime(&journal, Limits::default()) {
        Err(Error::ArtifactDigestMismatch { path, .. }) => {
            assert_eq!(path, copied_provider.canonicalize().unwrap());
        }
        Err(error) => panic!("unexpected replay error: {error:?}"),
        Ok(_) => panic!("digest mismatch was accepted"),
    }
}

#[test]
fn journal_record_limit_restores_the_prior_runtime() {
    let case = TempCase::new("journal-limit");
    let journal = case.path("composition.qj");
    let limits = Limits {
        max_journal_record_bytes: 64,
        ..Limits::default()
    };
    let mut runtime = persistent_runtime(&journal, limits).unwrap();

    assert!(matches!(
        runtime.apply_tree(application_tree("provider-a")),
        Err(Error::JournalRecordLimit { limit: 64, .. })
    ));
    assert_eq!(runtime.fiber_id("app"), None);
    assert_eq!(runtime.composition_revision(), 0);
    assert_eq!(runtime.journal_sequence(), Some(0));
    assert_eq!(runtime.observation().fibers, 1);
    assert_eq!(runtime.observation().journal_registrations, 1);
}

#[test]
fn empty_composition_restarts_empty_and_shutdown_is_clean() {
    let case = TempCase::new("empty-restart");
    let journal = case.path("composition.qj");
    let mut runtime = persistent_runtime(&journal, Limits::default()).unwrap();
    runtime.apply_tree(application_tree("provider-a")).unwrap();
    runtime.shutdown_persistent().unwrap();
    assert!(runtime.is_observationally_clean());
    drop(runtime);

    let mut runtime = persistent_runtime(&journal, Limits::default()).unwrap();
    assert_eq!(runtime.fiber_id("app"), None);
    assert_eq!(runtime.observation().fibers, 1);
    runtime.shutdown_persistent().unwrap();
    assert!(runtime.is_observationally_clean());
}

fn persistent_runtime(path: &Path, limits: Limits) -> quartz_kernel::Result<Runtime> {
    Runtime::open_persistent(
        limits,
        spec("journal", "journal").with_journal_paths(vec![path.to_path_buf()]),
    )
}

fn governed_tree(provider: &str, replacement: &str) -> ComponentTree {
    let mut tree = application_tree(provider);
    tree.roots.push(
        spec("controller", "durable-controller")
            .with_config(controller_config(1, 0))
            .with_patches(vec![CompositionPatch::replace(
                "app/provider",
                spec("provider", replacement),
            )]),
    );
    tree
}

fn application_tree(provider: &str) -> ComponentTree {
    application_tree_with_provider(spec("provider", provider))
}

fn application_tree_with_provider(provider: ComponentSpec) -> ComponentTree {
    ComponentTree {
        roots: vec![spec("app", "root").with_config(3).with_children(vec![
            spec("governor", "governor"),
            provider,
            spec("consumer", "consumer"),
        ])],
    }
}

fn controller_config(base_revision: u64, patch_index: u64) -> u64 {
    (base_revision << 32) | patch_index
}

fn assert_provider(runtime: &Runtime, expected_state: u64) {
    let interface = InterfaceId {
        kind: BindingKind::Value,
        namespace: "quartz.slice0".into(),
        interface: "value".into(),
        revision: 1,
    };
    assert_eq!(
        runtime.provider_identity(&interface),
        runtime.fiber_id("app/provider")
    );
    assert_eq!(
        runtime.state_value("app/provider", 10),
        Some(expected_state)
    );
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
            std::env::temp_dir().join(format!("quartz-slice2-{name}-{}-{id}", std::process::id()));
        fs::create_dir(&root).unwrap();
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

impl Drop for TempCase {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
