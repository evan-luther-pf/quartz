mod commands;
mod openai;
mod proposals;

use quartz_kernel::{
    ComponentSpec, ComponentTree, CompositionPatch, Error, EventGrant, ExchangeAdapter,
    ExchangeFailure, ExchangeGrant, ExchangeResponse, FiberState, Limits, Runtime, SnapshotGrant,
    TraceEvent, WorkspaceGrant,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

const USAGE: &str = "\
Usage: quartz [COMMAND]

Commands:
  --help
  --version
  --acceptance
  --idle [milliseconds]
  --durable-write <journal>
  --durable-recover <journal>
  --durable-verify <journal>
  --events-write <journal>
  --events-recover <journal>
  --events-verify <journal>
  --agent-start <journal>
  --agent-resume <expected-event-count> <journal>
  --agent-replace <journal>
  --agent-verify <journal>
  --repository-edit <directory>
  --reviewed-edit <directory>
  --promote-edit <directory>
  --production-model <model> <prompt> <journal>
  --propose <model> <task> <session> <source> <source> [source]
  --resume-proposals <session>
  --revise-proposal <model> <session> <index> <feedback>
  --promote-proposal <session> <index>
  --run-approved-command <session> -- <executable> [arg ...]
  --continue-task <model> <session>
";

#[derive(Debug, Eq, PartialEq)]
enum CliCommand {
    Help,
    Version,
    Acceptance,
    Idle(u64),
    DurableWrite(PathBuf),
    DurableRecover(PathBuf),
    DurableVerify(PathBuf),
    EventsWrite(PathBuf),
    EventsRecover(PathBuf),
    EventsVerify(PathBuf),
    AgentStart(PathBuf),
    AgentResume(usize, PathBuf),
    AgentReplace(PathBuf),
    AgentVerify(PathBuf),
    RepositoryEdit(PathBuf),
    ReviewedEdit(PathBuf),
    PromoteEdit(PathBuf),
    ProductionModel {
        model: String,
        prompt: PathBuf,
        journal: PathBuf,
    },
    Propose {
        model: String,
        task: PathBuf,
        session: PathBuf,
        sources: Vec<PathBuf>,
    },
    ResumeProposals(PathBuf),
    ReviseProposal {
        model: String,
        session: PathBuf,
        index: usize,
        feedback: PathBuf,
    },
    PromoteProposal {
        session: PathBuf,
        index: usize,
    },
    RunApprovedCommand {
        session: PathBuf,
        argv: Vec<String>,
    },
    ContinueTask {
        model: String,
        session: PathBuf,
    },
}

fn main() {
    if let Err(error) = run_cli(std::env::args().skip(1)) {
        eprintln!("quartz: {error}");
        std::process::exit(2);
    }
}

fn run_cli(args: impl IntoIterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let command =
        parse_args(args).map_err(|error| -> Box<dyn std::error::Error> { error.into() })?;
    match command {
        CliCommand::Help => print!("{USAGE}"),
        CliCommand::Version => println!("{}", version_text()),
        CliCommand::Acceptance => run_acceptance()?,
        CliCommand::Idle(millis) => {
            let _runtime = Runtime::new(Limits::default())?;
            println!("READY");
            std::thread::sleep(Duration::from_millis(millis));
        }
        CliCommand::DurableWrite(path) => run_durable_phase(&path, DurablePhase::Write)?,
        CliCommand::DurableRecover(path) => run_durable_phase(&path, DurablePhase::Recover)?,
        CliCommand::DurableVerify(path) => run_durable_phase(&path, DurablePhase::Verify)?,
        CliCommand::EventsWrite(path) => run_event_phase(&path, EventPhase::Write)?,
        CliCommand::EventsRecover(path) => run_event_phase(&path, EventPhase::Recover)?,
        CliCommand::EventsVerify(path) => run_event_phase(&path, EventPhase::Verify)?,
        CliCommand::AgentStart(path) => run_agent_phase(&path, AgentPhase::Start)?,
        CliCommand::AgentResume(expected, path) => {
            run_agent_phase(&path, AgentPhase::Resume(expected))?
        }
        CliCommand::AgentReplace(path) => run_agent_phase(&path, AgentPhase::Replace)?,
        CliCommand::AgentVerify(path) => run_agent_phase(&path, AgentPhase::Verify)?,
        CliCommand::RepositoryEdit(path) => {
            run_repository_edit_acceptance(&PathBuf::from(env!("QUARTZ_FIXTURE_DIR")), &path)?
        }
        CliCommand::ReviewedEdit(path) => {
            run_reviewed_edit_acceptance(&PathBuf::from(env!("QUARTZ_FIXTURE_DIR")), &path)?
        }
        CliCommand::PromoteEdit(path) => {
            run_promoted_edit_acceptance(&PathBuf::from(env!("QUARTZ_FIXTURE_DIR")), &path)?
        }
        CliCommand::ProductionModel {
            model,
            prompt,
            journal,
        } => run_production_model(&model, &prompt, &journal)?,
        CliCommand::Propose {
            model,
            task,
            session,
            sources,
        } => run_multi_proposal(&model, &task, &session, &sources)?,
        CliCommand::ReviseProposal {
            model,
            session,
            index,
            feedback,
        } => run_proposal_revision(&model, &session, index, &feedback)?,
        CliCommand::ResumeProposals(session) => {
            let session = fs::canonicalize(session)?;
            let state = reconstruct_proposal_session(&session)?;
            display_proposals(&session, &state)?;
        }
        CliCommand::PromoteProposal { session, index } => run_proposal_promotion(&session, index)?,
        CliCommand::RunApprovedCommand { session, argv } => run_approved_command(&session, argv)?,
        CliCommand::ContinueTask { model, session } => run_proposal_continuation(&model, &session)?,
    }
    Ok(())
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<CliCommand, String> {
    let mut args = args.into_iter();
    let first = args.next();
    let command = first.as_deref().unwrap_or("--help");
    let parsed = match command {
        "--help" => CliCommand::Help,
        "--version" => CliCommand::Version,
        "--acceptance" => CliCommand::Acceptance,
        "--idle" => {
            let millis = args
                .next()
                .map(|value| {
                    value
                        .parse()
                        .map_err(|_| format!("invalid milliseconds for `--idle`: `{value}`"))
                })
                .transpose()?
                .unwrap_or(2_000);
            CliCommand::Idle(millis)
        }
        "--durable-write" => CliCommand::DurableWrite(path_arg(&mut args, command, "<journal>")?),
        "--durable-recover" => {
            CliCommand::DurableRecover(path_arg(&mut args, command, "<journal>")?)
        }
        "--durable-verify" => CliCommand::DurableVerify(path_arg(&mut args, command, "<journal>")?),
        "--events-write" => CliCommand::EventsWrite(path_arg(&mut args, command, "<journal>")?),
        "--events-recover" => CliCommand::EventsRecover(path_arg(&mut args, command, "<journal>")?),
        "--events-verify" => CliCommand::EventsVerify(path_arg(&mut args, command, "<journal>")?),
        "--agent-start" => CliCommand::AgentStart(path_arg(&mut args, command, "<journal>")?),
        "--agent-resume" => {
            let value = required_arg(&mut args, command, "<expected-event-count> <journal>")?;
            let expected = value.parse().map_err(|_| {
                format!("invalid expected event count for `--agent-resume`: `{value}`")
            })?;
            CliCommand::AgentResume(
                expected,
                path_arg(&mut args, command, "<expected-event-count> <journal>")?,
            )
        }
        "--agent-replace" => CliCommand::AgentReplace(path_arg(&mut args, command, "<journal>")?),
        "--agent-verify" => CliCommand::AgentVerify(path_arg(&mut args, command, "<journal>")?),
        "--repository-edit" => {
            CliCommand::RepositoryEdit(path_arg(&mut args, command, "<directory>")?)
        }
        "--reviewed-edit" => CliCommand::ReviewedEdit(path_arg(&mut args, command, "<directory>")?),
        "--promote-edit" => CliCommand::PromoteEdit(path_arg(&mut args, command, "<directory>")?),
        "--production-model" => CliCommand::ProductionModel {
            model: required_arg(&mut args, command, "<model> <prompt> <journal>")?,
            prompt: path_arg(&mut args, command, "<model> <prompt> <journal>")?,
            journal: path_arg(&mut args, command, "<model> <prompt> <journal>")?,
        },
        "--propose" => {
            let model = required_arg(
                &mut args,
                command,
                "<model> <task> <session> <source> <source> [source]",
            )?;
            let task = path_arg(
                &mut args,
                command,
                "<model> <task> <session> <source> <source> [source]",
            )?;
            let session = path_arg(
                &mut args,
                command,
                "<model> <task> <session> <source> <source> [source]",
            )?;
            let sources: Vec<PathBuf> = args.by_ref().map(PathBuf::from).collect();
            if !(2..=3).contains(&sources.len())
                || sources.iter().any(|path| path.as_os_str().is_empty())
            {
                return Err(
                    "`--propose` requires two or three non-empty <source> paths; try `quartz --help`"
                        .into(),
                );
            }
            CliCommand::Propose {
                model,
                task,
                session,
                sources,
            }
        }
        "--revise-proposal" => {
            let expected = "<model> <session> <index> <feedback>";
            let model = required_arg(&mut args, command, expected)?;
            let session = path_arg(&mut args, command, expected)?;
            let value = required_arg(&mut args, command, expected)?;
            let index = value
                .parse()
                .map_err(|_| format!("invalid proposal index: `{value}`"))?;
            let feedback = path_arg(&mut args, command, expected)?;
            CliCommand::ReviseProposal {
                model,
                session,
                index,
                feedback,
            }
        }
        "--resume-proposals" => {
            CliCommand::ResumeProposals(path_arg(&mut args, command, "<session>")?)
        }
        "--promote-proposal" => {
            let session = path_arg(&mut args, command, "<session> <index>")?;
            let value = required_arg(&mut args, command, "<session> <index>")?;
            let index = value
                .parse()
                .map_err(|_| format!("invalid proposal index: `{value}`"))?;
            CliCommand::PromoteProposal { session, index }
        }
        "--run-approved-command" => {
            let expected = "<session> -- <executable> [arg ...]";
            let session = path_arg(&mut args, command, expected)?;
            let separator = required_arg(&mut args, command, expected)?;
            if separator != "--" {
                return Err(
                    "`--run-approved-command` requires `--` before the exact argv; try `quartz --help`"
                        .into(),
                );
            }
            let argv = args.by_ref().collect::<Vec<_>>();
            if argv.first().is_none_or(String::is_empty) {
                return Err(
                    "`--run-approved-command` requires a non-empty executable after `--`; try `quartz --help`"
                        .into(),
                );
            }
            CliCommand::RunApprovedCommand { session, argv }
        }
        "--continue-task" => {
            let expected = "<model> <session>";
            CliCommand::ContinueTask {
                model: required_arg(&mut args, command, expected)?,
                session: path_arg(&mut args, command, expected)?,
            }
        }
        unknown => {
            return Err(format!("unknown command `{unknown}`; try `quartz --help`"));
        }
    };
    if let Some(extra) = args.next() {
        return Err(format!(
            "unexpected trailing argument for `{command}`: `{extra}`; try `quartz --help`"
        ));
    }
    Ok(parsed)
}

fn required_arg(
    args: &mut impl Iterator<Item = String>,
    command: &str,
    expected: &str,
) -> Result<String, String> {
    args.next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("`{command}` requires {expected}; try `quartz --help`"))
}

fn path_arg(
    args: &mut impl Iterator<Item = String>,
    command: &str,
    expected: &str,
) -> Result<PathBuf, String> {
    required_arg(args, command, expected).map(PathBuf::from)
}

fn version_text() -> String {
    format!("quartz {}", env!("CARGO_PKG_VERSION"))
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
    run_exchange_acceptance(&fixtures)?;
    let repository =
        std::env::temp_dir().join(format!("quartz-slice7-smoke-{}", std::process::id()));
    run_repository_edit_acceptance(&fixtures, &repository)?;
    let reviewed = std::env::temp_dir().join(format!("quartz-slice8-smoke-{}", std::process::id()));
    run_reviewed_edit_acceptance(&fixtures, &reviewed)?;
    println!("root removed: subtree recovered to a clean context");
    println!("initial_composition_ns={initial_composition_ns}");
    println!("invalid_replacement_ns={invalid_replacement_ns}");
    println!("valid_replacement_ns={valid_replacement_ns}");
    println!("subtree_removal_ns={subtree_removal_ns}");
    println!("scenario_total_ns={scenario_total_ns}");
    println!("cross_component_resolve_ns_per={cross_component_resolve_ns:.3}");
    Ok(())
}

fn run_repository_edit_acceptance(
    fixtures: &Path,
    directory: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(directory)?;
    let source = directory.join("quartz-slice7-source.txt");
    let ledger = directory.join("quartz-slice7-mutations.qm");
    if source.exists() || ledger.exists() {
        return Err(format!(
            "repository editing smoke paths already exist in `{}`",
            directory.display()
        )
        .into());
    }
    let before = b"alpha";
    let edited_a = b"alpha!";
    let edited_b = b"alpha?";
    fs::write(&source, before)?;

    let mut runtime = Runtime::new(Limits::default())?;
    runtime.apply_tree(repository_edit_tree(
        fixtures,
        &source,
        &ledger,
        "repo-editor-a",
        b'!',
        7_001,
        edited_a,
    )?)?;
    assert_eq!(fs::read(&source)?, edited_a);
    let editor_a = active_id(&runtime, "root/editor")?;
    println!("sandboxed editor-a published one approved repository mutation");

    runtime.replace_entry(
        "root/editor",
        repository_editor_spec(
            fixtures,
            &source,
            &ledger,
            "repo-editor-b",
            b'?',
            7_002,
            edited_b,
        )?,
    )?;
    let editor_b = active_id(&runtime, "root/editor")?;
    assert_ne!(editor_a, editor_b);
    assert_eq!(fs::read(&source)?, edited_b);
    println!("editor-b replaced editor-a and published through the same capability");

    runtime.apply_tree(ComponentTree::default())?;
    assert_eq!(fs::read(&source)?, before);
    assert!(
        runtime.is_observationally_clean(),
        "repository editing final context: {:?}",
        runtime.observation()
    );
    fs::remove_file(&source)?;
    fs::remove_file(&ledger)?;
    if directory.read_dir()?.next().is_none() {
        fs::remove_dir(directory)?;
    }
    println!("repository editing subtree removed: source restored and context clean");
    Ok(())
}

fn run_reviewed_edit_acceptance(
    fixtures: &Path,
    directory: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(directory)?;
    let source = directory.join("quartz-slice8-source.txt");
    let prompt = directory.join("quartz-slice8-prompt.txt");
    let journal = directory.join("quartz-slice8-composition.qj");
    let events = journal.with_extension("qe");
    let exchange = journal.with_extension("qx");
    let mutation = directory.join("quartz-slice8-mutations.qm");
    if [&source, &prompt, &journal, &events, &exchange, &mutation]
        .into_iter()
        .any(|path| path.exists())
    {
        return Err(format!(
            "reviewed editing smoke paths already exist in `{}`",
            directory.display()
        )
        .into());
    }
    let before = b"alpha\n";
    let candidate = b"alpha reviewed by Quartz\n";
    fs::write(&source, before)?;
    fs::write(&prompt, b"Return only the complete reviewed file bytes.")?;

    let (response, provenance) =
        run_exchange_turn(&prompt, &journal, Arc::new(ReviewedEditExchange))?;
    assert_eq!(response, candidate);
    assert_eq!(provenance, "smoke:reviewed-edit");
    assert_eq!(fs::read(&source)?, before);
    println!("reviewed candidate committed durably; repository source unchanged");

    let persistence = || {
        ComponentSpec::new("event-store", artifact(fixtures, "event-store"))
            .with_journal_paths(vec![journal.clone()])
            .with_event_stream_paths(vec![events.clone()])
    };
    let reviewed_tree = |editor: &str,
                         operation: u64,
                         authority_maximum: u64,
                         replacement_base: Option<u64>|
     -> Result<ComponentTree, Error> {
        let mut children = vec![
            ComponentSpec::new("authority", artifact(fixtures, "mutation-authority"))
                .with_config(authority_maximum),
            proposal_editor_spec(fixtures, &source, &mutation, editor, operation, candidate)?,
        ];
        if replacement_base.is_some() {
            children.push(ComponentSpec::new(
                "governor",
                artifact(fixtures, "governor"),
            ));
        }
        let mut roots = vec![
            ComponentSpec::new("root", artifact(fixtures, "root"))
                .with_config(children.len() as u64)
                .with_children(children),
        ];
        if let Some(base_revision) = replacement_base {
            roots.push(
                ComponentSpec::new("zz-controller", artifact(fixtures, "durable-controller"))
                    .with_config(base_revision << 32)
                    .with_patches(vec![CompositionPatch::replace(
                        "root/editor",
                        proposal_editor_spec(
                            fixtures,
                            &source,
                            &mutation,
                            "proposal-editor-b",
                            8_002,
                            candidate,
                        )?,
                    )]),
            );
        }
        Ok(ComponentTree { roots })
    };
    let mut denied = Runtime::open_persistent(Limits::default(), persistence())?;
    denied.apply_tree(reviewed_tree("proposal-editor-a", 8_001, 8_000, None)?)?;
    let denied_state = denied.fiber_state("root/editor");
    assert!(
        matches!(
            &denied_state,
            Some(FiberState::Failed(message)) if message.contains("guest returned status 7")
        ),
        "denied editor state: {denied_state:?}"
    );
    assert_eq!(fs::read(&source)?, before);
    denied.shutdown_persistent()?;
    assert!(denied.is_observationally_clean());
    println!("denied candidate left the repository source unchanged");

    let mut runtime = Runtime::open_persistent(Limits::default(), persistence())?;
    let replacement_base = runtime.composition_revision() + 1;
    runtime.apply_tree(reviewed_tree(
        "proposal-editor-a",
        8_001,
        8_002,
        Some(replacement_base),
    )?)?;
    assert_eq!(fs::read(&source)?, candidate);
    assert!(runtime.trace().iter().any(|event| {
        matches!(
            event,
            TraceEvent::ReplacementCommitted { path, .. } if path == "root/editor"
        )
    }));
    active_id(&runtime, "root/editor")?;
    println!("fresh runtime reconstructed and published the exact approved candidate");
    println!("governed replacement reapplied it after prior editor recovery");

    runtime.shutdown_persistent()?;
    assert_eq!(fs::read(&source)?, before);
    assert!(
        runtime.is_observationally_clean(),
        "reviewed editing final context: {:?}",
        runtime.observation()
    );
    for path in [source, prompt, journal, events, exchange, mutation] {
        fs::remove_file(path)?;
    }
    if directory.read_dir()?.next().is_none() {
        fs::remove_dir(directory)?;
    }
    println!("reviewed editing subtree removed: source restored and context clean");
    Ok(())
}

fn proposal_editor_spec(
    fixtures: &Path,
    source: &Path,
    ledger: &Path,
    editor: &str,
    operation: u64,
    result: &[u8],
) -> Result<ComponentSpec, Error> {
    Ok(ComponentSpec::new("editor", artifact(fixtures, editor))
        .with_config(1)
        .with_workspace_grants(vec![WorkspaceGrant::new(
            source,
            ledger,
            operation,
            "reviewed candidate turn 1",
            digest(b"alpha\n"),
            digest(result),
            64 * 1024,
        )?]))
}

fn run_promoted_edit_acceptance(
    fixtures: &Path,
    directory: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(directory)?;
    let source = directory.join("quartz-slice9-source.txt");
    let prompt = directory.join("quartz-slice9-prompt.txt");
    let journal = directory.join("quartz-slice9-composition.qj");
    let events = journal.with_extension("qe");
    let exchange = journal.with_extension("qx");
    let mutation = directory.join("quartz-slice9-mutations.qm");
    if [&source, &prompt, &journal, &events, &exchange, &mutation]
        .into_iter()
        .any(|path| path.exists())
    {
        return Err(format!(
            "promoted editing smoke paths already exist in `{}`",
            directory.display()
        )
        .into());
    }
    let before = b"alpha\n";
    let candidate = b"alpha reviewed by Quartz\n";
    fs::write(&source, before)?;
    fs::write(&prompt, b"Return only the complete reviewed file bytes.")?;

    let (response, provenance) =
        run_exchange_turn(&prompt, &journal, Arc::new(ReviewedEditExchange))?;
    assert_eq!(response, candidate);
    assert_eq!(provenance, "smoke:reviewed-edit");
    assert_eq!(fs::read(&source)?, before);
    println!("promotion candidate committed durably; source unchanged");

    let persistence = || {
        ComponentSpec::new("event-store", artifact(fixtures, "event-store"))
            .with_journal_paths(vec![journal.clone()])
            .with_event_stream_paths(vec![events.clone()])
    };
    let tree = || -> Result<ComponentTree, Error> {
        Ok(ComponentTree {
            roots: vec![
                ComponentSpec::new("root", artifact(fixtures, "root"))
                    .with_config(3)
                    .with_children(vec![
                        ComponentSpec::new(
                            "mutation-authority",
                            artifact(fixtures, "mutation-authority"),
                        )
                        .with_config(9_001),
                        ComponentSpec::new(
                            "promotion-authority",
                            artifact(fixtures, "promotion-authority-a"),
                        )
                        .with_config(9_001),
                        promoted_editor_spec(
                            fixtures,
                            &source,
                            &mutation,
                            "promotion-editor-a",
                            9_001,
                            candidate,
                        )?,
                    ]),
            ],
        })
    };

    let mut runtime = Runtime::open_persistent(Limits::default(), persistence())?;
    runtime.apply_tree(tree()?)?;
    active_id(&runtime, "root/editor")?;
    assert_eq!(fs::read(&source)?, candidate);
    println!("separate authority durably promoted the exact reviewed candidate");
    drop(runtime);

    let mut restarted = Runtime::open_persistent(Limits::default(), persistence())?;
    active_id(&restarted, "root/editor")?;
    assert_eq!(fs::read(&source)?, candidate);
    println!("fresh runtime reconstructed the promotion without republishing");
    restarted.shutdown_persistent()?;
    assert_eq!(fs::read(&source)?, candidate);
    assert!(
        restarted.is_observationally_clean(),
        "promoted editing final context: {:?}",
        restarted.observation()
    );
    println!("promotion subtree removed: approved source retained and context clean");

    for path in [source, prompt, journal, events, exchange, mutation] {
        fs::remove_file(path)?;
    }
    if directory.read_dir()?.next().is_none() {
        fs::remove_dir(directory)?;
    }
    Ok(())
}

fn promoted_editor_spec(
    fixtures: &Path,
    source: &Path,
    ledger: &Path,
    editor: &str,
    operation: u64,
    result: &[u8],
) -> Result<ComponentSpec, Error> {
    Ok(ComponentSpec::new("editor", artifact(fixtures, editor))
        .with_config(1)
        .with_workspace_grants(vec![WorkspaceGrant::new(
            source,
            ledger,
            operation,
            "promoted reviewed candidate turn 1",
            digest(b"alpha\n"),
            digest(result),
            64 * 1024,
        )?]))
}

fn repository_edit_tree(
    fixtures: &Path,
    source: &Path,
    ledger: &Path,
    editor: &str,
    byte: u8,
    operation: u64,
    result: &[u8],
) -> Result<ComponentTree, Error> {
    Ok(ComponentTree {
        roots: vec![
            ComponentSpec::new("root", artifact(fixtures, "root"))
                .with_config(2)
                .with_children(vec![
                    ComponentSpec::new("authority", artifact(fixtures, "mutation-authority"))
                        .with_config(7_002),
                    repository_editor_spec(
                        fixtures, source, ledger, editor, byte, operation, result,
                    )?,
                ]),
        ],
    })
}

fn repository_editor_spec(
    fixtures: &Path,
    source: &Path,
    ledger: &Path,
    editor: &str,
    byte: u8,
    operation: u64,
    result: &[u8],
) -> Result<ComponentSpec, Error> {
    Ok(ComponentSpec::new("editor", artifact(fixtures, editor))
        .with_config(u64::from(byte))
        .with_workspace_grants(vec![WorkspaceGrant::new(
            source,
            ledger,
            operation,
            "slice7 repository source",
            digest(b"alpha"),
            digest(result),
            64,
        )?]))
}

fn digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a string cannot fail");
    }
    output
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

