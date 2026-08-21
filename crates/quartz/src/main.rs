mod commands;
mod openai;
mod proposals;
mod session;

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
  --propose <model> <task> <session-dir> <source> <source> [source ...]
  --resume-proposals <session-dir>
  --revise-proposal <model> <session-dir> <index> <feedback>
  --promote-proposal <session-dir> <index>
  --run-approved-command <session-dir> -- <executable> [arg ...]
  --continue-task <model> <session-dir>
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
                "<model> <task> <session> <source> <source> [source ...]",
            )?;
            let task = path_arg(
                &mut args,
                command,
                "<model> <task> <session> <source> <source> [source ...]",
            )?;
            let session = path_arg(
                &mut args,
                command,
                "<model> <task> <session> <source> <source> [source ...]",
            )?;
            let sources: Vec<PathBuf> = args.by_ref().map(PathBuf::from).collect();
            if sources.len() < 2 || sources.iter().any(|path| path.as_os_str().is_empty()) {
                return Err(
                    "`--propose` requires at least two non-empty <source> paths; try `quartz --help`"
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
    let prompt_bytes = admission.prompt_bytes()?;
    let prompt_text = String::from_utf8(prompt_bytes.clone())?;
    let prompt = session.join("admission.prompt");
    let journal = session.join("turn.qj");
    fs::write(&prompt, &prompt_bytes)?;

    let api_key =
        std::env::var("OPENAI_API_KEY").map_err(|_| "OPENAI_API_KEY is required for --propose")?;
    let adapter = Arc::new(openai::OpenAiResponses::new(api_key, model.to_owned())?);
    let mut session_log = session::SessionLog::open(&session)?;
    if !session_log.facts().is_empty() {
        return Err("proposal session already has durable facts".into());
    }
    let prompt_sha256 = session::sha256(&prompt_bytes);
    session_log.append(session::SessionFact::ModelStarted {
        turn: session::ModelTurn::Initial,
        model: model.to_owned(),
        prompt_sha256: prompt_sha256.clone(),
        prompt: prompt_text,
    })?;
    let (response, provenance) =
        run_exchange_turn_with_limits(&prompt, &journal, adapter, proposal_limits())?;
    let response = String::from_utf8(response)?;
    session_log.append(session::SessionFact::ModelCompleted {
        turn: session::ModelTurn::Initial,
        prompt_sha256,
        response_sha256: session::sha256(response.as_bytes()),
        response,
        provenance: provenance.clone(),
    })?;
    let state = reconstruct_proposal_session(&session)?;
    display_proposals(&session, &state)?;
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
    match &state.revision {
        Some(ProposalRevisionState::Completed { request, .. }) => {
            validate_revision_selector(request, model, index)?;
            display_proposals(&session, &state)?;
            println!("revision turn reconstructed; no exchange emitted");
            return Ok(());
        }
        Some(ProposalRevisionState::Interrupted(request)) => {
            validate_revision_selector(request, model, index)?;
            return Err("revision turn ended interrupted/unknown; it will not be retried".into());
        }
        _ => {}
    }

    let feedback_bytes = fs::read(feedback)?;
    let expected = match &state.revision {
        Some(ProposalRevisionState::Pending(request)) => {
            validate_revision_selector(request, model, index)?;
            if request.feedback.as_bytes() != feedback_bytes {
                return Err("durable rejection feedback changed".into());
            }
            request.clone()
        }
        None => proposals::Revision::new(
            model,
            &feedback_bytes,
            &state.admission,
            &state.proposals,
            index,
        )?,
        Some(ProposalRevisionState::Interrupted(_))
        | Some(ProposalRevisionState::Completed { .. }) => unreachable!("handled above"),
    };
    let prompt_bytes = expected.prompt_bytes()?;
    let prompt_text = String::from_utf8(prompt_bytes.clone())?;
    let prompt_path = proposals::materialize_revision_prompt(&session, &prompt_bytes)?;
    let journal = proposals::revision_journal_path(&session);

    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY is required to start or resume --revise-proposal")?;
    let adapter = Arc::new(openai::OpenAiResponses::new(api_key, model.to_owned())?);
    let mut session_log = session::SessionLog::open(&session)?;
    if state.revision.is_none() {
        session_log.append(session::SessionFact::ProposalRejected {
            proposal_index: index,
            revision: 0,
            candidate_sha256: state
                .current(&session, index)?
                .proposal
                .result_sha256
                .clone(),
            model: model.to_owned(),
            feedback: expected.feedback.clone(),
        })?;
    }
    let prompt_sha256 = session::sha256(&prompt_bytes);
    session_log.append(session::SessionFact::ModelStarted {
        turn: session::ModelTurn::Revision {
            proposal_index: index,
            revision: 1,
        },
        model: model.to_owned(),
        prompt_sha256: prompt_sha256.clone(),
        prompt: prompt_text,
    })?;
    let (response, provenance) =
        run_exchange_turn_with_limits(&prompt_path, &journal, adapter, proposal_limits())?;
    let response = String::from_utf8(response)?;
    session_log.append(session::SessionFact::ModelCompleted {
        turn: session::ModelTurn::Revision {
            proposal_index: index,
            revision: 1,
        },
        prompt_sha256,
        response_sha256: session::sha256(response.as_bytes()),
        response,
        provenance: provenance.clone(),
    })?;
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
    promotions: Vec<ProposalPromotionState>,
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
    Interrupted(proposals::Continuation),
    Completed {
        request: proposals::Continuation,
        response: proposals::ContinuationResponse,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ProposalPromotionState {
    Approved {
        proposal_index: usize,
        revision: u32,
        candidate_sha256: String,
    },
    Interrupted {
        proposal_index: usize,
        revision: u32,
        candidate_sha256: String,
        operation: u64,
    },
    Promoted {
        proposal_index: usize,
        revision: u32,
        candidate_sha256: String,
        operation: u64,
    },
}

struct CommandHistory {
    attempts: Vec<CommandAttemptState>,
}

#[derive(Clone)]
enum CommandAttemptState {
    Interrupted(commands::CommandStarted),
    Finished {
        started: commands::CommandStarted,
        finished: commands::CommandFinished,
    },
}

struct CurrentProposal<'a> {
    proposal_index: usize,
    proposal: &'a proposals::Proposal,
    path: PathBuf,
    revision: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PromotionStatus {
    Absent,
    Approved,
    Interrupted,
    Promoted,
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
                    proposal_index: index,
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
                ProposalContinuationState::Interrupted(_) => {
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
                    proposal_index: index,
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
                proposal_index: index,
                proposal: original,
                path: proposals::candidate_path(session, index),
                revision: 0,
            }),
        }
    }

    fn promotion_status(&self, current: &CurrentProposal<'_>) -> PromotionStatus {
        self.promotions
            .iter()
            .rev()
            .find_map(|state| match state {
                ProposalPromotionState::Approved {
                    proposal_index,
                    revision,
                    candidate_sha256,
                } if *proposal_index == current.proposal_index
                    && *revision == current.revision
                    && *candidate_sha256 == current.proposal.result_sha256 =>
                {
                    Some(PromotionStatus::Approved)
                }
                ProposalPromotionState::Interrupted {
                    proposal_index,
                    revision,
                    candidate_sha256,
                    ..
                } if *proposal_index == current.proposal_index
                    && *revision == current.revision
                    && *candidate_sha256 == current.proposal.result_sha256 =>
                {
                    Some(PromotionStatus::Interrupted)
                }
                ProposalPromotionState::Promoted {
                    proposal_index,
                    revision,
                    candidate_sha256,
                    ..
                } if *proposal_index == current.proposal_index
                    && *revision == current.revision
                    && *candidate_sha256 == current.proposal.result_sha256 =>
                {
                    Some(PromotionStatus::Promoted)
                }
                _ => None,
            })
            .unwrap_or(PromotionStatus::Absent)
    }
}

