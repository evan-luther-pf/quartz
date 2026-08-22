mod commands;
mod openai;

#[cfg(test)]
use quartz_kernel::DurableEventLog;
use quartz_kernel::{
    ComponentSpec, ComponentTree, CompositionPatch, Error, EventGrant, ExchangeAdapter,
    ExchangeFailure, ExchangeGrant, ExchangeResponse, FiberState, Limits, Runtime, SnapshotGrant,
    TraceEvent, WorkspaceGrant,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

const MAX_TASK_SOURCES: usize = 64;
const MAX_SOURCE_BYTES: usize = 32 * 1024;

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
  task <model> <task> <session-dir> <source> <source> [source ...] -- <executable> [arg ...]
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
    Task {
        model: String,
        task: PathBuf,
        session: PathBuf,
        sources: Vec<PathBuf>,
        argv: Vec<String>,
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
            run_repository_edit_acceptance(&component_directory()?, &path)?
        }
        CliCommand::ReviewedEdit(path) => {
            run_reviewed_edit_acceptance(&component_directory()?, &path)?
        }
        CliCommand::PromoteEdit(path) => {
            run_promoted_edit_acceptance(&component_directory()?, &path)?
        }
        CliCommand::ProductionModel {
            model,
            prompt,
            journal,
        } => run_production_model(&model, &prompt, &journal)?,
        CliCommand::Task {
            model,
            task,
            session,
            sources,
            argv,
        } => run_task(&model, &task, &session, &sources, &argv)?,
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
        "task" => {
            let expected =
                "<model> <task> <session> <source> <source> [source ...] -- <executable> [arg ...]";
            let model = required_arg(&mut args, command, expected)?;
            let task = path_arg(&mut args, command, expected)?;
            let session = path_arg(&mut args, command, expected)?;
            let remaining = args.by_ref().collect::<Vec<_>>();
            let separator = remaining
                .iter()
                .position(|argument| argument == "--")
                .ok_or_else(|| "`task` requires `--` before the exact command argv".to_owned())?;
            let sources = remaining[..separator]
                .iter()
                .map(PathBuf::from)
                .collect::<Vec<_>>();
            let argv = remaining[separator + 1..].to_vec();
            if !(2..=MAX_TASK_SOURCES).contains(&sources.len()) {
                return Err(format!(
                    "`task` requires 2..={MAX_TASK_SOURCES} source paths; try `quartz --help`"
                ));
            }
            commands::validate_argv(&argv)?;
            CliCommand::Task {
                model,
                task,
                session,
                sources,
                argv,
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

struct TerminalAdapter;

impl ExchangeAdapter for TerminalAdapter {
    fn identity(&self) -> &str {
        "terminal"
    }

    fn exchange(
        &self,
        request: &[u8],
        _timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<ExchangeResponse, ExchangeFailure> {
        let value: serde_json::Value =
            serde_json::from_slice(request).map_err(|_| ExchangeFailure::Protocol)?;
        let mut stdout = std::io::stdout().lock();
        if let Some(display) = value
            .get("decision")
            .or_else(|| value.get("diff"))
            .and_then(|value| value.as_str())
        {
            writeln!(stdout, "{display}").map_err(|_| ExchangeFailure::remote_failed_other())?;
        } else if value.get("argv").is_some() {
            writeln!(stdout, "command {}", value)
                .map_err(|_| ExchangeFailure::remote_failed_other())?;
            writeln!(stdout, "Approve with 'approve' or stop with 'stop'.")
                .map_err(|_| ExchangeFailure::remote_failed_other())?;
        } else {
            writeln!(stdout, "promotion {}", value)
                .map_err(|_| ExchangeFailure::remote_failed_other())?;
            writeln!(stdout, "Approve with 'approve'; any other response stops.")
                .map_err(|_| ExchangeFailure::remote_failed_other())?;
        }
        write!(stdout, "> ").map_err(|_| ExchangeFailure::remote_failed_other())?;
        stdout
            .flush()
            .map_err(|_| ExchangeFailure::remote_failed_other())?;
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|_| ExchangeFailure::Ambiguous)?;
        if line.is_empty() {
            return Err(ExchangeFailure::EmptyResponse);
        }
        if line.len() > max_response_bytes {
            return Err(ExchangeFailure::ResponseLimit);
        }
        Ok(ExchangeResponse {
            provenance: "terminal:stdin".into(),
            bytes: line.into_bytes(),
            usage: 0,
        })
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandRequest {
    schema: u32,
    attempt: u64,
    argv: Vec<String>,
}

struct CommandAdapter {
    argv: Vec<String>,
    repository: PathBuf,
    sources: Vec<String>,
}

impl ExchangeAdapter for CommandAdapter {
    fn identity(&self) -> &str {
        "command"
    }

    fn exchange(
        &self,
        request: &[u8],
        _timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<ExchangeResponse, ExchangeFailure> {
        let request: CommandRequest =
            serde_json::from_slice(request).map_err(|_| ExchangeFailure::Protocol)?;
        if request.schema != 1 || request.attempt == 0 || request.argv != self.argv {
            return Err(ExchangeFailure::RequestRejected);
        }
        let started = commands::CommandStarted::new(
            request.attempt,
            self.argv.clone(),
            &self.repository,
            &self.sources,
        )
        .map_err(|_| ExchangeFailure::RequestRejected)?;
        let execution = commands::execute(&started);
        let repository_after =
            commands::RepositoryIdentity::capture(&self.repository, &self.sources)
                .map_err(|_| ExchangeFailure::Ambiguous)?;
        let finished = commands::CommandFinished::new(&started, execution, repository_after)
            .map_err(|_| ExchangeFailure::Protocol)?;
        let bytes = finished.to_bytes().map_err(|_| ExchangeFailure::Protocol)?;
        if bytes.len() > max_response_bytes {
            return Err(ExchangeFailure::ResponseLimit);
        }
        Ok(ExchangeResponse {
            provenance: "command:supervised".into(),
            bytes,
            usage: 0,
        })
    }
}

fn run_task(
    model: &str,
    task: &Path,
    session: &Path,
    sources: &[PathBuf],
    argv: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let repository = fs::canonicalize(".")?;
    let task = fs::canonicalize(task)?;
    fs::create_dir_all(session)?;
    let session = fs::canonicalize(session)?;
    let mut canonical_sources = Vec::with_capacity(sources.len());
    let mut source_names = Vec::with_capacity(sources.len());
    for source in sources {
        let source = fs::canonicalize(source)?;
        if !source.is_file() {
            return Err(format!("source is not a regular file: {}", source.display()).into());
        }
        let relative = source
            .strip_prefix(&repository)
            .map_err(|_| "every source must be inside the current repository")?
            .to_str()
            .ok_or("source path is not UTF-8")?
            .to_owned();
        if source_names.contains(&relative) {
            return Err(format!("duplicate source path: {relative}").into());
        }
        canonical_sources.push(source);
        source_names.push(relative);
    }

    let task_bytes = fs::read(&task)?;
    let task_text = std::str::from_utf8(&task_bytes)?;
    if task_text.is_empty() || task_bytes.len() > 4 * 1024 {
        return Err("task must contain 1..=4096 UTF-8 bytes".into());
    }
    commands::validate_argv(argv)?;
    let input = serde_json::to_vec_pretty(&serde_json::json!({
        "schema": 3,
        "task": task_text,
        "argv": argv,
        "sources": source_names,
    }))?;
    let input_path = session.join("task-input.json");
    if input_path.exists() && fs::read(&input_path)? != input {
        return Err("task admission changed for the existing session".into());
    }
    fs::write(&input_path, input)?;

    let limits = proposal_limits();
    let fixtures = component_directory()?;
    let events = session.join("task.qe");
    let persistence = ComponentSpec::new("event-store", artifact(&fixtures, "event-store"))
        .with_journal_paths(vec![session.join("composition.qj")])
        .with_event_stream_paths(vec![events.clone()]);
    let model_adapter: Arc<dyn ExchangeAdapter> = Arc::new(openai::OpenAiResponses::new(
        std::env::var("OPENAI_API_KEY")
            .map_err(|_| "OPENAI_API_KEY is required for `quartz task`")?,
        model.to_owned(),
    )?);
    let terminal_adapter: Arc<dyn ExchangeAdapter> = Arc::new(TerminalAdapter);
    let command_adapter: Arc<dyn ExchangeAdapter> = Arc::new(CommandAdapter {
        argv: argv.to_vec(),
        repository: repository.clone(),
        sources: source_names,
    });
    let mut runtime = Runtime::open_persistent_with_exchanges(
        limits,
        persistence,
        vec![model_adapter, terminal_adapter, command_adapter],
    )?;
    let mut desired = task_tree(
        &fixtures,
        &session,
        &events,
        &input_path,
        &canonical_sources,
        limits,
        "repository-task-a",
    )?;
    let replacement = task_tree(
        &fixtures,
        &session,
        &events,
        &input_path,
        &canonical_sources,
        limits,
        "repository-task-b",
    )?
    .roots
    .into_iter()
    .find(|spec| spec.entry == "repository-task")
    .ok_or("repository-task replacement is absent")?;
    desired.roots.push(ComponentSpec::new(
        "governor",
        artifact(&fixtures, "governor"),
    ));
    desired.roots.push(
        ComponentSpec::new("zz-controller", artifact(&fixtures, "controller"))
            .with_config(1_u64 << 32)
            .with_patches(vec![CompositionPatch::replace(
                "repository-task",
                replacement,
            )]),
    );
    if runtime.fiber_id("repository-task").is_some() {
        runtime.reconcile_to_quiescence()?;
    } else {
        runtime.apply_tree(desired)?;
    }
    let outcome = runtime.fiber_state("repository-task");
    runtime.shutdown_persistent()?;
    task_outcome(outcome).map_err(Into::into)
}

fn task_outcome(state: Option<FiberState>) -> Result<(), String> {
    match state {
        Some(FiberState::Active) => Ok(()),
        Some(FiberState::Failed(category)) => Err(format!(
            "task failed: {}",
            match category.as_str() {
                "authentication"
                | "request-rejected"
                | "remote-failed:server_error"
                | "remote-failed:rate_limit_exceeded"
                | "remote-failed:invalid_prompt"
                | "remote-failed:vector_store_timeout"
                | "remote-failed:other"
                | "remote-cancelled"
                | "incomplete:max_output_tokens"
                | "incomplete:content_filter"
                | "incomplete:other"
                | "empty-response"
                | "response-limit"
                | "protocol"
                | "ambiguous"
                | "stop" => category.as_str(),
                _ => "protocol",
            }
        )),
        _ => Err("task failed: protocol".into()),
    }
}

fn task_tree(
    fixtures: &Path,
    session: &Path,
    _events: &Path,
    input: &Path,
    sources: &[PathBuf],
    limits: Limits,
    orchestrator: &str,
) -> Result<ComponentTree, Error> {
    let exchange_provider = |entry: &str, artifact_name: &str, mode: u64, adapter: &str| {
        ComponentSpec::new(entry, artifact(fixtures, artifact_name))
            .with_config(mode << 56)
            .with_exchange_grants(vec![ExchangeGrant::new(
                adapter,
                session.join(format!("{entry}.qx")),
                limits.max_payload_bytes,
                limits.max_payload_bytes,
                300_000,
            )])
    };
    let workspaces = sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            WorkspaceGrant::dynamic(
                source,
                session.join(format!("workspace-{index}.qm")),
                format!("quartz-task:{index}"),
                MAX_SOURCE_BYTES,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let orchestrator = ComponentSpec::new("repository-task", artifact(fixtures, orchestrator))
        .with_config(sources.len() as u64)
        .with_event_grants(vec![EventGrant::new("quartz.session", "fact", 1)])
        .with_snapshot_grants(vec![SnapshotGrant::from_file(input, "quartz-task:input")?])
        .with_workspace_grants(workspaces);
    Ok(ComponentTree {
        roots: vec![
            exchange_provider(
                "repository-model-provider",
                "repository-model-provider",
                1,
                "openai-responses",
            ),
            exchange_provider(
                "repository-terminal-provider",
                "repository-terminal-provider",
                2,
                "terminal",
            ),
            exchange_provider(
                "repository-command-provider",
                "repository-command-provider",
                3,
                "command",
            ),
            ComponentSpec::new(
                "repository-approval-authority",
                artifact(fixtures, "repository-approval-authority"),
            )
            .with_config(4 << 56),
            orchestrator,
        ],
    })
}

fn version_text() -> String {
    format!("quartz {}", env!("CARGO_PKG_VERSION"))
}

fn run_acceptance() -> Result<(), Box<dyn std::error::Error>> {
    let fixtures = component_directory()?;
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
    let fixtures = component_directory()?;
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
    let fixtures = component_directory()?;
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
    let fixtures = component_directory()?;
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

fn proposal_limits() -> Limits {
    Limits {
        max_snapshot_bytes: 64 * 1024,
        max_payload_bytes: 3 * 1024 * 1024,
        max_payload_total_bytes: 32 * 1024 * 1024,
        max_event_record_bytes: 3 * 1024 * 1024,
        max_exchange_record_bytes: 3 * 1024 * 1024,
        max_event_records: 1024,
        max_payload_records: 1024,
        max_mutation_record_bytes: 3 * 1024 * 1024,
        max_workspace_bytes: MAX_TASK_SOURCES * MAX_SOURCE_BYTES,
        ..Limits::default()
    }
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
    let fixtures = component_directory()?;
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

fn component_directory() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    match resolve_component_directory(
        &executable,
        std::env::var_os("QUARTZ_COMPONENT_DIR").map(PathBuf::from),
    ) {
        Ok(path) => Ok(path),
        Err(error) => {
            #[cfg(test)]
            if std::env::var_os("QUARTZ_COMPONENT_DIR").is_none() {
                return Ok(PathBuf::from(env!("QUARTZ_FIXTURE_DIR")));
            }
            Err(error)
        }
    }
}

fn resolve_component_directory(
    executable: &Path,
    override_path: Option<PathBuf>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = override_path {
        let path = fs::canonicalize(path)?;
        if path.is_dir() {
            return Ok(path);
        }
        return Err("QUARTZ_COMPONENT_DIR is not a directory".into());
    }
    let path = executable
        .parent()
        .ok_or("Quartz executable has no parent directory")?
        .join("components");
    if path.is_dir() {
        return Ok(path);
    }
    Err(format!(
        "Quartz component directory is absent: {}; set QUARTZ_COMPONENT_DIR for development",
        path.display()
    )
    .into())
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
            "task",
        ] {
            assert!(USAGE.contains(command), "usage omitted {command}");
        }

        for command in [
            "--propose",
            "--resume-proposals",
            "--revise-proposal",
            "--promote-proposal",
            "--run-approved-command",
            "--continue-task",
        ] {
            assert!(!USAGE.contains(command), "usage retained {command}");
            assert!(
                parse(&[command]).unwrap_err().contains("unknown command"),
                "parser retained {command}"
            );
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
                "task",
                "model",
                "task.txt",
                "session",
                "README.md",
                "lode/summary.md",
                "--",
                "cargo",
                "test",
            ]),
            Ok(CliCommand::Task {
                model: "model".into(),
                task: PathBuf::from("task.txt"),
                session: PathBuf::from("session"),
                sources: vec![PathBuf::from("README.md"), PathBuf::from("lode/summary.md")],
                argv: vec!["cargo".into(), "test".into()],
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

    #[test]
    fn component_directory_follows_the_executable_or_explicit_override() {
        let root =
            std::env::temp_dir().join(format!("quartz-component-directory-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        let bundle = root.join("bundle");
        let adjacent = bundle.join("components");
        let override_path = root.join("development-components");
        fs::create_dir_all(&adjacent).unwrap();
        fs::create_dir(&override_path).unwrap();
        assert_eq!(
            resolve_component_directory(&bundle.join("quartz"), None).unwrap(),
            adjacent
        );
        assert_eq!(
            resolve_component_directory(&bundle.join("quartz"), Some(override_path.clone()))
                .unwrap(),
            fs::canonicalize(override_path).unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn task_outcome_allows_only_complete_active_state() {
        assert_eq!(task_outcome(Some(FiberState::Active)), Ok(()));
        for category in [
            "authentication",
            "request-rejected",
            "remote-failed:server_error",
            "remote-failed:rate_limit_exceeded",
            "remote-failed:invalid_prompt",
            "remote-failed:vector_store_timeout",
            "remote-failed:other",
            "remote-cancelled",
            "incomplete:max_output_tokens",
            "incomplete:content_filter",
            "incomplete:other",
            "empty-response",
            "response-limit",
            "protocol",
            "ambiguous",
            "stop",
        ] {
            assert_eq!(
                task_outcome(Some(FiberState::Failed(category.into()))),
                Err(format!("task failed: {category}"))
            );
        }
        assert_eq!(
            task_outcome(Some(FiberState::Failed("secret-bearing detail".into()))),
            Err("task failed: protocol".into())
        );
        assert_eq!(
            task_outcome(Some(FiberState::Inactive)),
            Err("task failed: protocol".into())
        );
    }
}

#[cfg(test)]
mod repository_task_component_tests {
    use super::*;
    use crate::commands::CommandFinished;
    use parking_lot::Mutex;
    use quartz_kernel::{ExchangeTerminalMetadata, IncompleteReason, RemoteErrorCode};
    use std::{
        collections::VecDeque,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct ScriptedAdapter {
        identity: &'static str,
        responses: Mutex<VecDeque<Vec<u8>>>,
        calls: AtomicU64,
        requests: Mutex<Vec<Vec<u8>>>,
    }

    impl ScriptedAdapter {
        fn new(identity: &'static str, responses: Vec<String>) -> Arc<Self> {
            Arc::new(Self {
                identity,
                requests: Mutex::new(Vec::new()),
                responses: Mutex::new(responses.into_iter().map(String::into_bytes).collect()),
                calls: AtomicU64::new(0),
            })
        }
    }

    impl ExchangeAdapter for ScriptedAdapter {
        fn identity(&self) -> &str {
            self.identity
        }

        fn exchange(
            &self,
            request: &[u8],
            _timeout: Duration,
            max_response_bytes: usize,
        ) -> Result<ExchangeResponse, ExchangeFailure> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.requests.lock().push(request.to_vec());
            let bytes = self
                .responses
                .lock()
                .pop_front()
                .ok_or(ExchangeFailure::RequestRejected)?;
            if bytes.len() > max_response_bytes {
                return Err(ExchangeFailure::ResponseLimit);
            }
            Ok(ExchangeResponse {
                provenance: format!("scripted:{}", self.identity),
                bytes,
                usage: 0,
            })
        }
    }

    struct FailingAdapter {
        failure: Option<ExchangeFailure>,
        calls: AtomicU64,
    }

    impl FailingAdapter {
        fn new(failure: ExchangeFailure) -> Arc<Self> {
            Arc::new(Self {
                failure: Some(failure),
                calls: AtomicU64::new(0),
            })
        }

        fn invalid_response() -> Arc<Self> {
            Arc::new(Self {
                failure: None,
                calls: AtomicU64::new(0),
            })
        }
    }

    impl ExchangeAdapter for FailingAdapter {
        fn identity(&self) -> &str {
            "openai-responses"
        }

        fn exchange(
            &self,
            _request: &[u8],
            _timeout: Duration,
            _max_response_bytes: usize,
        ) -> Result<ExchangeResponse, ExchangeFailure> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            match self.failure.clone() {
                Some(failure) => Err(failure),
                None => Ok(ExchangeResponse {
                    provenance: "scripted:invalid".into(),
                    bytes: b"not-json".to_vec(),
                    usage: 0,
                }),
            }
        }
    }

    struct RecordingCommandAdapter {
        inner: CommandAdapter,
        calls: AtomicU64,
        requests: Mutex<Vec<Vec<u8>>>,
    }

    impl ExchangeAdapter for RecordingCommandAdapter {
        fn identity(&self) -> &str {
            self.inner.identity()
        }

        fn exchange(
            &self,
            request: &[u8],
            timeout: Duration,
            max_response_bytes: usize,
        ) -> Result<ExchangeResponse, ExchangeFailure> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.requests.lock().push(request.to_vec());
            self.inner.exchange(request, timeout, max_response_bytes)
        }
    }

    #[test]
    fn external_orchestrator_replaces_and_restarts_without_repeating_effects() {
        let root = std::env::temp_dir().join(format!(
            "quartz-repository-component-{}",
            std::process::id()
        ));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        let input = root.join("task-input.json");
        let source_a = root.join("a.txt");
        let source_b = root.join("b.txt");
        let session = root.join("session");
        fs::create_dir(&session).unwrap();
        fs::write(&source_a, "a\n").unwrap();
        fs::write(&source_b, "b\n").unwrap();
        fs::write(
            &input,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": 3,
                "task": "replace both files",
                "argv": ["/usr/bin/true"],
                "sources": ["a.txt", "b.txt"],
            }))
            .unwrap(),
        )
        .unwrap();
        let sources = vec![source_a.clone(), source_b.clone()];
        let fixtures = PathBuf::from(env!("QUARTZ_FIXTURE_DIR"));
        let events = session.join("task.qe");
        let limits = proposal_limits();
        let persistence = || {
            ComponentSpec::new("event-store", artifact(&fixtures, "event-store"))
                .with_journal_paths(vec![session.join("composition.qj")])
                .with_event_stream_paths(vec![events.clone()])
        };
        let model = ScriptedAdapter::new(
            "openai-responses",
            vec![
                r#"{"proposals":[{"path_index":0,"start_line":1,"end_line":1,"replacement":"alpha\n"},{"path_index":1,"start_line":1,"end_line":1,"replacement":"beta\n"}]}"#.into(),
                "COMPLETE\nAll checks passed.".into(),
            ],
        );
        let terminal = ScriptedAdapter::new("terminal", vec!["approve\n".into(); 5]);
        let command = Arc::new(RecordingCommandAdapter {
            inner: CommandAdapter {
                argv: vec!["/usr/bin/true".into()],
                repository: root.clone(),
                sources: vec!["a.txt".into(), "b.txt".into()],
            },
            calls: AtomicU64::new(0),
            requests: Mutex::new(Vec::new()),
        });
        let adapters = || {
            vec![
                model.clone() as Arc<dyn ExchangeAdapter>,
                terminal.clone() as Arc<dyn ExchangeAdapter>,
                command.clone() as Arc<dyn ExchangeAdapter>,
            ]
        };

        let mut app = task_tree(
            &fixtures,
            &session,
            &events,
            &input,
            &sources,
            limits,
            "repository-task-a",
        )
        .unwrap();
        let replacement = task_tree(
            &fixtures,
            &session,
            &events,
            &input,
            &sources,
            limits,
            "repository-task-b",
        )
        .unwrap()
        .roots
        .into_iter()
        .find(|spec| spec.entry == "repository-task")
        .unwrap();
        app.roots.push(ComponentSpec::new(
            "governor",
            artifact(&fixtures, "governor"),
        ));
        app.roots.push(
            ComponentSpec::new("zz-controller", artifact(&fixtures, "controller"))
                .with_config(1_u64 << 32)
                .with_patches(vec![CompositionPatch::replace(
                    "repository-task",
                    replacement,
                )]),
        );
        let mut runtime =
            Runtime::open_persistent_with_exchanges(limits, persistence(), adapters()).unwrap();
        runtime.apply_tree(app.clone()).unwrap();
        assert_eq!(fs::read(&source_a).unwrap(), b"alpha\n");
        assert_eq!(fs::read(&source_b).unwrap(), b"beta\n");
        let pre_restart_state = format!("{:?}", runtime.fiber_state("repository-task"));
        drop(runtime);

        let mut restarted =
            Runtime::open_persistent_with_exchanges(limits, persistence(), adapters()).unwrap();
        assert_eq!(fs::read(&source_a).unwrap(), b"alpha\n");
        assert_eq!(fs::read(&source_b).unwrap(), b"beta\n");
        let post_restart_state = format!("{:?}", restarted.fiber_state("repository-task"));
        let records = DurableEventLog::open(&events, limits).unwrap();
        let values = records
            .records()
            .iter()
            .map(|record| record.value)
            .collect::<Vec<_>>();
        let terminal_payloads = records
            .records()
            .iter()
            .filter(|record| matches!(record.value, 43 | 51))
            .map(|record| {
                String::from_utf8_lossy(&record.payload.as_ref().unwrap().bytes).into_owned()
            })
            .collect::<Vec<_>>();
        assert!(
            values.contains(&80),
            "events: {values:?}; pre={pre_restart_state}; post={post_restart_state}; payloads={terminal_payloads:?}"
        );
        assert_eq!(model.calls.load(Ordering::Relaxed), 2);
        assert_eq!(terminal.calls.load(Ordering::Relaxed), 5);
        assert_eq!(
            DurableEventLog::open(&events, limits)
                .unwrap()
                .records()
                .iter()
                .filter(|record| record.value == 43)
                .count(),
            1
        );
        assert_eq!(command.calls.load(Ordering::Relaxed), 1);
        let request: CommandRequest =
            serde_json::from_slice(command.requests.lock().first().unwrap()).unwrap();
        assert_eq!(request.argv, ["/usr/bin/true"]);
        assert_eq!(
            task_outcome(restarted.fiber_state("repository-task")),
            Ok(())
        );
        restarted.shutdown_persistent().unwrap();
        assert!(restarted.is_observationally_clean());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_command_is_corrected_then_completed_explicitly() {
        let root = std::env::temp_dir().join(format!(
            "quartz-repository-correction-{}",
            std::process::id()
        ));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        let input = root.join("task-input.json");
        let source_a = root.join("a.txt");
        let source_b = root.join("b.txt");
        let marker = root.join("command-attempted");
        let session = root.join("session");
        fs::create_dir(&session).unwrap();
        fs::write(&source_a, "a\n").unwrap();
        fs::write(&source_b, "b\n").unwrap();
        let command_argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "test -f {} || {{ touch {}; exit 1; }}",
                marker.display(),
                marker.display()
            ),
        ];
        fs::write(
            &input,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": 3,
                "task": "replace both files and correct the failed command",
                "argv": command_argv,
                "sources": ["a.txt", "b.txt"],
            }))
            .unwrap(),
        )
        .unwrap();
        let sources = vec![source_a.clone(), source_b.clone()];
        let fixtures = PathBuf::from(env!("QUARTZ_FIXTURE_DIR"));
        let events = session.join("task.qe");
        let limits = proposal_limits();
        let persistence = || {
            ComponentSpec::new("event-store", artifact(&fixtures, "event-store"))
                .with_journal_paths(vec![session.join("composition.qj")])
                .with_event_stream_paths(vec![events.clone()])
        };
        let model = ScriptedAdapter::new(
            "openai-responses",
            vec![
                r#"{"proposals":[{"path_index":0,"start_line":1,"end_line":1,"replacement":"alpha\n"},{"path_index":1,"start_line":1,"end_line":1,"replacement":"beta\n"}]}"#.into(),
                format!(
                    "PROPOSE 0\n{}",
                    serde_json::json!({
                        "start_line": 1,
                        "end_line": 1,
                        "replacement": "alpha corrected\n",
                    })
                ),
                "COMPLETE\nAll checks passed.".into(),
            ],
        );
        let terminal = ScriptedAdapter::new("terminal", vec!["approve\n".into(); 8]);
        let command = Arc::new(RecordingCommandAdapter {
            inner: CommandAdapter {
                argv: command_argv.clone(),
                repository: root.clone(),
                sources: vec!["a.txt".into(), "b.txt".into()],
            },
            calls: AtomicU64::new(0),
            requests: Mutex::new(Vec::new()),
        });
        let adapters = || {
            vec![
                model.clone() as Arc<dyn ExchangeAdapter>,
                terminal.clone() as Arc<dyn ExchangeAdapter>,
                command.clone() as Arc<dyn ExchangeAdapter>,
            ]
        };
        let app = task_tree(
            &fixtures,
            &session,
            &events,
            &input,
            &sources,
            limits,
            "repository-task-a",
        )
        .unwrap();
        let mut runtime =
            Runtime::open_persistent_with_exchanges(limits, persistence(), adapters()).unwrap();
        runtime.apply_tree(app).unwrap();
        let debug_log = DurableEventLog::open(&events, limits).unwrap();
        let debug_values = debug_log
            .records()
            .iter()
            .map(|record| record.value)
            .collect::<Vec<_>>();
        assert_eq!(
            fs::read(&source_a).unwrap(),
            b"alpha corrected\n",
            "events={debug_values:?}; state={:?}",
            runtime.fiber_state("repository-task")
        );
        assert_eq!(fs::read(&source_b).unwrap(), b"beta\n");
        assert_eq!(model.calls.load(Ordering::Relaxed), 3);
        assert_eq!(terminal.calls.load(Ordering::Relaxed), 8);
        assert_eq!(command.calls.load(Ordering::Relaxed), 2);
        for request in command.requests.lock().iter() {
            let request: CommandRequest = serde_json::from_slice(request).unwrap();
            assert_eq!(request.argv, command_argv);
        }
        let log = DurableEventLog::open(&events, limits).unwrap();
        let finished = log
            .records()
            .iter()
            .filter(|record| record.value == 43)
            .map(|record| {
                serde_json::from_slice::<CommandFinished>(&record.payload.as_ref().unwrap().bytes)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            finished
                .iter()
                .map(|result| result.exit_code)
                .collect::<Vec<_>>(),
            [Some(1), Some(0)]
        );
        assert_eq!(
            log.records()
                .iter()
                .filter(|record| record.value == 80)
                .count(),
            1
        );
        drop(runtime);

        let mut restarted =
            Runtime::open_persistent_with_exchanges(limits, persistence(), adapters()).unwrap();
        assert_eq!(model.calls.load(Ordering::Relaxed), 3);
        assert_eq!(terminal.calls.load(Ordering::Relaxed), 8);
        assert_eq!(command.calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            task_outcome(restarted.fiber_state("repository-task")),
            Ok(())
        );
        restarted.shutdown_persistent().unwrap();
        assert!(restarted.is_observationally_clean());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn terminal_exchange_categories_fail_the_root_and_replay_without_calls() {
        let metadata = |usage, byte: char| ExchangeTerminalMetadata {
            usage: Some(usage),
            response_id_sha256: Some(byte.to_string().repeat(64)),
        };
        for (failure, category, terminal_usage) in [
            (
                Some(ExchangeFailure::Authentication),
                "authentication",
                None,
            ),
            (
                Some(ExchangeFailure::RequestRejected),
                "request-rejected",
                None,
            ),
            (
                Some(ExchangeFailure::RemoteFailed {
                    code: RemoteErrorCode::ServerError,
                    terminal: metadata(101, 'a'),
                }),
                "remote-failed:server_error",
                Some(101),
            ),
            (
                Some(ExchangeFailure::RemoteCancelled {
                    terminal: metadata(102, 'b'),
                }),
                "remote-cancelled",
                Some(102),
            ),
            (
                Some(ExchangeFailure::Incomplete {
                    reason: IncompleteReason::MaxOutputTokens,
                    terminal: metadata(103, 'c'),
                }),
                "incomplete:max_output_tokens",
                Some(103),
            ),
            (
                Some(ExchangeFailure::Incomplete {
                    reason: IncompleteReason::ContentFilter,
                    terminal: metadata(104, 'd'),
                }),
                "incomplete:content_filter",
                Some(104),
            ),
            (
                Some(ExchangeFailure::Incomplete {
                    reason: IncompleteReason::Other,
                    terminal: metadata(105, 'e'),
                }),
                "incomplete:other",
                Some(105),
            ),
            (
                Some(ExchangeFailure::remote_failed_other()),
                "remote-failed:other",
                None,
            ),
            (Some(ExchangeFailure::EmptyResponse), "empty-response", None),
            (Some(ExchangeFailure::ResponseLimit), "response-limit", None),
            (None, "protocol", None),
            (Some(ExchangeFailure::Ambiguous), "ambiguous", None),
        ] {
            let root = std::env::temp_dir().join(format!(
                "quartz-repository-outcome-{category}-{}",
                std::process::id()
            ));
            if root.exists() {
                fs::remove_dir_all(&root).unwrap();
            }
            fs::create_dir_all(&root).unwrap();
            let session = root.join("session");
            fs::create_dir(&session).unwrap();
            let input = root.join("task-input.json");
            let source_a = root.join("a.txt");
            let source_b = root.join("b.txt");
            fs::write(&source_a, "a\n").unwrap();
            fs::write(&source_b, "b\n").unwrap();
            fs::write(
                &input,
                serde_json::to_vec_pretty(&serde_json::json!({
                    "schema": 3,
                    "task": "replace both files",
                    "argv": ["/usr/bin/true"],
                    "sources": ["a.txt", "b.txt"],
                }))
                .unwrap(),
            )
            .unwrap();
            let fixtures = PathBuf::from(env!("QUARTZ_FIXTURE_DIR"));
            let events = session.join("task.qe");
            let limits = proposal_limits();
            let persistence = || {
                ComponentSpec::new("event-store", artifact(&fixtures, "event-store"))
                    .with_journal_paths(vec![session.join("composition.qj")])
                    .with_event_stream_paths(vec![events.clone()])
            };
            let model = failure.map_or_else(FailingAdapter::invalid_response, FailingAdapter::new);
            let adapters = || {
                vec![
                    model.clone() as Arc<dyn ExchangeAdapter>,
                    ScriptedAdapter::new("terminal", Vec::new()) as Arc<dyn ExchangeAdapter>,
                    ScriptedAdapter::new("command", Vec::new()) as Arc<dyn ExchangeAdapter>,
                ]
            };
            let app = task_tree(
                &fixtures,
                &session,
                &events,
                &input,
                &[source_a, source_b],
                limits,
                "repository-task-a",
            )
            .unwrap();

            let mut runtime =
                Runtime::open_persistent_with_exchanges(limits, persistence(), adapters()).unwrap();
            runtime.apply_tree(app).unwrap();
            if let Some(usage) = terminal_usage {
                let ledger = String::from_utf8_lossy(
                    &fs::read(session.join("repository-model-provider.qx")).unwrap(),
                )
                .into_owned();
                assert!(ledger.contains(&format!(r#""usage":{usage}"#)));
                assert!(ledger.contains("response_id_sha256"));
                assert!(!ledger.contains("response body"));
                assert!(!ledger.contains("error message"));
            }
            let failed = Some(FiberState::Failed(category.into()));
            assert_eq!(runtime.fiber_state("repository-task"), failed);
            assert_eq!(
                task_outcome(runtime.fiber_state("repository-task")),
                Err(format!("task failed: {category}"))
            );
            assert_eq!(model.calls.load(Ordering::Relaxed), 1);
            drop(runtime);

            let mut restarted =
                Runtime::open_persistent_with_exchanges(limits, persistence(), adapters()).unwrap();
            assert_eq!(restarted.fiber_state("repository-task"), failed);
            assert_eq!(model.calls.load(Ordering::Relaxed), 1);
            restarted.shutdown_persistent().unwrap();
            assert!(restarted.is_observationally_clean());
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn user_stop_fails_the_root_and_replays_without_calls() {
        let root = std::env::temp_dir().join(format!(
            "quartz-repository-outcome-stop-{}",
            std::process::id()
        ));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        let session = root.join("session");
        fs::create_dir(&session).unwrap();
        let input = root.join("task-input.json");
        let source_a = root.join("a.txt");
        let source_b = root.join("b.txt");
        fs::write(&source_a, "a\n").unwrap();
        fs::write(&source_b, "b\n").unwrap();
        fs::write(
            &input,
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": 3,
                "task": "replace both files",
                "argv": ["/usr/bin/true"],
                "sources": ["a.txt", "b.txt"],
            }))
            .unwrap(),
        )
        .unwrap();
        let fixtures = PathBuf::from(env!("QUARTZ_FIXTURE_DIR"));
        let events = session.join("task.qe");
        let limits = proposal_limits();
        let persistence = || {
            ComponentSpec::new("event-store", artifact(&fixtures, "event-store"))
                .with_journal_paths(vec![session.join("composition.qj")])
                .with_event_stream_paths(vec![events.clone()])
        };
        let model = ScriptedAdapter::new(
            "openai-responses",
            vec![r#"{"proposals":[{"path_index":0,"start_line":1,"end_line":1,"replacement":"alpha\n"},{"path_index":1,"start_line":1,"end_line":1,"replacement":"beta\n"}]}"#.into()],
        );
        let terminal = ScriptedAdapter::new("terminal", vec!["stop\n".into()]);
        let adapters = || {
            vec![
                model.clone() as Arc<dyn ExchangeAdapter>,
                terminal.clone() as Arc<dyn ExchangeAdapter>,
                ScriptedAdapter::new("command", Vec::new()) as Arc<dyn ExchangeAdapter>,
            ]
        };
        let app = task_tree(
            &fixtures,
            &session,
            &events,
            &input,
            &[source_a, source_b],
            limits,
            "repository-task-a",
        )
        .unwrap();

        let mut runtime =
            Runtime::open_persistent_with_exchanges(limits, persistence(), adapters()).unwrap();
        runtime.apply_tree(app).unwrap();
        assert_eq!(
            runtime.fiber_state("repository-task"),
            Some(FiberState::Failed("stop".into()))
        );
        assert_eq!(
            task_outcome(runtime.fiber_state("repository-task")),
            Err("task failed: stop".into())
        );
        assert_eq!(model.calls.load(Ordering::Relaxed), 1);
        assert_eq!(terminal.calls.load(Ordering::Relaxed), 1);
        drop(runtime);

        let mut restarted =
            Runtime::open_persistent_with_exchanges(limits, persistence(), adapters()).unwrap();
        assert_eq!(
            restarted.fiber_state("repository-task"),
            Some(FiberState::Failed("stop".into()))
        );
        assert_eq!(model.calls.load(Ordering::Relaxed), 1);
        assert_eq!(terminal.calls.load(Ordering::Relaxed), 1);
        restarted.shutdown_persistent().unwrap();
        assert!(restarted.is_observationally_clean());
        fs::remove_dir_all(root).unwrap();
    }
}