fn run_multi_proposal(
    model: &str,
    task: &Path,
    session: &Path,
    sources: &[PathBuf],
) -> Result<(), Box<dyn std::error::Error>> {
    if session.exists() {
        if !session.is_dir() || session.read_dir()?.next().is_some() {
            return Err(format!(
                "proposal session `{}` must be absent or empty",
                session.display()
            )
            .into());
        }
    } else {
        fs::create_dir_all(session)?;
    }
    let session = fs::canonicalize(session)?;
    let admission = proposals::Admission::from_files(&repository_root()?, task, sources)?;
    let prompt = session.join("admission.prompt");
    let journal = session.join("turn.qj");
    fs::write(&prompt, admission.prompt_bytes()?)?;

    let api_key =
        std::env::var("OPENAI_API_KEY").map_err(|_| "OPENAI_API_KEY is required for --propose")?;
    let adapter = Arc::new(openai::OpenAiResponses::new(api_key, model.to_owned())?);
    let (response, provenance) =
        run_exchange_turn_with_limits(&prompt, &journal, adapter, proposal_limits())?;
    let candidates = proposals::parse_response(&response, &admission)?;
    proposals::materialize(&session, &candidates)?;
    display_proposals(
        &session,
        &ProposalSession {
            admission,
            proposals: candidates,
            revision: None,
            command: None,
            continuations: Vec::new(),
        },
    )?;
    println!("response provenance: {provenance}");
    println!("proposal turn reconstructed; no source changed");
    Ok(())
}

