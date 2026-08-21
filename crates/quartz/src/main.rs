use quartz_kernel::{
    ComponentSpec, ComponentTree, CompositionPatch, Error, FiberState, Limits, Runtime, TraceEvent,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--idle") => {
            let _runtime = Runtime::new(Limits::default())?;
            println!("READY");
            let millis = args
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(2_000);
            std::thread::sleep(std::time::Duration::from_millis(millis));
        }
        Some("--durable-write") => {
            run_durable_phase(&required_path(args.next())?, DurablePhase::Write)?
        }
        Some("--durable-recover") => {
            run_durable_phase(&required_path(args.next())?, DurablePhase::Recover)?
        }
        Some("--durable-verify") => {
            run_durable_phase(&required_path(args.next())?, DurablePhase::Verify)?
        }
        Some(argument) => return Err(format!("unknown argument `{argument}`").into()),
        None => run_acceptance()?,
    }
    Ok(())
}

fn run_acceptance() -> Result<(), Box<dyn std::error::Error>> {
    let fixtures = PathBuf::from(env!("QUARTZ_FIXTURE_DIR"));
    let mut runtime = Runtime::new(Limits::default())?;
    let started = Instant::now();
    let phase = Instant::now();
    runtime.apply_tree(slice_tree(&fixtures, "provider-a", 0))?;
    let initial_composition_ns = phase.elapsed().as_nanos();

    let provider_a = active_id(&runtime, "root/provider")?;
    let consumer_a = active_id(&runtime, "root/consumer")?;
    assert_eq!(
        runtime.committed_provider("root/consumer", 1),
        Some(provider_a)
    );
    assert_eq!(runtime.state_value("root/consumer", 900), Some(41));
    println!("provider-a active: fiber {}", provider_a.0);
    println!("consumer active against provider-a: fiber {}", consumer_a.0);

    let phase = Instant::now();
    let invalid = runtime.replace_entry(
        "root/provider",
        ComponentSpec::new("provider", artifact(&fixtures, "provider-bad")),
    );
    let invalid_replacement_ns = phase.elapsed().as_nanos();
    assert!(matches!(invalid, Err(Error::ReplacementRolledBack(_))));
    assert_eq!(active_id(&runtime, "root/provider")?, provider_a);
    assert_eq!(
        runtime.committed_provider("root/consumer", 1),
        Some(provider_a)
    );
    assert_eq!(runtime.state_value("root/consumer", 900), Some(41));
    println!("invalid replacement rolled back: provider-a remained the working generation");

    let phase = Instant::now();
    runtime.clear_trace();
    runtime.replace_entry(
        "root/provider",
        ComponentSpec::new("provider", artifact(&fixtures, "provider-b")),
    )?;
    let valid_replacement_ns = phase.elapsed().as_nanos();
    let provider_b = active_id(&runtime, "root/provider")?;
    assert_ne!(provider_a, provider_b);
    assert_eq!(
        runtime.committed_provider("root/consumer", 1),
        Some(provider_b)
    );
    assert_eq!(runtime.state_value("root/consumer", 900), Some(41));
    assert_replacement_order(&runtime.trace(), provider_a, provider_b);
    println!("provider-b active: fiber {}", provider_b.0);
    println!("consumer reactivated against provider-b identity");

    let phase = Instant::now();
    runtime.apply_tree(ComponentTree::default())?;
    let subtree_removal_ns = phase.elapsed().as_nanos();
    assert!(
        runtime.is_observationally_clean(),
        "final context: {:?}",
        runtime.observation()
    );
    let scenario_total_ns = started.elapsed().as_nanos();
    let cross_component_resolve_ns = measure_cross_component_resolve(&fixtures, 1_000_000)?;
    run_governed_acceptance(&fixtures)?;
    run_durable_acceptance(&fixtures)?;
    println!("root removed: subtree recovered to a clean context");
    println!("initial_composition_ns={initial_composition_ns}");
    println!("invalid_replacement_ns={invalid_replacement_ns}");
    println!("valid_replacement_ns={valid_replacement_ns}");
    println!("subtree_removal_ns={subtree_removal_ns}");
    println!("scenario_total_ns={scenario_total_ns}");
    println!("cross_component_resolve_ns_per={cross_component_resolve_ns:.3}");
    Ok(())
}

