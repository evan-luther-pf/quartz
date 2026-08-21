use quartz_kernel::{
    ComponentSpec, ComponentTree, Error, FiberState, Limits, Runtime, WorkspaceGrant,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_CASE: AtomicU64 = AtomicU64::new(1);
const ORIGINAL: &[u8] = b"alpha";
const EDITED_A: &[u8] = b"alpha!";
const EDITED_B: &[u8] = b"alpha?";

#[test]
fn sandboxed_editor_replacement_and_subtree_removal_recover_the_source() {
    let case = TempCase::new("replacement");
    let source = case.source();
    let ledger = case.path("mutations.qm");
    let mut runtime = Runtime::new(Limits::default()).unwrap();

    runtime
        .apply_tree(edit_tree(
            &source,
            &ledger,
            edit("repo-editor-a", b'!', 7_001, EDITED_A, 64),
            7_002,
        ))
        .unwrap();
    let editor_a = runtime.fiber_id("root/editor").unwrap();
    assert_eq!(runtime.fiber_state("root/editor"), Some(FiberState::Active));
    assert_eq!(fs::read(&source).unwrap(), EDITED_A);

    runtime
        .replace_entry(
            "root/editor",
            editor_spec(
                &source,
                &ledger,
                edit("repo-editor-b", b'?', 7_002, EDITED_B, 64),
            ),
        )
        .unwrap();
    assert_ne!(runtime.fiber_id("root/editor"), Some(editor_a));
    assert_eq!(runtime.fiber_state("root/editor"), Some(FiberState::Active));
    assert_eq!(fs::read(&source).unwrap(), EDITED_B);

    runtime.apply_tree(ComponentTree::default()).unwrap();
    assert_eq!(fs::read(&source).unwrap(), ORIGINAL);
    assert!(runtime.is_observationally_clean());
}

#[test]
fn applied_publication_reconstructs_without_duplicate_replacement() {
    let case = TempCase::new("reconstruction");
    let source = case.source();
    let ledger = case.path("mutations.qm");
    let tree = edit_tree(
        &source,
        &ledger,
        edit("repo-editor-a", b'!', 7_001, EDITED_A, 64),
        7_001,
    );
    let mut first = Runtime::new(Limits::default()).unwrap();
    first.apply_tree(tree.clone()).unwrap();
    assert_eq!(fs::read(&source).unwrap(), EDITED_A);
    let applied_ledger_bytes = fs::metadata(&ledger).unwrap().len();
    std::mem::forget(first);

    let mut recovered = Runtime::new(Limits::default()).unwrap();
    recovered.apply_tree(tree).unwrap();
    assert_eq!(fs::read(&source).unwrap(), EDITED_A);
    assert_eq!(fs::metadata(&ledger).unwrap().len(), applied_ledger_bytes);

    recovered.apply_tree(ComponentTree::default()).unwrap();
    assert_eq!(fs::read(&source).unwrap(), ORIGINAL);
    assert!(recovered.is_observationally_clean());
}

#[test]
fn workspace_bounds_ungranted_access_and_noncanonical_paths_fail_closed() {
    let bounded = TempCase::new("bounded");
    let bounded_source = bounded.source();
    let bounded_ledger = bounded.path("mutations.qm");
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    runtime
        .apply_tree(edit_tree(
            &bounded_source,
            &bounded_ledger,
            edit("repo-editor-a", b'!', 7_001, EDITED_A, ORIGINAL.len()),
            7_001,
        ))
        .unwrap();
    assert!(matches!(
        runtime.fiber_state("root/editor"),
        Some(FiberState::Failed(message)) if message.contains("guest returned status 5")
    ));
    assert_eq!(fs::read(&bounded_source).unwrap(), ORIGINAL);
    assert!(!bounded_ledger.exists());

    let ungranted = TempCase::new("ungranted");
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    let error = runtime
        .apply_tree(ComponentTree {
            roots: vec![ComponentSpec::new("editor", artifact("repo-editor-a"))],
        })
        .unwrap_err();
    assert!(matches!(error, Error::Manifest(message) if message.contains("workspace read")));
    assert_eq!(fs::read(ungranted.source()).unwrap(), ORIGINAL);

    let traversal = TempCase::new("traversal");
    let traversal_source = traversal.source();
    fs::create_dir(traversal.path("nested")).unwrap();
    let valid = workspace_grant(
        &traversal_source,
        &traversal.path("mutations.qm"),
        7_001,
        EDITED_A,
        64,
    );
    let noncanonical = WorkspaceGrant {
        source_path: traversal.path("nested/../source.txt"),
        ..valid
    };
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    let error = runtime
        .apply_tree(ComponentTree {
            roots: vec![
                ComponentSpec::new("editor", artifact("repo-editor-a"))
                    .with_workspace_grants(vec![noncanonical]),
            ],
        })
        .unwrap_err();
    assert!(matches!(error, Error::Manifest(message) if message.contains("not canonical")));
    assert_eq!(fs::read(&traversal_source).unwrap(), ORIGINAL);
}

#[test]
fn denied_approval_and_stale_sources_do_not_publish() {
    let denied = TempCase::new("denied");
    let denied_source = denied.source();
    let denied_ledger = denied.path("mutations.qm");
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    runtime
        .apply_tree(edit_tree(
            &denied_source,
            &denied_ledger,
            edit("repo-editor-a", b'!', 7_001, EDITED_A, 64),
            7_000,
        ))
        .unwrap();
    assert!(matches!(
        runtime.fiber_state("root/editor"),
        Some(FiberState::Failed(message)) if message.contains("guest returned status 7")
    ));
    assert_eq!(fs::read(&denied_source).unwrap(), ORIGINAL);
    assert!(!denied_ledger.exists());

    let stale = TempCase::new("stale");
    let stale_source = stale.source();
    let stale_ledger = stale.path("mutations.qm");
    let tree = edit_tree(
        &stale_source,
        &stale_ledger,
        edit("repo-editor-a", b'!', 7_001, EDITED_A, 64),
        7_001,
    );
    fs::write(&stale_source, b"external").unwrap();
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    runtime.apply_tree(tree).unwrap();
    assert!(matches!(
        runtime.fiber_state("root/editor"),
        Some(FiberState::Failed(message)) if message.contains("workspace digest mismatch")
    ));
    assert_eq!(fs::read(&stale_source).unwrap(), b"external");
    assert!(!stale_ledger.exists());
}

#[test]
fn conflicting_external_edits_are_ambiguous_and_never_clobbered() {
    let case = TempCase::new("ambiguous");
    let source = case.source();
    let ledger = case.path("mutations.qm");
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    runtime
        .apply_tree(edit_tree(
            &source,
            &ledger,
            edit("repo-editor-a", b'!', 7_001, EDITED_A, 64),
            7_001,
        ))
        .unwrap();
    fs::write(&source, b"external").unwrap();

    let error = runtime.apply_tree(ComponentTree::default()).unwrap_err();
    assert!(matches!(error, Error::MutationAmbiguous(path) if path == source));
    assert_eq!(fs::read(&source).unwrap(), b"external");
    assert!(!runtime.is_observationally_clean());
}

#[test]
fn mutation_operation_reuse_with_different_bytes_is_rejected() {
    let case = TempCase::new("collision");
    let source = case.source();
    let ledger = case.path("mutations.qm");
    let mut runtime = Runtime::new(Limits::default()).unwrap();
    runtime
        .apply_tree(edit_tree(
            &source,
            &ledger,
            edit("repo-editor-a", b'!', 7_001, EDITED_A, 64),
            7_002,
        ))
        .unwrap();
    runtime.apply_tree(ComponentTree::default()).unwrap();
    assert_eq!(fs::read(&source).unwrap(), ORIGINAL);
    drop(runtime);

    let mut runtime = Runtime::new(Limits::default()).unwrap();
    runtime
        .apply_tree(edit_tree(
            &source,
            &ledger,
            edit("repo-editor-b", b'?', 7_001, EDITED_B, 64),
            7_002,
        ))
        .unwrap();
    assert!(matches!(
        runtime.fiber_state("root/editor"),
        Some(FiberState::Failed(message)) if message.contains("reused with a different workspace grant")
    ));
    assert_eq!(fs::read(&source).unwrap(), ORIGINAL);
}

#[derive(Clone, Copy)]
struct Edit<'a> {
    module: &'a str,
    byte: u8,
    operation: u64,
    result: &'a [u8],
    max_bytes: usize,
}