fn run_proposal_revision(
    model: &str,
    session: &Path,
    index: usize,
    feedback: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let session = fs::canonicalize(session)?;
    let state = reconstruct_proposal_session(&session)?;
    if state.is_complete() {
        return Err("proposal session is explicitly complete".into());
    }
    if state.command.is_some() || !state.continuations.is_empty() {
        return Err("proposal rejection is available only before command cycles begin".into());
    }
    let (admission, candidates) = reconstruct_base_proposals(&session)?;
    let journal = proposals::revision_journal_path(&session);
    let mut prompt_is_durable = false;
    let expected = if journal.exists() {
        match read_durable_proposal_turn(&journal)? {
            DurableProposalTurn::Completed { prompt, response } => {
                let durable = proposals::Revision::from_prompt(&prompt, &admission, &candidates)?;
                validate_revision_selector(&durable, model, index)?;
                proposals::parse_revision_response(&response, &durable)?;
                let state = reconstruct_proposal_session(&session)?;
                display_proposals(&session, &state)?;
                println!("revision turn reconstructed; no exchange emitted");
                return Ok(());
            }
            DurableProposalTurn::Interrupted { prompt } => {
                let durable = proposals::Revision::from_prompt(&prompt, &admission, &candidates)?;
                validate_revision_selector(&durable, model, index)?;
                return Err(
                    "revision turn ended interrupted/unknown; it will not be retried".into(),
                );
            }
            DurableProposalTurn::Pending {
                prompt: Some(prompt),
            } => {
                prompt_is_durable = true;
                let durable = proposals::Revision::from_prompt(&prompt, &admission, &candidates)?;
                validate_revision_selector(&durable, model, index)?;
                durable
            }
            DurableProposalTurn::Pending { prompt: None } => proposals::Revision::new(
                model,
                &fs::read(feedback)?,
                &admission,
                &candidates,
                index,
            )?,
        }
    } else {
        proposals::Revision::new(model, &fs::read(feedback)?, &admission, &candidates, index)?
    };
    let prompt_bytes = expected.prompt_bytes()?;
    let prompt_path = proposals::revision_prompt_path(&session);
    if prompt_is_durable {
        proposals::materialize_revision_prompt(&session, &prompt_bytes)?;
    } else if prompt_path.exists() {
        if fs::read(&prompt_path)? != prompt_bytes {
            return Err("revision prompt cache changed or describes another rejection".into());
        }
    } else {
        proposals::materialize_revision_prompt(&session, &prompt_bytes)?;
    }

    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY is required to start or resume --revise-proposal")?;
    let adapter = Arc::new(openai::OpenAiResponses::new(api_key, model.to_owned())?);
    let (response, provenance) =
        run_exchange_turn_with_limits(&prompt_path, &journal, adapter, proposal_limits())?;
    let corrected = proposals::parse_revision_response(&response, &expected)?;
    proposals::materialize_revision(&session, &expected, &corrected)?;
    let state = reconstruct_proposal_session(&session)?;
    display_proposals(&session, &state)?;
    println!("response provenance: {provenance}");
    println!("revision turn reconstructed; no source changed");
    Ok(())
}

fn validate_revision_selector(
    revision: &proposals::Revision,
    model: &str,
    index: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if revision.model != model || revision.proposal_index != index {
        return Err("durable revision belongs to another model or proposal index".into());
    }
    Ok(())
}

struct ProposalSession {
    admission: proposals::Admission,
    proposals: Vec<proposals::Proposal>,
    revision: Option<ProposalRevisionState>,
    command: Option<CommandHistory>,
    continuations: Vec<ProposalContinuationState>,
}

enum ProposalRevisionState {
    Pending(proposals::Revision),
    Interrupted(proposals::Revision),
    Completed {
        request: proposals::Revision,
        proposal: proposals::Proposal,
    },
}

enum ProposalContinuationState {
    Pending(proposals::Continuation),
    Interrupted(proposals::Continuation),
    Completed {
        request: proposals::Continuation,
        response: proposals::ContinuationResponse,
    },
}

struct CommandHistory {
    attempts: Vec<CommandAttemptState>,
}

enum CommandAttemptState {
    Interrupted(commands::CommandStarted),
    Finished {
        started: commands::CommandStarted,
        finished: commands::CommandFinished,
    },
}

struct CurrentProposal<'a> {
    proposal: &'a proposals::Proposal,
    path: PathBuf,
    revision: u32,
}

enum DurableProposalTurn {
    Pending { prompt: Option<Vec<u8>> },
    Interrupted { prompt: Vec<u8> },
    Completed { prompt: Vec<u8>, response: Vec<u8> },
}

impl CommandHistory {
    fn latest_finished(&self) -> Result<&commands::CommandFinished, Box<dyn std::error::Error>> {
        match self.attempts.last() {
            Some(CommandAttemptState::Finished { finished, .. }) => Ok(finished),
            Some(CommandAttemptState::Interrupted(_)) => Err(
                "latest approved command is interrupted/unknown; it cannot continue the model task"
                    .into(),
            ),
            None => Err("approved command history is empty".into()),
        }
    }

    fn finished(
        &self,
        sequence: u32,
    ) -> Result<&commands::CommandFinished, Box<dyn std::error::Error>> {
        let index = usize::try_from(
            sequence
                .checked_sub(1)
                .ok_or("continuation sequence must be positive")?,
        )?;
        self.attempts
            .iter()
            .filter_map(|attempt| match attempt {
                CommandAttemptState::Finished { finished, .. } => Some(finished),
                CommandAttemptState::Interrupted(_) => None,
            })
            .nth(index)
            .ok_or_else(|| {
                format!("continuation {sequence} has no matching finished command").into()
            })
    }

    fn finished_count(&self) -> usize {
        self.attempts
            .iter()
            .filter(|attempt| matches!(attempt, CommandAttemptState::Finished { .. }))
            .count()
    }

    fn next_attempt(&self) -> Result<u64, Box<dyn std::error::Error>> {
        let attempt = match self.attempts.last() {
            Some(CommandAttemptState::Interrupted(started)) => started.attempt,
            Some(CommandAttemptState::Finished { started, .. }) => started.attempt,
            None => return Ok(1),
        };
        attempt
            .checked_add(1)
            .ok_or_else(|| "approved command attempt overflow".into())
    }
}

impl ProposalSession {
    fn is_complete(&self) -> bool {
        matches!(
            self.continuations.last(),
            Some(ProposalContinuationState::Completed {
                response: proposals::ContinuationResponse::Complete(_),
                ..
            })
        )
    }

    fn next_continuation_sequence(&self) -> Result<u32, Box<dyn std::error::Error>> {
        u32::try_from(self.continuations.len())?
            .checked_add(1)
            .ok_or_else(|| "continuation sequence overflow".into())
    }

    fn current(
        &self,
        session: &Path,
        index: usize,
    ) -> Result<CurrentProposal<'_>, Box<dyn std::error::Error>> {
        for continuation in self.continuations.iter().rev() {
            if let ProposalContinuationState::Completed {
                response:
                    proposals::ContinuationResponse::Proposal {
                        proposal_index,
                        revision,
                        proposal,
                        ..
                    },
                ..
            } = continuation
                && *proposal_index == index
            {
                return Ok(CurrentProposal {
                    proposal,
                    path: proposals::continuation_candidate_path(session, index, *revision),
                    revision: *revision,
                });
            }
        }
        self.current_without_continuation(session, index)
    }

    fn generations_before_continuation(
        &self,
        session: &Path,
        sequence: u32,
    ) -> Result<Vec<proposals::ProposalGeneration>, Box<dyn std::error::Error>> {
        if usize::try_from(sequence)? != self.continuations.len() + 1 {
            return Err("continuation sequence does not follow reconstructed history".into());
        }
        let mut generations = Vec::with_capacity(self.proposals.len());
        for index in 0..self.proposals.len() {
            let current = self.current_without_continuation(session, index)?;
            let admitted_path_index = self
                .admission
                .files
                .iter()
                .position(|file| file.path == current.proposal.path)
                .ok_or("current proposal path left the admission")?;
            generations.push(proposals::ProposalGeneration {
                proposal_index: index,
                admitted_path_index,
                revision: current.revision,
                proposal: current.proposal.clone(),
            });
        }
        for continuation in &self.continuations {
            match continuation {
                ProposalContinuationState::Completed {
                    response:
                        proposals::ContinuationResponse::Proposal {
                            admitted_path_index,
                            proposal_index,
                            revision,
                            proposal,
                        },
                    ..
                } => {
                    let generation = proposals::ProposalGeneration {
                        proposal_index: *proposal_index,
                        admitted_path_index: *admitted_path_index,
                        revision: *revision,
                        proposal: proposal.clone(),
                    };
                    if let Some(existing) = generations
                        .iter_mut()
                        .find(|existing| existing.proposal_index == *proposal_index)
                    {
                        *existing = generation;
                    } else {
                        generations.push(generation);
                    }
                }
                ProposalContinuationState::Completed {
                    response: proposals::ContinuationResponse::Complete(_),
                    ..
                } => {
                    return Err("explicit completion cannot precede another continuation".into());
                }
                ProposalContinuationState::Pending(_)
                | ProposalContinuationState::Interrupted(_) => {
                    return Err(
                        "nonterminal continuation cannot precede another continuation".into(),
                    );
                }
            }
        }
        generations.sort_by_key(|generation| generation.proposal_index);
        Ok(generations)
    }

    fn current_without_continuation(
        &self,
        session: &Path,
        index: usize,
    ) -> Result<CurrentProposal<'_>, Box<dyn std::error::Error>> {
        let original = self
            .proposals
            .get(index)
            .ok_or_else(|| format!("proposal index {index} is absent"))?;
        match &self.revision {
            Some(ProposalRevisionState::Completed { request, proposal })
                if request.proposal_index == index =>
            {
                Ok(CurrentProposal {
                    proposal,
                    path: proposals::revision_candidate_path(session, index),
                    revision: 1,
                })
            }
            Some(ProposalRevisionState::Pending(request))
            | Some(ProposalRevisionState::Interrupted(request))
                if request.proposal_index == index =>
            {
                Err(format!("proposal {index} was rejected and has no completed correction").into())
            }
            _ => Ok(CurrentProposal {
                proposal: original,
                path: proposals::candidate_path(session, index),
                revision: 0,
            }),
        }
    }

    fn current_generations(
        &self,
        session: &Path,
    ) -> Result<Vec<proposals::ProposalGeneration>, Box<dyn std::error::Error>> {
        self.generations_before_continuation(session, self.next_continuation_sequence()?)
    }
}

fn reconstruct_base_proposals(
    session: &Path,
) -> Result<(proposals::Admission, Vec<proposals::Proposal>), Box<dyn std::error::Error>> {
    let (prompt, response) = match read_durable_proposal_turn(&session.join("turn.qj"))? {
        DurableProposalTurn::Completed { prompt, response } => (prompt, response),
        DurableProposalTurn::Pending { .. } => {
            return Err("initial proposal turn is not terminal".into());
        }
        DurableProposalTurn::Interrupted { .. } => {
            return Err("initial proposal turn ended interrupted/unknown".into());
        }
    };
    let admission = proposals::Admission::from_prompt(&prompt)?;
    let candidates = proposals::parse_response(&response, &admission)?;
    proposals::materialize(session, &candidates)?;
    Ok((admission, candidates))
}

