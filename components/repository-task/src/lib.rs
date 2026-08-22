wit_bindgen::generate!({
    path: "../../wit",
    world: "module",
});

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const MODE_SHIFT: u64 = 56;
const MODE_MASK: u64 = 0xff << MODE_SHIFT;
const MODE_ORCHESTRATOR: u64 = 0;
const MODE_MODEL: u64 = 1;
const MODE_TERMINAL: u64 = 2;
const MODE_COMMAND: u64 = 3;
const MODE_AUTHORITY: u64 = 4;
const FAILURE_AUTHENTICATION: i32 = 11;
const FAILURE_REQUEST_REJECTED: i32 = 12;
const FAILURE_REMOTE_FAILED: i32 = 13;
const FAILURE_EMPTY_RESPONSE: i32 = 14;
const FAILURE_RESPONSE_LIMIT: i32 = 15;
const FAILURE_PROTOCOL: i32 = 16;
const FAILURE_AMBIGUOUS: i32 = 17;
const FAILURE_STOP: i32 = 18;

const EVENT_STARTED: u64 = 1;
const EVENT_INITIAL_PROMPT: u64 = 10;
const EVENT_INITIAL_RESPONSE: u64 = 11;
const EVENT_GENERATION: u64 = 12;
const EVENT_REVISION_PROMPT: u64 = 13;
const EVENT_REVISION_RESPONSE: u64 = 14;
const EVENT_REVIEW_PROMPT: u64 = 20;
const EVENT_REVIEW_RESPONSE: u64 = 21;
const EVENT_MUTATION_AUTHORIZED: u64 = 22;
const EVENT_CANDIDATE_APPLIED: u64 = 23;
const EVENT_PROMOTION_PROMPT: u64 = 30;
const EVENT_PROMOTION_RESPONSE: u64 = 31;
const EVENT_PROMOTION_AUTHORIZED: u64 = 32;
const EVENT_PROMOTED: u64 = 33;
const EVENT_COMMAND_PROMPT: u64 = 40;
const EVENT_COMMAND_DECISION: u64 = 41;
const EVENT_COMMAND_STARTED: u64 = 42;
const EVENT_COMMAND_RESULT: u64 = 43;
const EVENT_CONTINUATION_PROMPT: u64 = 50;
const EVENT_CONTINUATION_RESPONSE: u64 = 51;
const EVENT_COMPLETE: u64 = 80;
const EVENT_STOPPED: u64 = 90;

const MAX_TASK_BYTES: usize = 4 * 1024;
const MAX_SOURCE_BYTES: usize = 32 * 1024;
const MAX_MODEL_BYTES: usize = 3 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_FEEDBACK_BYTES: usize = 4 * 1024;
const MAX_COMPLETION_BYTES: usize = 4 * 1024;
const MAX_ARG_BYTES: usize = 4 * 1024;
const MAX_ARGV_BYTES: usize = 32 * 1024;
const MAX_ARGC: usize = 1024;

