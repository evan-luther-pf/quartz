use quartz_kernel::{
    ComponentSpec, ComponentTree, CompositionPatch, EventGrant, FiberState, Limits, Runtime,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_CASE: AtomicU64 = AtomicU64::new(1);
const FACT_KIND_SHIFT: u64 = 56;
const FACT_TURN_SHIFT: u64 = 48;
const FACT_INVOCATION_SHIFT: u64 = 32;

#[test]
fn deterministic_turn_resumes_once_and_survives_tool_replacement() {
    let case = TempCase::new("complete-turn");
    let mut runtime = persistent_runtime(&case).unwrap();
    runtime
        .apply_tree(agent_tree("agent-tool-a", 1, 1))
        .unwrap();
    assert_facts(&runtime, &[(1, 1, 0, 1)]);
    drop(runtime);

    let turn_one = [
        (2, 1, 17, 1),
        (3, 1, 18, 1),
        (4, 1, 18, 101),
        (2, 1, 19, 2),
        (5, 1, 19, 1001),
        (6, 1, 19, 7),
        (7, 1, 0, 1),
    ];
    for (offset, expected) in turn_one.iter().enumerate() {
        runtime = persistent_runtime(&case).unwrap();
        assert_eq!(runtime.events().len(), offset + 2);
        assert_eq!(decode(runtime.events().last().unwrap().value), *expected);
        drop(runtime);
    }

    runtime = persistent_runtime(&case).unwrap();
    assert_eq!(runtime.events().len(), 8);
    assert_eq!(runtime.state_value("a-loop", 913), Some(1001));
    let first_transcript = values(&runtime);
    let old_tool = runtime.fiber_id("d-tool").unwrap();
    let base_revision = runtime.composition_revision();
    let controller_config = (base_revision + 1) << 32;
    runtime
        .apply_tree(tool_replacement_tree(controller_config))
        .unwrap();
    assert_eq!(values(&runtime), first_transcript);
    assert_eq!(
        runtime.state_value("y-controller", 700),
        Some(0),
        "governed patch must be accepted"
    );
    assert_ne!(runtime.fiber_id("d-tool"), Some(old_tool));
    assert_eq!(runtime.fiber_state("d-tool"), Some(FiberState::Active));
    runtime
        .apply_tree(second_prompt_tree(controller_config))
        .unwrap();
    assert_eq!(runtime.events().len(), 9);
    assert_eq!(decode(runtime.events()[8].value), (1, 2, 0, 2));
    drop(runtime);

    let turn_two = [
        (2, 2, 33, 1),
        (3, 2, 34, 1),
        (4, 2, 34, 202),
        (2, 2, 35, 2),
        (5, 2, 35, 2002),
        (6, 2, 35, 7),
        (7, 2, 0, 1),
    ];
    for (offset, expected) in turn_two.iter().enumerate() {
        runtime = persistent_runtime(&case).unwrap();
        assert_eq!(runtime.events().len(), offset + 10);
        assert_eq!(decode(runtime.events().last().unwrap().value), *expected);
        drop(runtime);
    }

    runtime = persistent_runtime(&case).unwrap();
    assert_eq!(runtime.events().len(), 16);
    assert_eq!(&values(&runtime)[..8], first_transcript.as_slice());
    assert_eq!(runtime.state_value("a-loop", 912), Some(202));
    assert_eq!(runtime.state_value("a-loop", 913), Some(2002));
    runtime.apply_tree(ComponentTree::default()).unwrap();
    assert!(runtime.fiber_id("a-loop").is_none());
    assert!(runtime.fiber_id("b-gateway").is_none());
    assert!(runtime.fiber_id("e-governor").is_none());
    assert!(runtime.fiber_id("y-controller").is_none());
    assert!(runtime.fiber_id("zz-client").is_none());
    assert!(runtime.fiber_id("c-provider").is_none());
    assert!(runtime.fiber_id("d-tool").is_none());
    assert!(runtime.fiber_id("z-client").is_none());
    runtime.shutdown_persistent().unwrap();
    assert!(runtime.is_observationally_clean());
}

#[test]
fn deterministic_provider_failure_preserves_request_for_stable_retry() {
    let case = TempCase::new("provider-retry");
    let mut runtime = persistent_runtime(&case).unwrap();
    runtime
        .apply_tree(agent_tree("agent-tool-a", 1, 1))
        .unwrap();
    drop(runtime);

    runtime = persistent_runtime(&case).unwrap();
    assert_facts(&runtime, &[(1, 1, 0, 1), (2, 1, 17, 1)]);
    let request = runtime.events()[1].value;
    let controller_config = (runtime.composition_revision() + 1) << 32;
    runtime
        .apply_tree(provider_failure_tree(controller_config))
        .unwrap();
    assert!(matches!(
        runtime.fiber_state("a-loop"),
        Some(FiberState::Failed(_))
    ));
    assert_eq!(runtime.state_value("y-provider-controller", 700), Some(0));
    assert_eq!(runtime.events().len(), 2);
    assert_eq!(runtime.events()[1].value, request);

    runtime
        .apply_tree(agent_tree("agent-tool-a", 1, 2))
        .unwrap();
    assert_eq!(runtime.events().len(), 2);
    drop(runtime);
    runtime = persistent_runtime(&case).unwrap();
    assert_eq!(runtime.events().len(), 3);
    assert_eq!(decode(runtime.events()[1].value), (2, 1, 17, 1));
    assert_eq!(decode(runtime.events()[2].value), (3, 1, 18, 1));
    runtime.shutdown_persistent().unwrap();
    assert!(runtime.is_observationally_clean());
}

#[test]
fn ambiguous_non_idempotent_tool_is_interrupted_without_execution() {
    let case = TempCase::new("interrupted-tool");
    let mut runtime = persistent_runtime(&case).unwrap();
    runtime
        .apply_tree(agent_tree("agent-tool-a", 1, 3))
        .unwrap();
    assert_eq!(runtime.events().len(), 1);
    drop(runtime);

    let expected = [(2, 1, 17, 1), (3, 1, 18, 2), (8, 1, 18, 1), (7, 1, 0, 2)];
    for (offset, fact) in expected.iter().enumerate() {
        runtime = persistent_runtime(&case).unwrap();
        assert_eq!(runtime.events().len(), offset + 2);
        assert_eq!(decode(runtime.events().last().unwrap().value), *fact);
        drop(runtime);
    }

    runtime = persistent_runtime(&case).unwrap();
    assert_eq!(runtime.events().len(), 5);
    assert!(
        !runtime
            .events()
            .iter()
            .any(|event| decode(event.value).0 == 4)
    );
    runtime.shutdown_persistent().unwrap();
    assert!(runtime.is_observationally_clean());
}

fn persistent_runtime(case: &TempCase) -> quartz_kernel::Result<Runtime> {
    Runtime::open_persistent(
        Limits::default(),
        spec("event-store", "event-store")
            .with_journal_paths(vec![case.path("composition.qj")])
            .with_event_stream_paths(vec![case.path("events.qe")]),
    )
}

fn agent_tree(tool: &str, prompt: u64, provider_mode: u64) -> ComponentTree {
    let event_grant = || EventGrant::new("quartz.agent", "turn", 1);
    ComponentTree {
        roots: vec![
            spec("a-loop", "agent-loop").with_event_grants(vec![event_grant()]),
            spec("b-gateway", "agent-gateway"),
            spec("c-provider", "agent-provider").with_config(provider_mode),
            spec("d-tool", tool).with_config(if tool.ends_with('a') { 1 } else { 2 }),
            spec("e-governor", "governor").with_config(0),
            spec("z-client", "agent-client")
                .with_config(prompt)
                .with_event_grants(vec![event_grant()]),
        ],
    }
}

fn tool_replacement_tree(controller_config: u64) -> ComponentTree {
    let mut tree = agent_tree("agent-tool-a", 1, 1);
    tree.roots.push(replacement_controller(controller_config));
    tree
}

fn second_prompt_tree(controller_config: u64) -> ComponentTree {
    let mut tree = agent_tree("agent-tool-b", 1, 1);
    tree.roots.push(replacement_controller(controller_config));
    tree.roots.push(
        spec("zz-client", "agent-client")
            .with_config(2)
            .with_event_grants(vec![EventGrant::new("quartz.agent", "turn", 1)]),
    );
    tree
}

fn replacement_controller(config: u64) -> ComponentSpec {
    spec("y-controller", "durable-controller")
        .with_config(config)
        .with_patches(vec![CompositionPatch::replace(
            "d-tool",
            spec("d-tool", "agent-tool-b").with_config(2),
        )])
}

fn provider_failure_tree(controller_config: u64) -> ComponentTree {
    let mut tree = agent_tree("agent-tool-a", 1, 1);
    tree.roots.push(
        spec("y-provider-controller", "durable-controller")
            .with_config(controller_config)
            .with_patches(vec![CompositionPatch::replace(
                "c-provider",
                spec("c-provider", "agent-provider").with_config(2),
            )]),
    );
    tree
}

fn assert_facts(runtime: &Runtime, expected: &[(u64, u64, u64, u64)]) {
    let actual = runtime
        .events()
        .iter()
        .map(|event| decode(event.value))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

fn values(runtime: &Runtime) -> Vec<u64> {
    runtime.events().iter().map(|event| event.value).collect()
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
            std::env::temp_dir().join(format!("quartz-slice4-{name}-{}-{id}", std::process::id()));
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