#[cfg(test)]
fn reconstruct_base_proposals(
    session: &Path,
) -> Result<(proposals::Admission, Vec<proposals::Proposal>), Box<dyn std::error::Error>> {
    let log = session::SessionLog::open(session)?;
    reconstruct_base_proposals_from_facts(session, log.facts())
}

fn reconstruct_base_proposals_from_facts(
    session: &Path,
    facts: &[session::RecordedFact],
) -> Result<(proposals::Admission, Vec<proposals::Proposal>), Box<dyn std::error::Error>> {
    let Some(started) = facts.first() else {
        return Err("proposal session has no durable facts".into());
    };
    let (prompt, prompt_sha256) = match &started.fact {
        session::SessionFact::ModelStarted {
            turn: session::ModelTurn::Initial,
            model,
            prompt_sha256,
            prompt,
        } => {
            if model.is_empty() {
                return Err("initial proposal model is empty".into());
            }
            validate_session_text(prompt, prompt_sha256, "initial proposal prompt")?;
            (prompt.as_bytes(), prompt_sha256)
        }
        _ => return Err("session fact 1 is not an initial proposal start".into()),
    };
    let Some(completed) = facts.get(1) else {
        return Err("initial proposal turn ended interrupted/unknown".into());
    };
    let response = match &completed.fact {
        session::SessionFact::ModelCompleted {
            turn: session::ModelTurn::Initial,
            prompt_sha256: completed_prompt,
            response_sha256,
            response,
            provenance,
        } => {
            if completed_prompt != prompt_sha256 || provenance.is_empty() {
                return Err("initial proposal completion does not bind its start".into());
            }
            validate_session_text(response, response_sha256, "initial proposal response")?;
            response.as_bytes()
        }
        _ => return Err("initial proposal start has no matching completion".into()),
    };
    let admission = proposals::Admission::from_prompt(prompt)?;
    let candidates = proposals::parse_response(response, &admission)?;
    proposals::materialize(session, &candidates)?;
    Ok((admission, candidates))
}

