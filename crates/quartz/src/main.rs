use quartz_kernel::{
    ComponentSpec, ComponentTree, CompositionPatch, Error, EventGrant, FiberState, Limits, Runtime,
    SnapshotGrant, TraceEvent,
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
        Some("--events-write") => run_event_phase(&required_path(args.next())?, EventPhase::Write)?,
        Some("--events-recover") => {
            run_event_phase(&required_path(args.next())?, EventPhase::Recover)?
        }
        Some("--events-verify") => {
            run_event_phase(&required_path(args.next())?, EventPhase::Verify)?
        }
        Some("--agent-start") => run_agent_phase(&required_path(args.next())?, AgentPhase::Start)?,
        Some("--agent-resume") => {
            let expected = args
                .next()
                .ok_or("agent resume requires an expected event count")?
                .parse()?;
            run_agent_phase(&required_path(args.next())?, AgentPhase::Resume(expected))?
        }
        Some("--agent-replace") => {
            run_agent_phase(&required_path(args.next())?, AgentPhase::Replace)?
        }
        Some("--agent-verify") => {
            run_agent_phase(&required_path(args.next())?, AgentPhase::Verify)?
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
    run_event_acceptance(&fixtures)?;
    run_agent_acceptance(&fixtures)?;
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

#[derive(Clone, Copy)]
enum EventPhase {
    Write,
    Recover,
    Verify,
}

fn run_event_acceptance(fixtures: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let journal =
        std::env::temp_dir().join(format!("quartz-slice3-smoke-{}.qj", std::process::id()));
    let events = journal.with_extension("qe");
    for path in [&journal, &events] {
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    let executable = std::env::current_exe()?;
    for argument in ["--events-write", "--events-recover", "--events-verify"] {
        let status = Command::new(&executable)
            .arg(argument)
            .arg(&journal)
            .status()?;
        if !status.success() {
            return Err(format!("event phase `{argument}` failed with {status}").into());
        }
    }
    fs::remove_file(&journal)?;
    fs::remove_file(&events)?;
    assert!(fixtures.join("event-store.wasm").is_file());
    Ok(())
}

fn run_event_phase(journal: &Path, phase: EventPhase) -> Result<(), Box<dyn std::error::Error>> {
    let fixtures = PathBuf::from(env!("QUARTZ_FIXTURE_DIR"));
    let events = journal.with_extension("qe");
    let mut runtime = Runtime::open_persistent(
        Limits::default(),
        ComponentSpec::new("event-store", artifact(&fixtures, "event-store"))
            .with_journal_paths(vec![journal.to_path_buf()])
            .with_event_stream_paths(vec![events]),
    )?;
    match phase {
        EventPhase::Write => {
            runtime.apply_tree(event_tree(&fixtures, false))?;
            assert_eq!(runtime.events().len(), 1);
            assert_eq!(runtime.events()[0].value, 37);
            println!("durable event commit: session fact synchronized before process exit");
        }
        EventPhase::Recover => {
            assert_eq!(runtime.events().len(), 1);
            assert_eq!(runtime.events()[0].value, 37);
            runtime.apply_tree(event_tree(&fixtures, true))?;
            assert_eq!(runtime.state_value("projection", 901), Some(37));
            println!("event restart: projection reconstructed from one durable fact");
        }
        EventPhase::Verify => {
            assert_eq!(runtime.events().len(), 1);
            assert_eq!(runtime.state_value("projection", 901), Some(37));
            runtime.shutdown_persistent()?;
            assert!(runtime.is_observationally_clean());
            println!("event second restart: projection reconstructed; shutdown clean");
        }
    }
    Ok(())
}

fn event_tree(fixtures: &Path, projection: bool) -> ComponentTree {
    let mut roots = vec![
        ComponentSpec::new("append", artifact(fixtures, "event-appender"))
            .with_config(37)
            .with_event_grants(vec![EventGrant::new("quartz.session", "value", 1)]),
    ];
    if projection {
        roots.push(ComponentSpec::new(
            "projection",
            artifact(fixtures, "event-projection"),
        ));
    }
    ComponentTree { roots }
}

#[derive(Clone, Copy)]
enum AgentPhase {
    Start,
    Resume(usize),
    Replace,
    Verify,
}

fn run_agent_acceptance(fixtures: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let journal =
        std::env::temp_dir().join(format!("quartz-slice5-smoke-{}.qj", std::process::id()));
    let events = journal.with_extension("qe");
    let (source_a, source_b) = repository_sources()?;
    for path in [&journal, &events] {
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    let executable = std::env::current_exe()?;
    run_agent_process(&executable, "--agent-start", None, &journal)?;
    for count in 2..=8 {
        run_agent_process(&executable, "--agent-resume", Some(count), &journal)?;
    }
    run_agent_process(&executable, "--agent-replace", None, &journal)?;
    for count in 10..=16 {
        run_agent_process(&executable, "--agent-resume", Some(count), &journal)?;
    }
    run_agent_process(&executable, "--agent-verify", None, &journal)?;
    fs::remove_file(&journal)?;
    fs::remove_file(&events)?;
    assert!(source_a.is_file());
    assert!(source_b.is_file());
    assert!(fixtures.join("repo-agent-loop.wasm").is_file());
    Ok(())
}

fn run_agent_process(
    executable: &Path,
    argument: &str,
    expected: Option<usize>,
    journal: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut command = Command::new(executable);
    command.arg(argument);
    if let Some(expected) = expected {
        command.arg(expected.to_string());
    }
    let status = command.arg(journal).status()?;
    if !status.success() {
        return Err(format!("agent phase `{argument}` failed with {status}").into());
    }
    Ok(())
}

fn run_agent_phase(journal: &Path, phase: AgentPhase) -> Result<(), Box<dyn std::error::Error>> {
    let fixtures = PathBuf::from(env!("QUARTZ_FIXTURE_DIR"));
    let events = journal.with_extension("qe");
    let (source_a, source_b) = repository_sources()?;
    let mut runtime = Runtime::open_persistent(
        Limits::default(),
        ComponentSpec::new("event-store", artifact(&fixtures, "event-store"))
            .with_journal_paths(vec![journal.to_path_buf()])
            .with_event_stream_paths(vec![events]),
    )?;
    match phase {
        AgentPhase::Start => {
            assert!(runtime.events().is_empty());
            runtime.apply_tree(agent_tree(&fixtures, &source_a, &source_b, false, false))?;
            assert_eq!(runtime.events().len(), 1);
            println!("agent boundary 1: public repository question committed");
        }
        AgentPhase::Resume(expected) => {
            assert_eq!(runtime.events().len(), expected);
            let value = runtime.events().last().expect("resumed event").value;
            println!(
                "agent boundary {expected}: fact kind {} turn {} reconstructed",
                value >> 56,
                (value >> 48) & 0xff
            );
        }
        AgentPhase::Replace => {
            assert_eq!(runtime.events().len(), 8);
            let base_revision = runtime.composition_revision();
            runtime.apply_tree(agent_replacement_tree(
                &fixtures,
                &source_a,
                &source_b,
                base_revision,
                false,
            ))?;
            assert_eq!(runtime.events().len(), 8);
            assert_eq!(runtime.state_value("y-controller", 700), Some(0));
            runtime.apply_tree(agent_replacement_tree(
                &fixtures,
                &source_a,
                &source_b,
                base_revision,
                true,
            ))?;
            assert_eq!(runtime.events().len(), 9);
            println!("agent boundary 9: inspector B active; second repository question committed");
        }
        AgentPhase::Verify => {
            let records = runtime.events();
            assert_eq!(
                records.iter().map(|event| event.value).collect::<Vec<_>>(),
                expected_agent_facts()
            );
            let payloads = records
                .iter()
                .filter_map(|event| event.payload.as_ref())
                .collect::<Vec<_>>();
            assert_eq!(payloads.len(), 2);
            assert_eq!(
                payloads[0].provenance,
                fs::canonicalize(&source_a)?.display().to_string()
            );
            assert_eq!(
                payloads[1].provenance,
                fs::canonicalize(&source_b)?.display().to_string()
            );
            println!(
                "agent answer 6002 cites {} and {}; two exact transcripts reconstructed",
                payloads[0].provenance, payloads[1].provenance
            );
            runtime.shutdown_persistent()?;
            assert!(runtime.is_observationally_clean());
            println!("agent inspection authority recovered; shutdown clean");
        }
    }
    Ok(())
}

fn agent_tree(
    fixtures: &Path,
    source_a: &Path,
    source_b: &Path,
    inspector_b: bool,
    second_prompt: bool,
) -> ComponentTree {
    let event_grant = || EventGrant::new("quartz.agent", "repository-turn", 2);
    let all_snapshots = || vec![snapshot_grant(source_a), snapshot_grant(source_b)];
    let (tool, config, tool_snapshots) = if inspector_b {
        (
            "repo-inspector-b",
            fnv1a(&fs::read(source_b).expect("read repository B")),
            all_snapshots(),
        )
    } else {
        (
            "repo-inspector-a",
            fnv1a(&fs::read(source_a).expect("read repository A")),
            vec![snapshot_grant(source_a)],
        )
    };
    let mut roots = vec![
        ComponentSpec::new("a-loop", artifact(fixtures, "repo-agent-loop"))
            .with_event_grants(vec![event_grant()])
            .with_snapshot_grants(all_snapshots()),
        ComponentSpec::new("b-gateway", artifact(fixtures, "agent-gateway")),
        ComponentSpec::new("c-provider", artifact(fixtures, "repo-agent-provider")),
        ComponentSpec::new("d-tool", artifact(fixtures, tool))
            .with_config(config)
            .with_snapshot_grants(tool_snapshots),
        ComponentSpec::new("e-governor", artifact(fixtures, "governor")),
        ComponentSpec::new("z-client", artifact(fixtures, "agent-client"))
            .with_config(1)
            .with_event_grants(vec![event_grant()]),
    ];
    if second_prompt {
        roots.push(
            ComponentSpec::new("zz-client", artifact(fixtures, "agent-client"))
                .with_config(2)
                .with_event_grants(vec![event_grant()]),
        );
    }
    ComponentTree { roots }
}

fn agent_replacement_tree(
    fixtures: &Path,
    source_a: &Path,
    source_b: &Path,
    base_revision: u64,
    replaced: bool,
) -> ComponentTree {
    let mut tree = agent_tree(fixtures, source_a, source_b, replaced, replaced);
    tree.roots.push(
        ComponentSpec::new("y-controller", artifact(fixtures, "durable-controller"))
            .with_config((base_revision + 1) << 32)
            .with_patches(vec![CompositionPatch::replace(
                "d-tool",
                ComponentSpec::new("d-tool", artifact(fixtures, "repo-inspector-b"))
                    .with_config(fnv1a(&fs::read(source_b).expect("read repository B")))
                    .with_snapshot_grants(vec![snapshot_grant(source_a), snapshot_grant(source_b)]),
            )]),
    );
    tree
}

fn snapshot_grant(path: &Path) -> SnapshotGrant {
    SnapshotGrant::from_file(
        path,
        fs::canonicalize(path)
            .expect("canonical snapshot")
            .display()
            .to_string(),
    )
    .expect("admit immutable repository snapshot")
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

fn expected_agent_facts() -> Vec<u64> {
    [
        (1, 1, 0, 1),
        (2, 1, 17, 1),
        (3, 1, 18, 1),
        (4, 1, 18, 5001),
        (2, 1, 19, 5001),
        (5, 1, 0, 6001),
        (6, 1, 0, 1),
        (7, 1, 0, 1),
        (1, 2, 0, 2),
        (2, 2, 33, 1),
        (3, 2, 34, 1),
        (4, 2, 34, 5002),
        (2, 2, 35, 5002),
        (5, 2, 0, 6002),
        (6, 2, 0, 1),
        (7, 2, 0, 1),
    ]
    .into_iter()
    .map(|(kind, turn, invocation, data)| (kind << 56) | (turn << 48) | (invocation << 32) | data)
    .collect()
}
fn repository_sources() -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let root = fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))?;
    Ok((root.join("README.md"), root.join("lode/summary.md")))
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