fn edit<'a>(
    module: &'a str,
    byte: u8,
    operation: u64,
    result: &'a [u8],
    max_bytes: usize,
) -> Edit<'a> {
    Edit {
        module,
        byte,
        operation,
        result,
        max_bytes,
    }
}

fn edit_tree(
    source: &Path,
    ledger: &Path,
    edit: Edit<'_>,
    authority_maximum: u64,
) -> ComponentTree {
    ComponentTree {
        roots: vec![
            ComponentSpec::new("root", artifact("root"))
                .with_config(2)
                .with_children(vec![
                    ComponentSpec::new("authority", artifact("mutation-authority"))
                        .with_config(authority_maximum),
                    editor_spec(source, ledger, edit),
                ]),
        ],
    }
}

fn editor_spec(source: &Path, ledger: &Path, edit: Edit<'_>) -> ComponentSpec {
    ComponentSpec::new("editor", artifact(edit.module))
        .with_config(u64::from(edit.byte))
        .with_workspace_grants(vec![workspace_grant(
            source,
            ledger,
            edit.operation,
            edit.result,
            edit.max_bytes,
        )])
}

fn workspace_grant(
    source: &Path,
    ledger: &Path,
    operation: u64,
    result: &[u8],
    max_bytes: usize,
) -> WorkspaceGrant {
    WorkspaceGrant::new(
        source,
        ledger,
        operation,
        "slice7 test repository",
        sha256(ORIGINAL),
        sha256(result),
        max_bytes,
    )
    .unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").unwrap();
    }
    output
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
            std::env::temp_dir().join(format!("quartz-slice7-{name}-{}-{id}", std::process::id()));
        fs::create_dir(&root).unwrap();
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn source(&self) -> PathBuf {
        let path = self.path("source.txt");
        if !path.exists() {
            fs::write(&path, ORIGINAL).unwrap();
        }
        fs::canonicalize(path).unwrap()
    }
}

impl Drop for TempCase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