fn reconstruct_proposal_session(
    session: &Path,
) -> Result<ProposalSession, Box<dyn std::error::Error>> {
    let session = fs::canonicalize(session)?;
    let log = session::SessionLog::open(&session)?;
    let (admission, candidates) = reconstruct_base_proposals_from_facts(&session, log.facts())?;
    let mut state = ProposalSession {
        admission,
        proposals: candidates,
        revision: None,
        promotions: Vec::new(),
        command: None,
        continuations: Vec::new(),
    };
    let mut cursor = 2;
    while let Some(record) = log.facts().get(cursor) {
        if let Some(ProposalRevisionState::Pending(expected)) = &state.revision {
            let expected = expected.clone();
            let request = match &record.fact {
                session::SessionFact::ModelStarted {
                    turn:
                        session::ModelTurn::Revision {
                            proposal_index,
                            revision,
                        },
                    model,
                    prompt_sha256,
                    prompt,
                } if *proposal_index == expected.proposal_index && *revision == 1 => {
                    validate_session_text(prompt, prompt_sha256, "revision prompt")?;
                    let request = proposals::Revision::from_prompt(
                        prompt.as_bytes(),
                        &state.admission,
                        &state.proposals,
                    )?;
                    validate_revision_selector(&request, model, *proposal_index)?;
                    if request != expected {
                        return Err("revision start changed the durable rejection".into());
                    }
                    proposals::materialize_revision_prompt(&session, prompt.as_bytes())?;
                    request
                }
                _ => return Err("proposal rejection is not followed by its revision start".into()),
            };
            state.revision = Some(ProposalRevisionState::Interrupted(request));
            cursor += 1;
            continue;
        }
        if let Some(ProposalRevisionState::Interrupted(expected)) = &state.revision {
            let expected = expected.clone();
            let prompt = expected.prompt_bytes()?;
            let prompt_sha256 = session::sha256(&prompt);
            match &record.fact {
                session::SessionFact::ModelCompleted {
                    turn:
                        session::ModelTurn::Revision {
                            proposal_index,
                            revision,
                        },
                    prompt_sha256: completed_prompt,
                    response_sha256,
                    response,
                    provenance,
                } if *proposal_index == expected.proposal_index && *revision == 1 => {
                    if *completed_prompt != prompt_sha256 || provenance.is_empty() {
                        return Err("revision completion does not bind its start".into());
                    }
                    validate_session_text(response, response_sha256, "revision response")?;
                    let proposal =
                        proposals::parse_revision_response(response.as_bytes(), &expected)?;
                    proposals::materialize_revision(&session, &expected, &proposal)?;
                    state.revision = Some(ProposalRevisionState::Completed {
                        request: expected,
                        proposal,
                    });
                    cursor += 1;
                    continue;
                }
                _ => return Err("interrupted revision is followed by another session fact".into()),
            }
        }
        if let Some(ProposalPromotionState::Approved {
            proposal_index,
            revision,
            candidate_sha256,
        }) = state.promotions.last()
        {
            match &record.fact {
                session::SessionFact::PromotionStarted {
                    proposal_index: started_index,
                    revision: started_revision,
                    candidate_sha256: started_candidate,
                    operation,
                } if started_index == proposal_index
                    && started_revision == revision
                    && started_candidate == candidate_sha256
                    && *operation == proposal_operation(*proposal_index, *revision)? =>
                {
                    state.promotions.push(ProposalPromotionState::Interrupted {
                        proposal_index: *proposal_index,
                        revision: *revision,
                        candidate_sha256: candidate_sha256.clone(),
                        operation: *operation,
                    });
                    cursor += 1;
                    continue;
                }
                _ => return Err("proposal approval is not followed by its promotion start".into()),
            }
        }
        if let Some(ProposalPromotionState::Interrupted {
            proposal_index,
            revision,
            candidate_sha256,
            operation,
        }) = state.promotions.last()
        {
            match &record.fact {
                session::SessionFact::ProposalPromoted {
                    proposal_index: completed_index,
                    revision: completed_revision,
                    candidate_sha256: completed_candidate,
                    operation: completed_operation,
                } if completed_index == proposal_index
                    && completed_revision == revision
                    && completed_candidate == candidate_sha256
                    && completed_operation == operation =>
                {
                    state.promotions.push(ProposalPromotionState::Promoted {
                        proposal_index: *proposal_index,
                        revision: *revision,
                        candidate_sha256: candidate_sha256.clone(),
                        operation: *operation,
                    });
                    cursor += 1;
                    continue;
                }
                _ => return Err("interrupted promotion is followed by another session fact".into()),
            }
        }
        if let Some(CommandAttemptState::Interrupted(started)) = state
            .command
            .as_ref()
            .and_then(|history| history.attempts.last())
            .cloned()
        {
            match &record.fact {
                session::SessionFact::CommandFinished {
                    attempt,
                    start_sha256,
                    payload_sha256,
                    payload,
                } if *attempt == started.attempt && *start_sha256 == started.sha256()? => {
                    validate_session_text(payload, payload_sha256, "CommandFinished payload")?;
                    let finished =
                        commands::CommandFinished::from_bytes(payload.as_bytes(), &started)?;
                    let history = state.command.as_mut().expect("history checked above");
                    let last = history.attempts.last_mut().expect("attempt checked above");
                    *last = CommandAttemptState::Finished { started, finished };
                    cursor += 1;
                    continue;
                }
                _ => {
                    return Err(
                        "interrupted approved command is followed by another session fact".into(),
                    );
                }
            }
        }
        if let Some(ProposalContinuationState::Interrupted(expected)) = state.continuations.last() {
            let expected = expected.clone();
            let prompt = expected.prompt_bytes()?;
            let prompt_sha256 = session::sha256(&prompt);
            match &record.fact {
                session::SessionFact::ModelCompleted {
                    turn: session::ModelTurn::Continuation { sequence },
                    prompt_sha256: completed_prompt,
                    response_sha256,
                    response,
                    provenance,
                } if *sequence == expected.sequence => {
                    if *completed_prompt != prompt_sha256 || provenance.is_empty() {
                        return Err("continuation completion does not bind its start".into());
                    }
                    validate_session_text(response, response_sha256, "continuation response")?;
                    let response =
                        proposals::parse_continuation_response(response.as_bytes(), &expected)?;
                    if matches!(response, proposals::ContinuationResponse::Complete(_)) {
                        return Err("explicit completion requires a task-completed fact".into());
                    }
                    proposals::materialize_continuation_response(&session, &expected, &response)?;
                    *state
                        .continuations
                        .last_mut()
                        .expect("continuation checked above") =
                        ProposalContinuationState::Completed {
                            request: expected,
                            response,
                        };
                    cursor += 1;
                    continue;
                }
                session::SessionFact::TaskCompleted {
                    sequence,
                    prompt_sha256: completed_prompt,
                    response_sha256,
                    response,
                    provenance,
                } if *sequence == expected.sequence => {
                    if *completed_prompt != prompt_sha256 || provenance.is_empty() {
                        return Err("task completion does not bind its continuation start".into());
                    }
                    validate_session_text(response, response_sha256, "completion response")?;
                    let response =
                        proposals::parse_continuation_response(response.as_bytes(), &expected)?;
                    if !matches!(response, proposals::ContinuationResponse::Complete(_)) {
                        return Err(
                            "task-completed fact does not contain explicit completion".into()
                        );
                    }
                    proposals::materialize_continuation_response(&session, &expected, &response)?;
                    *state
                        .continuations
                        .last_mut()
                        .expect("continuation checked above") =
                        ProposalContinuationState::Completed {
                            request: expected,
                            response,
                        };
                    cursor += 1;
                    continue;
                }
                _ => {
                    return Err(
                        "interrupted continuation is followed by another session fact".into(),
                    );
                }
            }
        }

        match &record.fact {
            session::SessionFact::ProposalRejected {
                proposal_index,
                revision,
                candidate_sha256,
                model,
                feedback,
            } => {
                if state.is_complete()
                    || state.command.is_some()
                    || !state.continuations.is_empty()
                    || state.revision.is_some()
                    || *revision != 0
                {
                    return Err("proposal rejection is not legal in the derived state".into());
                }
                let current = state.current(&session, *proposal_index)?;
                if current.revision != *revision
                    || current.proposal.result_sha256 != *candidate_sha256
                {
                    return Err("proposal rejection names a stale generation".into());
                }
                let request = proposals::Revision::new(
                    model,
                    feedback.as_bytes(),
                    &state.admission,
                    &state.proposals,
                    *proposal_index,
                )?;
                state.revision = Some(ProposalRevisionState::Pending(request));
            }
            session::SessionFact::ProposalApproved {
                proposal_index,
                revision,
                candidate_sha256,
            } => {
                if state.is_complete() {
                    return Err("proposal approval follows explicit completion".into());
                }
                let current = state.current(&session, *proposal_index)?;
                if current.revision != *revision
                    || current.proposal.result_sha256 != *candidate_sha256
                    || state.promotion_status(&current) != PromotionStatus::Absent
                {
                    return Err("proposal approval names a stale or consumed generation".into());
                }
                state.promotions.push(ProposalPromotionState::Approved {
                    proposal_index: *proposal_index,
                    revision: *revision,
                    candidate_sha256: candidate_sha256.clone(),
                });
            }
            session::SessionFact::CommandStarted {
                attempt,
                payload_sha256,
                payload,
            } => {
                validate_new_command_fact(&state, &session)?;
                validate_session_text(payload, payload_sha256, "CommandStarted payload")?;
                let started = commands::CommandStarted::from_bytes(payload.as_bytes())?;
                let expected_attempt = state
                    .command
                    .as_ref()
                    .map_or(Ok(1), CommandHistory::next_attempt)?;
                if *attempt != expected_attempt || started.attempt != *attempt {
                    return Err("approved command attempt does not follow session history".into());
                }
                state
                    .command
                    .get_or_insert_with(|| CommandHistory {
                        attempts: Vec::new(),
                    })
                    .attempts
                    .push(CommandAttemptState::Interrupted(started));
            }
            session::SessionFact::ModelStarted {
                turn: session::ModelTurn::Continuation { sequence },
                model,
                prompt_sha256,
                prompt,
            } => {
                validate_continuation_fact_start(&state, &session, *sequence)?;
                validate_session_text(prompt, prompt_sha256, "continuation prompt")?;
                let finished = state
                    .command
                    .as_ref()
                    .ok_or("continuation exists without command history")?
                    .finished(*sequence)?;
                let current = state.generations_before_continuation(&session, *sequence)?;
                let request = proposals::Continuation::from_prompt(
                    *sequence,
                    prompt.as_bytes(),
                    &state.admission,
                    &current,
                    finished,
                )?;
                if request.model != *model {
                    return Err("continuation start belongs to another model".into());
                }
                proposals::materialize_continuation_prompt(&session, *sequence, prompt.as_bytes())?;
                state
                    .continuations
                    .push(ProposalContinuationState::Interrupted(request));
            }
            _ => return Err(format!("session fact {} is not legal here", record.id).into()),
        }
        cursor += 1;
    }
    Ok(state)
}