fn reconstruct_proposal_session(
    session: &Path,
) -> Result<ProposalSession, Box<dyn std::error::Error>> {
    let session = fs::canonicalize(session)?;
    let (admission, candidates) = reconstruct_base_proposals(&session)?;
    let revision_journal = proposals::revision_journal_path(&session);
    let revision = if revision_journal.exists() {
        match read_durable_proposal_turn(&revision_journal)? {
            DurableProposalTurn::Pending {
                prompt: Some(prompt),
            } => {
                let request = proposals::Revision::from_prompt(&prompt, &admission, &candidates)?;
                proposals::materialize_revision_prompt(&session, &prompt)?;
                Some(ProposalRevisionState::Pending(request))
            }
            DurableProposalTurn::Pending { prompt: None } => {
                return Err("revision journal has no durable rejection prompt".into());
            }
            DurableProposalTurn::Interrupted { prompt } => {
                let request = proposals::Revision::from_prompt(&prompt, &admission, &candidates)?;
                proposals::materialize_revision_prompt(&session, &prompt)?;
                Some(ProposalRevisionState::Interrupted(request))
            }
            DurableProposalTurn::Completed { prompt, response } => {
                let request = proposals::Revision::from_prompt(&prompt, &admission, &candidates)?;
                proposals::materialize_revision_prompt(&session, &prompt)?;
                let proposal = proposals::parse_revision_response(&response, &request)?;
                proposals::materialize_revision(&session, &request, &proposal)?;
                Some(ProposalRevisionState::Completed { request, proposal })
            }
        }
    } else if proposals::revision_prompt_path(&session).exists() {
        return Err(
            "revision prompt exists without durable turn evidence; rerun `--revise-proposal`"
                .into(),
        );
    } else {
        None
    };
    let command = read_command_history(&session)?;
    let finished_count = command.as_ref().map_or(0, CommandHistory::finished_count);
    let mut state = ProposalSession {
        admission,
        proposals: candidates,
        revision,
        command,
        continuations: Vec::new(),
    };
    for sequence_index in 1..=finished_count {
        let sequence = u32::try_from(sequence_index)?;
        let continuation_journal = proposals::continuation_journal_path(&session, sequence);
        if !continuation_journal.exists() {
            if proposals::continuation_prompt_path(&session, sequence).exists() {
                return Err(format!(
                    "continuation {sequence} prompt exists without durable turn evidence; rerun `--continue-task`"
                )
                .into());
            }
            if sequence_index < finished_count {
                return Err(format!(
                    "finished command {sequence} has no continuation before a later command"
                )
                .into());
            }
            break;
        }
        let finished = state
            .command
            .as_ref()
            .ok_or("continuation exists without approved command evidence")?
            .finished(sequence)?;
        let current = state.generations_before_continuation(&session, sequence)?;
        let continuation = match read_durable_proposal_turn(&continuation_journal)? {
            DurableProposalTurn::Pending {
                prompt: Some(prompt),
            } => {
                let request = proposals::Continuation::from_prompt(
                    sequence,
                    &prompt,
                    &state.admission,
                    &current,
                    finished,
                )?;
                proposals::materialize_continuation_prompt(&session, sequence, &prompt)?;
                ProposalContinuationState::Pending(request)
            }
            DurableProposalTurn::Pending { prompt: None } => {
                return Err(format!(
                    "continuation {sequence} journal has no durable continuation prompt"
                )
                .into());
            }
            DurableProposalTurn::Interrupted { prompt } => {
                let request = proposals::Continuation::from_prompt(
                    sequence,
                    &prompt,
                    &state.admission,
                    &current,
                    finished,
                )?;
                proposals::materialize_continuation_prompt(&session, sequence, &prompt)?;
                ProposalContinuationState::Interrupted(request)
            }
            DurableProposalTurn::Completed { prompt, response } => {
                let request = proposals::Continuation::from_prompt(
                    sequence,
                    &prompt,
                    &state.admission,
                    &current,
                    finished,
                )?;
                proposals::materialize_continuation_prompt(&session, sequence, &prompt)?;
                let response = proposals::parse_continuation_response(&response, &request)?;
                proposals::materialize_continuation_response(&session, &request, &response)?;
                ProposalContinuationState::Completed { request, response }
            }
        };
        let permits_later_command = matches!(
            continuation,
            ProposalContinuationState::Completed {
                response: proposals::ContinuationResponse::Proposal { .. },
                ..
            }
        );
        state.continuations.push(continuation);
        if sequence_index < finished_count && !permits_later_command {
            return Err(format!(
                "continuation {sequence} does not permit the later finished command"
            )
            .into());
        }
    }
    validate_continuation_artifacts(&session, state.continuations.len())?;
    Ok(state)
}

fn validate_continuation_artifacts(
    session: &Path,
    reconstructed_count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in fs::read_dir(session)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let sequence = [
            (".qj", "continuation-"),
            (".prompt", "continuation-"),
            (".json", "continuation-"),
            (".txt", "completion-"),
        ]
        .into_iter()
        .find_map(|(suffix, prefix)| {
            name.strip_prefix(prefix)
                .and_then(|value| value.strip_suffix(suffix))
                .and_then(|value| value.parse::<usize>().ok())
        });
        if sequence.is_some_and(|sequence| sequence > reconstructed_count) {
            return Err(format!("orphaned continuation artifact `{name}`").into());
        }
        if let Some((_, revision)) = name
            .strip_prefix("proposal-")
            .and_then(|value| value.split_once(".revision-"))
            && let Some(revision) = revision.strip_suffix(".candidate")
            && let Ok(revision) = revision.parse::<usize>()
            && revision >= 2
            && revision - 1 > reconstructed_count
        {
            return Err(format!("orphaned continuation candidate `{name}`").into());
        }
    }
    Ok(())
}

fn display_proposals(
    session: &Path,
    state: &ProposalSession,
) -> Result<(), Box<dyn std::error::Error>> {
    for (index, candidate) in state.proposals.iter().enumerate() {
        let base_path = proposals::candidate_path(session, index);
        if fs::read(&base_path)? != candidate.content {
            return Err(format!("materialized candidate {index} changed").into());
        }
        let current_revision = state
            .current(session, index)
            .ok()
            .map(|current| current.revision);
        let revision = state.revision.as_ref().filter(|revision| match revision {
            ProposalRevisionState::Pending(request)
            | ProposalRevisionState::Interrupted(request)
            | ProposalRevisionState::Completed { request, .. } => request.proposal_index == index,
        });
        println!(
            "proposal {index} revision 0: {}",
            if current_revision == Some(0) {
                "current"
            } else {
                "superseded"
            }
        );
        display_proposal(candidate, &base_path);
        match revision {
            Some(ProposalRevisionState::Pending(request)) => {
                println!("proposal {index} revision 1: pending");
                println!("  rejection_feedback={:?}", request.feedback);
            }
            Some(ProposalRevisionState::Interrupted(request)) => {
                println!("proposal {index} revision 1: interrupted/unknown");
                println!("  rejection_feedback={:?}", request.feedback);
            }
            Some(ProposalRevisionState::Completed { request, proposal }) => {
                let path = proposals::revision_candidate_path(session, index);
                if fs::read(&path)? != proposal.content {
                    return Err(
                        format!("materialized revision for proposal {index} changed").into(),
                    );
                }
                println!(
                    "proposal {index} revision 1: {}",
                    if current_revision == Some(1) {
                        "current"
                    } else {
                        "superseded"
                    }
                );
                println!("  rejection_feedback={:?}", request.feedback);
                display_proposal(proposal, &path);
            }
            None => {}
        }
    }
    if let Some(command) = &state.command {
        let mut continuation_index = 0;
        for attempt in &command.attempts {
            match attempt {
                CommandAttemptState::Interrupted(started) => {
                    println!("command attempt {}: interrupted/unknown", started.attempt);
                    println!("  argv={:?}", started.argv);
                    println!("  repository={}", started.repository.canonical_root);
                }
                CommandAttemptState::Finished { started, finished } => {
                    println!("command attempt {}: finished", started.attempt);
                    println!("  argv={:?}", started.argv);
                    println!("  repository={}", started.repository.canonical_root);
                    println!("  exit_code={:?}", finished.exit_code);
                    println!("  signal={:?}", finished.signal);
                    println!("  timed_out={}", finished.timed_out);
                    println!("  spawn_error={:?}", finished.spawn_error);
                    println!("  duration_ms={}", finished.duration_ms);
                    println!(
                        "  stdout={:?} truncated={}",
                        String::from_utf8_lossy(&finished.stdout.bytes()?),
                        finished.stdout.truncated
                    );
                    println!(
                        "  stderr={:?} truncated={}",
                        String::from_utf8_lossy(&finished.stderr.bytes()?),
                        finished.stderr.truncated
                    );
                    if let Some(continuation) = state.continuations.get(continuation_index) {
                        display_continuation(session, state, continuation)?;
                    }
                    continuation_index += 1;
                }
            }
        }
    }
    Ok(())
}

fn display_continuation(
    session: &Path,
    state: &ProposalSession,
    continuation: &ProposalContinuationState,
) -> Result<(), Box<dyn std::error::Error>> {
    match continuation {
        ProposalContinuationState::Pending(request) => {
            println!(
                "continuation {}: pending model={}",
                request.sequence, request.model
            );
        }
        ProposalContinuationState::Interrupted(request) => {
            println!(
                "continuation {}: interrupted/unknown model={}",
                request.sequence, request.model
            );
        }
        ProposalContinuationState::Completed {
            request,
            response:
                proposals::ContinuationResponse::Proposal {
                    proposal_index,
                    admitted_path_index,
                    revision,
                    proposal,
                },
        } => {
            let path = proposals::continuation_candidate_path(session, *proposal_index, *revision);
            if fs::read(&path)? != proposal.content {
                return Err(format!(
                    "materialized continuation {} proposal changed",
                    request.sequence
                )
                .into());
            }
            let status = if state.current(session, *proposal_index)?.revision == *revision {
                "current"
            } else {
                "superseded"
            };
            println!(
                "proposal {proposal_index} revision {revision}: {status} admitted_path_index={admitted_path_index}"
            );
            println!(
                "  continuation_sequence={} continuation_model={}",
                request.sequence, request.model
            );
            display_proposal(proposal, &path);
        }
        ProposalContinuationState::Completed {
            request,
            response: proposals::ContinuationResponse::Complete(summary),
        } => {
            println!(
                "continuation {}: COMPLETE model={}",
                request.sequence, request.model
            );
            print!("{summary}");
            if !summary.ends_with('\n') {
                println!();
            }
        }
    }
    Ok(())
}

fn display_proposal(candidate: &proposals::Proposal, path: &Path) {
    println!("  path={}", candidate.path);
    println!("  before_sha256={}", candidate.before_sha256);
    println!("  result_sha256={}", candidate.result_sha256);
    println!("  candidate={}", path.display());
    print!("{}", proposals::render_diff(candidate));
}

fn read_durable_proposal_turn(
    journal: &Path,
) -> Result<DurableProposalTurn, Box<dyn std::error::Error>> {
    let fixtures = PathBuf::from(env!("QUARTZ_FIXTURE_DIR"));
    let events = journal.with_extension("qe");
    let mut runtime = Runtime::open_persistent(
        proposal_limits(),
        ComponentSpec::new("event-store", artifact(&fixtures, "event-store"))
            .with_journal_paths(vec![journal.to_path_buf()])
            .with_event_stream_paths(vec![events]),
    )?;
    let records = runtime.events();
    let payload = |kind| -> Result<Option<Vec<u8>>, Box<dyn std::error::Error>> {
        let matches: Vec<_> = records
            .iter()
            .filter(|event| event.value >> 56 == kind && ((event.value >> 48) & 0xff) == 1)
            .collect();
        if matches.len() > 1 {
            return Err(format!(
                "durable proposal turn has {} facts of kind {kind}; expected at most one",
                matches.len()
            )
            .into());
        }
        matches
            .first()
            .map(|event| {
                event
                    .payload
                    .as_ref()
                    .map(|payload| payload.bytes.clone())
                    .ok_or_else(|| {
                        format!("durable proposal fact kind {kind} has no payload").into()
                    })
            })
            .transpose()
    };
    let prompt = payload(1)?;
    let response = payload(5)?;
    let stop_count = records
        .iter()
        .filter(|event| event.value >> 56 == 7 && ((event.value >> 48) & 0xff) == 1)
        .count();
    let interrupted_count = records
        .iter()
        .filter(|event| event.value >> 56 == 8 && ((event.value >> 48) & 0xff) == 1)
        .count();
    runtime.shutdown_persistent()?;
    if !runtime.is_observationally_clean() {
        return Err("proposal reconstruction authority did not recover".into());
    }
    match (stop_count, interrupted_count, prompt, response) {
        (0, 0 | 1, prompt, _) => Ok(DurableProposalTurn::Pending { prompt }),
        (1, 0, Some(prompt), Some(response)) => {
            Ok(DurableProposalTurn::Completed { prompt, response })
        }
        (1, 1, Some(prompt), None) => Ok(DurableProposalTurn::Interrupted { prompt }),
        _ => Err("durable proposal turn has an invalid terminal fact sequence".into()),
    }
}

fn command_journal_path(session: &Path) -> PathBuf {
    session.join("command.qj")
}

