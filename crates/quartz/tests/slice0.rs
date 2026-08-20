use quartz_kernel::{ComponentSpec, ComponentTree, Error, FiberState, Limits, Runtime, TraceEvent};
use std::path::{Path, PathBuf};

#[test]
fn recovery_is_lifo_and_disposal_is_exactly_once() {
    let mut runtime = runtime();
    runtime.apply_tree(slice_tree("provider-a", 0)).unwrap();
    let provider = runtime.fiber_id("root/provider").unwrap();
    runtime.clear_trace();

    runtime.apply_tree(ComponentTree::default()).unwrap();
    let trace = runtime.trace();
    let recovered: Vec<_> = trace
        .iter()
        .filter_map(|event| match event {
            TraceEvent::EffectRecovered {
                fiber,
                effect,
                kind,
            } if *fiber == provider => Some((*effect, kind.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(recovered.len(), 2);
    assert_eq!(recovered[0].1, "coeffect");
    assert_eq!(recovered[1].1, "state");
    assert!(recovered[0].0 > recovered[1].0);

    let mut all_effects: Vec<_> = trace
        .iter()
        .filter_map(|event| match event {
            TraceEvent::EffectRecovered { effect, .. } => Some(*effect),
            _ => None,
        })
        .collect();
    let total = all_effects.len();
    all_effects.sort_unstable();
    all_effects.dedup();
    assert_eq!(
        all_effects.len(),
        total,
        "an inverse recovered more than once"
    );

    runtime.clear_trace();
    runtime.apply_tree(ComponentTree::default()).unwrap();
    assert!(
        !runtime
            .trace()
            .iter()
            .any(|event| matches!(event, TraceEvent::EffectRecovered { .. }))
    );
    assert!(runtime.is_observationally_clean());
}

#[test]
fn provider_identity_change_reactivates_equal_value_consumer_in_order() {
    let mut runtime = runtime();
    runtime.apply_tree(slice_tree("provider-a", 0)).unwrap();
    let provider_a = runtime.fiber_id("root/provider").unwrap();
    let consumer_a = runtime.fiber_id("root/consumer").unwrap();
    assert_eq!(runtime.state_value("root/consumer", 900), Some(41));
    runtime.clear_trace();

    runtime
        .replace_entry("root/provider", spec("provider", "provider-b"))
        .unwrap();
    let provider_b = runtime.fiber_id("root/provider").unwrap();
    let consumer_b = runtime.fiber_id("root/consumer").unwrap();
    assert_ne!(provider_a, provider_b);
    assert_eq!(
        consumer_a, consumer_b,
        "consumer fiber identity is stable across episodes"
    );
    assert_eq!(
        runtime.committed_provider("root/consumer", 1),
        Some(provider_b)
    );
    assert_eq!(runtime.state_value("root/consumer", 900), Some(41));

    let trace = runtime.trace();
    let consumer_unavailable = position(
        &trace,
        |event| matches!(event, TraceEvent::FiberUnavailable { fiber, .. } if *fiber == consumer_a),
    );
    let consumer_inactive = position(
        &trace,
        |event| matches!(event, TraceEvent::FiberInactive { fiber, .. } if *fiber == consumer_a),
    );
    let provider_recovery = position(
        &trace,
        |event| matches!(event, TraceEvent::EffectRecovered { fiber, kind, .. } if *fiber == provider_a && kind == "coeffect"),
    );
    let provider_b_active = position(
        &trace,
        |event| matches!(event, TraceEvent::FiberActivated { fiber, .. } if *fiber == provider_b),
    );
    let consumer_reactivated = trace
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            matches!(event, TraceEvent::FiberActivated { fiber, .. } if *fiber == consumer_a)
                .then_some(index)
        })
        .last()
        .unwrap();
    assert!(consumer_unavailable < consumer_inactive);
    assert!(consumer_inactive < provider_recovery);
    assert!(provider_recovery < provider_b_active);
    assert!(provider_b_active < consumer_reactivated);
}

#[test]
fn target_churn_diverts_activation_and_chains_unloading() {
    let mut runtime = runtime();
    runtime.declare_tree(slice_tree("provider-a", 2)).unwrap();
    step_until(&mut runtime, |runtime| {
        runtime.fiber_state("root/consumer") == Some(FiberState::Activating)
            && runtime.state_value("root/consumer", 100).is_some()
    });
    let consumer = runtime.fiber_id("root/consumer").unwrap();
    runtime.clear_trace();
    runtime
        .replace_entry("root/provider", spec("provider", "provider-b"))
        .unwrap();
    assert_eq!(
        runtime.fiber_state("root/consumer"),
        Some(FiberState::Active)
    );
    assert!(runtime.trace().iter().any(|event| {
        matches!(event, TraceEvent::FiberUnavailable { fiber, .. } if *fiber == consumer)
    }));
    assert!(runtime.trace().iter().any(|event| {
        matches!(event, TraceEvent::EffectRecovered { fiber, kind, .. } if *fiber == consumer && kind == "state")
    }));

    let tree = slice_tree("provider-b", 2);
    runtime.declare_tree(ComponentTree::default()).unwrap();
    step_until(&mut runtime, |runtime| {
        runtime.fiber_state("root") == Some(FiberState::Unloading)
    });
    runtime.clear_trace();
    runtime.declare_tree(tree).unwrap();
    runtime.reconcile_to_quiescence().unwrap();
    assert_eq!(runtime.fiber_state("root"), Some(FiberState::Active));
    let trace = runtime.trace();
    let inactive = position(
        &trace,
        |event| matches!(event, TraceEvent::FiberInactive { path, .. } if path == "root"),
    );
    let activating = position(
        &trace,
        |event| matches!(event, TraceEvent::FiberActivating { path, .. } if path == "root"),
    );
    assert!(
        inactive < activating,
        "unloading must finish before the restored target activates"
    );
}

#[test]
fn failed_replacement_recovers_partial_effects_and_restores_prior_generation() {
    let mut runtime = runtime();
    runtime.apply_tree(slice_tree("provider-a", 0)).unwrap();
    let provider = runtime.fiber_id("root/provider").unwrap();
    runtime.clear_trace();

    let error = runtime
        .replace_entry("root/provider", spec("provider", "provider-bad"))
        .unwrap_err();
    assert!(matches!(error, Error::ReplacementRolledBack(_)));
    assert_eq!(runtime.fiber_id("root/provider"), Some(provider));
    assert_eq!(
        runtime.fiber_state("root/provider"),
        Some(FiberState::Active)
    );
    assert_eq!(
        runtime.committed_provider("root/consumer", 1),
        Some(provider)
    );
    assert_eq!(runtime.state_value("root/consumer", 900), Some(41));
    assert!(runtime.trace().iter().any(|event| {
        matches!(event, TraceEvent::ReplacementRolledBack { restored, .. } if *restored == provider)
    }));
}

#[test]
fn cancellation_during_activation_recovers_partial_subtree() {
    let mut runtime = runtime();
    let tree = ComponentTree {
        roots: vec![spec("root", "root").with_config(2).with_children(vec![
            spec("first", "provider-a"),
            spec("second", "consumer"),
        ])],
    };
    runtime.declare_tree(tree).unwrap();
    step_until(&mut runtime, |runtime| {
        runtime.fiber_state("root") == Some(FiberState::Activating)
            && runtime.fiber_id("root/first").is_some()
    });
    runtime.declare_tree(ComponentTree::default()).unwrap();
    runtime.reconcile_to_quiescence().unwrap();
    assert!(runtime.is_observationally_clean());
}

#[test]
fn dependency_cycles_are_rejected_before_activation() {
    let mut runtime = runtime();
    let tree = ComponentTree {
        roots: vec![
            spec("root", "root")
                .with_config(2)
                .with_children(vec![spec("a", "cycle-a"), spec("b", "cycle-b")]),
        ],
    };
    assert!(matches!(
        runtime.apply_tree(tree),
        Err(Error::DependencyCycle(_))
    ));
    assert!(runtime.is_observationally_clean());
}

#[test]
fn component_creation_is_bounded_before_activation() {
    let mut runtime = Runtime::new(Limits {
        max_components: 2,
        ..Limits::default()
    })
    .unwrap();
    let tree = ComponentTree {
        roots: vec![
            spec("root", "root")
                .with_config(2)
                .with_children(vec![spec("a", "provider-a"), spec("b", "consumer")]),
        ],
    };
    assert!(matches!(
        runtime.apply_tree(tree),
        Err(Error::ComponentLimit { .. })
    ));
    assert!(runtime.is_observationally_clean());
}

#[test]
fn undeclared_dependency_access_fails_activation() {
    let mut runtime = runtime();
    let tree = ComponentTree {
        roots: vec![
            spec("root", "root")
                .with_config(1)
                .with_children(vec![spec("undeclared", "undeclared")]),
        ],
    };
    runtime.apply_tree(tree).unwrap();
    assert!(matches!(
        runtime.fiber_state("root/undeclared"),
        Some(FiberState::Failed(error)) if error.contains("status 2")
    ));
    runtime.apply_tree(ComponentTree::default()).unwrap();
    assert!(runtime.is_observationally_clean());
}

#[test]
fn removing_parent_recovers_complete_subtree_and_module_instances() {
    let mut runtime = runtime();
    runtime.apply_tree(slice_tree("provider-a", 0)).unwrap();
    let active = runtime.observation();
    assert!(active.fibers >= 3);
    assert!(active.bindings >= 1);
    assert!(active.live_artifacts >= 3);

    runtime.apply_tree(ComponentTree::default()).unwrap();
    assert!(
        runtime.is_observationally_clean(),
        "remaining context: {:?}",
        runtime.observation()
    );
}

fn runtime() -> Runtime {
    Runtime::new(Limits::default()).unwrap()
}

fn slice_tree(provider: &str, consumer_delay: u64) -> ComponentTree {
    ComponentTree {
        roots: vec![spec("root", "root").with_config(2).with_children(vec![
            spec("provider", provider),
            spec("consumer", "consumer").with_config(consumer_delay),
        ])],
    }
}

fn spec(entry: &str, module: &str) -> ComponentSpec {
    ComponentSpec::new(entry, artifact(module))
}

fn artifact(module: &str) -> PathBuf {
    Path::new(env!("QUARTZ_FIXTURE_DIR"))
        .join(module)
        .with_extension("wasm")
}

fn position(trace: &[TraceEvent], predicate: impl Fn(&TraceEvent) -> bool) -> usize {
    trace.iter().position(predicate).expect("trace event")
}

fn step_until(runtime: &mut Runtime, predicate: impl Fn(&Runtime) -> bool) {
    for _ in 0..1_000 {
        if predicate(runtime) {
            return;
        }
        assert!(runtime.step().unwrap(), "runtime quiesced before predicate");
    }
    panic!("runtime did not reach predicate");
}