fn validate_session_text(value: &str, digest: &str, label: &str) -> Result<(), String> {
    if session::sha256(value.as_bytes()) != digest {
        return Err(format!("{label} digest mismatch"));
    }
    Ok(())
}

fn validate_new_command_fact(
    state: &ProposalSession,
    session: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if state.is_complete() {
        return Err("proposal session is explicitly complete".into());
    }
    if matches!(
        state.continuations.last(),
        Some(ProposalContinuationState::Interrupted(_))
    ) {
        return Err("latest continuation is not terminal".into());
    }
    if matches!(
        state
            .command
            .as_ref()
            .and_then(|history| history.attempts.last()),
        Some(CommandAttemptState::Interrupted(_))
    ) {
        return Err("latest approved command is interrupted/unknown".into());
    }
    let finished = state
        .command
        .as_ref()
        .map_or(0, CommandHistory::finished_count);
    if finished > state.continuations.len() {
        return Err("latest finished command has no continuation".into());
    }
    for index in 0..state.proposals.len() {
        let current = state.current(session, index)?;
        if state.promotion_status(&current) != PromotionStatus::Promoted {
            return Err(format!(
                "proposal {index} revision {} is not durably promoted",
                current.revision
            )
            .into());
        }
    }
    Ok(())
}

fn validate_continuation_fact_start(
    state: &ProposalSession,
    session: &Path,
    sequence: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    if state.is_complete() {
        return Err("proposal session is explicitly complete".into());
    }
    if sequence != state.next_continuation_sequence()? {
        return Err("continuation sequence does not follow session history".into());
    }
    validate_new_command_fact_for_continuation(state, session)?;
    let finished = state
        .command
        .as_ref()
        .ok_or("continuation requires approved command evidence")?;
    if finished.finished_count() != usize::try_from(sequence)? {
        return Err("continuation does not consume the latest finished command".into());
    }
    Ok(())
}