fn run_governed_acceptance(fixtures: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut runtime = Runtime::new(Limits::default())?;
    runtime.apply_tree(ComponentTree {
        roots: vec![
            ComponentSpec::new("root", artifact(fixtures, "root"))
                .with_config(4)
                .with_children(vec![
                    ComponentSpec::new("governor", artifact(fixtures, "governor")),
                    ComponentSpec::new("provider", artifact(fixtures, "provider-a")),
                    ComponentSpec::new("consumer", artifact(fixtures, "consumer")),
                    ComponentSpec::new("controller", artifact(fixtures, "controller"))
                        .with_config(1_u64 << 32)
                        .with_patches(vec![CompositionPatch::replace(
                            "root/provider",
                            ComponentSpec::new("provider", artifact(fixtures, "provider-b")),
                        )]),
                ]),
        ],
    })?;
    assert_eq!(runtime.composition_revision(), 2);
    assert_eq!(runtime.state_value("root/controller", 700), Some(0));
    assert_eq!(runtime.state_value("root/provider", 10), Some(2));
    assert_eq!(runtime.state_value("root/consumer", 900), Some(41));
    println!("controller patch authorized: provider-b committed");

    runtime.replace_entry(
        "root/controller",
        ComponentSpec::new("controller", artifact(fixtures, "root")),
    )?;
    assert_eq!(runtime.state_value("root/provider", 10), Some(1));
    assert_eq!(runtime.state_value("root/consumer", 900), Some(41));
    assert_eq!(runtime.observation().composition_effects, 0);
    println!("controller recovered: accepted provider patch inverted");

    runtime.apply_tree(ComponentTree::default())?;
    assert!(
        runtime.is_observationally_clean(),
        "governed final context: {:?}",
        runtime.observation()
    );
    println!("governed composition removed: context clean");
    Ok(())
}

#[derive(Clone, Copy)]
enum DurablePhase {
    Write,
    Recover,
    Verify,
}

fn run_durable_acceptance(fixtures: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let journal =
        std::env::temp_dir().join(format!("quartz-slice2-smoke-{}.qj", std::process::id()));
    if journal.exists() {
        fs::remove_file(&journal)?;
    }
    let executable = std::env::current_exe()?;
    for argument in ["--durable-write", "--durable-recover", "--durable-verify"] {
        let status = Command::new(&executable)
            .arg(argument)
            .arg(&journal)
            .status()?;
        if !status.success() {
            return Err(format!("durable phase `{argument}` failed with {status}").into());
        }
    }
    fs::remove_file(&journal)?;
    assert!(fixtures.join("journal.wasm").is_file());
    Ok(())
}

fn run_durable_phase(
    journal: &Path,
    phase: DurablePhase,
) -> Result<(), Box<dyn std::error::Error>> {
    let fixtures = PathBuf::from(env!("QUARTZ_FIXTURE_DIR"));
    let mut runtime = Runtime::open_persistent(
        Limits::default(),
        ComponentSpec::new("journal", artifact(&fixtures, "journal"))
            .with_journal_paths(vec![journal.to_path_buf()]),
    )?;
    match phase {
        DurablePhase::Write => {
            runtime.apply_tree(durable_tree(&fixtures, true, "provider-a"))?;
            assert_eq!(runtime.state_value("app/provider", 10), Some(2));
            assert_eq!(runtime.observation().composition_effects, 1);
            assert_eq!(runtime.journal_sequence(), Some(1));
            println!("durable commit: provider-b journaled before process exit");
        }
        DurablePhase::Recover => {
            assert_eq!(runtime.state_value("app/provider", 10), Some(2));
            assert_eq!(runtime.observation().composition_effects, 1);
            runtime.apply_tree(durable_tree(&fixtures, false, "provider-b"))?;
            assert_eq!(runtime.state_value("app/provider", 10), Some(1));
            assert_eq!(runtime.observation().composition_effects, 0);
            assert_eq!(runtime.journal_sequence(), Some(2));
            println!("durable restart: provider-b reconstructed; inverse journaled provider-a");
        }
        DurablePhase::Verify => {
            assert_eq!(runtime.state_value("app/provider", 10), Some(1));
            assert_eq!(runtime.observation().composition_effects, 0);
            runtime.shutdown_persistent()?;
            assert!(runtime.is_observationally_clean());
            println!("durable second restart: provider-a reconstructed; shutdown clean");
        }
    }
    Ok(())
}