fn command_fact_path(session: &Path, attempt: u64, kind: u64) -> Result<PathBuf, String> {
    let label = match kind {
        commands::COMMAND_STARTED_KIND => "started",
        commands::COMMAND_FINISHED_KIND => "finished",
        _ => return Err(format!("unsupported command fact kind {kind}")),
    };
    Ok(session.join(format!("command-{attempt}-{label}.json")))
}

fn command_event_value(kind: u64, attempt: u64) -> Result<u64, String> {
    if kind > 0xff || attempt == 0 || attempt >= (1_u64 << 56) {
        return Err("command event value is out of range".into());
    }
    Ok((kind << 56) | attempt)
}

fn append_command_fact(
    session: &Path,
    kind: u64,
    attempt: u64,
    bytes: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let fixtures = PathBuf::from(env!("QUARTZ_FIXTURE_DIR"));
    let journal = command_journal_path(session);
    let events = journal.with_extension("qe");
    let fact = command_fact_path(session, attempt, kind)?;
    commands::materialize_fact(&fact, bytes)?;
    let value = command_event_value(kind, attempt)?;
    let mut runtime = Runtime::open_persistent(
        proposal_limits(),
        ComponentSpec::new("event-store", artifact(&fixtures, "event-store"))
            .with_journal_paths(vec![journal])
            .with_event_stream_paths(vec![events]),
    )?;
    runtime.apply_tree(ComponentTree {
        roots: vec![
            ComponentSpec::new("command-fact", artifact(&fixtures, "candidate-appender"))
                .with_config(value)
                .with_event_grants(vec![EventGrant::new(
                    commands::COMMAND_EVENT_NAMESPACE,
                    commands::COMMAND_EVENT_NAME,
                    commands::COMMAND_EVENT_REVISION,
                )])
                .with_snapshot_grants(vec![SnapshotGrant::from_file(
                    &fact,
                    format!("Command fact {kind} attempt {attempt}"),
                )?]),
        ],
    })?;
    let matching = runtime
        .events()
        .into_iter()
        .filter(|event| event.value == value)
        .collect::<Vec<_>>();
    if matching.len() != 1
        || matching[0]
            .payload
            .as_ref()
            .is_none_or(|payload| payload.bytes != bytes)
    {
        return Err("approved command fact did not commit exactly once".into());
    }
    runtime.shutdown_persistent()?;
    if !runtime.is_observationally_clean() {
        return Err("approved command fact authority did not recover".into());
    }
    Ok(())
}

fn read_command_history(
    session: &Path,
) -> Result<Option<CommandHistory>, Box<dyn std::error::Error>> {
    let journal = command_journal_path(session);
    if !journal.exists() {
        return Ok(None);
    }
    let fixtures = PathBuf::from(env!("QUARTZ_FIXTURE_DIR"));
    let events = journal.with_extension("qe");
    let mut runtime = Runtime::open_persistent(
        proposal_limits(),
        ComponentSpec::new("event-store", artifact(&fixtures, "event-store"))
            .with_journal_paths(vec![journal])
            .with_event_stream_paths(vec![events]),
    )?;
    let records = runtime.events();
    runtime.shutdown_persistent()?;
    if !runtime.is_observationally_clean() {
        return Err("approved command reconstruction authority did not recover".into());
    }
    let mut attempts = Vec::new();
    for record in records {
        if record.event
            != EventGrant::new(
                commands::COMMAND_EVENT_NAMESPACE,
                commands::COMMAND_EVENT_NAME,
                commands::COMMAND_EVENT_REVISION,
            )
        {
            return Err("approved command stream contains another event identity".into());
        }
        let kind = record.value >> 56;
        let attempt = record.value & ((1_u64 << 56) - 1);
        let payload = record
            .payload
            .ok_or("approved command event has no durable payload")?
            .bytes;
        commands::materialize_fact(&command_fact_path(session, attempt, kind)?, &payload)?;
        match kind {
            commands::COMMAND_STARTED_KIND => {
                let started = commands::CommandStarted::from_bytes(&payload)?;
                if started.attempt != attempt
                    || attempt != u64::try_from(attempts.len())?.saturating_add(1)
                {
                    return Err("approved command starts have an invalid attempt sequence".into());
                }
                attempts.push(CommandAttemptState::Interrupted(started));
            }
            commands::COMMAND_FINISHED_KIND => {
                let Some(CommandAttemptState::Interrupted(started)) = attempts.last() else {
                    return Err("CommandFinished has no matching CommandStarted".into());
                };
                if started.attempt != attempt {
                    return Err("CommandFinished attempt does not match the latest start".into());
                }
                let finished = commands::CommandFinished::from_bytes(&payload, started)?;
                let started = started.clone();
                *attempts.last_mut().expect("matching start exists") =
                    CommandAttemptState::Finished { started, finished };
            }
            _ => {
                return Err(format!("approved command stream has unknown fact kind {kind}").into());
            }
        }
    }
    if attempts.is_empty() {
        Ok(None)
    } else {
        Ok(Some(CommandHistory { attempts }))
    }
}

fn run_approved_command(
    session: &Path,
    argv: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let session = fs::canonicalize(session)?;
    let state = reconstruct_proposal_session(&session)?;
    if state.is_complete() {
        return Err("proposal session is explicitly complete".into());
    }
    match state.continuations.last() {
        Some(ProposalContinuationState::Pending(_)) => {
            return Err("pending continuation must finish before another command".into());
        }
        Some(ProposalContinuationState::Interrupted(_)) => {
            return Err("interrupted/unknown continuation blocks later commands".into());
        }
        Some(ProposalContinuationState::Completed {
            response: proposals::ContinuationResponse::Complete(_),
            ..
        }) => {
            return Err("proposal session is explicitly complete".into());
        }
        Some(ProposalContinuationState::Completed {
            response: proposals::ContinuationResponse::Proposal { .. },
            ..
        })
        | None => {}
    }
    if let Some(history) = &state.command
        && history.finished_count() != state.continuations.len()
    {
        return Err("latest finished command requires a model continuation".into());
    }
    require_current_proposals_promoted(&session, &state)?;
    let attempt = match &state.command {
        Some(history) => history.next_attempt()?,
        None => 1,
    };
    let repository = repository_root()?;
    let admitted_paths = state
        .admission
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let started = commands::CommandStarted::new(attempt, argv, &repository, &admitted_paths)?;
    let started_bytes = started.to_bytes()?;
    append_command_fact(
        &session,
        commands::COMMAND_STARTED_KIND,
        attempt,
        &started_bytes,
    )?;
    let execution = commands::execute(&started);
    let repository_after = commands::RepositoryIdentity::capture(&repository, &admitted_paths)?;
    let finished = commands::CommandFinished::new(&started, execution, repository_after)?;
    let finished_bytes = finished.to_bytes()?;
    append_command_fact(
        &session,
        commands::COMMAND_FINISHED_KIND,
        attempt,
        &finished_bytes,
    )?;
    let reconstructed = reconstruct_proposal_session(&session)?;
    display_proposals(&session, &reconstructed)?;
    Ok(())
}

fn run_proposal_continuation(
    model: &str,
    session: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let session = fs::canonicalize(session)?;
    let state = reconstruct_proposal_session(&session)?;
    validate_continuation_start(model, &state)?;
    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY is required to start or resume --continue-task")?;
    run_proposal_continuation_with_adapter(
        model,
        &session,
        Arc::new(openai::OpenAiResponses::new(api_key, model.to_owned())?),
    )
}

fn validate_continuation_start(
    model: &str,
    state: &ProposalSession,
) -> Result<(), Box<dyn std::error::Error>> {
    match state.continuations.last() {
        Some(ProposalContinuationState::Pending(request)) => {
            if request.model != model {
                return Err("durable continuation belongs to another model".into());
            }
            return Ok(());
        }
        Some(ProposalContinuationState::Interrupted(request)) => {
            if request.model != model {
                return Err("durable continuation belongs to another model".into());
            }
            return Err(
                "continuation turn ended interrupted/unknown; it will not be retried".into(),
            );
        }
        Some(ProposalContinuationState::Completed {
            response: proposals::ContinuationResponse::Complete(_),
            ..
        }) => return Err("proposal session is explicitly complete".into()),
        Some(ProposalContinuationState::Completed {
            response: proposals::ContinuationResponse::Proposal { .. },
            ..
        })
        | None => {}
    }
    let history = state
        .command
        .as_ref()
        .ok_or("proposal session has no approved command")?;
    history.latest_finished()?;
    if history.finished_count() != state.continuations.len() + 1 {
        return Err("no finished command is awaiting a model continuation".into());
    }
    Ok(())
}

fn run_proposal_continuation_with_adapter(
    model: &str,
    session: &Path,
    adapter: Arc<dyn ExchangeAdapter>,
) -> Result<(), Box<dyn std::error::Error>> {
    let session = fs::canonicalize(session)?;
    let state = reconstruct_proposal_session(&session)?;
    validate_continuation_start(model, &state)?;
    let request = match state.continuations.last() {
        Some(ProposalContinuationState::Pending(request)) => request.clone(),
        _ => {
            let sequence = state.next_continuation_sequence()?;
            let current = state.generations_before_continuation(&session, sequence)?;
            let finished = state
                .command
                .as_ref()
                .ok_or("proposal session has no approved command")?
                .finished(sequence)?;
            proposals::Continuation::new(
                sequence,
                model,
                &state.admission,
                &current,
                finished,
                &repository_root()?,
            )?
        }
    };
    let prompt_bytes = request.prompt_bytes()?;
    let prompt = proposals::continuation_prompt_path(&session, request.sequence);
    proposals::materialize_continuation_prompt(&session, request.sequence, &prompt_bytes)?;
    let journal = proposals::continuation_journal_path(&session, request.sequence);
    let (response, provenance) =
        run_exchange_turn_with_limits(&prompt, &journal, adapter, proposal_limits())?;
    let response = proposals::parse_continuation_response(&response, &request)?;
    proposals::materialize_continuation_response(&session, &request, &response)?;
    let reconstructed = reconstruct_proposal_session(&session)?;
    display_proposals(&session, &reconstructed)?;
    println!("response provenance: {provenance}");
    println!(
        "continuation {} reconstructed; approved command was not rerun",
        request.sequence
    );
    Ok(())
}

fn require_current_proposals_promoted(
    session: &Path,
    state: &ProposalSession,
) -> Result<(), Box<dyn std::error::Error>> {
    for generation in state.current_generations(session)? {
        let current = state.current(session, generation.proposal_index)?;
        if fs::read(&current.path)? != current.proposal.content {
            return Err(format!(
                "proposal {} current candidate cache changed",
                generation.proposal_index
            )
            .into());
        }
        let (journal, mutation) =
            proposal_promotion_paths(session, generation.proposal_index, generation.revision);
        if !journal.is_file() || !mutation.is_file() {
            return Err(format!(
                "proposal {} revision {} has not been explicitly promoted",
                generation.proposal_index, generation.revision
            )
            .into());
        }
        let source = proposals::resolve_source(&repository_root()?, &current.proposal.path)?;
        if digest(&fs::read(source)?) != current.proposal.result_sha256 {
            return Err(format!(
                "proposal {} promoted source no longer matches its current generation",
                generation.proposal_index
            )
            .into());
        }
    }
    Ok(())
}

fn proposal_promotion_paths(session: &Path, index: usize, revision: u32) -> (PathBuf, PathBuf) {
    if revision < 2 {
        (
            session.join(format!("promotion-{index}.qj")),
            session.join(format!("promotion-{index}.qm")),
        )
    } else {
        (
            session.join(format!("promotion-{index}-revision-{revision}.qj")),
            session.join(format!("promotion-{index}-revision-{revision}.qm")),
        )
    }
}

fn proposal_operation(index: usize, revision: u32) -> Result<u64, Box<dyn std::error::Error>> {
    let index = u64::try_from(index)?
        .checked_add(1)
        .ok_or("proposal operation overflow")?;
    if revision < 2 {
        Ok(index)
    } else {
        Ok((u64::from(revision)
            .checked_add(1)
            .ok_or("proposal revision overflow")?
            << 32)
            | index)
    }
}

