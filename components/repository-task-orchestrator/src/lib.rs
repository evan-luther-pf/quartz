#![no_std]

extern crate alloc;

use alloc::{
    borrow::ToOwned,
    collections::BTreeSet,
    string::{String, ToString},
    vec::Vec,
};
use core::panic::PanicInfo;
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
#[global_allocator]
static ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

#[panic_handler]
fn panic(_: &PanicInfo<'_>) -> ! {
    core::arch::wasm32::unreachable()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn memcmp(left: *const u8, right: *const u8, length: usize) -> i32 {
    for index in 0..length {
        let (left, right) = unsafe { (*left.add(index), *right.add(index)) };
        if left != right {
            return i32::from(left) - i32::from(right);
        }
    }
    0
}

wit_bindgen::generate!({
    path: "../../wit",
    world: "module",
});

const INVALID: i32 = 2;
const DENIED: i32 = 7;
const MAX_FACT_BYTES: u64 = 512 * 1024;

struct Orchestrator;

impl Guest for Orchestrator {
    fn start(config: u64) -> u64 {
        config
    }

    fn step(fact_code: u64) -> i32 {
        match commit_fact(fact_code) {
            Ok(()) => 1,
            Err(status) => -status,
        }
    }

    fn invoke(_instance: u64, _operation: u64, _arg0: u64, _arg1: u64) -> i64 {
        -4
    }

    fn drop(_instance: u64) {}
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "kind")]
enum ModelTurn {
    Initial,
    Revision {
        proposal_index: usize,
        revision: u32,
    },
    Continuation {
        sequence: u32,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case", tag = "kind")]
enum SessionFact {
    ModelStarted {
        turn: ModelTurn,
        model: String,
        prompt_sha256: String,
        prompt: String,
    },
    ModelCompleted {
        turn: ModelTurn,
        prompt_sha256: String,
        response_sha256: String,
        response: String,
        provenance: String,
    },
    ProposalRejected {
        proposal_index: usize,
        revision: u32,
        candidate_sha256: String,
        model: String,
        feedback: String,
    },
    ProposalApproved {
        proposal_index: usize,
        revision: u32,
        candidate_sha256: String,
    },
    PromotionStarted {
        proposal_index: usize,
        revision: u32,
        candidate_sha256: String,
        operation: u64,
    },
    ProposalPromoted {
        proposal_index: usize,
        revision: u32,
        candidate_sha256: String,
        operation: u64,
    },
    CommandStarted {
        attempt: u64,
        payload_sha256: String,
        payload: String,
    },
    CommandFinished {
        attempt: u64,
        start_sha256: String,
        payload_sha256: String,
        payload: String,
    },
    TaskCompleted {
        sequence: u32,
        prompt_sha256: String,
        response_sha256: String,
        response: String,
        provenance: String,
    },
}

fn commit_fact(code: u64) -> Result<(), i32> {
    let count = checked_i64(event_count())?;
    let previous_code = if count == 0 {
        0
    } else {
        checked_i64(read_event(count - 1))?
    };
    if !allowed(previous_code, code) {
        return Err(DENIED);
    }

    let bytes = read_snapshot()?;
    let fact: SessionFact = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
    if fact_code(&fact) != code {
        return Err(DENIED);
    }
    let history = read_history(count)?;
    validate_fact(&fact, &history)?;

    checked_status(event_output_set_len(0, bytes.len() as u64))?;
    for (offset, byte) in bytes.into_iter().enumerate() {
        checked_status(event_output_write_byte(0, offset as u64, u32::from(byte)))?;
    }
    checked_status(resume_event_output(0, 0, code))
}

fn read_snapshot() -> Result<Vec<u8>, i32> {
    let length = checked_i64(snapshot_len(0))?;
    if length == 0 || length > MAX_FACT_BYTES {
        return Err(INVALID);
    }
    let mut bytes = Vec::with_capacity(length as usize);
    for offset in 0..length {
        let byte = snapshot_byte(0, offset);
        if byte < 0 {
            return Err(-byte);
        }
        bytes.push(byte as u8);
    }
    Ok(bytes)
}

fn read_history(count: u64) -> Result<Vec<SessionFact>, i32> {
    let mut facts = Vec::with_capacity(count as usize);
    for index in 0..count {
        let length = checked_i64(event_payload_len(index))?;
        if length == 0 || length > MAX_FACT_BYTES {
            return Err(INVALID);
        }
        let mut bytes = Vec::with_capacity(length as usize);
        for offset in 0..length {
            let byte = event_payload_byte(index, offset);
            if byte < 0 {
                return Err(-byte);
            }
            bytes.push(byte as u8);
        }
        let fact: SessionFact = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
        if fact_code(&fact) != checked_i64(read_event(index))? {
            return Err(DENIED);
        }
        facts.push(fact);
    }
    Ok(facts)
}

fn validate_fact(fact: &SessionFact, history: &[SessionFact]) -> Result<(), i32> {
    match fact {
        SessionFact::ModelStarted {
            turn,
            model,
            prompt_sha256,
            prompt,
        } => {
            if model.is_empty() || prompt.is_empty() || sha256(prompt.as_bytes()) != *prompt_sha256
            {
                return Err(DENIED);
            }
            validate_model_start(turn, model, history)
        }
        SessionFact::ModelCompleted {
            turn,
            prompt_sha256,
            response_sha256,
            response,
            provenance,
        } => {
            let Some(SessionFact::ModelStarted {
                turn: started_turn,
                prompt_sha256: started_sha256,
                prompt,
                ..
            }) = history.last()
            else {
                return Err(DENIED);
            };
            if turn != started_turn
                || prompt_sha256 != started_sha256
                || provenance.is_empty()
                || response.is_empty()
                || sha256(response.as_bytes()) != *response_sha256
            {
                return Err(DENIED);
            }
            validate_model_response(turn, prompt, response)
        }
        SessionFact::ProposalRejected {
            proposal_index,
            revision,
            candidate_sha256,
            model,
            feedback,
        } => validate_candidate_decision(*proposal_index, *revision, candidate_sha256, history)
            .and_then(|_| {
                if model.is_empty() || feedback.trim().is_empty() {
                    Err(DENIED)
                } else {
                    Ok(())
                }
            }),
        SessionFact::ProposalApproved {
            proposal_index,
            revision,
            candidate_sha256,
        } => validate_candidate_decision(*proposal_index, *revision, candidate_sha256, history),
        SessionFact::PromotionStarted {
            proposal_index,
            revision,
            candidate_sha256,
            operation,
        } => {
            let Some(SessionFact::ProposalApproved {
                proposal_index: approved_index,
                revision: approved_revision,
                candidate_sha256: approved_sha256,
            }) = history.last()
            else {
                return Err(DENIED);
            };
            if operation == &0
                || proposal_index != approved_index
                || revision != approved_revision
                || candidate_sha256 != approved_sha256
            {
                return Err(DENIED);
            }
            Ok(())
        }
        SessionFact::ProposalPromoted {
            proposal_index,
            revision,
            candidate_sha256,
            operation,
        } => {
            let Some(SessionFact::PromotionStarted {
                proposal_index: started_index,
                revision: started_revision,
                candidate_sha256: started_sha256,
                operation: started_operation,
            }) = history.last()
            else {
                return Err(DENIED);
            };
            if proposal_index != started_index
                || revision != started_revision
                || candidate_sha256 != started_sha256
                || operation != started_operation
            {
                return Err(DENIED);
            }
            Ok(())
        }
        SessionFact::CommandStarted {
            attempt,
            payload_sha256,
            payload,
        } => {
            let expected = history
                .iter()
                .filter(|fact| matches!(fact, SessionFact::CommandStarted { .. }))
                .count() as u64
                + 1;
            if *attempt != expected || sha256(payload.as_bytes()) != *payload_sha256 {
                return Err(DENIED);
            }
            serde_json::from_str::<Value>(payload).map_err(|_| INVALID)?;
            Ok(())
        }
        SessionFact::CommandFinished {
            attempt,
            start_sha256,
            payload_sha256,
            payload,
        } => {
            let Some(SessionFact::CommandStarted {
                attempt: started_attempt,
                payload_sha256: started_sha256,
                ..
            }) = history.last()
            else {
                return Err(DENIED);
            };
            if attempt != started_attempt
                || start_sha256 != started_sha256
                || sha256(payload.as_bytes()) != *payload_sha256
            {
                return Err(DENIED);
            }
            serde_json::from_str::<Value>(payload).map_err(|_| INVALID)?;
            Ok(())
        }
        SessionFact::TaskCompleted {
            sequence,
            prompt_sha256,
            response_sha256,
            response,
            provenance,
        } => {
            let Some(SessionFact::ModelStarted {
                turn:
                    ModelTurn::Continuation {
                        sequence: started_sequence,
                    },
                prompt_sha256: started_prompt_sha256,
                ..
            }) = history.last()
            else {
                return Err(DENIED);
            };
            if sequence != started_sequence
                || prompt_sha256 != started_prompt_sha256
                || provenance.is_empty()
                || !response.starts_with("COMPLETE\n")
                || response[9..].trim().is_empty()
                || sha256(response.as_bytes()) != *response_sha256
            {
                return Err(DENIED);
            }
            Ok(())
        }
    }
}

fn validate_model_start(turn: &ModelTurn, model: &str, history: &[SessionFact]) -> Result<(), i32> {
    match turn {
        ModelTurn::Initial if history.is_empty() => Ok(()),
        ModelTurn::Revision {
            proposal_index,
            revision,
        } => match history.last() {
            Some(SessionFact::ProposalRejected {
                proposal_index: rejected_index,
                revision: rejected_revision,
                model: rejected_model,
                ..
            }) if proposal_index == rejected_index
                && *revision == rejected_revision.saturating_add(1)
                && model == rejected_model =>
            {
                Ok(())
            }
            _ => Err(DENIED),
        },
        ModelTurn::Continuation { sequence } => {
            let expected = history
                .iter()
                .filter(|fact| {
                    matches!(
                        fact,
                        SessionFact::ModelStarted {
                            turn: ModelTurn::Continuation { .. },
                            ..
                        }
                    )
                })
                .count() as u32
                + 1;
            if *sequence == expected
                && matches!(history.last(), Some(SessionFact::CommandFinished { .. }))
            {
                Ok(())
            } else {
                Err(DENIED)
            }
        }
        _ => Err(DENIED),
    }
}

fn validate_model_response(turn: &ModelTurn, prompt: &str, response: &str) -> Result<(), i32> {
    match turn {
        ModelTurn::Initial => validate_initial_response(prompt, response),
        ModelTurn::Revision { .. } => validate_revision_response(prompt, response),
        ModelTurn::Continuation { .. } => validate_continuation_response(prompt, response),
    }
}

fn validate_initial_response(prompt: &str, response: &str) -> Result<(), i32> {
    let prompt: Value = serde_json::from_str(prompt).map_err(|_| INVALID)?;
    let files = exact_object(
        &prompt,
        &[
            "schema",
            "instructions",
            "task",
            "files",
            "required_response",
        ],
    )?
    .get("files")
    .and_then(Value::as_array)
    .ok_or(INVALID)?;
    if files.len() < 2 {
        return Err(DENIED);
    }
    let response: Value = serde_json::from_str(response).map_err(|_| INVALID)?;
    let proposals = exact_object(&response, &["proposals"])?
        .get("proposals")
        .and_then(Value::as_array)
        .ok_or(INVALID)?;
    if proposals.len() < 2 || proposals.len() > files.len() {
        return Err(DENIED);
    }
    let mut paths = BTreeSet::new();
    for proposal in proposals {
        let edit = exact_object(
            proposal,
            &[
                "path",
                "source_sha256",
                "byte_start",
                "byte_end",
                "replacement",
                "result_sha256",
            ],
        )?;
        let path = string(edit, "path")?;
        if !paths.insert(path) {
            return Err(DENIED);
        }
        let file = files
            .iter()
            .find(|file| file.get("path").and_then(Value::as_str) == Some(path))
            .ok_or(DENIED)?;
        validate_ranged_edit(
            edit,
            exact_object(file, &["path", "before_sha256", "content"])?,
        )?;
    }
    Ok(())
}

fn validate_revision_response(prompt: &str, response: &str) -> Result<(), i32> {
    let prompt: Value = serde_json::from_str(prompt).map_err(|_| INVALID)?;
    let prompt = exact_object(
        &prompt,
        &[
            "schema",
            "instructions",
            "model",
            "admission",
            "rejection",
            "required_response",
        ],
    )?;
    let admission = exact_object(
        prompt.get("admission").ok_or(INVALID)?,
        &["prompt_sha256", "task", "files"],
    )?;
    let rejected = exact_object(
        prompt.get("rejection").ok_or(INVALID)?,
        &[
            "proposal_index",
            "path",
            "source_sha256",
            "byte_start",
            "byte_end",
            "prior_replacement",
            "prior_result_sha256",
            "feedback",
        ],
    )?;
    let response: Value = serde_json::from_str(response).map_err(|_| INVALID)?;
    let edit = exact_object(
        exact_object(&response, &["proposal"])?
            .get("proposal")
            .ok_or(INVALID)?,
        &[
            "path",
            "source_sha256",
            "byte_start",
            "byte_end",
            "replacement",
            "result_sha256",
        ],
    )?;
    let path = string(edit, "path")?;
    if Some(path) != rejected.get("path").and_then(Value::as_str) {
        return Err(DENIED);
    }
    let file = admission
        .get("files")
        .and_then(Value::as_array)
        .and_then(|files| {
            files
                .iter()
                .find(|file| file.get("path").and_then(Value::as_str) == Some(path))
        })
        .ok_or(DENIED)?;
    validate_ranged_edit(
        edit,
        exact_object(file, &["path", "before_sha256", "content"])?,
    )
}

fn validate_continuation_response(prompt: &str, response: &str) -> Result<(), i32> {
    let prompt: Value = serde_json::from_str(prompt).map_err(|_| INVALID)?;
    let prompt = exact_object(
        &prompt,
        &[
            "schema",
            "instructions",
            "model",
            "task",
            "admitted_sources",
            "current_proposals",
            "command_finished",
            "required_response",
        ],
    )?;
    if let Some(summary) = response.strip_prefix("COMPLETE\n") {
        let command_succeeded = prompt
            .get("command_finished")
            .and_then(|command| command.get("exit_code"))
            .and_then(Value::as_i64)
            == Some(0);
        return if command_succeeded && !summary.trim().is_empty() {
            Ok(())
        } else {
            Err(DENIED)
        };
    }
    let remainder = response.strip_prefix("PROPOSE ").ok_or(DENIED)?;
    let (index_text, edit) = remainder.split_once('\n').ok_or(DENIED)?;
    let index = index_text.parse::<usize>().map_err(|_| INVALID)?;
    if index.to_string() != index_text {
        return Err(DENIED);
    }
    let edit: Value = serde_json::from_str(edit).map_err(|_| INVALID)?;
    let edit = exact_object(
        &edit,
        &[
            "source_sha256",
            "byte_start",
            "byte_end",
            "replacement",
            "result_sha256",
        ],
    )?;
    let source = prompt
        .get("admitted_sources")
        .and_then(Value::as_array)
        .and_then(|sources| sources.get(index))
        .ok_or(DENIED)?;
    let source = exact_object(
        source,
        &["admitted_path_index", "path", "sha256", "content"],
    )?;
    if source
        .get("admitted_path_index")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        != Some(index)
    {
        return Err(DENIED);
    }
    validate_edit_bytes(edit, string(source, "content")?, string(source, "sha256")?)
}

fn validate_candidate_decision(
    proposal_index: usize,
    revision: u32,
    candidate_sha256: &str,
    history: &[SessionFact],
) -> Result<(), i32> {
    if !valid_sha256(candidate_sha256) {
        return Err(DENIED);
    }
    let Some(completed) = history.iter().rev().find_map(|fact| match fact {
        SessionFact::ModelCompleted { turn, response, .. } => Some((turn, response)),
        SessionFact::TaskCompleted { .. } => None,
        _ => None,
    }) else {
        return Err(DENIED);
    };
    let expected = candidate_digest(completed.0, completed.1, proposal_index, revision)?;
    if expected == candidate_sha256 {
        Ok(())
    } else {
        Err(DENIED)
    }
}

fn candidate_digest(
    turn: &ModelTurn,
    response: &str,
    proposal_index: usize,
    revision: u32,
) -> Result<String, i32> {
    match turn {
        ModelTurn::Initial if revision == 0 => {
            let value: Value = serde_json::from_str(response).map_err(|_| INVALID)?;
            value
                .get("proposals")
                .and_then(Value::as_array)
                .and_then(|proposals| proposals.get(proposal_index))
                .and_then(|proposal| proposal.get("result_sha256"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or(DENIED)
        }
        ModelTurn::Revision {
            proposal_index: revised,
            revision: expected,
        } if proposal_index == *revised && revision == *expected => {
            let value: Value = serde_json::from_str(response).map_err(|_| INVALID)?;
            value
                .pointer("/proposal/result_sha256")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or(DENIED)
        }
        ModelTurn::Continuation { sequence } if revision == sequence.saturating_add(1) => {
            let (_, edit) = response.split_once('\n').ok_or(DENIED)?;
            let value: Value = serde_json::from_str(edit).map_err(|_| INVALID)?;
            value
                .get("result_sha256")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or(DENIED)
        }
        _ => Err(DENIED),
    }
}

fn validate_ranged_edit(edit: &Map<String, Value>, file: &Map<String, Value>) -> Result<(), i32> {
    validate_edit_bytes(
        edit,
        string(file, "content")?,
        string(file, "before_sha256")?,
    )
}

fn validate_edit_bytes(
    edit: &Map<String, Value>,
    source_text: &str,
    source_sha256: &str,
) -> Result<(), i32> {
    let source = source_text.as_bytes();
    if !valid_sha256(source_sha256)
        || sha256(source) != source_sha256
        || string(edit, "source_sha256")? != source_sha256
    {
        return Err(DENIED);
    }
    let start = usize_field(edit, "byte_start")?;
    let end = usize_field(edit, "byte_end")?;
    if start > end
        || end > source.len()
        || !source_text.is_char_boundary(start)
        || !source_text.is_char_boundary(end)
    {
        return Err(DENIED);
    }
    let replacement = string(edit, "replacement")?.as_bytes();
    let mut result = Vec::with_capacity(source.len() - (end - start) + replacement.len());
    result.extend_from_slice(&source[..start]);
    result.extend_from_slice(replacement);
    result.extend_from_slice(&source[end..]);
    let result_sha256 = string(edit, "result_sha256")?;
    if result.is_empty()
        || result == source
        || !valid_sha256(result_sha256)
        || sha256(&result) != result_sha256
    {
        return Err(DENIED);
    }
    Ok(())
}

fn exact_object<'a>(value: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>, i32> {
    let object = value.as_object().ok_or(INVALID)?;
    if object.len() != keys.len() || keys.iter().any(|key| !object.contains_key(*key)) {
        return Err(DENIED);
    }
    Ok(object)
}

fn string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, i32> {
    object.get(key).and_then(Value::as_str).ok_or(INVALID)
}

fn usize_field(object: &Map<String, Value>, key: &str) -> Result<usize, i32> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or(INVALID)
}

fn fact_code(fact: &SessionFact) -> u64 {
    match fact {
        SessionFact::ModelStarted { .. } => 1,
        SessionFact::ModelCompleted { .. } => 2,
        SessionFact::ProposalRejected { .. } => 3,
        SessionFact::ProposalApproved { .. } => 4,
        SessionFact::PromotionStarted { .. } => 5,
        SessionFact::ProposalPromoted { .. } => 6,
        SessionFact::CommandStarted { .. } => 7,
        SessionFact::CommandFinished { .. } => 8,
        SessionFact::TaskCompleted { .. } => 9,
    }
}

fn allowed(previous: u64, next: u64) -> bool {
    matches!(
        (previous, next),
        (0, 1) | (1, 2 | 9) | (2, 3 | 4) | (3, 1) | (4, 5) | (5, 6) | (6, 4 | 7) | (7, 8) | (8, 1)
    )
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use core::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("string write");
    }
    output
}

fn checked_i64(value: i64) -> Result<u64, i32> {
    u64::try_from(value).map_err(|_| (-value) as i32)
}

fn checked_status(status: i32) -> Result<(), i32> {
    if status == 0 { Ok(()) } else { Err(status) }
}

export!(Orchestrator);