fn durable_tree(fixtures: &Path, controller: bool, provider: &str) -> ComponentTree {
    let mut roots = vec![
        ComponentSpec::new("app", artifact(fixtures, "root"))
            .with_config(3)
            .with_children(vec![
                ComponentSpec::new("governor", artifact(fixtures, "governor")),
                ComponentSpec::new("provider", artifact(fixtures, provider)),
                ComponentSpec::new("consumer", artifact(fixtures, "consumer")),
            ]),
    ];
    if controller {
        roots.push(
            ComponentSpec::new("controller", artifact(fixtures, "durable-controller"))
                .with_config(1_u64 << 32)
                .with_patches(vec![CompositionPatch::replace(
                    "app/provider",
                    ComponentSpec::new("provider", artifact(fixtures, "provider-b")),
                )]),
        );
    }
    ComponentTree { roots }
}

fn required_path(value: Option<String>) -> Result<PathBuf, Box<dyn std::error::Error>> {
    value
        .map(PathBuf::from)
        .ok_or_else(|| "durable phase requires a journal path".into())
}

fn measure_cross_component_resolve(
    fixtures: &Path,
    iterations: u64,
) -> Result<f64, Box<dyn std::error::Error>> {
    let mut runtime = Runtime::new(Limits::default())?;
    runtime.declare_tree(ComponentTree {
        roots: vec![
            ComponentSpec::new("root", artifact(fixtures, "root"))
                .with_config(2)
                .with_children(vec![
                    ComponentSpec::new("provider", artifact(fixtures, "provider-a")),
                    ComponentSpec::new("probe", artifact(fixtures, "call-probe"))
                        .with_config(iterations),
                ]),
        ],
    })?;
    for _ in 0..10_000 {
        if runtime.fiber_state("root/probe") == Some(FiberState::Activating) {
            break;
        }
        if !runtime.step()? {
            return Err("call probe quiesced before activation".into());
        }
    }
    if runtime.fiber_state("root/probe") != Some(FiberState::Activating) {
        return Err("call probe did not begin activation".into());
    }
    let started = Instant::now();
    runtime.step()?;
    let elapsed = started.elapsed();
    if runtime.fiber_state("root/probe") != Some(FiberState::Active) {
        return Err("call probe did not finish activation".into());
    }
    runtime.apply_tree(ComponentTree::default())?;
    Ok(elapsed.as_secs_f64() * 1e9 / iterations as f64)
}

fn slice_tree(fixtures: &Path, provider: &str, consumer_delay: u64) -> ComponentTree {
    ComponentTree {
        roots: vec![
            ComponentSpec::new("root", artifact(fixtures, "root"))
                .with_config(2)
                .with_children(vec![
                    ComponentSpec::new("provider", artifact(fixtures, provider)),
                    ComponentSpec::new("consumer", artifact(fixtures, "consumer"))
                        .with_config(consumer_delay),
                ]),
        ],
    }
}

fn artifact(fixtures: &Path, name: &str) -> PathBuf {
    fixtures.join(name).with_extension("wasm")
}

fn active_id(
    runtime: &Runtime,
    path: &str,
) -> Result<quartz_kernel::FiberId, Box<dyn std::error::Error>> {
    if runtime.fiber_state(path) != Some(FiberState::Active) {
        return Err(format!("`{path}` is not active: {:?}", runtime.fiber_state(path)).into());
    }
    runtime
        .fiber_id(path)
        .ok_or_else(|| format!("`{path}` is absent").into())
}

fn assert_replacement_order(
    trace: &[TraceEvent],
    provider_a: quartz_kernel::FiberId,
    provider_b: quartz_kernel::FiberId,
) {
    let consumer_unavailable = trace.iter().position(|event| {
        matches!(event, TraceEvent::FiberUnavailable { path, .. } if path == "root/consumer")
    }).expect("consumer unload trace");
    let old_recovery = trace.iter().position(|event| {
        matches!(event, TraceEvent::EffectRecovered { fiber, kind, .. } if *fiber == provider_a && kind == "coeffect")
    }).expect("provider A recovery trace");
    let new_activation = trace.iter().position(|event| {
        matches!(event, TraceEvent::FiberActivated { fiber, .. } if *fiber == provider_b)
    }).expect("provider B activation trace");
    assert!(consumer_unavailable < old_recovery);
    assert!(old_recovery < new_activation);
}
