use quartz_kernel::{ComponentSpec, ComponentTree, Error, EventGrant, Limits, Runtime};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_CASE: AtomicU64 = AtomicU64::new(1);

#[test]
fn committed_event_projects_after_unclean_restart() {
    let case = TempCase::new("cold-projection");
    let journal = case.path("composition.qj");
    let events = case.path("events.qe");
    let mut runtime = persistent_runtime(&journal, &events, Limits::default()).unwrap();
    runtime
        .apply_tree(event_tree(&[("append", 42)], false))
        .unwrap();
    assert_eq!(runtime.events().len(), 1);
    assert_eq!(runtime.events()[0].value, 42);
    drop(runtime);

    let mut runtime = persistent_runtime(&journal, &events, Limits::default()).unwrap();
    assert_eq!(runtime.events().len(), 1);
    assert_eq!(runtime.state_value("projection", 901), None);
    runtime
        .apply_tree(event_tree(&[("append", 42)], true))
        .unwrap();
    assert_eq!(runtime.state_value("projection", 901), Some(42));
    drop(runtime);

    let runtime = persistent_runtime(&journal, &events, Limits::default()).unwrap();
    assert_eq!(runtime.events().len(), 1);
    assert_eq!(runtime.events()[0].value, 42);
    assert_eq!(runtime.state_value("projection", 901), Some(42));
}

#[test]
fn failed_and_denied_appenders_write_nothing() {
    let failed = TempCase::new("failed-append");
    let failed_journal = failed.path("composition.qj");
    let failed_events = failed.path("events.qe");
    let mut runtime =
        persistent_runtime(&failed_journal, &failed_events, Limits::default()).unwrap();
    runtime
        .apply_tree(event_tree(&[("append", (1 << 32) | 7)], false))
        .unwrap();
    assert!(runtime.events().is_empty());
    drop(runtime);
    let runtime = persistent_runtime(&failed_journal, &failed_events, Limits::default()).unwrap();
    assert!(runtime.events().is_empty());
    drop(runtime);

    let denied = TempCase::new("denied-append");
    let denied_journal = denied.path("composition.qj");
    let denied_events = denied.path("events.qe");
    let mut runtime =
        persistent_runtime(&denied_journal, &denied_events, Limits::default()).unwrap();
    runtime
        .apply_tree(ComponentTree {
            roots: vec![event_spec("denied", "event-denied", 9)],
        })
        .unwrap();
    assert!(runtime.events().is_empty());
    drop(runtime);
    let runtime = persistent_runtime(&denied_journal, &denied_events, Limits::default()).unwrap();
    assert!(runtime.events().is_empty());
}

#[test]
fn event_sequence_continues_across_restart() {
    let case = TempCase::new("sequence");
    let journal = case.path("composition.qj");
    let events = case.path("events.qe");
    let mut runtime = persistent_runtime(&journal, &events, Limits::default()).unwrap();
    runtime
        .apply_tree(event_tree(&[("first", 11)], false))
        .unwrap();
    drop(runtime);

    let mut runtime = persistent_runtime(&journal, &events, Limits::default()).unwrap();
    runtime
        .apply_tree(event_tree(&[("first", 11), ("second", 22)], false))
        .unwrap();
    assert_eq!(
        runtime
            .events()
            .iter()
            .map(|event| (event.sequence, event.id, event.value))
            .collect::<Vec<_>>(),
        vec![(1, 1, 11), (2, 2, 22)]
    );
}

#[test]
fn torn_event_tail_is_repaired() {
    let case = TempCase::new("torn-tail");
    let journal = case.path("composition.qj");
    let events = case.path("events.qe");
    let mut runtime = persistent_runtime(&journal, &events, Limits::default()).unwrap();
    runtime
        .apply_tree(event_tree(&[("append", 5)], false))
        .unwrap();
    drop(runtime);
    let committed_len = fs::metadata(&events).unwrap().len();

    OpenOptions::new()
        .append(true)
        .open(&events)
        .unwrap()
        .write_all(b"torn")
        .unwrap();
    let runtime = persistent_runtime(&journal, &events, Limits::default()).unwrap();
    assert_eq!(runtime.events().len(), 1);
    assert_eq!(fs::metadata(&events).unwrap().len(), committed_len);
}