fn run_proposal_promotion(session: &Path, index: usize) -> Result<(), Box<dyn std::error::Error>> {
    let session = fs::canonicalize(session)?;
    let state = reconstruct_proposal_session(&session)?;
    if state.is_complete() {
        return Err("proposal session is explicitly complete".into());
    }
    let current = state.current(&session, index)?;
    let candidate = current.proposal;
    let candidate_path = current.path;
    let repository_root = repository_root()?;
    let source = proposals::resolve_source(&repository_root, &candidate.path)?;
    if fs::read(&candidate_path)? != candidate.content {
        return Err(format!("proposal candidate {index} changed before approval").into());
    }
    let (journal, mutation) = proposal_promotion_paths(&session, index, current.revision);
    let live = fs::read(&source)?;
    let live_digest = digest(&live);
    if live_digest != candidate.before_sha256
        && !(live_digest == candidate.result_sha256 && journal.exists() && mutation.exists())
    {
        return Err(format!(
            "source `{}` drifted before proposal {index} promotion",
            candidate.path
        )
        .into());
    }
    let operation = proposal_operation(index, current.revision)?;
    let fixtures = PathBuf::from(env!("QUARTZ_FIXTURE_DIR"));
    let persistence = || {
        ComponentSpec::new("journal", artifact(&fixtures, "journal"))
            .with_journal_paths(vec![journal.clone()])
    };
    let desired = proposal_promotion_tree(
        &fixtures,
        &source,
        &candidate_path,
        &mutation,
        operation,
        candidate,
    )?;

    let mut runtime = Runtime::open_persistent(proposal_limits(), persistence())?;
    runtime.apply_tree(desired)?;
    active_id(&runtime, "root/promoter")?;
    if fs::read(&source)? != candidate.content {
        return Err(format!("proposal {index} promotion produced different bytes").into());
    }
    drop(runtime);

    let mut restarted = Runtime::open_persistent(proposal_limits(), persistence())?;
    active_id(&restarted, "root/promoter")?;
    if fs::read(&source)? != candidate.content {
        return Err(format!("proposal {index} changed after restart").into());
    }
    restarted.shutdown_persistent()?;
    if !restarted.is_observationally_clean() || fs::read(&source)? != candidate.content {
        return Err(format!("proposal {index} did not retain a clean promotion").into());
    }
    println!(
        "proposal {index} promoted: {} result_sha256={} restart=verified context=clean",
        candidate.path, candidate.result_sha256
    );
    Ok(())
}

fn proposal_promotion_tree(
    fixtures: &Path,
    source: &Path,
    candidate_path: &Path,
    mutation: &Path,
    operation: u64,
    candidate: &proposals::Proposal,
) -> Result<ComponentTree, Error> {
    Ok(ComponentTree {
        roots: vec![
            ComponentSpec::new("root", artifact(fixtures, "root"))
                .with_config(3)
                .with_children(vec![
                    ComponentSpec::new(
                        "mutation-authority",
                        artifact(fixtures, "mutation-authority"),
                    )
                    .with_config(operation),
                    ComponentSpec::new(
                        "promotion-authority",
                        artifact(fixtures, "promotion-authority-a"),
                    )
                    .with_config(operation),
                    ComponentSpec::new("promoter", artifact(fixtures, "proposal-promoter"))
                        .with_config(operation)
                        .with_snapshot_grants(vec![SnapshotGrant::from_file(
                            candidate_path,
                            format!("approved proposal {} result", candidate.path),
                        )?])
                        .with_workspace_grants(vec![WorkspaceGrant::new(
                            source,
                            mutation,
                            operation,
                            format!("approved proposal {}", candidate.path),
                            candidate.before_sha256.clone(),
                            candidate.result_sha256.clone(),
                            proposals::MAX_SOURCE_BYTES,
                        )?]),
                ]),
        ],
    })
}

fn proposal_limits() -> Limits {
    Limits {
        max_snapshot_bytes: 512 * 1024,
        max_payload_bytes: 512 * 1024,
        max_payload_total_bytes: 2 * 1024 * 1024,
        max_event_record_bytes: 512 * 1024,
        max_exchange_record_bytes: 512 * 1024,
        max_mutation_record_bytes: 512 * 1024,
        ..Limits::default()
    }
}

fn repository_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(fs::canonicalize(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
    )?)
}

fn run_production_model(
    model: &str,
    prompt: &Path,
    journal: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY is required for --production-model")?;
    let adapter = Arc::new(openai::OpenAiResponses::new(api_key, model.to_owned())?);
    let (response, provenance) = run_exchange_turn(prompt, journal, adapter)?;
    println!("{}", std::str::from_utf8(&response)?);
    println!("response provenance: {provenance}");
    println!("production response reconstructed; shutdown clean");
    Ok(())
}

fn run_exchange_acceptance(fixtures: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("quartz-slice6-smoke-{}", std::process::id()));
    fs::create_dir_all(&root)?;
    let prompt = root.join("prompt.txt");
    let journal = root.join("composition.qj");
    fs::write(&prompt, b"Return one bounded smoke response.")?;
    let (response, provenance) = run_exchange_turn(&prompt, &journal, Arc::new(SmokeExchange))?;
    assert_eq!(response, b"bounded production-path response");
    assert_eq!(provenance, "smoke:exchange");
    fs::remove_dir_all(&root)?;
    assert!(fixtures.join("production-agent-provider.wasm").is_file());
    println!("production exchange: exact response reconstructed; authority recovered");
    Ok(())
}

fn run_exchange_turn(
    prompt: &Path,
    journal: &Path,
    adapter: Arc<dyn ExchangeAdapter>,
) -> Result<(Vec<u8>, String), Box<dyn std::error::Error>> {
    run_exchange_turn_with_limits(prompt, journal, adapter, Limits::default())
}

fn run_exchange_turn_with_limits(
    prompt: &Path,
    journal: &Path,
    adapter: Arc<dyn ExchangeAdapter>,
    limits: Limits,
) -> Result<(Vec<u8>, String), Box<dyn std::error::Error>> {
    let fixtures = PathBuf::from(env!("QUARTZ_FIXTURE_DIR"));
    let prompt = fs::canonicalize(prompt)?;
    let prompt_bytes = fs::read(&prompt)?;
    std::str::from_utf8(&prompt_bytes)?;
    if prompt_bytes.len() > limits.max_payload_bytes {
        return Err(format!(
            "production prompt is {} bytes; admitted limit is {}",
            prompt_bytes.len(),
            limits.max_payload_bytes
        )
        .into());
    }
    let events = journal.with_extension("qe");
    let exchange = journal.with_extension("qx");
    let desired = production_tree(
        &fixtures,
        &prompt,
        &exchange,
        adapter.identity(),
        limits.max_payload_bytes,
    );
    let mut completed = None;

    for _ in 0..8 {
        let mut runtime = Runtime::open_persistent_with_exchange(
            limits,
            ComponentSpec::new("event-store", artifact(&fixtures, "event-store"))
                .with_journal_paths(vec![journal.to_path_buf()])
                .with_event_stream_paths(vec![events.clone()]),
            adapter.clone(),
        )?;
        runtime.apply_tree(desired.clone())?;
        if let Some(FiberState::Failed(error)) = runtime.fiber_state("a-loop") {
            return Err(format!("production agent loop failed: {error}").into());
        }
        let records = runtime.events();
        if records.iter().any(|event| event.value >> 56 == 7) {
            let response = records
                .iter()
                .find(|event| event.value >> 56 == 5)
                .and_then(|event| event.payload.clone())
                .ok_or("production turn stopped without a durable response")?;
            completed = Some((response.bytes, response.provenance));
            drop(runtime);
            break;
        }
    }
    let (response, provenance) = completed.ok_or("production turn did not reach a stop fact")?;

    let mut runtime = Runtime::open_persistent_with_exchange(
        limits,
        ComponentSpec::new("event-store", artifact(&fixtures, "event-store"))
            .with_journal_paths(vec![journal.to_path_buf()])
            .with_event_stream_paths(vec![events]),
        adapter,
    )?;
    let reconstructed = runtime
        .events()
        .into_iter()
        .find(|event| event.value >> 56 == 5)
        .and_then(|event| event.payload)
        .ok_or("durable response was not reconstructed")?;
    if reconstructed.bytes != response || reconstructed.provenance != provenance {
        return Err("reconstructed production response changed".into());
    }
    runtime.shutdown_persistent()?;
    if !runtime.is_observationally_clean() {
        return Err("production authority was not fully recovered".into());
    }
    Ok((response, provenance))
}

struct SmokeExchange;

impl ExchangeAdapter for SmokeExchange {
    fn identity(&self) -> &str {
        "smoke-exchange"
    }

    fn exchange(
        &self,
        request: &[u8],
        _timeout: Duration,
        _max_response_bytes: usize,
    ) -> Result<ExchangeResponse, ExchangeFailure> {
        assert_eq!(request, b"Return one bounded smoke response.");
        Ok(ExchangeResponse {
            bytes: b"bounded production-path response".to_vec(),
            provenance: "smoke:exchange".into(),
            usage: 5,
        })
    }
}

struct ReviewedEditExchange;

impl ExchangeAdapter for ReviewedEditExchange {
    fn identity(&self) -> &str {
        "reviewed-edit-exchange"
    }

    fn exchange(
        &self,
        request: &[u8],
        _timeout: Duration,
        _max_response_bytes: usize,
    ) -> Result<ExchangeResponse, ExchangeFailure> {
        assert_eq!(request, b"Return only the complete reviewed file bytes.");
        Ok(ExchangeResponse {
            bytes: b"alpha reviewed by Quartz\n".to_vec(),
            provenance: "smoke:reviewed-edit".into(),
            usage: 7,
        })
    }
}

