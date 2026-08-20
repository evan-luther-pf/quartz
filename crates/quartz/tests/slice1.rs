use quartz_kernel::{
    BindingKind, ComponentSpec, ComponentTree, CompositionPatch, Error, FiberState, InterfaceId,
    Limits, Runtime, TraceEvent,
};
use std::path::{Path, PathBuf};

#[test]
fn authorized_replacement_uses_callable_authority_and_recovers_with_actor() {
    let mut runtime = runtime();
    runtime
        .declare_tree(governed_tree(
            "provider-a",
            controller_config(1, 0),
            vec![replace_provider("provider-b")],
        ))
        .unwrap();
    step_until(&mut runtime, |runtime| {
        runtime.fiber_state("root/controller") == Some(FiberState::Activating)
    });
    let provider_a = runtime.fiber_id("root/provider").unwrap();
    runtime.reconcile_to_quiescence().unwrap();

    let provider_b = runtime.fiber_id("root/provider").unwrap();
    assert_ne!(provider_a, provider_b);
    assert_eq!(runtime.state_value("root/controller", 700), Some(0));
    assert_eq!(runtime.state_value("root/consumer", 900), Some(41));
    assert_eq!(runtime.composition_revision(), 2);
    assert!(runtime.trace().iter().any(|event| {
        matches!(event, TraceEvent::PatchCommitted { actor, target, revision }
            if *actor == runtime.fiber_id("root/controller").unwrap()
                && target == "root/provider"
                && *revision == 2)
    }));
    assert!(matches!(
        runtime.replace_entry("root/provider", spec("provider", "provider-a")),
        Err(Error::PatchTargetOwned(path)) if path == "root/provider"
    ));

    let controller = runtime.fiber_id("root/controller").unwrap();
    runtime.clear_trace();
    runtime
        .replace_entry("root/controller", spec("controller", "root"))
        .unwrap();

    assert_ne!(runtime.fiber_id("root/provider"), Some(provider_b));
    assert_provider_a(&runtime);
    assert_eq!(runtime.state_value("root/consumer", 900), Some(41));
    assert!(runtime.trace().iter().any(|event| {
        matches!(event, TraceEvent::EffectRecovered { fiber, kind, .. }
            if *fiber == controller && kind == "composition")
    }));
    assert_eq!(runtime.observation().composition_effects, 0);
}

#[test]
fn authority_denial_changes_nothing() {
    let mut runtime = runtime();
    runtime
        .apply_tree(governed_tree(
            "provider-a",
            controller_config(1, 1),
            vec![
                replace_provider("provider-b"),
                replace_provider("provider-bad"),
            ],
        ))
        .unwrap();

    assert_eq!(runtime.state_value("root/controller", 700), Some(7));
    assert_provider_a(&runtime);
    assert_eq!(runtime.composition_revision(), 1);
    assert_eq!(runtime.observation().composition_effects, 0);
    assert!(
        !runtime
            .trace()
            .iter()
            .any(|event| matches!(event, TraceEvent::PatchCommitted { .. }))
    );
}

#[test]
fn stale_revision_is_rejected_before_mutation() {
    let mut runtime = runtime();
    runtime
        .apply_tree(governed_tree(
            "provider-a",
            controller_config(0, 0),
            vec![replace_provider("provider-b")],
        ))
        .unwrap();

    assert_eq!(runtime.state_value("root/controller", 700), Some(8));
    assert_provider_a(&runtime);
    assert_eq!(runtime.composition_revision(), 1);
    assert_eq!(runtime.observation().pending_patches, 0);
}

#[test]
fn queued_patch_is_cancelled_when_requester_activation_fails() {
    let mut runtime = runtime();
    runtime
        .apply_tree(governed_tree(
            "provider-a",
            controller_config(1, 0) | (1_u64 << 63),
            vec![replace_provider("provider-b")],
        ))
        .unwrap();

    assert_provider_a(&runtime);
    assert!(matches!(
        runtime.fiber_state("root/controller"),
        Some(FiberState::Failed(_))
    ));
    assert_eq!(runtime.composition_revision(), 1);
    assert_eq!(runtime.observation().composition_effects, 0);
    assert!(runtime.trace().iter().any(|event| {
        matches!(event, TraceEvent::PatchRejected { error, .. }
            if error == "requester activation did not commit")
    }));
}

#[test]
fn malformed_patch_is_rejected_during_tree_admission() {
    let mut runtime = runtime();
    let malformed = CompositionPatch::replace("root/provider", spec("not-provider", "provider-b"));
    let result = runtime.declare_tree(governed_tree(
        "provider-a",
        controller_config(1, 0),
        vec![malformed],
    ));

    assert!(matches!(result, Err(Error::InvalidPatch(_))));
    assert_eq!(runtime.composition_revision(), 0);
    assert!(runtime.is_observationally_clean());
}