#[test]
fn interior_event_corruption_fails_closed() {
    let case = TempCase::new("interior-corruption");
    let journal = case.path("composition.qj");
    let events = case.path("events.qe");
    let mut runtime = persistent_runtime(&journal, &events, Limits::default()).unwrap();
    runtime
        .apply_tree(event_tree(&[("append", 5)], false))
        .unwrap();
    drop(runtime);

    let mut bytes = fs::read(&events).unwrap();
    bytes[24] ^= 0x01;
    fs::write(&events, bytes).unwrap();
    match persistent_runtime(&journal, &events, Limits::default()) {
        Err(Error::EventCorrupt(_)) => {}
        Err(error) => panic!("unexpected corruption error: {error:?}"),
        Ok(_) => panic!("interior event corruption was accepted"),
    }
}

#[test]
fn event_record_count_and_size_are_bounded() {
    let count_case = TempCase::new("count-bound");
    let count_limits = Limits {
        max_event_records: 1,
        ..Limits::default()
    };
    let mut runtime = persistent_runtime(
        &count_case.path("composition.qj"),
        &count_case.path("events.qe"),
        count_limits,
    )
    .unwrap();
    runtime
        .apply_tree(event_tree(&[("first", 1), ("second", 2)], false))
        .unwrap();
    assert_eq!(runtime.events().len(), 1);

    let size_case = TempCase::new("size-bound");
    let size_limits = Limits {
        max_event_record_bytes: 32,
        ..Limits::default()
    };
    let mut runtime = persistent_runtime(
        &size_case.path("composition.qj"),
        &size_case.path("events.qe"),
        size_limits,
    )
    .unwrap();
    runtime
        .apply_tree(event_tree(&[("append", 1)], false))
        .unwrap();
    assert!(runtime.events().is_empty());
}

#[test]
fn recovered_outbox_retry_is_idempotent() {
    let case = TempCase::new("outbox-retry");
    let journal = case.path("composition.qj");
    let events = case.path("events.qe");
    let mut runtime = persistent_runtime(&journal, &events, Limits::default()).unwrap();
    runtime
        .apply_tree(event_tree(&[("append", 17)], false))
        .unwrap();
    drop(runtime);

    let bytes = fs::read(&journal).unwrap();
    let payload_len = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
    let first_frame_end = 8 + 12 + payload_len + 32;
    OpenOptions::new()
        .write(true)
        .open(&journal)
        .unwrap()
        .set_len(first_frame_end as u64)
        .unwrap();

    let runtime = persistent_runtime(&journal, &events, Limits::default()).unwrap();
    assert_eq!(runtime.events().len(), 1);
    assert_eq!(runtime.events()[0].id, 1);
    assert_eq!(runtime.events()[0].value, 17);
    assert_eq!(runtime.journal_sequence(), Some(2));
}

#[test]
fn clean_shutdown_recovers_event_capabilities() {
    let case = TempCase::new("clean-shutdown");
    let journal = case.path("composition.qj");
    let events = case.path("events.qe");
    let mut runtime = persistent_runtime(&journal, &events, Limits::default()).unwrap();
    runtime
        .apply_tree(event_tree(&[("append", 3)], true))
        .unwrap();
    runtime.shutdown_persistent().unwrap();
    assert!(runtime.is_observationally_clean());
}

fn persistent_runtime(
    journal: &Path,
    events: &Path,
    limits: Limits,
) -> quartz_kernel::Result<Runtime> {
    Runtime::open_persistent(
        limits,
        spec("event-store", "event-store")
            .with_journal_paths(vec![journal.to_path_buf()])
            .with_event_stream_paths(vec![events.to_path_buf()]),
    )
}

fn event_tree(appenders: &[(&str, u64)], projection: bool) -> ComponentTree {
    let mut roots = appenders
        .iter()
        .map(|(entry, value)| event_spec(entry, "event-appender", *value))
        .collect::<Vec<_>>();
    if projection {
        roots.push(spec("projection", "event-projection"));
    }
    ComponentTree { roots }
}

fn event_spec(entry: &str, module: &str, value: u64) -> ComponentSpec {
    spec(entry, module)
        .with_config(value)
        .with_event_grants(vec![EventGrant::new("quartz.session", "value", 1)])
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
            std::env::temp_dir().join(format!("quartz-slice3-{name}-{}-{id}", std::process::id()));
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