const INITIAL_INSTRUCTIONS: &str = "Edit only the admitted files needed for the task. Return only the required JSON object. Every proposal must use one admitted path and matching source_sha256. Return one half-open UTF-8 byte range and exact replacement text. Return at least two proposals. Do not use Markdown fences or commentary.";
const REVISION_INSTRUCTIONS: &str = "Revise only the rejected proposal identified below. Return only the required JSON object. The proposal must use the rejected admitted path and matching source_sha256. Return one half-open UTF-8 byte range and exact replacement text. Address the exact feedback and change the rejected result. Do not use Markdown fences or commentary.";
const CONTINUATION_INSTRUCTIONS: &str = "Continue the same repository task from the exact approved-command evidence. Return exactly `PROPOSE <admitted-path-index>\n<strict ranged-edit JSON>` or `COMPLETE\n<bounded final summary>`. The ranged-edit object must contain only source_sha256, byte_start, byte_end, and replacement. PROPOSE may select only an admitted path index and must change the exact post-command source. COMPLETE is valid only when the command succeeded. Do not use Markdown fences or commentary outside the selected grammar.";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct TaskInput {
    schema: u32,
    task: String,
    argv: Vec<String>,
    sources: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct AdmittedFile {
    path: String,
    source_sha256: String,
    content: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Admission {
    schema: u32,
    task: String,
    argv: Vec<String>,
    files: Vec<AdmittedFile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Proposal {
    path: String,
    source_sha256: String,
    source: String,
    byte_start: usize,
    byte_end: usize,
    replacement: String,
    result_sha256: String,
    result: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Generation {
    proposal_index: usize,
    admitted_path_index: usize,
    revision: u32,
    proposal: Proposal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    proposal_index: usize,
    source: usize,
    revision: u32,
    operation: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Review {
    operation: Operation,
    diff: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CommandRequest {
    schema: u32,
    attempt: u64,
    argv: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RepositoryFile {
    byte_len: Option<u64>,
    content: Option<String>,
    path: String,
    sha256: Option<String>,
    status: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RepositoryIdentity {
    canonical_root: String,
    canonical_root_sha256: String,
    files: Vec<RepositoryFile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct BoundedOutput {
    content: String,
    encoding: String,
    read_error: Option<String>,
    truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CommandFinished {
    argv: Vec<String>,
    attempt: u64,
    command_started_sha256: String,
    duration_ms: u64,
    exit_code: Option<i32>,
    kind: String,
    repository: RepositoryIdentity,
    repository_after: RepositoryIdentity,
    schema: u32,
    signal: Option<i32>,
    spawn_error: Option<String>,
    stderr: BoundedOutput,
    stdout: BoundedOutput,
    timed_out: bool,
}

impl CommandFinished {
    fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
            && self.signal.is_none()
            && self.spawn_error.is_none()
            && !self.timed_out
            && self.stdout.read_error.is_none()
            && self.stderr.read_error.is_none()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    summary: String,
}

struct Fact {
    index: u64,
    value: u64,
    bytes: Vec<u8>,
}

struct RepositoryTask;

impl Guest for RepositoryTask {
    fn start(config: u64) -> u64 {
        config
    }

    fn step(instance: u64) -> i32 {
        let mode = (instance & MODE_MASK) >> MODE_SHIFT;
        match mode {
            MODE_ORCHESTRATOR => orchestrate((instance & !MODE_MASK) as usize),
            MODE_MODEL => provide_exchange(30),
            MODE_TERMINAL => provide_exchange(31),
            MODE_COMMAND => provide_exchange(32),
            MODE_AUTHORITY => provide_authority(),
            _ => -4,
        }
    }

    fn invoke(instance: u64, operation: u64, arg0: u64, arg1: u64) -> i64 {
        let mode = (instance & MODE_MASK) >> MODE_SHIFT;
        match mode {
            MODE_MODEL | MODE_TERMINAL | MODE_COMMAND if operation == 1 => exchange(arg0, arg1),
            MODE_AUTHORITY if operation == 1 => authorize(arg0, arg1),
            _ => -4,
        }
    }

    fn drop(_instance: u64) {}
}

fn provide_exchange(slot: u64) -> i32 {
    match checked(open_exchange(0)).and_then(|_| checked(publish_callable(slot))) {
        Ok(_) => 1,
        Err(status) => -status.abs(),
    }
}

fn provide_authority() -> i32 {
    match checked(publish_callable(33)).and_then(|_| checked(publish_callable(34))) {
        Ok(_) => 1,
        Err(status) => -status.abs(),
    }
}

fn authorize(operation: u64, source: u64) -> i64 {
    let Ok(facts) = facts() else { return -101 };
    for pair in facts.windows(2).rev() {
        let expected = match pair[1].value {
            EVENT_MUTATION_AUTHORIZED => EVENT_REVIEW_RESPONSE,
            EVENT_PROMOTION_AUTHORIZED => EVENT_PROMOTION_RESPONSE,
            _ => continue,
        };
        if pair[0].value != expected || trim(&pair[0].bytes) != "approve" {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Operation>(&pair[1].bytes) else {
            continue;
        };
        if record.operation == operation && record.source as u64 == source {
            return 1;
        }
    }
    0
}

fn orchestrate(source_count: usize) -> i32 {
    match orchestrate_inner(source_count) {
        Ok(_) => 1,
        Err(status) => -terminal_failure(status),
    }
}

fn terminal_failure(status: i32) -> i32 {
    match status.abs() {
        FAILURE_AUTHENTICATION
        | FAILURE_REQUEST_REJECTED
        | FAILURE_REMOTE_FAILED
        | FAILURE_EMPTY_RESPONSE
        | FAILURE_RESPONSE_LIMIT
        | FAILURE_PROTOCOL
        | FAILURE_AMBIGUOUS
        | FAILURE_STOP => status.abs(),
        5 => FAILURE_RESPONSE_LIMIT,
        10 => FAILURE_AMBIGUOUS,
        _ => FAILURE_PROTOCOL,
    }
}

fn orchestrate_inner(source_count: usize) -> Result<(), i32> {
    if !(2..=64).contains(&source_count) {
        return Err(4);
    }
    let facts = facts()?;
    validate_external_adjacency(&facts)?;
    if let Some(fact) = facts.last() {
        if fact.value == EVENT_COMPLETE {
            return Ok(());
        }
        if fact.value == EVENT_STOPPED {
            return Err(FAILURE_STOP);
        }
    }
    let admission = match facts.first() {
        None => {
            append(EVENT_STARTED, &admit(source_count)?)?;
            return Ok(());
        }
        Some(fact) if fact.value == EVENT_STARTED => {
            let admission: Admission = parse(&fact.bytes)?;
            validate_admission(&admission, source_count)?;
            admission
        }
        _ => return Err(4),
    };

    let mut generations = current_generations(&facts, &admission)?;
    if generations.is_empty() {
        return initial_generation(&facts, &admission);
    }
    if append_missing_initial_generation(&facts, &admission, &generations)? {
        return Ok(());
    }

    generations.sort_by_key(|generation| generation.proposal_index);
    for generation in &generations {
        let operation = operation(generation);
        if !has_operation(&facts, EVENT_CANDIDATE_APPLIED, &operation) {
            return review_generation(&facts, &admission, generation, &operation);
        }
    }
    for generation in &generations {
        let operation = operation(generation);
        if !has_operation(&facts, EVENT_PROMOTED, &operation) {
            return promote_generation(&facts, generation, &operation);
        }
    }
    command_cycle(&facts, &admission, &generations)
}

fn admit(source_count: usize) -> Result<Admission, i32> {
    let input: TaskInput = parse(&snapshot(0)?)?;
    if input.schema != 2 || input.sources.len() != source_count {
        return Err(4);
    }
    let mut files = Vec::with_capacity(source_count);
    for (index, path) in input.sources.iter().enumerate() {
        let bytes = workspace(index as u64)?;
        let content = String::from_utf8(bytes).map_err(|_| 4)?;
        files.push(AdmittedFile {
            path: path.clone(),
            source_sha256: sha256(content.as_bytes()),
            content,
        });
    }
    let admission = Admission {
        schema: 2,
        task: input.task,
        argv: input.argv,
        files,
    };
    validate_admission(&admission, source_count)?;
    Ok(admission)
}

fn validate_admission(admission: &Admission, source_count: usize) -> Result<(), i32> {
    if admission.schema != 2
        || admission.task.is_empty()
        || admission.task.len() > MAX_TASK_BYTES
        || admission.files.len() != source_count
        || validate_argv(&admission.argv).is_err()
    {
        return Err(4);
    }
    let mut paths = BTreeSet::new();
    for file in &admission.files {
        if file.path.is_empty()
            || file.content.is_empty()
            || file.content.len() > MAX_SOURCE_BYTES
            || !paths.insert(file.path.clone())
            || file.source_sha256 != sha256(file.content.as_bytes())
        {
            return Err(4);
        }
    }
    Ok(())
}

fn initial_generation(facts: &[Fact], admission: &Admission) -> Result<(), i32> {
    let prompt = facts
        .iter()
        .rfind(|fact| fact.value == EVENT_INITIAL_PROMPT);
    if prompt.is_none() {
        append_bytes(EVENT_INITIAL_PROMPT, &initial_prompt(admission)?)?;
        return Ok(());
    }
    let prompt = prompt.unwrap();
    let response = facts
        .iter()
        .rfind(|fact| fact.value == EVENT_INITIAL_RESPONSE);
    if response.is_none_or(|fact| fact.index < prompt.index) {
        perform_exchange(30, prompt.index, EVENT_INITIAL_RESPONSE)?;
        return Ok(());
    }
    let proposals = parse_initial_response(&response.unwrap().bytes, admission)?;
    let proposal = proposals.into_iter().next().ok_or(4)?;
    let admitted_path_index = admission
        .files
        .iter()
        .position(|file| file.path == proposal.path)
        .ok_or(4)?;
    append(
        EVENT_GENERATION,
        &Generation {
            proposal_index: 0,
            admitted_path_index,
            revision: 0,
            proposal,
        },
    )
}

fn append_missing_initial_generation(
    facts: &[Fact],
    admission: &Admission,
    generations: &[Generation],
) -> Result<bool, i32> {
    let Some(response) = facts
        .iter()
        .find(|fact| fact.value == EVENT_INITIAL_RESPONSE)
    else {
        return Ok(false);
    };
    let proposals = parse_initial_response(&response.bytes, admission)?;
    for (proposal_index, proposal) in proposals.into_iter().enumerate() {
        if let Some(generation) = generations
            .iter()
            .find(|generation| generation.proposal_index == proposal_index)
        {
            if generation.revision == 0 && generation.proposal != proposal {
                return Err(4);
            }
            continue;
        }
        let admitted_path_index = admission
            .files
            .iter()
            .position(|file| file.path == proposal.path)
            .ok_or(4)?;
        append(
            EVENT_GENERATION,
            &Generation {
                proposal_index,
                admitted_path_index,
                revision: 0,
                proposal,
            },
        )?;
        return Ok(true);
    }
    Ok(false)
}

fn review_generation(
    facts: &[Fact],
    admission: &Admission,
    generation: &Generation,
    operation: &Operation,
) -> Result<(), i32> {
    let prompt = matching_operation(facts, EVENT_REVIEW_PROMPT, operation, |bytes| {
        parse::<Review>(bytes).map(|review| review.operation)
    });
    if prompt.is_none() {
        append(
            EVENT_REVIEW_PROMPT,
            &Review {
                operation: operation.clone(),
                diff: render_diff(&generation.proposal),
            },
        )?;
        return Ok(());
    }
    let prompt = prompt.unwrap();
    let response = facts
        .iter()
        .rfind(|fact| fact.value == EVENT_REVIEW_RESPONSE && fact.index == prompt.index + 1);
    if response.is_none() {
        perform_exchange(31, prompt.index, EVENT_REVIEW_RESPONSE)?;
        return Ok(());
    }
    let response = response.unwrap();
    match trim(&response.bytes) {
        "stop" => append(
            EVENT_STOPPED,
            &Completion {
                summary: "Stopped by user.".into(),
            },
        ),
        "approve" => {
            if !has_operation(facts, EVENT_MUTATION_AUTHORIZED, operation) {
                append(EVENT_MUTATION_AUTHORIZED, operation)?;
                return Ok(());
            }
            write_workspace(
                generation.admitted_path_index as u64,
                generation.proposal.result.as_bytes(),
            )?;
            checked_i64(call_provider(
                33,
                1,
                operation.operation,
                operation.source as u64,
            ))?;
            checked(publish_dynamic_workspace(
                generation.admitted_path_index as u64,
                operation.operation,
            ))?;
            append(EVENT_CANDIDATE_APPLIED, operation)
        }
        line => {
            let feedback = line
                .strip_prefix("reject ")
                .filter(|feedback| {
                    !feedback.trim().is_empty() && feedback.len() <= MAX_FEEDBACK_BYTES
                })
                .ok_or(4)?;
            revise_generation(
                facts,
                admission,
                generation,
                operation,
                response.index,
                feedback,
            )
        }
    }
}

fn revise_generation(
    facts: &[Fact],
    admission: &Admission,
    generation: &Generation,
    operation: &Operation,
    rejection_index: u64,
    feedback: &str,
) -> Result<(), i32> {
    let prompt = matching_operation(facts, EVENT_REVISION_PROMPT, operation, |bytes| {
        let value: Value = parse(bytes)?;
        let op: Operation =
            serde_json::from_value(value.get("operation").cloned().ok_or(4)?).map_err(|_| 4)?;
        Ok(op)
    })
    .filter(|fact| fact.index > rejection_index);
    if prompt.is_none() {
        append_bytes(
            EVENT_REVISION_PROMPT,
            &revision_prompt(admission, generation, operation, feedback)?,
        )?;
        return Ok(());
    }
    let prompt = prompt.unwrap();
    let response = facts
        .iter()
        .rfind(|fact| fact.value == EVENT_REVISION_RESPONSE && fact.index == prompt.index + 1);
    if response.is_none() {
        perform_exchange(30, prompt.index, EVENT_REVISION_RESPONSE)?;
        return Ok(());
    }
    let proposal = parse_revision_response(&response.unwrap().bytes, generation)?;
    append(
        EVENT_GENERATION,
        &Generation {
            proposal_index: generation.proposal_index,
            admitted_path_index: generation.admitted_path_index,
            revision: generation.revision.checked_add(1).ok_or(4)?,
            proposal,
        },
    )
}

fn promote_generation(
    facts: &[Fact],
    generation: &Generation,
    operation: &Operation,
) -> Result<(), i32> {
    let prompt = matching_operation(facts, EVENT_PROMOTION_PROMPT, operation, |bytes| {
        parse(bytes)
    });
    if prompt.is_none() {
        append(EVENT_PROMOTION_PROMPT, operation)?;
        return Ok(());
    }
    let prompt = prompt.unwrap();
    let response = facts
        .iter()
        .rfind(|fact| fact.value == EVENT_PROMOTION_RESPONSE && fact.index == prompt.index + 1);
    if response.is_none() {
        perform_exchange(31, prompt.index, EVENT_PROMOTION_RESPONSE)?;
        return Ok(());
    }
    if trim(&response.unwrap().bytes) != "approve" {
        return append(
            EVENT_STOPPED,
            &Completion {
                summary: "Promotion not approved.".into(),
            },
        );
    }
    if !has_operation(facts, EVENT_PROMOTION_AUTHORIZED, operation) {
        append(EVENT_PROMOTION_AUTHORIZED, operation)?;
        return Ok(());
    }
    checked_i64(call_provider(
        34,
        1,
        operation.operation,
        operation.source as u64,
    ))?;
    checked(promote_dynamic_workspace(
        generation.admitted_path_index as u64,
        operation.operation,
    ))?;
    append(EVENT_PROMOTED, operation)
}

fn command_cycle(
    facts: &[Fact],
    admission: &Admission,
    generations: &[Generation],
) -> Result<(), i32> {
    if let Some(result) = facts
        .iter()
        .rfind(|fact| fact.value == EVENT_COMMAND_RESULT)
    {
        let response = facts
            .iter()
            .find(|fact| fact.value == EVENT_CONTINUATION_RESPONSE && fact.index > result.index);
        let derived = response.is_some_and(|response| {
            facts.iter().any(|fact| {
                fact.index > response.index
                    && matches!(fact.value, EVENT_GENERATION | EVENT_COMPLETE)
            })
        });
        if !derived {
            let started = facts
                .iter()
                .find(|fact| fact.value == EVENT_COMMAND_STARTED && fact.index + 1 == result.index)
                .ok_or(4)?;
            let request: CommandRequest = parse(&started.bytes)?;
            let finished: CommandFinished = parse(&result.bytes)?;
            let expected = expected_sources(facts, admission, result.index)?;
            validate_command_finished(&finished, &request, admission, &expected)?;
            return continuation_cycle(facts, admission, generations, &finished, result.index);
        }
    }

    let attempt = facts
        .iter()
        .filter(|fact| fact.value == EVENT_COMMAND_RESULT)
        .count() as u64
        + 1;
    let request = CommandRequest {
        schema: 1,
        attempt,
        argv: admission.argv.clone(),
    };
    let prompt = facts.iter().rfind(|fact| {
        fact.value == EVENT_COMMAND_PROMPT
            && parse::<CommandRequest>(&fact.bytes).is_ok_and(|value| value == request)
    });
    if prompt.is_none() {
        append(EVENT_COMMAND_PROMPT, &request)?;
        return Ok(());
    }
    let prompt = prompt.unwrap();
    let decision = facts
        .iter()
        .rfind(|fact| fact.value == EVENT_COMMAND_DECISION && fact.index == prompt.index + 1);
    if decision.is_none() {
        perform_exchange(31, prompt.index, EVENT_COMMAND_DECISION)?;
        return Ok(());
    }
    match trim(&decision.unwrap().bytes) {
        "stop" => {
            return append(
                EVENT_STOPPED,
                &Completion {
                    summary: "Stopped by user.".into(),
                },
            );
        }
        "approve" => {}
        _ => return Err(4),
    }
    let started = facts.iter().rfind(|fact| {
        fact.value == EVENT_COMMAND_STARTED
            && parse::<CommandRequest>(&fact.bytes).is_ok_and(|value| value == request)
    });
    if started.is_none() {
        append(EVENT_COMMAND_STARTED, &request)?;
        return Ok(());
    }
    let started = started.unwrap();
    let result = facts
        .iter()
        .rfind(|fact| fact.value == EVENT_COMMAND_RESULT && fact.index == started.index + 1);
    if result.is_none() {
        perform_exchange(32, started.index, EVENT_COMMAND_RESULT)?;
        return Ok(());
    }
    let result = result.unwrap();
    let finished: CommandFinished = parse(&result.bytes)?;
    let expected = expected_sources(facts, admission, result.index)?;
    validate_command_finished(&finished, &request, admission, &expected)?;
    continuation_cycle(facts, admission, generations, &finished, result.index)
}

fn continuation_cycle(
    facts: &[Fact],
    admission: &Admission,
    generations: &[Generation],
    finished: &CommandFinished,
    result_index: u64,
) -> Result<(), i32> {
    let prompt = facts
        .iter()
        .rfind(|fact| fact.value == EVENT_CONTINUATION_PROMPT && fact.index > result_index);
    if prompt.is_none() {
        append_bytes(
            EVENT_CONTINUATION_PROMPT,
            &continuation_prompt(admission, generations, finished)?,
        )?;
        return Ok(());
    }
    let prompt = prompt.unwrap();
    let response = facts
        .iter()
        .rfind(|fact| fact.value == EVENT_CONTINUATION_RESPONSE && fact.index == prompt.index + 1);
    if response.is_none() {
        perform_exchange(30, prompt.index, EVENT_CONTINUATION_RESPONSE)?;
        return Ok(());
    }
    match parse_continuation_response(&response.unwrap().bytes, admission, generations, finished)? {
        ContinuationResponse::Complete(summary) => append(EVENT_COMPLETE, &Completion { summary }),
        ContinuationResponse::Proposal {
            admitted_path_index,
            proposal,
        } => {
            let current = generations
                .iter()
                .find(|generation| generation.admitted_path_index == admitted_path_index);
            let proposal_index = current.map_or_else(
                || {
                    generations
                        .iter()
                        .map(|item| item.proposal_index)
                        .max()
                        .map_or(0, |value| value + 1)
                },
                |generation| generation.proposal_index,
            );
            let revision = current.map_or(Ok(0), |generation| {
                generation.revision.checked_add(1).ok_or(4)
            })?;
            append(
                EVENT_GENERATION,
                &Generation {
                    proposal_index,
                    admitted_path_index,
                    revision,
                    proposal,
                },
            )
        }
    }
}

enum ContinuationResponse {
    Complete(String),
    Proposal {
        admitted_path_index: usize,
        proposal: Proposal,
    },
}

fn initial_prompt(admission: &Admission) -> Result<Vec<u8>, i32> {
    bounded_json(&json!({
        "schema": 2,
        "instructions": INITIAL_INSTRUCTIONS,
        "task": admission.task,
        "files": admission.files.iter().map(|file| json!({
            "path": file.path,
            "before_sha256": file.source_sha256,
            "content": file.content,
        })).collect::<Vec<_>>(),
        "required_response": {"proposals": [{
            "path": "one admitted path",
            "source_sha256": "that file's admitted SHA-256",
            "byte_start": "inclusive UTF-8 byte offset",
            "byte_end": "exclusive UTF-8 byte offset",
            "replacement": "exact UTF-8 replacement text, which may be empty"
        }]}
    }))
}

fn revision_prompt(
    admission: &Admission,
    generation: &Generation,
    operation: &Operation,
    feedback: &str,
) -> Result<Vec<u8>, i32> {
    bounded_json(&json!({
        "schema": 2,
        "instructions": REVISION_INSTRUCTIONS,
        "task": admission.task,
        "operation": operation,
        "rejection": {
            "proposal_index": generation.proposal_index,
            "admitted_path_index": generation.admitted_path_index,
            "source": generation.proposal.source,
            "path": generation.proposal.path,
            "source_sha256": generation.proposal.source_sha256,
            "byte_start": generation.proposal.byte_start,
            "byte_end": generation.proposal.byte_end,
            "prior_replacement": generation.proposal.replacement,
            "prior_result_sha256": generation.proposal.result_sha256,
            "feedback": feedback,
        },
        "required_response": {"proposal": {
            "path": "the rejected admitted path",
            "source_sha256": "the rejected generation's source SHA-256",
            "byte_start": "inclusive UTF-8 byte offset",
            "byte_end": "exclusive UTF-8 byte offset",
            "replacement": "exact corrected UTF-8 replacement text"
        }}
    }))
}

fn continuation_prompt(
    admission: &Admission,
    generations: &[Generation],
    finished: &CommandFinished,
) -> Result<Vec<u8>, i32> {
    let sources = continuation_sources(admission, finished)?;
    bounded_json(&json!({
        "schema": 2,
        "instructions": CONTINUATION_INSTRUCTIONS,
        "task": admission.task,
        "admitted_sources": sources.iter().enumerate().map(|(index, source)| json!({
            "admitted_path_index": index,
            "path": source.path,
            "sha256": source.source_sha256,
            "content": source.content,
        })).collect::<Vec<_>>(),
        "current_proposals": generations,
        "command_finished": finished,
        "required_response": [
            "PROPOSE <admitted-path-index>\\n{\"source_sha256\":\"<post-command source SHA-256>\",\"byte_start\":<inclusive UTF-8 byte offset>,\"byte_end\":<exclusive UTF-8 byte offset>,\"replacement\":\"<exact UTF-8 replacement>\"}",
            "COMPLETE\\n<bounded final summary>"
        ]
    }))
}

fn bounded_json(value: &Value) -> Result<Vec<u8>, i32> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| 4)?;
    if bytes.is_empty() || bytes.len() > MAX_MODEL_BYTES {
        Err(4)
    } else {
        Ok(bytes)
    }
}

fn parse_initial_response(bytes: &[u8], admission: &Admission) -> Result<Vec<Proposal>, i32> {
    check_response_bound(bytes)?;
    let value: Value = parse(bytes)?;
    let object = exact_object(&value, &["proposals"])?;
    let proposals = object.get("proposals").and_then(Value::as_array).ok_or(4)?;
    if proposals.len() < 2 || proposals.len() > admission.files.len() {
        return Err(4);
    }
    let mut seen = BTreeSet::new();
    proposals
        .iter()
        .map(|value| {
            let wire = exact_object(
                value,
                &[
                    "path",
                    "source_sha256",
                    "byte_start",
                    "byte_end",
                    "replacement",
                ],
            )?;
            let path = string_field(wire, "path")?;
            if !seen.insert(path.to_owned()) {
                return Err(4);
            }
            let admitted = admission
                .files
                .iter()
                .find(|file| file.path == path)
                .ok_or(4)?;
            parse_ranged_edit(wire, admitted, true)
        })
        .collect()
}

fn parse_revision_response(bytes: &[u8], generation: &Generation) -> Result<Proposal, i32> {
    check_response_bound(bytes)?;
    let value: Value = parse(bytes)?;
    let object = exact_object(&value, &["proposal"])?;
    let wire = exact_object(
        object.get("proposal").ok_or(4)?,
        &[
            "path",
            "source_sha256",
            "byte_start",
            "byte_end",
            "replacement",
        ],
    )?;
    if string_field(wire, "path")? != generation.proposal.path {
        return Err(4);
    }
    let admitted = AdmittedFile {
        path: generation.proposal.path.clone(),
        source_sha256: generation.proposal.source_sha256.clone(),
        content: generation.proposal.source.clone(),
    };
    let proposal = parse_ranged_edit(wire, &admitted, true)?;
    if proposal.result == generation.proposal.result {
        return Err(4);
    }
    Ok(proposal)
}

fn parse_continuation_response(
    bytes: &[u8],
    admission: &Admission,
    _generations: &[Generation],
    finished: &CommandFinished,
) -> Result<ContinuationResponse, i32> {
    check_response_bound(bytes)?;
    if let Some(summary) = bytes.strip_prefix(b"COMPLETE\n") {
        if !finished.succeeded()
            || summary.is_empty()
            || summary.len() > MAX_COMPLETION_BYTES
            || std::str::from_utf8(summary)
                .map_err(|_| 4)?
                .trim()
                .is_empty()
        {
            return Err(4);
        }
        return Ok(ContinuationResponse::Complete(
            String::from_utf8(summary.to_vec()).map_err(|_| 4)?,
        ));
    }
    let remainder = bytes.strip_prefix(b"PROPOSE ").ok_or(4)?;
    let newline = remainder.iter().position(|byte| *byte == b'\n').ok_or(4)?;
    let index = std::str::from_utf8(&remainder[..newline]).map_err(|_| 4)?;
    let admitted_path_index: usize = index.parse().map_err(|_| 4)?;
    if admitted_path_index.to_string() != index {
        return Err(4);
    }
    let sources = continuation_sources(admission, finished)?;
    let admitted = sources.get(admitted_path_index).ok_or(4)?;
    let value: Value = parse(&remainder[newline + 1..])?;
    let wire = exact_object(
        &value,
        &["source_sha256", "byte_start", "byte_end", "replacement"],
    )?;
    Ok(ContinuationResponse::Proposal {
        admitted_path_index,
        proposal: parse_ranged_edit(wire, admitted, false)?,
    })
}

fn parse_ranged_edit(
    wire: &Map<String, Value>,
    admitted: &AdmittedFile,
    has_path: bool,
) -> Result<Proposal, i32> {
    let source_sha256 = string_field(wire, "source_sha256")?.to_owned();
    if !valid_sha256(&source_sha256) || source_sha256 != admitted.source_sha256 {
        return Err(4);
    }
    let byte_start = usize_field(wire, "byte_start")?;
    let byte_end = usize_field(wire, "byte_end")?;
    if byte_start > byte_end
        || byte_end > admitted.content.len()
        || !admitted.content.is_char_boundary(byte_start)
        || !admitted.content.is_char_boundary(byte_end)
    {
        return Err(4);
    }
    let replacement = string_field(wire, "replacement")?.to_owned();
    let result_len = admitted
        .content
        .len()
        .checked_sub(byte_end - byte_start)
        .and_then(|value| value.checked_add(replacement.len()))
        .ok_or(4)?;
    if result_len == 0 || result_len > MAX_SOURCE_BYTES {
        return Err(4);
    }
    let mut result = String::with_capacity(result_len);
    result.push_str(&admitted.content[..byte_start]);
    result.push_str(&replacement);
    result.push_str(&admitted.content[byte_end..]);
    if result == admitted.content {
        return Err(4);
    }
    let path = if has_path {
        string_field(wire, "path")?.to_owned()
    } else {
        admitted.path.clone()
    };
    Ok(Proposal {
        path,
        source: admitted.content.clone(),
        source_sha256,
        byte_start,
        byte_end,
        replacement,
        result_sha256: sha256(result.as_bytes()),
        result,
    })
}

fn current_generations(facts: &[Fact], admission: &Admission) -> Result<Vec<Generation>, i32> {
    let mut current = Vec::<Generation>::new();
    for fact in facts.iter().filter(|fact| fact.value == EVENT_GENERATION) {
        let generation: Generation = parse(&fact.bytes)?;
        validate_generation(&generation, admission)?;
        if let Some(existing) = current
            .iter_mut()
            .find(|item| item.proposal_index == generation.proposal_index)
        {
            if generation.revision != existing.revision.checked_add(1).ok_or(4)?
                || generation.admitted_path_index != existing.admitted_path_index
            {
                return Err(4);
            }
            *existing = generation;
        } else {
            if generation.revision != 0
                || current
                    .iter()
                    .any(|item| item.admitted_path_index == generation.admitted_path_index)
            {
                return Err(4);
            }
            current.push(generation);
        }
    }
    Ok(current)
}

fn validate_generation(generation: &Generation, admission: &Admission) -> Result<(), i32> {
    let file = admission
        .files
        .get(generation.admitted_path_index)
        .ok_or(4)?;
    let proposal = &generation.proposal;
    if proposal.path != file.path
        || !valid_sha256(&proposal.source_sha256)
        || proposal.source_sha256 != sha256(proposal.source.as_bytes())
        || proposal.byte_start > proposal.byte_end
        || proposal.byte_end > proposal.source.len()
        || !proposal.source.is_char_boundary(proposal.byte_start)
        || !proposal.source.is_char_boundary(proposal.byte_end)
        || proposal.result_sha256 != sha256(proposal.result.as_bytes())
        || proposal.result.is_empty()
        || proposal.result.len() > MAX_SOURCE_BYTES
    {
        return Err(4);
    }
    let mut reconstructed = String::new();
    reconstructed.push_str(&proposal.source[..proposal.byte_start]);
    reconstructed.push_str(&proposal.replacement);
    reconstructed.push_str(&proposal.source[proposal.byte_end..]);
    if reconstructed != proposal.result {
        return Err(4);
    }
    Ok(())
}

fn expected_sources(
    facts: &[Fact],
    admission: &Admission,
    before_index: u64,
) -> Result<Vec<String>, i32> {
    let mut sources = admission
        .files
        .iter()
        .map(|file| file.content.clone())
        .collect::<Vec<_>>();
    for fact in facts.iter().filter(|fact| fact.index < before_index) {
        match fact.value {
            EVENT_GENERATION => {
                let generation: Generation = parse(&fact.bytes)?;
                let target = sources.get_mut(generation.admitted_path_index).ok_or(4)?;
                *target = generation.proposal.result;
            }
            EVENT_COMMAND_RESULT => {
                let finished: CommandFinished = parse(&fact.bytes)?;
                sources = continuation_sources(admission, &finished)?
                    .into_iter()
                    .map(|file| file.content)
                    .collect();
            }
            _ => {}
        }
    }
    Ok(sources)
}

fn validate_command_finished(
    finished: &CommandFinished,
    request: &CommandRequest,
    admission: &Admission,
    expected_sources: &[String],
) -> Result<(), i32> {
    if finished.schema != 1
        || finished.kind != "CommandFinished"
        || finished.attempt != request.attempt
        || finished.argv != request.argv
        || !valid_sha256(&finished.command_started_sha256)
        || finished.repository.canonical_root != finished.repository_after.canonical_root
        || finished.repository.canonical_root_sha256
            != sha256(finished.repository.canonical_root.as_bytes())
        || finished.repository_after.canonical_root_sha256
            != finished.repository.canonical_root_sha256
        || finished.repository.files.len() != admission.files.len()
        || finished.repository_after.files.len() != admission.files.len()
        || (finished.spawn_error.is_some()
            && (finished.exit_code.is_some() || finished.signal.is_some() || finished.timed_out))
        || (finished.spawn_error.is_none()
            && finished.exit_code.is_none()
            && finished.signal.is_none())
        || !valid_output(&finished.stdout)
        || !valid_output(&finished.stderr)
    {
        return Err(4);
    }
    if expected_sources.len() != admission.files.len() {
        return Err(4);
    }
    for ((admitted, file), content) in admission
        .files
        .iter()
        .zip(&finished.repository.files)
        .zip(expected_sources)
    {
        if file.path != admitted.path
            || file.status != "regular"
            || file.content.as_deref() != Some(content)
            || file.byte_len != u64::try_from(content.len()).ok()
            || file.sha256.as_deref() != Some(&sha256(content.as_bytes()))
        {
            return Err(4);
        }
    }
    continuation_sources(admission, finished).map(|_| ())
}

fn valid_output(output: &BoundedOutput) -> bool {
    let encoded_len = match output.encoding.as_str() {
        "utf-8" => output.content.len(),
        "hex"
            if output.content.len().is_multiple_of(2)
                && output
                    .content
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) =>
        {
            output.content.len() / 2
        }
        _ => return false,
    };
    encoded_len <= 32 * 1024
        && output
            .read_error
            .as_ref()
            .is_none_or(|error| error.len() <= 4 * 1024)
}

fn continuation_sources(
    admission: &Admission,
    finished: &CommandFinished,
) -> Result<Vec<AdmittedFile>, i32> {
    let mut sources = Vec::with_capacity(admission.files.len());
    for (admitted, file) in admission.files.iter().zip(&finished.repository_after.files) {
        let content = file.content.clone().ok_or(4)?;
        if file.path != admitted.path
            || file.status != "regular"
            || file.byte_len != u64::try_from(content.len()).ok()
            || file.sha256.as_deref() != Some(&sha256(content.as_bytes()))
            || content.is_empty()
            || content.len() > MAX_SOURCE_BYTES
        {
            return Err(4);
        }
        sources.push(AdmittedFile {
            path: file.path.clone(),
            source_sha256: sha256(content.as_bytes()),
            content,
        });
    }
    Ok(sources)
}

fn validate_external_adjacency(facts: &[Fact]) -> Result<(), i32> {
    for (index, fact) in facts.iter().enumerate() {
        let previous = index.checked_sub(1).and_then(|value| facts.get(value));
        let expected = match fact.value {
            EVENT_INITIAL_RESPONSE => Some(EVENT_INITIAL_PROMPT),
            EVENT_REVISION_RESPONSE => Some(EVENT_REVISION_PROMPT),
            EVENT_REVIEW_RESPONSE => Some(EVENT_REVIEW_PROMPT),
            EVENT_MUTATION_AUTHORIZED => Some(EVENT_REVIEW_RESPONSE),
            EVENT_CANDIDATE_APPLIED => Some(EVENT_MUTATION_AUTHORIZED),
            EVENT_PROMOTION_RESPONSE => Some(EVENT_PROMOTION_PROMPT),
            EVENT_PROMOTION_AUTHORIZED => Some(EVENT_PROMOTION_RESPONSE),
            EVENT_PROMOTED => Some(EVENT_PROMOTION_AUTHORIZED),
            EVENT_COMMAND_DECISION => Some(EVENT_COMMAND_PROMPT),
            EVENT_COMMAND_STARTED => Some(EVENT_COMMAND_DECISION),
            EVENT_COMMAND_RESULT => Some(EVENT_COMMAND_STARTED),
            EVENT_CONTINUATION_RESPONSE => Some(EVENT_CONTINUATION_PROMPT),
            _ => None,
        };
        if expected.is_some_and(|value| previous.is_none_or(|item| item.value != value)) {
            return Err(4);
        }
        if index > 0 && facts[index - 1].value == EVENT_COMPLETE
            || index > 0 && facts[index - 1].value == EVENT_STOPPED
        {
            return Err(4);
        }
    }
    Ok(())
}

fn matching_operation<'a, F>(
    facts: &'a [Fact],
    event: u64,
    expected: &Operation,
    parse_operation: F,
) -> Option<&'a Fact>
where
    F: Fn(&[u8]) -> Result<Operation, i32>,
{
    facts.iter().rfind(|fact| {
        fact.value == event
            && parse_operation(&fact.bytes).is_ok_and(|operation| operation == *expected)
    })
}

fn has_operation(facts: &[Fact], event: u64, expected: &Operation) -> bool {
    matching_operation(facts, event, expected, parse).is_some()
}

fn operation(generation: &Generation) -> Operation {
    let bytes = serde_json::to_vec(generation).unwrap();
    let digest = Sha256::digest(bytes);
    Operation {
        proposal_index: generation.proposal_index,
        source: generation.admitted_path_index,
        revision: generation.revision,
        operation: u64::from_le_bytes(digest[..8].try_into().unwrap()).max(1),
    }
}

fn render_diff(proposal: &Proposal) -> String {
    format!(
        "--- a/{0}\n+++ b/{0}\n@@ bytes {1}..{2} @@\n-{3}\n+{4}\n\nApprove with 'approve', reject with 'reject <feedback>', or stop with 'stop'.",
        proposal.path,
        proposal.byte_start,
        proposal.byte_end,
        proposal.source.replace('\n', "\n-"),
        proposal.result.replace('\n', "\n+"),
    )
}

fn validate_argv(argv: &[String]) -> Result<(), i32> {
    let total = argv.iter().try_fold(0usize, |total, argument| {
        if argument.is_empty() || argument.len() > MAX_ARG_BYTES {
            return Err(4);
        }
        total.checked_add(argument.len()).ok_or(4)
    })?;
    if argv.is_empty() || argv.len() > MAX_ARGC || total > MAX_ARGV_BYTES {
        Err(4)
    } else {
        Ok(())
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn exact_object<'a>(value: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>, i32> {
    let object = value.as_object().ok_or(4)?;
    if object.len() != keys.len() || keys.iter().any(|key| !object.contains_key(*key)) {
        Err(4)
    } else {
        Ok(object)
    }
}

fn string_field<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, i32> {
    object.get(key).and_then(Value::as_str).ok_or(4)
}

fn usize_field(object: &Map<String, Value>, key: &str) -> Result<usize, i32> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or(4)
}

fn check_response_bound(bytes: &[u8]) -> Result<(), i32> {
    if bytes.is_empty() || bytes.len() > MAX_RESPONSE_BYTES {
        Err(4)
    } else {
        Ok(())
    }
}

fn facts() -> Result<Vec<Fact>, i32> {
    let count = checked_i64(event_count())? as u64;
    let mut facts = Vec::with_capacity(count as usize);
    for index in 0..count {
        let value = checked_i64(read_event(index))? as u64;
        let length = checked_i64(event_payload_len(index))? as u64;
        let mut bytes = Vec::with_capacity(length as usize);
        for offset in 0..length {
            bytes.push(checked_i32(event_payload_byte(index, offset))? as u8);
        }
        facts.push(Fact {
            index,
            value,
            bytes,
        });
    }
    Ok(facts)
}

fn snapshot(index: u64) -> Result<Vec<u8>, i32> {
    let length = checked_i64(snapshot_len(index))? as u64;
    let mut bytes = Vec::with_capacity(length as usize);
    for offset in 0..length {
        bytes.push(checked_i32(snapshot_byte(index, offset))? as u8);
    }
    Ok(bytes)
}

fn workspace(index: u64) -> Result<Vec<u8>, i32> {
    let length = checked_i64(workspace_len(index))? as u64;
    let mut bytes = Vec::with_capacity(length as usize);
    for offset in 0..length {
        bytes.push(checked_i32(workspace_byte(index, offset))? as u8);
    }
    Ok(bytes)
}

fn write_workspace(index: u64, bytes: &[u8]) -> Result<(), i32> {
    checked(workspace_set_len(index, bytes.len() as u64))?;
    for (offset, byte) in bytes.iter().copied().enumerate() {
        checked(workspace_write_byte(index, offset as u64, byte as u32))?;
    }
    Ok(())
}

fn append<T: Serialize>(value: u64, payload: &T) -> Result<(), i32> {
    append_bytes(value, &serde_json::to_vec(payload).map_err(|_| 4)?)
}

fn append_bytes(value: u64, bytes: &[u8]) -> Result<(), i32> {
    checked(event_buffer_set_len(bytes.len() as u64))?;
    for (offset, byte) in bytes.iter().copied().enumerate() {
        checked(event_buffer_write_byte(offset as u64, byte as u32))?;
    }
    checked(continue_buffered_event(0, value))?;
    Ok(())
}

fn perform_exchange(slot: u64, request_index: u64, response_value: u64) -> Result<(), i32> {
    checked_i64(call_provider(
        slot,
        1,
        request_index,
        invocation(slot, request_index),
    ))?;
    checked(continue_exchange(0, response_value))?;
    Ok(())
}

fn invocation(slot: u64, request_index: u64) -> u64 {
    let mut hash = Sha256::new();
    hash.update(slot.to_le_bytes());
    hash.update(request_index.to_le_bytes());
    let bytes = hash.finalize();
    u64::from_le_bytes(bytes[..8].try_into().unwrap()).max(1)
}

fn checked(status: i32) -> Result<i32, i32> {
    if status == 0 { Ok(status) } else { Err(status) }
}

fn checked_i32(value: i32) -> Result<i32, i32> {
    if value < 0 { Err(-value) } else { Ok(value) }
}

fn checked_i64(value: i64) -> Result<i64, i32> {
    if value < 0 {
        Err((-value) as i32)
    } else {
        Ok(value)
    }
}

fn parse<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, i32> {
    serde_json::from_slice(bytes).map_err(|_| 4)
}

fn trim(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap_or("").trim()
}

fn sha256(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").unwrap();
    }
    output
}

export!(RepositoryTask);

#[cfg(test)]
mod tests {
    use super::*;

    fn admission() -> Admission {
        Admission {
            schema: 2,
            task: "change both files".into(),
            argv: vec!["cargo".into(), "test".into()],
            files: vec![
                AdmittedFile {
                    path: "a.txt".into(),
                    source_sha256: sha256(b"alpha\n"),
                    content: "alpha\n".into(),
                },
                AdmittedFile {
                    path: "b.txt".into(),
                    source_sha256: sha256(b"beta\n"),
                    content: "beta\n".into(),
                },
            ],
        }
    }

    #[test]
    fn strict_ranges_reject_unknown_digest_and_split_utf8() {
        let admission = admission();
        let valid = json!({"proposals": [
            {"path":"a.txt","source_sha256":admission.files[0].source_sha256,"byte_start":0,"byte_end":6,"replacement":"alpha revised\n"},
            {"path":"b.txt","source_sha256":admission.files[1].source_sha256,"byte_start":0,"byte_end":5,"replacement":"beta revised\n"}
        ]});
        assert_eq!(
            parse_initial_response(&serde_json::to_vec(&valid).unwrap(), &admission)
                .unwrap()
                .len(),
            2
        );
        let mut digest = valid.clone();
        digest["proposals"][0]["result_sha256"] = Value::String("0".repeat(64));
        assert!(parse_initial_response(&serde_json::to_vec(&digest).unwrap(), &admission).is_err());
        let utf8 = AdmittedFile {
            path: "a.txt".into(),
            source_sha256: sha256("é\n".as_bytes()),
            content: "é\n".into(),
        };
        let split = json!({"source_sha256":utf8.source_sha256,"byte_start":1,"byte_end":1,"replacement":"x"});
        assert!(parse_ranged_edit(split.as_object().unwrap(), &utf8, false).is_err());
    }

    #[test]
    fn completion_requires_success_and_canonical_grammar() {
        let admission = admission();
        let finished = CommandFinished {
            argv: admission.argv.clone(),
            attempt: 1,
            command_started_sha256: "0".repeat(64),
            duration_ms: 1,
            exit_code: Some(1),
            kind: "CommandFinished".into(),
            repository: RepositoryIdentity {
                canonical_root: "/tmp".into(),
                canonical_root_sha256: "0".repeat(64),
                files: vec![],
            },
            repository_after: RepositoryIdentity {
                canonical_root: "/tmp".into(),
                canonical_root_sha256: "0".repeat(64),
                files: vec![],
            },
            schema: 1,
            signal: None,
            spawn_error: None,
            stderr: BoundedOutput {
                content: String::new(),
                encoding: "utf-8".into(),
                read_error: None,
                truncated: false,
            },
            stdout: BoundedOutput {
                content: String::new(),
                encoding: "utf-8".into(),
                read_error: None,
                truncated: false,
            },
            timed_out: false,
        };
        assert!(
            parse_continuation_response(b"COMPLETE\npassed", &admission, &[], &finished).is_err()
        );
        let mut success = finished;
        success.exit_code = Some(0);
        assert!(matches!(
            parse_continuation_response(b"COMPLETE\npassed", &admission, &[], &success),
            Ok(ContinuationResponse::Complete(_))
        ));
        assert!(
            parse_continuation_response(b"complete\npassed", &admission, &[], &success).is_err()
        );
    }

    fn repository(files: &[(&str, &str)]) -> RepositoryIdentity {
        RepositoryIdentity {
            canonical_root: "/tmp/repository".into(),
            canonical_root_sha256: sha256(b"/tmp/repository"),
            files: files
                .iter()
                .map(|(path, content)| RepositoryFile {
                    byte_len: Some(content.len() as u64),
                    content: Some((*content).into()),
                    path: (*path).into(),
                    sha256: Some(sha256(content.as_bytes())),
                    status: "regular".into(),
                })
                .collect(),
        }
    }

    #[test]
    fn repeated_revisions_and_continuations_use_strict_current_sources() {
        let admission = admission();
        let proposal = Proposal {
            path: "a.txt".into(),
            source_sha256: admission.files[0].source_sha256.clone(),
            source: "alpha\n".into(),
            byte_start: 0,
            byte_end: 6,
            replacement: "alpha one\n".into(),
            result_sha256: sha256(b"alpha one\n"),
            result: "alpha one\n".into(),
        };
        let first = Generation {
            proposal_index: 0,
            admitted_path_index: 0,
            revision: 0,
            proposal,
        };
        let revision = json!({"proposal":{
            "path":"a.txt",
            "source_sha256":admission.files[0].source_sha256,
            "byte_start":0,
            "byte_end":6,
            "replacement":"alpha two\n"
        }});
        let second_proposal =
            parse_revision_response(&serde_json::to_vec(&revision).unwrap(), &first).unwrap();
        let second = Generation {
            proposal_index: 0,
            admitted_path_index: 0,
            revision: 1,
            proposal: second_proposal,
        };
        let revision = json!({"proposal":{
            "path":"a.txt",
            "source_sha256":admission.files[0].source_sha256,
            "byte_start":0,
            "byte_end":6,
            "replacement":"alpha three\n"
        }});
        assert_eq!(
            parse_revision_response(&serde_json::to_vec(&revision).unwrap(), &second)
                .unwrap()
                .result,
            "alpha three\n"
        );

        let request = CommandRequest {
            schema: 1,
            attempt: 1,
            argv: admission.argv.clone(),
        };
        let repository = repository(&[("a.txt", "alpha two\n"), ("b.txt", "beta\n")]);
        let finished = CommandFinished {
            argv: request.argv.clone(),
            attempt: 1,
            command_started_sha256: sha256(&serde_json::to_vec(&request).unwrap()),
            duration_ms: 1,
            exit_code: Some(1),
            kind: "CommandFinished".into(),
            repository: repository.clone(),
            repository_after: repository,
            schema: 1,
            signal: None,
            spawn_error: None,
            stderr: BoundedOutput {
                content: String::new(),
                encoding: "utf-8".into(),
                read_error: None,
                truncated: false,
            },
            stdout: BoundedOutput {
                content: String::new(),
                encoding: "utf-8".into(),
                read_error: None,
                truncated: false,
            },
            timed_out: false,
        };
        let response = format!(
            "PROPOSE 0\n{}",
            json!({
                "source_sha256": sha256(b"alpha two\n"),
                "byte_start": 0,
                "byte_end": 10,
                "replacement": "alpha fixed\n"
            })
        );
        let ContinuationResponse::Proposal {
            admitted_path_index,
            proposal,
        } = parse_continuation_response(response.as_bytes(), &admission, &[second], &finished)
            .unwrap()
        else {
            panic!("expected proposal")
        };
        assert_eq!(admitted_path_index, 0);
        assert_eq!(proposal.source, "alpha two\n");
        assert_eq!(proposal.result, "alpha fixed\n");
    }

    #[test]
    fn command_completion_binds_request_argv_and_repository_bytes() {
        let admission = admission();
        let request = CommandRequest {
            schema: 1,
            attempt: 2,
            argv: admission.argv.clone(),
        };
        let repository = repository(&[("a.txt", "alpha\n"), ("b.txt", "beta\n")]);
        let mut finished = CommandFinished {
            argv: request.argv.clone(),
            attempt: request.attempt,
            command_started_sha256: sha256(&serde_json::to_vec(&request).unwrap()),
            duration_ms: 1,
            exit_code: Some(0),
            kind: "CommandFinished".into(),
            repository: repository.clone(),
            repository_after: repository,
            schema: 1,
            signal: None,
            spawn_error: None,
            stderr: BoundedOutput {
                content: String::new(),
                encoding: "utf-8".into(),
                read_error: None,
                truncated: false,
            },
            stdout: BoundedOutput {
                content: String::new(),
                encoding: "utf-8".into(),
                read_error: None,
                truncated: false,
            },
            timed_out: false,
        };
        let expected = ["alpha\n".to_owned(), "beta\n".to_owned()];
        assert!(validate_command_finished(&finished, &request, &admission, &expected).is_ok());
        finished.argv.push("--changed".into());
        assert!(validate_command_finished(&finished, &request, &admission, &expected).is_err());
        finished.argv = request.argv.clone();
        finished.repository_after.files[0].content = Some("tampered\n".into());
        assert!(validate_command_finished(&finished, &request, &admission, &expected).is_err());

        let mut wire = serde_json::to_value(&finished).unwrap();
        wire.as_object_mut()
            .unwrap()
            .insert("unknown".into(), Value::Bool(true));
        assert!(parse::<CommandFinished>(&serde_json::to_vec(&wire).unwrap()).is_err());
    }
}