#[test]
fn failing_patch_restores_prior_generation_and_fails_requester() {
    let mut runtime = runtime();
    runtime
        .declare_tree(governed_tree(
            "provider-a",
            controller_config(1, 0),
            vec![replace_provider("provider-bad")],
        ))
        .unwrap();
    step_until(&mut runtime, |runtime| {
        runtime.fiber_state("root/controller") == Some(FiberState::Activating)
    });
    let provider_a = runtime.fiber_id("root/provider").unwrap();
    runtime.reconcile_to_quiescence().unwrap();

    assert_eq!(runtime.fiber_id("root/provider"), Some(provider_a));
    assert_eq!(runtime.state_value("root/consumer", 900), Some(41));
    assert!(matches!(
        runtime.fiber_state("root/controller"),
        Some(FiberState::Failed(_))
    ));
    assert_eq!(runtime.composition_revision(), 1);
    assert_eq!(runtime.observation().composition_effects, 0);
    assert!(
        runtime
            .trace()
            .iter()
            .any(|event| matches!(event, TraceEvent::PatchRejected { .. }))
    );
}

#[test]
fn top_level_add_and_remove_patches_are_reversible() {
    let mut add_runtime = runtime();
    add_runtime
        .apply_tree(authority_tree(
            controller_config(1, 0),
            vec![CompositionPatch::add_root(spec("aux", "root"))],
            false,
        ))
        .unwrap();
    assert_eq!(add_runtime.fiber_state("aux"), Some(FiberState::Active));
    add_runtime
        .replace_entry("root/controller", spec("controller", "root"))
        .unwrap();
    assert_eq!(add_runtime.fiber_state("aux"), None);
    assert_eq!(add_runtime.observation().composition_effects, 0);

    let mut remove_runtime = runtime();
    remove_runtime
        .apply_tree(authority_tree(
            controller_config(1, 0),
            vec![CompositionPatch::remove_root("aux")],
            true,
        ))
        .unwrap();
    assert_eq!(remove_runtime.fiber_state("aux"), None);
    remove_runtime
        .replace_entry("root/controller", spec("controller", "root"))
        .unwrap();
    assert_eq!(remove_runtime.fiber_state("aux"), Some(FiberState::Active));
    assert_eq!(remove_runtime.observation().composition_effects, 0);
}

fn governed_tree(provider: &str, config: u64, patches: Vec<CompositionPatch>) -> ComponentTree {
    ComponentTree {
        roots: vec![spec("root", "root").with_config(4).with_children(vec![
            spec("governor", "governor"),
            spec("provider", provider),
            spec("consumer", "consumer"),
            spec("controller", "controller")
                .with_config(config)
                .with_patches(patches),
        ])],
    }
}

fn authority_tree(config: u64, patches: Vec<CompositionPatch>, include_aux: bool) -> ComponentTree {
    let mut roots = vec![spec("root", "root").with_config(2).with_children(vec![
        spec("governor", "governor"),
        spec("controller", "controller")
            .with_config(config)
            .with_patches(patches),
    ])];
    if include_aux {
        roots.push(spec("aux", "root"));
    }
    ComponentTree { roots }
}

fn replace_provider(module: &str) -> CompositionPatch {
    CompositionPatch::replace("root/provider", spec("provider", module))
}

fn controller_config(base_revision: u64, patch_index: u64) -> u64 {
    (base_revision << 32) | patch_index
}

fn assert_provider_a(runtime: &Runtime) {
    let interface = InterfaceId {
        kind: BindingKind::Value,
        namespace: "quartz.slice0".into(),
        interface: "value".into(),
        revision: 1,
    };
    assert_eq!(
        runtime.provider_identity(&interface),
        runtime.fiber_id("root/provider")
    );
    assert_eq!(runtime.state_value("root/provider", 10), Some(1));
}

fn runtime() -> Runtime {
    Runtime::new(Limits::default()).unwrap()
}

fn spec(entry: &str, module: &str) -> ComponentSpec {
    ComponentSpec::new(entry, artifact(module))
}

fn artifact(module: &str) -> PathBuf {
    Path::new(env!("QUARTZ_FIXTURE_DIR"))
        .join(module)
        .with_extension("wasm")
}

fn step_until(runtime: &mut Runtime, predicate: impl Fn(&Runtime) -> bool) {
    for _ in 0..Limits::default().max_reconciliation_steps {
        if predicate(runtime) {
            return;
        }
        assert!(runtime.step().unwrap(), "runtime became quiescent early");
    }
    panic!("predicate did not become true");
}