fn production_tree(
    fixtures: &Path,
    prompt: &Path,
    exchange: &Path,
    adapter: &str,
    max_request_bytes: usize,
) -> ComponentTree {
    let event_grant = || EventGrant::new("quartz.agent", "repository-turn", 2);
    ComponentTree {
        roots: vec![
            ComponentSpec::new("a-loop", artifact(fixtures, "agent-loop"))
                .with_config(1)
                .with_event_grants(vec![event_grant()]),
            ComponentSpec::new("b-gateway", artifact(fixtures, "agent-gateway")),
            ComponentSpec::new(
                "c-provider",
                artifact(fixtures, "production-agent-provider"),
            )
            .with_exchange_grants(vec![ExchangeGrant::new(
                adapter,
                exchange,
                max_request_bytes,
                64 * 1024,
                120_000,
            )]),
            ComponentSpec::new("d-tool", artifact(fixtures, "agent-tool-a")),
            ComponentSpec::new("z-client", artifact(fixtures, "production-agent-client"))
                .with_config(1)
                .with_event_grants(vec![event_grant()])
                .with_snapshot_grants(vec![snapshot_grant(prompt)]),
        ],
    }
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

#[cfg(test)]
mod cli_tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<CliCommand, String> {
        parse_args(args.iter().map(|value| (*value).to_owned()))
    }

    #[test]
    fn empty_and_help_select_the_same_usage() {
        assert_eq!(parse(&[]), Ok(CliCommand::Help));
        assert_eq!(parse(&["--help"]), Ok(CliCommand::Help));
        for command in [
            "--help",
            "--version",
            "--acceptance",
            "--idle",
            "--durable-write",
            "--durable-recover",
            "--durable-verify",
            "--events-write",
            "--events-recover",
            "--events-verify",
            "--agent-start",
            "--agent-resume",
            "--agent-replace",
            "--agent-verify",
            "--repository-edit",
            "--reviewed-edit",
            "--promote-edit",
            "--production-model",
            "--propose",
            "--resume-proposals",
            "--revise-proposal",
            "--promote-proposal",
            "--run-approved-command",
            "--continue-task",
        ] {
            assert!(USAGE.contains(command), "usage omitted {command}");
        }
    }

    #[test]
    fn version_uses_the_package_version() {
        assert_eq!(parse(&["--version"]), Ok(CliCommand::Version));
        assert_eq!(
            version_text(),
            concat!("quartz ", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn acceptance_is_explicit() {
        assert_eq!(parse(&[]), Ok(CliCommand::Help));
        assert_eq!(parse(&["--acceptance"]), Ok(CliCommand::Acceptance));
    }

    #[test]
    fn internal_scenario_commands_keep_their_shapes() {
        assert_eq!(parse(&["--idle"]), Ok(CliCommand::Idle(2_000)));
        assert_eq!(parse(&["--idle", "5"]), Ok(CliCommand::Idle(5)));
        assert_eq!(
            parse(&["--durable-write", "state.qj"]),
            Ok(CliCommand::DurableWrite(PathBuf::from("state.qj")))
        );
        assert_eq!(
            parse(&["--events-verify", "events.qj"]),
            Ok(CliCommand::EventsVerify(PathBuf::from("events.qj")))
        );
        assert_eq!(
            parse(&["--agent-resume", "7", "agent.qj"]),
            Ok(CliCommand::AgentResume(7, PathBuf::from("agent.qj")))
        );
        assert_eq!(
            parse(&["--repository-edit", "workspace"]),
            Ok(CliCommand::RepositoryEdit(PathBuf::from("workspace")))
        );
        assert_eq!(
            parse(&["--production-model", "model", "prompt", "turn.qj"]),
            Ok(CliCommand::ProductionModel {
                model: "model".into(),
                prompt: PathBuf::from("prompt"),
                journal: PathBuf::from("turn.qj"),
            })
        );
        assert_eq!(
            parse(&[
                "--propose",
                "model",
                "task",
                "session",
                "README.md",
                "lode/summary.md",
            ]),
            Ok(CliCommand::Propose {
                model: "model".into(),
                task: PathBuf::from("task"),
                session: PathBuf::from("session"),
                sources: vec![PathBuf::from("README.md"), PathBuf::from("lode/summary.md"),],
            })
        );
        assert_eq!(
            parse(&["--revise-proposal", "model", "session", "1", "feedback.txt",]),
            Ok(CliCommand::ReviseProposal {
                model: "model".into(),
                session: PathBuf::from("session"),
                index: 1,
                feedback: PathBuf::from("feedback.txt"),
            })
        );
        assert_eq!(
            parse(&["--resume-proposals", "session"]),
            Ok(CliCommand::ResumeProposals(PathBuf::from("session")))
        );
        assert_eq!(
            parse(&["--promote-proposal", "session", "1"]),
            Ok(CliCommand::PromoteProposal {
                session: PathBuf::from("session"),
                index: 1,
            })
        );
        assert_eq!(
            parse(&[
                "--run-approved-command",
                "session",
                "--",
                "cargo",
                "test",
                "--workspace",
            ]),
            Ok(CliCommand::RunApprovedCommand {
                session: PathBuf::from("session"),
                argv: vec!["cargo".into(), "test".into(), "--workspace".into()],
            })
        );
        assert_eq!(
            parse(&["--continue-task", "model", "session"]),
            Ok(CliCommand::ContinueTask {
                model: "model".into(),
                session: PathBuf::from("session"),
            })
        );
    }

    #[test]
    fn invalid_argument_shapes_fail_clearly() {
        for (args, message) in [
            (&["--durable-write"][..], "requires <journal>"),
            (
                &["--agent-resume", "bad", "state.qj"][..],
                "invalid expected event count",
            ),
            (
                &["--agent-resume", "7"][..],
                "requires <expected-event-count> <journal>",
            ),
            (
                &["--production-model", "model", "prompt"][..],
                "requires <model> <prompt> <journal>",
            ),
            (
                &["--propose", "model", "task", "session", "one"][..],
                "requires two or three",
            ),
            (
                &[
                    "--propose",
                    "model",
                    "task",
                    "session",
                    "one",
                    "two",
                    "three",
                    "four",
                ][..],
                "requires two or three",
            ),
            (
                &["--revise-proposal", "model", "session", "bad", "feedback"][..],
                "invalid proposal index",
            ),
            (
                &["--revise-proposal", "model", "session", "0"][..],
                "requires <model> <session> <index> <feedback>",
            ),
            (
                &["--promote-proposal", "session", "bad"][..],
                "invalid proposal index",
            ),
            (
                &["--resume-proposals", "session", "extra"][..],
                "unexpected trailing argument",
            ),
            (
                &["--run-approved-command", "session", "cargo", "test"][..],
                "requires `--`",
            ),
            (
                &["--run-approved-command", "session", "--"][..],
                "requires a non-empty executable",
            ),
            (
                &["--continue-task", "model"][..],
                "requires <model> <session>",
            ),
            (&["--idle", "bad"][..], "invalid milliseconds"),
            (&["--help", "extra"][..], "unexpected trailing argument"),
            (&["--unknown"][..], "unknown command"),
        ] {
            let error = parse(args).unwrap_err();
            assert!(
                error.contains(message),
                "`{error}` did not contain `{message}`"
            );
        }
    }
}

#[cfg(test)]
mod proposal_runtime_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_CASE: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn promoter_requires_authority_and_reconstructs_exact_publication() {
        let root = temporary_directory();
        let source = root.join("source.txt");
        let candidate_path = root.join("candidate.txt");
        let denied_ledger = root.join("denied.qm");
        let journal = root.join("promotion.qj");
        let mutation = root.join("promotion.qm");
        let before = b"before\n";
        let result = b"after\n";
        fs::write(&source, before).unwrap();
        fs::write(&candidate_path, result).unwrap();
        let candidate = proposals::Proposal {
            path: "source.txt".into(),
            before_sha256: digest(before),
            result_sha256: digest(result),
            before: before.to_vec(),
            content: result.to_vec(),
        };
        let fixtures = PathBuf::from(env!("QUARTZ_FIXTURE_DIR"));

        let mut denied = proposal_promotion_tree(
            &fixtures,
            &source,
            &candidate_path,
            &denied_ledger,
            1,
            &candidate,
        )
        .unwrap();
        denied.roots[0].children[0].config = 0;
        let mut runtime = Runtime::new(proposal_limits()).unwrap();
        runtime.apply_tree(denied).unwrap();
        assert!(matches!(
            runtime.fiber_state("root/promoter"),
            Some(FiberState::Failed(_))
        ));
        assert_eq!(fs::read(&source).unwrap(), before);
        runtime.apply_tree(ComponentTree::default()).unwrap();
        assert!(runtime.is_observationally_clean());

        let persistence = || {
            ComponentSpec::new("journal", artifact(&fixtures, "journal"))
                .with_journal_paths(vec![journal.clone()])
        };
        let desired = proposal_promotion_tree(
            &fixtures,
            &source,
            &candidate_path,
            &mutation,
            1,
            &candidate,
        )
        .unwrap();
        let mut runtime = Runtime::open_persistent(proposal_limits(), persistence()).unwrap();
        runtime.apply_tree(desired).unwrap();
        assert_eq!(
            runtime.fiber_state("root/promoter"),
            Some(FiberState::Active)
        );
        assert_eq!(fs::read(&source).unwrap(), result);
        drop(runtime);

        let mut restarted = Runtime::open_persistent(proposal_limits(), persistence()).unwrap();
        assert_eq!(
            restarted.fiber_state("root/promoter"),
            Some(FiberState::Active)
        );
        assert_eq!(fs::read(&source).unwrap(), result);
        restarted.shutdown_persistent().unwrap();
        assert!(restarted.is_observationally_clean());
        assert_eq!(fs::read(&source).unwrap(), result);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn revision_turn_reconstructs_current_generation_without_duplicate_exchange() {
        let session = temporary_directory();
        let admission = proposals::Admission {
            task: "revise both files".into(),
            files: vec![
                admitted("alpha.txt", b"alpha\n"),
                admitted("beta.txt", b"beta\n"),
            ],
        };
        let base_prompt = admission.prompt_bytes().unwrap();
        let base_response = serde_json::to_vec(&serde_json::json!({
            "proposals": [
                {
                    "path": "alpha.txt",
                    "before_sha256": admission.files[0].before_sha256,
                    "content": "alpha rejected\n"
                },
                {
                    "path": "beta.txt",
                    "before_sha256": admission.files[1].before_sha256,
                    "content": "beta accepted\n"
                }
            ]
        }))
        .unwrap();
        let base_prompt_path = session.join("admission.prompt");
        fs::write(&base_prompt_path, &base_prompt).unwrap();
        let base_calls = Arc::new(AtomicU64::new(0));
        run_exchange_turn_with_limits(
            &base_prompt_path,
            &session.join("turn.qj"),
            Arc::new(FixedExchange::success(
                "base-revision-test",
                base_prompt,
                base_response,
                base_calls.clone(),
            )),
            proposal_limits(),
        )
        .unwrap();
        let (_, proposals) = reconstruct_base_proposals(&session).unwrap();
        let revision = proposals::Revision::new(
            "test-model",
            b"Use the corrected alpha label.",
            &admission,
            &proposals,
            0,
        )
        .unwrap();
        let revision_prompt = revision.prompt_bytes().unwrap();
        let revision_response = serde_json::to_vec(&serde_json::json!({
            "proposal": {
                "path": "alpha.txt",
                "before_sha256": admission.files[0].before_sha256,
                "content": "alpha corrected\n"
            }
        }))
        .unwrap();
        let revision_prompt_path =
            proposals::materialize_revision_prompt(&session, &revision_prompt).unwrap();
        let revision_calls = Arc::new(AtomicU64::new(0));
        let revision_adapter = Arc::new(FixedExchange::success(
            "correction-revision-test",
            revision_prompt,
            revision_response,
            revision_calls.clone(),
        ));
        let revision_journal = proposals::revision_journal_path(&session);
        run_exchange_turn_with_limits(
            &revision_prompt_path,
            &revision_journal,
            revision_adapter.clone(),
            proposal_limits(),
        )
        .unwrap();
        run_exchange_turn_with_limits(
            &revision_prompt_path,
            &revision_journal,
            revision_adapter,
            proposal_limits(),
        )
        .unwrap();
        assert_eq!(base_calls.load(Ordering::Relaxed), 1);
        assert_eq!(revision_calls.load(Ordering::Relaxed), 1);

        let state = reconstruct_proposal_session(&session).unwrap();
        let current = state.current(&session, 0).unwrap();
        assert_eq!(current.proposal.content, b"alpha corrected\n");
        assert_eq!(fs::read(current.path).unwrap(), current.proposal.content);
        let sibling = state.current(&session, 1).unwrap();
        assert_eq!(sibling.proposal.content, b"beta accepted\n");
        fs::remove_file(proposals::revision_prompt_path(&session)).unwrap();
        fs::remove_file(proposals::revision_candidate_path(&session, 0)).unwrap();
        fs::remove_file(session.join("revision-1.json")).unwrap();
        run_proposal_revision("test-model", &session, 0, &session.join("missing-feedback"))
            .unwrap();
        assert!(proposals::revision_prompt_path(&session).is_file());
        assert!(proposals::revision_candidate_path(&session, 0).is_file());
        assert!(session.join("revision-1.json").is_file());
        fs::remove_dir_all(session).unwrap();
    }

    #[test]
    fn interrupted_revision_exchange_is_terminal_and_never_retried() {
        let root = temporary_directory();
        let prompt = root.join("revision-1.prompt");
        let journal = root.join("revision-1.qj");
        fs::write(&prompt, b"durable rejected proposal feedback").unwrap();
        let calls = Arc::new(AtomicU64::new(0));
        let adapter = Arc::new(FixedExchange::ambiguous(
            "ambiguous-revision-test",
            b"durable rejected proposal feedback".to_vec(),
            calls.clone(),
        ));
        assert!(
            run_exchange_turn_with_limits(&prompt, &journal, adapter.clone(), proposal_limits())
                .is_err()
        );
        assert!(
            run_exchange_turn_with_limits(&prompt, &journal, adapter, proposal_limits()).is_err()
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(matches!(
            read_durable_proposal_turn(&journal).unwrap(),
            DurableProposalTurn::Interrupted { .. }
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_command_result_reconstructs_correction_and_blocks_stale_promotion() {
        let (root, session, sources) = promoted_session("failed-correction");
        run_approved_command(
            &session,
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf validation-failed >&2; exit 7".into(),
            ],
        )
        .unwrap();
        let state = reconstruct_proposal_session(&session).unwrap();
        let history = state.command.as_ref().unwrap();
        let finished = history.latest_finished().unwrap();
        assert_eq!(finished.exit_code, Some(7));
        assert_eq!(finished.stderr.bytes().unwrap(), b"validation-failed");
        let current = state.generations_before_continuation(&session, 1).unwrap();
        let request = proposals::Continuation::new(
            1,
            "test-model",
            &state.admission,
            &current,
            finished,
            &repository_root().unwrap(),
        )
        .unwrap();
        let calls = Arc::new(AtomicU64::new(0));
        run_proposal_continuation_with_adapter(
            "test-model",
            &session,
            Arc::new(FixedExchange::success(
                "failed-command-correction",
                request.prompt_bytes().unwrap(),
                b"PROPOSE 0\nalpha corrected after failure\n".to_vec(),
                calls.clone(),
            )),
        )
        .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let reconstructed = reconstruct_proposal_session(&session).unwrap();
        let corrected = reconstructed.current(&session, 0).unwrap();
        assert_eq!(corrected.revision, 2);
        assert_eq!(
            corrected.proposal.content,
            b"alpha corrected after failure\n"
        );
        assert_eq!(fs::read(&sources[0]).unwrap(), b"alpha proposed\n");
        assert!(session.join("promotion-0.qj").is_file());
        assert!(!session.join("promotion-0-revision-2.qj").exists());
        assert!(
            run_approved_command(
                &session,
                vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
            )
            .is_err()
        );
        run_proposal_promotion(&session, 0).unwrap();
        assert_eq!(
            fs::read(&sources[0]).unwrap(),
            b"alpha corrected after failure\n"
        );
        assert!(session.join("promotion-0-revision-2.qj").is_file());
        assert!(session.join("promotion-0-revision-2.qm").is_file());
        run_approved_command(
            &session,
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf validation-passed".into(),
            ],
        )
        .unwrap();
        let after_second_command = reconstruct_proposal_session(&session).unwrap();
        let history = after_second_command.command.as_ref().unwrap();
        assert_eq!(history.attempts.len(), 2);
        let second_finished = history.latest_finished().unwrap();
        assert!(second_finished.succeeded());
        let current = after_second_command
            .generations_before_continuation(&session, 2)
            .unwrap();
        assert!(
            proposals::Continuation::from_prompt(
                2,
                &request.prompt_bytes().unwrap(),
                &after_second_command.admission,
                &current,
                second_finished,
            )
            .is_err()
        );
        let completion = proposals::Continuation::new(
            2,
            "test-model",
            &after_second_command.admission,
            &current,
            second_finished,
            &repository_root().unwrap(),
        )
        .unwrap();
        run_proposal_continuation_with_adapter(
            "test-model",
            &session,
            Arc::new(FixedExchange::success(
                "second-command-complete",
                completion.prompt_bytes().unwrap(),
                b"COMPLETE\nCorrection validated successfully.\n".to_vec(),
                Arc::new(AtomicU64::new(0)),
            )),
        )
        .unwrap();
        for (attempt, kind) in [
            (1, commands::COMMAND_STARTED_KIND),
            (1, commands::COMMAND_FINISHED_KIND),
            (2, commands::COMMAND_STARTED_KIND),
            (2, commands::COMMAND_FINISHED_KIND),
        ] {
            fs::remove_file(command_fact_path(&session, attempt, kind).unwrap()).unwrap();
        }
        fs::remove_file(proposals::continuation_prompt_path(&session, 1)).unwrap();
        fs::remove_file(session.join("continuation-1.json")).unwrap();
        fs::remove_file(proposals::continuation_candidate_path(&session, 0, 2)).unwrap();
        fs::remove_file(proposals::continuation_prompt_path(&session, 2)).unwrap();
        fs::remove_file(session.join("continuation-2.json")).unwrap();
        fs::remove_file(proposals::completion_summary_path(&session, 2)).unwrap();
        let complete = reconstruct_proposal_session(&session).unwrap();
        assert!(complete.is_complete());
        assert_eq!(complete.continuations.len(), 2);
        assert!(matches!(
            complete.continuations[0],
            ProposalContinuationState::Completed {
                response: proposals::ContinuationResponse::Proposal { revision: 2, .. },
                ..
            }
        ));
        assert!(matches!(
            complete.continuations[1],
            ProposalContinuationState::Completed {
                response: proposals::ContinuationResponse::Complete(_),
                ..
            }
        ));
        assert!(
            run_approved_command(
                &session,
                vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
            )
            .is_err()
        );
        assert!(run_proposal_continuation("test-model", &session).is_err());
        assert!(run_proposal_promotion(&session, 0).is_err());
        assert!(run_proposal_revision("test-model", &session, 0, &root.join("unused")).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn successful_command_restarts_then_requires_explicit_complete() {
        let (root, session, _) = promoted_session("successful-complete");
        run_approved_command(
            &session,
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf validation-passed".into(),
            ],
        )
        .unwrap();
        let restarted = reconstruct_proposal_session(&session).unwrap();
        let finished = restarted
            .command
            .as_ref()
            .unwrap()
            .latest_finished()
            .unwrap();
        assert!(finished.succeeded());
        assert_eq!(finished.stdout.bytes().unwrap(), b"validation-passed");
        assert!(restarted.continuations.is_empty());
        let current = restarted
            .generations_before_continuation(&session, 1)
            .unwrap();
        let request = proposals::Continuation::new(
            1,
            "test-model",
            &restarted.admission,
            &current,
            finished,
            &repository_root().unwrap(),
        )
        .unwrap();
        let calls = Arc::new(AtomicU64::new(0));
        run_proposal_continuation_with_adapter(
            "test-model",
            &session,
            Arc::new(FixedExchange::success(
                "successful-command-complete",
                request.prompt_bytes().unwrap(),
                b"COMPLETE\nApproved command passed; task complete.\n".to_vec(),
                calls.clone(),
            )),
        )
        .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        fs::remove_file(proposals::continuation_prompt_path(&session, 1)).unwrap();
        fs::remove_file(proposals::completion_summary_path(&session, 1)).unwrap();
        fs::remove_file(session.join("continuation-1.json")).unwrap();
        let reconstructed = reconstruct_proposal_session(&session).unwrap();
        assert!(proposals::continuation_prompt_path(&session, 1).is_file());
        assert!(proposals::completion_summary_path(&session, 1).is_file());
        assert!(matches!(
            reconstructed.continuations.last(),
            Some(ProposalContinuationState::Completed {
                response: proposals::ContinuationResponse::Complete(_),
                ..
            })
        ));
        assert!(run_proposal_continuation("test-model", &session).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupted_command_reconstruction_never_spawns_without_new_approval() {
        let (root, session, _) = promoted_session("interrupted-command");
        let marker = root.join("must-not-exist");
        let state = reconstruct_proposal_session(&session).unwrap();
        let paths = state
            .admission
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let started = commands::CommandStarted::new(
            1,
            vec![
                "/bin/sh".into(),
                "-c".into(),
                format!("touch {}", marker.display()),
            ],
            &repository_root().unwrap(),
            &paths,
        )
        .unwrap();
        append_command_fact(
            &session,
            commands::COMMAND_STARTED_KIND,
            1,
            &started.to_bytes().unwrap(),
        )
        .unwrap();
        for _ in 0..2 {
            let restarted = reconstruct_proposal_session(&session).unwrap();
            assert!(matches!(
                restarted.command.as_ref().unwrap().attempts.last(),
                Some(CommandAttemptState::Interrupted(_))
            ));
        }
        assert!(run_proposal_continuation("test-model", &session).is_err());
        assert!(!marker.exists());
        run_approved_command(
            &session,
            vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
        )
        .unwrap();
        let renewed = reconstruct_proposal_session(&session).unwrap();
        assert_eq!(renewed.command.as_ref().unwrap().attempts.len(), 2);
        assert!(matches!(
            renewed.command.as_ref().unwrap().attempts[0],
            CommandAttemptState::Interrupted(_)
        ));
        assert!(matches!(
            renewed.command.as_ref().unwrap().attempts[1],
            CommandAttemptState::Finished { .. }
        ));
        let current = renewed
            .generations_before_continuation(&session, 1)
            .unwrap();
        let continuation = proposals::Continuation::new(
            1,
            "test-model",
            &renewed.admission,
            &current,
            renewed.command.as_ref().unwrap().latest_finished().unwrap(),
            &repository_root().unwrap(),
        )
        .unwrap();
        assert_eq!(continuation.sequence, 1);
        assert_eq!(continuation.command.attempt, 2);
        assert!(!marker.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupted_later_continuation_is_terminal_and_never_retried() {
        let (root, session, _) = promoted_session("interrupted-later-continuation");
        run_approved_command(
            &session,
            vec!["/bin/sh".into(), "-c".into(), "exit 7".into()],
        )
        .unwrap();
        let failed = reconstruct_proposal_session(&session).unwrap();
        let failed_result = failed.command.as_ref().unwrap().latest_finished().unwrap();
        let current = failed.generations_before_continuation(&session, 1).unwrap();
        let correction = proposals::Continuation::new(
            1,
            "test-model",
            &failed.admission,
            &current,
            failed_result,
            &repository_root().unwrap(),
        )
        .unwrap();
        run_proposal_continuation_with_adapter(
            "test-model",
            &session,
            Arc::new(FixedExchange::success(
                "first-cycle-correction",
                correction.prompt_bytes().unwrap(),
                b"PROPOSE 0\nalpha corrected before interruption\n".to_vec(),
                Arc::new(AtomicU64::new(0)),
            )),
        )
        .unwrap();
        run_proposal_promotion(&session, 0).unwrap();
        run_approved_command(
            &session,
            vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
        )
        .unwrap();
        let second = reconstruct_proposal_session(&session).unwrap();
        let second_result = second.command.as_ref().unwrap().latest_finished().unwrap();
        let current = second.generations_before_continuation(&session, 2).unwrap();
        let continuation = proposals::Continuation::new(
            2,
            "test-model",
            &second.admission,
            &current,
            second_result,
            &repository_root().unwrap(),
        )
        .unwrap();
        let calls = Arc::new(AtomicU64::new(0));
        let adapter = Arc::new(FixedExchange::ambiguous(
            "interrupted-second-continuation",
            continuation.prompt_bytes().unwrap(),
            calls.clone(),
        ));
        assert!(
            run_proposal_continuation_with_adapter("test-model", &session, adapter.clone())
                .is_err()
        );
        assert!(run_proposal_continuation_with_adapter("test-model", &session, adapter).is_err());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let restarted = reconstruct_proposal_session(&session).unwrap();
        assert_eq!(restarted.continuations.len(), 2);
        assert!(matches!(
            restarted.continuations[1],
            ProposalContinuationState::Interrupted(ref request) if request.sequence == 2
        ));
        assert!(
            run_approved_command(
                &session,
                vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
            )
            .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn promoted_session(label: &str) -> (PathBuf, PathBuf, [PathBuf; 2]) {
        let repository = repository_root().unwrap();
        let root = repository.join(".quartz").join(format!(
            "loop-test-{label}-{}-{}",
            std::process::id(),
            NEXT_CASE.fetch_add(1, Ordering::Relaxed)
        ));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        let session = root.join("session");
        fs::create_dir_all(&session).unwrap();
        let sources = [root.join("alpha.txt"), root.join("beta.txt")];
        fs::write(&sources[0], b"alpha original\n").unwrap();
        fs::write(&sources[1], b"beta original\n").unwrap();
        let paths = sources
            .iter()
            .map(|path| {
                path.strip_prefix(&repository)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        let admission = proposals::Admission {
            task: "edit both files and validate them".into(),
            files: vec![
                admitted(&paths[0], b"alpha original\n"),
                admitted(&paths[1], b"beta original\n"),
            ],
        };
        let prompt = admission.prompt_bytes().unwrap();
        let response = serde_json::to_vec(&serde_json::json!({
            "proposals": [
                {
                    "path": paths[0],
                    "before_sha256": admission.files[0].before_sha256,
                    "content": "alpha proposed\n"
                },
                {
                    "path": paths[1],
                    "before_sha256": admission.files[1].before_sha256,
                    "content": "beta proposed\n"
                }
            ]
        }))
        .unwrap();
        let prompt_path = session.join("admission.prompt");
        fs::write(&prompt_path, &prompt).unwrap();
        run_exchange_turn_with_limits(
            &prompt_path,
            &session.join("turn.qj"),
            Arc::new(FixedExchange::success(
                "promoted-session-base",
                prompt,
                response,
                Arc::new(AtomicU64::new(0)),
            )),
            proposal_limits(),
        )
        .unwrap();
        reconstruct_proposal_session(&session).unwrap();
        run_proposal_promotion(&session, 0).unwrap();
        run_proposal_promotion(&session, 1).unwrap();
        (root, session, sources)
    }

    struct FixedExchange {
        identity: &'static str,
        expected: Vec<u8>,
        response: Result<Vec<u8>, ExchangeFailure>,
        calls: Arc<AtomicU64>,
    }

    impl FixedExchange {
        fn success(
            identity: &'static str,
            expected: Vec<u8>,
            response: Vec<u8>,
            calls: Arc<AtomicU64>,
        ) -> Self {
            Self {
                identity,
                expected,
                response: Ok(response),
                calls,
            }
        }

        fn ambiguous(identity: &'static str, expected: Vec<u8>, calls: Arc<AtomicU64>) -> Self {
            Self {
                identity,
                expected,
                response: Err(ExchangeFailure::Ambiguous),
                calls,
            }
        }
    }

    impl ExchangeAdapter for FixedExchange {
        fn identity(&self) -> &str {
            self.identity
        }

        fn exchange(
            &self,
            request: &[u8],
            _timeout: Duration,
            _max_response_bytes: usize,
        ) -> Result<ExchangeResponse, ExchangeFailure> {
            assert_eq!(request, self.expected);
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.response.clone().map(|bytes| ExchangeResponse {
                provenance: format!("test:{}", self.identity),
                bytes,
                usage: 1,
            })
        }
    }

    fn admitted(path: &str, content: &[u8]) -> proposals::AdmittedFile {
        proposals::AdmittedFile {
            path: path.into(),
            before_sha256: digest(content),
            content: content.to_vec(),
        }
    }

    fn temporary_directory() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "quartz-proposal-runtime-{}-{}",
            std::process::id(),
            NEXT_CASE.fetch_add(1, Ordering::Relaxed)
        ));
        if path.exists() {
            fs::remove_dir_all(&path).unwrap();
        }
        fs::create_dir_all(&path).unwrap();
        path
    }
}