fn validate_new_command_fact_for_continuation(
    state: &ProposalSession,
    session: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    for index in 0..state.proposals.len() {
        let current = state.current(session, index)?;
        if state.promotion_status(&current) != PromotionStatus::Promoted {
            return Err(format!(
                "proposal {index} revision {} is not durably promoted",
                current.revision
            )
            .into());
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
        if fs::read(&base_path)? != candidate.result {
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
                if fs::read(&path)? != proposal.result {
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
            if fs::read(&path)? != proposal.result {
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
    println!("  source_sha256={}", candidate.source_sha256);
    println!(
        "  byte_range={}..{}",
        candidate.byte_start, candidate.byte_end
    );
    println!("  result_sha256={}", candidate.result_sha256);
    println!("  candidate={}", path.display());
    print!("{}", proposals::render_diff(candidate));
}

fn run_approved_command(
    session: &Path,
    argv: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let session = fs::canonicalize(session)?;
    let state = reconstruct_proposal_session(&session)?;
    validate_new_command_fact(&state, &session)?;
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
    let started_payload = String::from_utf8(started_bytes)?;
    let mut session_log = session::SessionLog::open(&session)?;
    session_log.append(session::SessionFact::CommandStarted {
        attempt,
        payload_sha256: session::sha256(started_payload.as_bytes()),
        payload: started_payload,
    })?;

    let execution = commands::execute(&started);
    let repository_after = commands::RepositoryIdentity::capture(&repository, &admitted_paths)?;
    let finished = commands::CommandFinished::new(&started, execution, repository_after)?;
    let finished_bytes = finished.to_bytes()?;
    let finished_payload = String::from_utf8(finished_bytes)?;
    session_log.append(session::SessionFact::CommandFinished {
        attempt,
        start_sha256: started.sha256()?,
        payload_sha256: session::sha256(finished_payload.as_bytes()),
        payload: finished_payload,
    })?;
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
    let sequence = state.next_continuation_sequence()?;
    let current = state.generations_before_continuation(&session, sequence)?;
    let finished = state
        .command
        .as_ref()
        .ok_or("proposal session has no approved command")?
        .finished(sequence)?;
    let request = proposals::Continuation::new(
        sequence,
        model,
        &state.admission,
        &current,
        finished,
        &repository_root()?,
    )?;
    let prompt_bytes = request.prompt_bytes()?;
    let prompt_text = String::from_utf8(prompt_bytes.clone())?;
    let prompt = proposals::continuation_prompt_path(&session, request.sequence);
    proposals::materialize_continuation_prompt(&session, request.sequence, &prompt_bytes)?;
    let journal = proposals::continuation_journal_path(&session, request.sequence);
    let mut session_log = session::SessionLog::open(&session)?;
    let prompt_sha256 = session::sha256(&prompt_bytes);
    session_log.append(session::SessionFact::ModelStarted {
        turn: session::ModelTurn::Continuation { sequence },
        model: model.to_owned(),
        prompt_sha256: prompt_sha256.clone(),
        prompt: prompt_text,
    })?;
    let (response, provenance) =
        run_exchange_turn_with_limits(&prompt, &journal, adapter, proposal_limits())?;
    let response_text = String::from_utf8(response)?;
    let parsed = proposals::parse_continuation_response(response_text.as_bytes(), &request)?;
    let response_sha256 = session::sha256(response_text.as_bytes());
    let fact = match parsed {
        proposals::ContinuationResponse::Complete(_) => session::SessionFact::TaskCompleted {
            sequence,
            prompt_sha256,
            response_sha256,
            response: response_text,
            provenance: provenance.clone(),
        },
        proposals::ContinuationResponse::Proposal { .. } => session::SessionFact::ModelCompleted {
            turn: session::ModelTurn::Continuation { sequence },
            prompt_sha256,
            response_sha256,
            response: response_text,
            provenance: provenance.clone(),
        },
    };
    session_log.append(fact)?;
    let reconstructed = reconstruct_proposal_session(&session)?;
    display_proposals(&session, &reconstructed)?;
    println!("response provenance: {provenance}");
    println!(
        "continuation {} reconstructed; approved command was not rerun",
        request.sequence
    );
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
    match state.promotion_status(&current) {
        PromotionStatus::Promoted => {
            display_proposals(&session, &state)?;
            println!("proposal {index} promotion reconstructed; no mutation emitted");
            return Ok(());
        }
        PromotionStatus::Interrupted => {
            return Err(
                "proposal promotion ended interrupted/unknown; it will not be retried".into(),
            );
        }
        PromotionStatus::Absent | PromotionStatus::Approved => {}
    }
    let candidate = current.proposal;
    let candidate_path = current.path.clone();
    let repository_root = repository_root()?;
    let source = proposals::resolve_source(&repository_root, &candidate.path)?;
    if fs::read(&candidate_path)? != candidate.result {
        return Err(format!("proposal candidate {index} changed before approval").into());
    }
    let (journal, mutation) = proposal_promotion_paths(&session, index, current.revision);
    let live = fs::read(&source)?;
    let live_digest = digest(&live);
    if live_digest != candidate.source_sha256
        && !(live_digest == candidate.result_sha256 && journal.exists() && mutation.exists())
    {
        return Err(format!(
            "source `{}` drifted before proposal {index} promotion",
            candidate.path
        )
        .into());
    }
    let operation = proposal_operation(index, current.revision)?;
    let mut session_log = session::SessionLog::open(&session)?;
    if state.promotion_status(&current) == PromotionStatus::Absent {
        session_log.append(session::SessionFact::ProposalApproved {
            proposal_index: index,
            revision: current.revision,
            candidate_sha256: candidate.result_sha256.clone(),
        })?;
    }
    session_log.append(session::SessionFact::PromotionStarted {
        proposal_index: index,
        revision: current.revision,
        candidate_sha256: candidate.result_sha256.clone(),
        operation,
    })?;

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
    if fs::read(&source)? != candidate.result {
        return Err(format!("proposal {index} promotion produced different bytes").into());
    }
    drop(runtime);

    let mut restarted = Runtime::open_persistent(proposal_limits(), persistence())?;
    active_id(&restarted, "root/promoter")?;
    if fs::read(&source)? != candidate.result {
        return Err(format!("proposal {index} changed after restart").into());
    }
    restarted.shutdown_persistent()?;
    if !restarted.is_observationally_clean() || fs::read(&source)? != candidate.result {
        return Err(format!("proposal {index} did not retain a clean promotion").into());
    }
    session_log.append(session::SessionFact::ProposalPromoted {
        proposal_index: index,
        revision: current.revision,
        candidate_sha256: candidate.result_sha256.clone(),
        operation,
    })?;
    let reconstructed = reconstruct_proposal_session(&session)?;
    display_proposals(&session, &reconstructed)?;
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
                            candidate.source_sha256.clone(),
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
                "lode/terminology.md",
                "lode/practices.md",
            ]),
            Ok(CliCommand::Propose {
                model: "model".into(),
                task: PathBuf::from("task"),
                session: PathBuf::from("session"),
                sources: vec![
                    PathBuf::from("README.md"),
                    PathBuf::from("lode/summary.md"),
                    PathBuf::from("lode/terminology.md"),
                    PathBuf::from("lode/practices.md"),
                ],
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
                "requires at least two",
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
            source_sha256: digest(before),
            byte_start: 0,
            byte_end: before.len(),
            result_sha256: digest(result),
            source: before.to_vec(),
            replacement: result.to_vec(),
            result: result.to_vec(),
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
        let base_response = serde_json::to_vec(&serde_json::json!({
            "proposals": [
                ranged_proposal("alpha.txt", &admission.files[0], "alpha rejected\n"),
                ranged_proposal("beta.txt", &admission.files[1], "beta accepted\n")
            ]
        }))
        .unwrap();
        initialize_proposal_session(&session, &admission, &base_response);
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
            "proposal": ranged_proposal(
                "alpha.txt",
                &admission.files[0],
                "alpha corrected\n"
            )
        }))
        .unwrap();
        let mut log = session::SessionLog::open(&session).unwrap();
        log.append(session::SessionFact::ProposalRejected {
            proposal_index: 0,
            revision: 0,
            candidate_sha256: proposals[0].result_sha256.clone(),
            model: "test-model".into(),
            feedback: "Use the corrected alpha label.".into(),
        })
        .unwrap();
        let revision_prompt_sha256 = session::sha256(&revision_prompt);
        log.append(session::SessionFact::ModelStarted {
            turn: session::ModelTurn::Revision {
                proposal_index: 0,
                revision: 1,
            },
            model: "test-model".into(),
            prompt_sha256: revision_prompt_sha256.clone(),
            prompt: String::from_utf8(revision_prompt).unwrap(),
        })
        .unwrap();
        log.append(session::SessionFact::ModelCompleted {
            turn: session::ModelTurn::Revision {
                proposal_index: 0,
                revision: 1,
            },
            prompt_sha256: revision_prompt_sha256,
            response_sha256: session::sha256(&revision_response),
            response: String::from_utf8(revision_response).unwrap(),
            provenance: "test:revision".into(),
        })
        .unwrap();

        let state = reconstruct_proposal_session(&session).unwrap();
        let current = state.current(&session, 0).unwrap();
        assert_eq!(current.proposal.result, b"alpha corrected\n");
        assert_eq!(fs::read(current.path).unwrap(), current.proposal.result);
        let sibling = state.current(&session, 1).unwrap();
        assert_eq!(sibling.proposal.result, b"beta accepted\n");
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
    fn started_only_initial_turn_is_terminal_on_restart() {
        let session = temporary_directory();
        let admission = proposals::Admission {
            task: "initial interruption".into(),
            files: vec![
                admitted("alpha.txt", b"alpha\n"),
                admitted("beta.txt", b"beta\n"),
            ],
        };
        let prompt = admission.prompt_bytes().unwrap();
        let mut log = session::SessionLog::open(&session).unwrap();
        log.append(session::SessionFact::ModelStarted {
            turn: session::ModelTurn::Initial,
            model: "test-model".into(),
            prompt_sha256: session::sha256(&prompt),
            prompt: String::from_utf8(prompt).unwrap(),
        })
        .unwrap();
        assert!(reconstruct_proposal_session(&session).is_err());
        let before = log.facts().len();
        assert!(
            run_multi_proposal(
                "test-model",
                Path::new("unused-task"),
                &session,
                &[PathBuf::from("unused-a"), PathBuf::from("unused-b")],
            )
            .is_err()
        );
        assert_eq!(
            session::SessionLog::open(&session).unwrap().facts().len(),
            before
        );
        fs::remove_dir_all(session).unwrap();
    }

    #[test]
    fn started_only_revision_and_promotion_are_terminal_on_restart() {
        let revision_session = temporary_directory();
        let admission = proposals::Admission {
            task: "revision interruption".into(),
            files: vec![
                admitted("alpha.txt", b"alpha\n"),
                admitted("beta.txt", b"beta\n"),
            ],
        };
        let response = serde_json::to_vec(&serde_json::json!({
            "proposals": [
                ranged_proposal("alpha.txt", &admission.files[0], "alpha proposed\n"),
                ranged_proposal("beta.txt", &admission.files[1], "beta proposed\n")
            ]
        }))
        .unwrap();
        initialize_proposal_session(&revision_session, &admission, &response);
        let state = reconstruct_proposal_session(&revision_session).unwrap();
        let request = proposals::Revision::new(
            "test-model",
            b"correct alpha",
            &state.admission,
            &state.proposals,
            0,
        )
        .unwrap();
        let prompt = request.prompt_bytes().unwrap();
        let mut log = session::SessionLog::open(&revision_session).unwrap();
        log.append(session::SessionFact::ProposalRejected {
            proposal_index: 0,
            revision: 0,
            candidate_sha256: state.proposals[0].result_sha256.clone(),
            model: "test-model".into(),
            feedback: "correct alpha".into(),
        })
        .unwrap();
        log.append(session::SessionFact::ModelStarted {
            turn: session::ModelTurn::Revision {
                proposal_index: 0,
                revision: 1,
            },
            model: "test-model".into(),
            prompt_sha256: session::sha256(&prompt),
            prompt: String::from_utf8(prompt).unwrap(),
        })
        .unwrap();
        let restarted = reconstruct_proposal_session(&revision_session).unwrap();
        assert!(matches!(
            restarted.revision,
            Some(ProposalRevisionState::Interrupted(_))
        ));
        let before = log.facts().len();
        assert!(
            run_proposal_revision(
                "test-model",
                &revision_session,
                0,
                &revision_session.join("unused-feedback"),
            )
            .is_err()
        );
        assert_eq!(
            session::SessionLog::open(&revision_session)
                .unwrap()
                .facts()
                .len(),
            before
        );
        fs::remove_dir_all(revision_session).unwrap();

        let promotion_session = temporary_directory();
        initialize_proposal_session(&promotion_session, &admission, &response);
        let state = reconstruct_proposal_session(&promotion_session).unwrap();
        let current = state.current(&promotion_session, 0).unwrap();
        let operation = proposal_operation(0, current.revision).unwrap();
        let mut log = session::SessionLog::open(&promotion_session).unwrap();
        log.append(session::SessionFact::ProposalApproved {
            proposal_index: 0,
            revision: current.revision,
            candidate_sha256: current.proposal.result_sha256.clone(),
        })
        .unwrap();
        log.append(session::SessionFact::PromotionStarted {
            proposal_index: 0,
            revision: current.revision,
            candidate_sha256: current.proposal.result_sha256.clone(),
            operation,
        })
        .unwrap();
        let restarted = reconstruct_proposal_session(&promotion_session).unwrap();
        let current = restarted.current(&promotion_session, 0).unwrap();
        assert_eq!(
            restarted.promotion_status(&current),
            PromotionStatus::Interrupted
        );
        let before = log.facts().len();
        assert!(run_proposal_promotion(&promotion_session, 0).is_err());
        assert_eq!(
            session::SessionLog::open(&promotion_session)
                .unwrap()
                .facts()
                .len(),
            before
        );
        fs::remove_dir_all(promotion_session).unwrap();
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
                continuation_proposal(0, &request.sources[0], "alpha corrected after failure\n"),
                calls.clone(),
            )),
        )
        .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let reconstructed = reconstruct_proposal_session(&session).unwrap();
        let corrected = reconstructed.current(&session, 0).unwrap();
        assert_eq!(corrected.revision, 2);
        assert_eq!(
            corrected.proposal.result,
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
    fn command_sequence_and_terminal_identity_tampering_fail_closed() {
        let (sequence_root, sequence_session, _) = promoted_session("command-sequence-tamper");
        let sequence_state = reconstruct_proposal_session(&sequence_session).unwrap();
        let paths = sequence_state
            .admission
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let out_of_sequence = commands::CommandStarted::new(
            2,
            vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
            &repository_root().unwrap(),
            &paths,
        )
        .unwrap();
        let payload = String::from_utf8(out_of_sequence.to_bytes().unwrap()).unwrap();
        session::SessionLog::open(&sequence_session)
            .unwrap()
            .append(session::SessionFact::CommandStarted {
                attempt: 2,
                payload_sha256: session::sha256(payload.as_bytes()),
                payload,
            })
            .unwrap();
        assert!(reconstruct_proposal_session(&sequence_session).is_err());
        fs::remove_dir_all(sequence_root).unwrap();

        let (identity_root, identity_session, _) = promoted_session("command-identity-tamper");
        let identity_state = reconstruct_proposal_session(&identity_session).unwrap();
        let paths = identity_state
            .admission
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let started = commands::CommandStarted::new(
            1,
            vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
            &repository_root().unwrap(),
            &paths,
        )
        .unwrap();
        let payload = String::from_utf8(started.to_bytes().unwrap()).unwrap();
        let mut log = session::SessionLog::open(&identity_session).unwrap();
        log.append(session::SessionFact::CommandStarted {
            attempt: 1,
            payload_sha256: session::sha256(payload.as_bytes()),
            payload,
        })
        .unwrap();
        let terminal_payload = "{}";
        log.append(session::SessionFact::CommandFinished {
            attempt: 1,
            start_sha256: "0".repeat(64),
            payload_sha256: session::sha256(terminal_payload.as_bytes()),
            payload: terminal_payload.into(),
        })
        .unwrap();
        assert!(reconstruct_proposal_session(&identity_session).is_err());
        fs::remove_dir_all(identity_root).unwrap();
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
        let log = session::SessionLog::open(&session).unwrap();
        let kinds = log
            .facts()
            .iter()
            .map(|record| session_fact_kind(&record.fact))
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                "model-started",
                "model-completed",
                "proposal-approved",
                "promotion-started",
                "proposal-promoted",
                "proposal-approved",
                "promotion-started",
                "proposal-promoted",
                "command-started",
                "command-finished",
                "model-started",
                "task-completed",
            ]
        );
        assert!(run_proposal_continuation("test-model", &session).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupted_command_reconstruction_blocks_later_facts() {
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
        let payload = String::from_utf8(started.to_bytes().unwrap()).unwrap();
        session::SessionLog::open(&session)
            .unwrap()
            .append(session::SessionFact::CommandStarted {
                attempt: 1,
                payload_sha256: session::sha256(payload.as_bytes()),
                payload,
            })
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
        let fact_count = session::SessionLog::open(&session).unwrap().facts().len();
        assert!(
            run_approved_command(
                &session,
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    format!("touch {}", marker.display()),
                ],
            )
            .is_err()
        );
        assert!(!marker.exists());
        assert_eq!(
            session::SessionLog::open(&session).unwrap().facts().len(),
            fact_count
        );
        let current = state.current(&session, 0).unwrap();
        session::SessionLog::open(&session)
            .unwrap()
            .append(session::SessionFact::ProposalApproved {
                proposal_index: 0,
                revision: current.revision,
                candidate_sha256: current.proposal.result_sha256.clone(),
            })
            .unwrap();
        assert!(reconstruct_proposal_session(&session).is_err());
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
                continuation_proposal(
                    0,
                    &correction.sources[0],
                    "alpha corrected before interruption\n",
                ),
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
        let response = serde_json::to_vec(&serde_json::json!({
            "proposals": [
                ranged_proposal(&paths[0], &admission.files[0], "alpha proposed\n"),
                ranged_proposal(&paths[1], &admission.files[1], "beta proposed\n")
            ]
        }))
        .unwrap();
        initialize_proposal_session(&session, &admission, &response);
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

    fn session_fact_kind(fact: &session::SessionFact) -> &'static str {
        match fact {
            session::SessionFact::ModelStarted { .. } => "model-started",
            session::SessionFact::ModelCompleted { .. } => "model-completed",
            session::SessionFact::ProposalRejected { .. } => "proposal-rejected",
            session::SessionFact::ProposalApproved { .. } => "proposal-approved",
            session::SessionFact::PromotionStarted { .. } => "promotion-started",
            session::SessionFact::ProposalPromoted { .. } => "proposal-promoted",
            session::SessionFact::CommandStarted { .. } => "command-started",
            session::SessionFact::CommandFinished { .. } => "command-finished",
            session::SessionFact::TaskCompleted { .. } => "task-completed",
        }
    }

    fn initialize_proposal_session(
        session: &Path,
        admission: &proposals::Admission,
        response: &[u8],
    ) {
        let prompt = admission.prompt_bytes().unwrap();
        fs::write(session.join("admission.prompt"), &prompt).unwrap();
        let prompt_sha256 = session::sha256(&prompt);
        let mut log = session::SessionLog::open(session).unwrap();
        log.append(session::SessionFact::ModelStarted {
            turn: session::ModelTurn::Initial,
            model: "test-model".into(),
            prompt_sha256: prompt_sha256.clone(),
            prompt: String::from_utf8(prompt).unwrap(),
        })
        .unwrap();
        log.append(session::SessionFact::ModelCompleted {
            turn: session::ModelTurn::Initial,
            prompt_sha256,
            response_sha256: session::sha256(response),
            response: String::from_utf8(response.to_vec()).unwrap(),
            provenance: "test:initial".into(),
        })
        .unwrap();
        reconstruct_proposal_session(session).unwrap();
    }

    fn admitted(path: &str, content: &[u8]) -> proposals::AdmittedFile {
        proposals::AdmittedFile {
            path: path.into(),
            before_sha256: digest(content),
            content: content.to_vec(),
        }
    }

    fn ranged_proposal(
        path: &str,
        source: &proposals::AdmittedFile,
        result: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "path": path,
            "source_sha256": source.before_sha256,
            "byte_start": 0,
            "byte_end": source.content.len(),
            "replacement": result,
        })
    }

    fn continuation_proposal(
        index: usize,
        source: &proposals::AdmittedFile,
        result: &str,
    ) -> Vec<u8> {
        let mut proposal = ranged_proposal(&source.path, source, result);
        proposal.as_object_mut().unwrap().remove("path");
        format!(
            "PROPOSE {index}\n{}",
            serde_json::to_string(&proposal).unwrap()
        )
        .into_bytes()
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
