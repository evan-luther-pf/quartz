use crate::commands::{CommandFinished, RepositoryIdentity};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

pub(crate) const MAX_TASK_BYTES: usize = 4 * 1024;
pub(crate) const MAX_SOURCE_BYTES: usize = 32 * 1024;
pub(crate) const MAX_PROMPT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_RESPONSE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_FEEDBACK_BYTES: usize = 4 * 1024;
pub(crate) const MAX_REVISION_PROMPT_BYTES: usize = 256 * 1024;
pub(crate) const MAX_CONTINUATION_PROMPT_BYTES: usize = 384 * 1024;
pub(crate) const MAX_COMPLETION_SUMMARY_BYTES: usize = 4 * 1024;

const REVISION_INSTRUCTIONS: &str = "Revise only the rejected proposal identified below. Return only the required JSON object. The proposal must use the rejected admitted path and matching original before_sha256, and content must be the complete replacement file. Address the exact feedback and change the rejected content. Do not use Markdown fences or commentary.";

const INSTRUCTIONS: &str = "Edit only the admitted files needed for the task. Return only the required JSON object. Every proposal must use one admitted path and matching before_sha256, and content must be the complete replacement file. Return at least two proposals. Do not use Markdown fences or commentary.";

const CONTINUATION_INSTRUCTIONS: &str = "Continue the same repository task from the exact approved-command evidence. Return exactly `PROPOSE <admitted-path-index>\\n<exact complete-file candidate bytes>` or `COMPLETE\\n<bounded final summary>`. PROPOSE may select only an admitted path index and must change the exact post-command source. COMPLETE is valid only when the command succeeded. Do not use Markdown fences or commentary outside the selected grammar.";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Admission {
    pub(crate) task: String,
    pub(crate) files: Vec<AdmittedFile>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AdmittedFile {
    pub(crate) path: String,
    pub(crate) before_sha256: String,
    pub(crate) content: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Proposal {
    pub(crate) path: String,
    pub(crate) before_sha256: String,
    pub(crate) result_sha256: String,
    pub(crate) before: Vec<u8>,
    pub(crate) content: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Revision {
    pub(crate) model: String,
    pub(crate) proposal_index: usize,
    pub(crate) feedback: String,
    pub(crate) admission: Admission,
    pub(crate) rejected: Proposal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProposalGeneration {
    pub(crate) proposal_index: usize,
    pub(crate) admitted_path_index: usize,
    pub(crate) revision: u32,
    pub(crate) proposal: Proposal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Continuation {
    pub(crate) model: String,
    pub(crate) admission: Admission,
    pub(crate) current: Vec<ProposalGeneration>,
    pub(crate) command: CommandFinished,
    pub(crate) sources: Vec<AdmittedFile>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ContinuationResponse {
    Proposal {
        admitted_path_index: usize,
        proposal_index: usize,
        proposal: Proposal,
    },
    Complete(String),
}

impl Admission {
    pub(crate) fn from_files(
        repository_root: &Path,
        task_path: &Path,
        source_paths: &[PathBuf],
    ) -> Result<Self, String> {
        if !(2..=3).contains(&source_paths.len()) {
            return Err("proposal admission requires two or three source files".into());
        }
        let repository_root = fs::canonicalize(repository_root)
            .map_err(|error| format!("canonicalize repository root: {error}"))?;
        let task_bytes = fs::read(task_path)
            .map_err(|error| format!("read task `{}`: {error}", task_path.display()))?;
        if task_bytes.is_empty() || task_bytes.len() > MAX_TASK_BYTES {
            return Err(format!(
                "task must contain 1..={MAX_TASK_BYTES} UTF-8 bytes"
            ));
        }
        let task = std::str::from_utf8(&task_bytes)
            .map_err(|_| "proposal task is not UTF-8".to_owned())?
            .to_owned();
        let mut seen = BTreeSet::new();
        let mut files = Vec::with_capacity(source_paths.len());
        for source_path in source_paths {
            let canonical = fs::canonicalize(source_path).map_err(|error| {
                format!("canonicalize source `{}`: {error}", source_path.display())
            })?;
            if !canonical.starts_with(&repository_root) || !canonical.is_file() {
                return Err(format!(
                    "source `{}` is not a regular file under `{}`",
                    source_path.display(),
                    repository_root.display()
                ));
            }
            if !seen.insert(canonical.clone()) {
                return Err(format!(
                    "duplicate admitted source `{}`",
                    canonical.display()
                ));
            }
            let relative = canonical
                .strip_prefix(&repository_root)
                .expect("source prefix checked")
                .to_str()
                .ok_or_else(|| format!("source path is not UTF-8: {}", canonical.display()))?
                .to_owned();
            validate_relative_path(&relative)?;
            let content = fs::read(&canonical)
                .map_err(|error| format!("read source `{}`: {error}", canonical.display()))?;
            if content.len() > MAX_SOURCE_BYTES {
                return Err(format!(
                    "source `{relative}` is {} bytes; limit is {MAX_SOURCE_BYTES}",
                    content.len()
                ));
            }
            std::str::from_utf8(&content)
                .map_err(|_| format!("source `{relative}` is not UTF-8"))?;
            files.push(AdmittedFile {
                path: relative,
                before_sha256: sha256(&content),
                content,
            });
        }
        let admission = Self { task, files };
        admission.prompt_bytes()?;
        Ok(admission)
    }

    pub(crate) fn prompt_bytes(&self) -> Result<Vec<u8>, String> {
        let files: Vec<_> = self
            .files
            .iter()
            .map(|file| {
                json!({
                    "path": file.path,
                    "before_sha256": file.before_sha256,
                    "content": std::str::from_utf8(&file.content)
                        .expect("admission content validated as UTF-8"),
                })
            })
            .collect();
        let prompt = serde_json::to_vec_pretty(&json!({
            "schema": 1,
            "instructions": INSTRUCTIONS,
            "task": self.task,
            "files": files,
            "required_response": {
                "proposals": [{
                    "path": "one admitted path",
                    "before_sha256": "that file's admitted SHA-256",
                    "content": "the complete replacement file"
                }]
            }
        }))
        .map_err(|error| format!("serialize proposal prompt: {error}"))?;
        if prompt.len() > MAX_PROMPT_BYTES {
            return Err(format!(
                "generated prompt is {} bytes; limit is {MAX_PROMPT_BYTES}",
                prompt.len()
            ));
        }
        Ok(prompt)
    }

    pub(crate) fn from_prompt(prompt: &[u8]) -> Result<Self, String> {
        if prompt.is_empty() || prompt.len() > MAX_PROMPT_BYTES {
            return Err(format!(
                "durable proposal prompt must contain 1..={MAX_PROMPT_BYTES} bytes"
            ));
        }
        let value: Value = serde_json::from_slice(prompt)
            .map_err(|error| format!("invalid durable proposal prompt JSON: {error}"))?;
        let object = exact_object(
            &value,
            &[
                "schema",
                "instructions",
                "task",
                "files",
                "required_response",
            ],
            "proposal prompt",
        )?;
        if object.get("schema").and_then(Value::as_u64) != Some(1) {
            return Err("unsupported proposal prompt schema".into());
        }
        let instructions = string_field(object, "instructions", "proposal prompt")?;
        if instructions != INSTRUCTIONS {
            return Err("proposal prompt instructions changed".into());
        }
        let task = string_field(object, "task", "proposal prompt")?.to_owned();
        if task.is_empty() || task.len() > MAX_TASK_BYTES {
            return Err("proposal prompt task is empty or oversized".into());
        }
        exact_object(
            object
                .get("required_response")
                .expect("required key checked"),
            &["proposals"],
            "required response",
        )?;
        let values = object
            .get("files")
            .and_then(Value::as_array)
            .ok_or_else(|| "proposal prompt files must be an array".to_owned())?;
        if !(2..=3).contains(&values.len()) {
            return Err("proposal prompt must contain two or three files".into());
        }
        let mut paths = BTreeSet::new();
        let mut files = Vec::with_capacity(values.len());
        for value in values {
            let file = exact_object(
                value,
                &["path", "before_sha256", "content"],
                "admitted file",
            )?;
            let path = string_field(file, "path", "admitted file")?.to_owned();
            validate_relative_path(&path)?;
            if !paths.insert(path.clone()) {
                return Err(format!("duplicate admitted path `{path}`"));
            }
            let before_sha256 = string_field(file, "before_sha256", "admitted file")?.to_owned();
            validate_sha256(&before_sha256, "admitted before digest")?;
            let content = string_field(file, "content", "admitted file")?
                .as_bytes()
                .to_vec();
            if content.len() > MAX_SOURCE_BYTES {
                return Err(format!(
                    "admitted file `{path}` exceeds {MAX_SOURCE_BYTES} bytes"
                ));
            }
            if sha256(&content) != before_sha256 {
                return Err(format!("admitted digest mismatch for `{path}`"));
            }
            files.push(AdmittedFile {
                path,
                before_sha256,
                content,
            });
        }
        Ok(Self { task, files })
    }

    fn file(&self, path: &str) -> Option<&AdmittedFile> {
        self.files.iter().find(|file| file.path == path)
    }
}

impl Revision {
    pub(crate) fn new(
        model: &str,
        feedback: &[u8],
        admission: &Admission,
        proposals: &[Proposal],
        proposal_index: usize,
    ) -> Result<Self, String> {
        validate_model(model)?;
        let feedback = validate_feedback(feedback)?;
        let rejected = proposals
            .get(proposal_index)
            .ok_or_else(|| format!("proposal index {proposal_index} is absent"))?
            .clone();
        let revision = Self {
            model: model.to_owned(),
            proposal_index,
            feedback,
            admission: admission.clone(),
            rejected,
        };
        revision.prompt_bytes()?;
        Ok(revision)
    }

    pub(crate) fn prompt_bytes(&self) -> Result<Vec<u8>, String> {
        let admission_prompt = self.admission.prompt_bytes()?;
        let prompt = serde_json::to_vec_pretty(&json!({
            "schema": 1,
            "instructions": REVISION_INSTRUCTIONS,
            "model": self.model,
            "admission": {
                "prompt_sha256": sha256(&admission_prompt),
                "task": self.admission.task,
                "files": admitted_files_json(&self.admission.files),
            },
            "rejection": {
                "proposal_index": self.proposal_index,
                "path": self.rejected.path,
                "before_sha256": self.rejected.before_sha256,
                "prior_result_sha256": self.rejected.result_sha256,
                "prior_content": std::str::from_utf8(&self.rejected.content)
                    .expect("validated rejected proposal must remain UTF-8"),
                "feedback": self.feedback,
            },
            "required_response": {
                "proposal": {
                    "path": "the rejected admitted path",
                    "before_sha256": "that file's original admitted SHA-256",
                    "content": "the complete corrected replacement file"
                }
            }
        }))
        .map_err(|error| format!("serialize revision prompt: {error}"))?;
        if prompt.len() > MAX_REVISION_PROMPT_BYTES {
            return Err(format!(
                "generated revision prompt is {} bytes; limit is {MAX_REVISION_PROMPT_BYTES}",
                prompt.len()
            ));
        }
        Ok(prompt)
    }

    pub(crate) fn from_prompt(
        prompt: &[u8],
        admission: &Admission,
        proposals: &[Proposal],
    ) -> Result<Self, String> {
        if prompt.is_empty() || prompt.len() > MAX_REVISION_PROMPT_BYTES {
            return Err(format!(
                "durable revision prompt must contain 1..={MAX_REVISION_PROMPT_BYTES} bytes"
            ));
        }
        let value: Value = serde_json::from_slice(prompt)
            .map_err(|error| format!("invalid durable revision prompt JSON: {error}"))?;
        let object = exact_object(
            &value,
            &[
                "schema",
                "instructions",
                "model",
                "admission",
                "rejection",
                "required_response",
            ],
            "revision prompt",
        )?;
        if object.get("schema").and_then(Value::as_u64) != Some(1) {
            return Err("unsupported revision prompt schema".into());
        }
        if string_field(object, "instructions", "revision prompt")? != REVISION_INSTRUCTIONS {
            return Err("revision prompt instructions changed".into());
        }
        let model = string_field(object, "model", "revision prompt")?.to_owned();
        validate_model(&model)?;

        let admitted = exact_object(
            object.get("admission").expect("required key checked"),
            &["prompt_sha256", "task", "files"],
            "revision admission",
        )?;
        let prompt_sha256 = string_field(admitted, "prompt_sha256", "revision admission")?;
        validate_sha256(prompt_sha256, "revision admission prompt digest")?;
        if prompt_sha256 != sha256(&admission.prompt_bytes()?) {
            return Err("revision admission prompt digest changed".into());
        }
        if string_field(admitted, "task", "revision admission")? != admission.task {
            return Err("revision admission task changed".into());
        }
        let files = admitted
            .get("files")
            .and_then(Value::as_array)
            .ok_or_else(|| "revision admission files must be an array".to_owned())?;
        if files.len() != admission.files.len() {
            return Err("revision admission file count changed".into());
        }
        for (value, expected) in files.iter().zip(&admission.files) {
            let file = exact_object(
                value,
                &["path", "before_sha256", "content"],
                "revision admitted file",
            )?;
            if string_field(file, "path", "revision admitted file")? != expected.path
                || string_field(file, "before_sha256", "revision admitted file")?
                    != expected.before_sha256
                || string_field(file, "content", "revision admitted file")?.as_bytes()
                    != expected.content
            {
                return Err("revision admitted file changed".into());
            }
        }

        let rejection = exact_object(
            object.get("rejection").expect("required key checked"),
            &[
                "proposal_index",
                "path",
                "before_sha256",
                "prior_result_sha256",
                "prior_content",
                "feedback",
            ],
            "revision rejection",
        )?;
        let proposal_index = rejection
            .get("proposal_index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| "revision proposal index must be a non-negative integer".to_owned())?;
        let rejected = proposals
            .get(proposal_index)
            .ok_or_else(|| format!("revision proposal index {proposal_index} is absent"))?;
        if string_field(rejection, "path", "revision rejection")? != rejected.path
            || string_field(rejection, "before_sha256", "revision rejection")?
                != rejected.before_sha256
            || string_field(rejection, "prior_result_sha256", "revision rejection")?
                != rejected.result_sha256
            || string_field(rejection, "prior_content", "revision rejection")?.as_bytes()
                != rejected.content
        {
            return Err("revision rejected proposal changed".into());
        }
        let feedback = validate_feedback(
            string_field(rejection, "feedback", "revision rejection")?.as_bytes(),
        )?;
        let required = exact_object(
            object
                .get("required_response")
                .expect("required key checked"),
            &["proposal"],
            "revision required response",
        )?;
        exact_object(
            required.get("proposal").expect("required key checked"),
            &["path", "before_sha256", "content"],
            "revision response template",
        )?;
        Ok(Self {
            model,
            proposal_index,
            feedback,
            admission: admission.clone(),
            rejected: rejected.clone(),
        })
    }
}

impl Continuation {
    pub(crate) fn new(
        model: &str,
        admission: &Admission,
        current: &[ProposalGeneration],
        command: &CommandFinished,
        repository_root: &Path,
    ) -> Result<Self, String> {
        validate_model(model)?;
        validate_current_generations(admission, current)?;
        let paths = admission
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let repository_after = RepositoryIdentity::capture(repository_root, &paths)?;
        repository_after.require_regular()?;
        if repository_after != command.repository_after {
            return Err("repository changed after the approved command finished".into());
        }
        let mut sources = Vec::with_capacity(admission.files.len());
        for admitted in &admission.files {
            let source = resolve_source(repository_root, &admitted.path)?;
            let content = fs::read(&source).map_err(|error| {
                format!("read post-command source `{}`: {error}", admitted.path)
            })?;
            validate_source_bytes(&admitted.path, &content)?;
            sources.push(AdmittedFile {
                path: admitted.path.clone(),
                before_sha256: sha256(&content),
                content,
            });
        }
        let continuation = Self {
            model: model.to_owned(),
            admission: admission.clone(),
            current: current.to_vec(),
            command: command.clone(),
            sources,
        };
        continuation.prompt_bytes()?;
        Ok(continuation)
    }

    pub(crate) fn prompt_bytes(&self) -> Result<Vec<u8>, String> {
        validate_model(&self.model)?;
        validate_current_generations(&self.admission, &self.current)?;
        validate_continuation_sources(&self.admission, &self.command, &self.sources)?;
        let admitted_sources = self
            .sources
            .iter()
            .enumerate()
            .map(|(index, source)| {
                json!({
                    "admitted_path_index": index,
                    "path": source.path,
                    "sha256": source.before_sha256,
                    "content": std::str::from_utf8(&source.content)
                        .expect("validated continuation source must remain UTF-8"),
                })
            })
            .collect::<Vec<_>>();
        let prompt = json!({
            "schema": 1,
            "instructions": CONTINUATION_INSTRUCTIONS,
            "model": self.model,
            "task": self.admission.task,
            "admitted_sources": admitted_sources,
            "current_proposals": continuation_generations(&self.current),
            "command_finished": self.command.to_value(),
            "required_response": [
                "PROPOSE <admitted-path-index>\\n<exact complete-file candidate bytes>",
                "COMPLETE\\n<bounded final summary>",
            ],
        });
        let bytes = serde_json::to_vec_pretty(&prompt)
            .map_err(|error| format!("serialize continuation prompt: {error}"))?;
        if bytes.is_empty() || bytes.len() > MAX_CONTINUATION_PROMPT_BYTES {
            return Err(format!(
                "generated continuation prompt must contain 1..={MAX_CONTINUATION_PROMPT_BYTES} bytes"
            ));
        }
        Ok(bytes)
    }

    pub(crate) fn from_prompt(
        prompt: &[u8],
        admission: &Admission,
        current: &[ProposalGeneration],
        command: &CommandFinished,
    ) -> Result<Self, String> {
        if prompt.is_empty() || prompt.len() > MAX_CONTINUATION_PROMPT_BYTES {
            return Err(format!(
                "durable continuation prompt must contain 1..={MAX_CONTINUATION_PROMPT_BYTES} bytes"
            ));
        }
        let wire: Value = serde_json::from_slice(prompt)
            .map_err(|error| format!("invalid durable continuation prompt: {error}"))?;
        let object = exact_object(
            &wire,
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
            "continuation prompt",
        )?;
        let required_response = json!([
            "PROPOSE <admitted-path-index>\\n<exact complete-file candidate bytes>",
            "COMPLETE\\n<bounded final summary>",
        ]);
        if object.get("schema").and_then(Value::as_u64) != Some(1)
            || string_field(object, "instructions", "continuation prompt")?
                != CONTINUATION_INSTRUCTIONS
            || string_field(object, "task", "continuation prompt")? != admission.task
            || object.get("required_response") != Some(&required_response)
            || object.get("command_finished") != Some(&command.to_value())
        {
            return Err("durable continuation prompt contract changed".into());
        }
        let model = string_field(object, "model", "continuation prompt")?.to_owned();
        validate_model(&model)?;
        validate_current_generations(admission, current)?;
        let expected_generations = Value::Array(continuation_generations(current));
        if object.get("current_proposals") != Some(&expected_generations) {
            return Err("durable continuation current proposals changed".into());
        }
        let admitted_sources = object
            .get("admitted_sources")
            .and_then(Value::as_array)
            .ok_or_else(|| "continuation prompt admitted_sources must be an array".to_owned())?;
        if admitted_sources.len() != admission.files.len() {
            return Err("durable continuation admitted source count changed".into());
        }
        let mut sources = Vec::with_capacity(admitted_sources.len());
        for (index, (source_value, admitted)) in
            admitted_sources.iter().zip(&admission.files).enumerate()
        {
            let source = exact_object(
                source_value,
                &["admitted_path_index", "path", "sha256", "content"],
                "continuation admitted source",
            )?;
            let admitted_path_index = source
                .get("admitted_path_index")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    "continuation admitted source index must be a non-negative integer".to_owned()
                })?;
            let path = string_field(source, "path", "continuation admitted source")?.to_owned();
            let source_sha256 =
                string_field(source, "sha256", "continuation admitted source")?.to_owned();
            let content = string_field(source, "content", "continuation admitted source")?
                .as_bytes()
                .to_vec();
            if admitted_path_index != index
                || path != admitted.path
                || source_sha256 != sha256(&content)
            {
                return Err("durable continuation admitted source changed".into());
            }
            validate_source_bytes(&path, &content)?;
            sources.push(AdmittedFile {
                path,
                before_sha256: source_sha256,
                content,
            });
        }
        validate_continuation_sources(admission, command, &sources)?;
        Ok(Self {
            model,
            admission: admission.clone(),
            current: current.to_vec(),
            command: command.clone(),
            sources,
        })
    }
}

pub(crate) fn parse_continuation_response(
    response: &[u8],
    continuation: &Continuation,
) -> Result<ContinuationResponse, String> {
    if response.is_empty() || response.len() > MAX_RESPONSE_BYTES {
        return Err(format!(
            "continuation response must contain 1..={MAX_RESPONSE_BYTES} bytes"
        ));
    }
    if let Some(summary) = response.strip_prefix(b"COMPLETE\n") {
        if !continuation.command.succeeded() {
            return Err("COMPLETE requires a successful approved command".into());
        }
        if summary.is_empty() || summary.len() > MAX_COMPLETION_SUMMARY_BYTES {
            return Err(format!(
                "completion summary must contain 1..={MAX_COMPLETION_SUMMARY_BYTES} bytes"
            ));
        }
        let summary = std::str::from_utf8(summary)
            .map_err(|_| "completion summary is not UTF-8")?
            .to_owned();
        if summary.trim().is_empty() {
            return Err("completion summary cannot be blank".into());
        }
        return Ok(ContinuationResponse::Complete(summary));
    }
    let remainder = response.strip_prefix(b"PROPOSE ").ok_or_else(|| {
        "continuation response must start with `PROPOSE ` or `COMPLETE\\n`".to_owned()
    })?;
    let newline = remainder
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| "PROPOSE response has no candidate bytes".to_owned())?;
    let index_bytes = &remainder[..newline];
    let index_text =
        std::str::from_utf8(index_bytes).map_err(|_| "PROPOSE path index is not ASCII")?;
    let admitted_path_index: usize = index_text
        .parse()
        .map_err(|_| format!("invalid admitted path index `{index_text}`"))?;
    if admitted_path_index.to_string() != index_text {
        return Err("PROPOSE path index is not canonical decimal".into());
    }
    let source = continuation
        .sources
        .get(admitted_path_index)
        .ok_or_else(|| format!("admitted path index {admitted_path_index} is absent"))?;
    let content = remainder[newline + 1..].to_vec();
    validate_source_bytes(&source.path, &content)?;
    if content == source.content {
        return Err(format!(
            "continued proposal for `{}` does not change the post-command source",
            source.path
        ));
    }
    let proposal_index = continuation
        .current
        .iter()
        .find(|generation| generation.admitted_path_index == admitted_path_index)
        .map(|generation| generation.proposal_index)
        .unwrap_or_else(|| {
            continuation
                .current
                .iter()
                .map(|generation| generation.proposal_index)
                .max()
                .map_or(0, |index| index + 1)
        });
    Ok(ContinuationResponse::Proposal {
        admitted_path_index,
        proposal_index,
        proposal: Proposal {
            path: source.path.clone(),
            before_sha256: source.before_sha256.clone(),
            result_sha256: sha256(&content),
            before: source.content.clone(),
            content,
        },
    })
}

fn continuation_generations(current: &[ProposalGeneration]) -> Vec<Value> {
    current
        .iter()
        .map(|generation| {
            json!({
                "proposal_index": generation.proposal_index,
                "admitted_path_index": generation.admitted_path_index,
                "revision": generation.revision,
                "path": generation.proposal.path,
                "before_sha256": generation.proposal.before_sha256,
                "result_sha256": generation.proposal.result_sha256,
                "content": std::str::from_utf8(&generation.proposal.content)
                    .expect("validated proposal content must remain UTF-8"),
            })
        })
        .collect()
}

fn validate_current_generations(
    admission: &Admission,
    current: &[ProposalGeneration],
) -> Result<(), String> {
    if current.is_empty() {
        return Err("continuation requires at least one current proposal".into());
    }
    let mut proposal_indices = BTreeSet::new();
    let mut admitted_indices = BTreeSet::new();
    for generation in current {
        let admitted = admission
            .files
            .get(generation.admitted_path_index)
            .ok_or_else(|| {
                format!(
                    "current proposal {} has an absent admitted path index",
                    generation.proposal_index
                )
            })?;
        if !proposal_indices.insert(generation.proposal_index)
            || !admitted_indices.insert(generation.admitted_path_index)
            || admitted.path != generation.proposal.path
            || generation.revision > 1
            || generation.proposal.result_sha256 != sha256(&generation.proposal.content)
            || generation.proposal.before_sha256 != sha256(&generation.proposal.before)
        {
            return Err("current proposal generation identity is invalid".into());
        }
        validate_source_bytes(&generation.proposal.path, &generation.proposal.before)?;
        validate_source_bytes(&generation.proposal.path, &generation.proposal.content)?;
    }
    Ok(())
}

fn validate_continuation_sources(
    admission: &Admission,
    command: &CommandFinished,
    sources: &[AdmittedFile],
) -> Result<(), String> {
    if sources.len() != admission.files.len()
        || command.repository_after.files.len() != admission.files.len()
    {
        return Err("continuation admitted source count changed".into());
    }
    for ((source, admitted), identity) in sources
        .iter()
        .zip(&admission.files)
        .zip(&command.repository_after.files)
    {
        if source.path != admitted.path
            || source.before_sha256 != sha256(&source.content)
            || identity.path != source.path
            || identity.status != "regular"
            || identity.sha256.as_deref() != Some(&source.before_sha256)
            || identity.byte_len != u64::try_from(source.content.len()).ok()
        {
            return Err("continuation source does not match terminal command evidence".into());
        }
        validate_source_bytes(&source.path, &source.content)?;
    }
    Ok(())
}

fn validate_source_bytes(path: &str, content: &[u8]) -> Result<(), String> {
    if content.is_empty() || content.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "source `{path}` must contain 1..={MAX_SOURCE_BYTES} bytes"
        ));
    }
    std::str::from_utf8(content).map_err(|_| format!("source `{path}` is not UTF-8"))?;
    Ok(())
}

fn admitted_files_json(files: &[AdmittedFile]) -> Vec<Value> {
    files
        .iter()
        .map(|file| {
            json!({
                "path": file.path,
                "before_sha256": file.before_sha256,
                "content": std::str::from_utf8(&file.content)
                    .expect("validated admission content must remain UTF-8"),
            })
        })
        .collect()
}

pub(crate) fn parse_response(
    response: &[u8],
    admission: &Admission,
) -> Result<Vec<Proposal>, String> {
    if response.is_empty() || response.len() > MAX_RESPONSE_BYTES {
        return Err(format!(
            "proposal response must contain 1..={MAX_RESPONSE_BYTES} bytes"
        ));
    }
    let value: Value = serde_json::from_slice(response)
        .map_err(|error| format!("invalid proposal response JSON: {error}"))?;
    let object = exact_object(&value, &["proposals"], "proposal response")?;
    let values = object
        .get("proposals")
        .and_then(Value::as_array)
        .ok_or_else(|| "proposal response `proposals` must be an array".to_owned())?;
    if values.len() < 2 || values.len() > admission.files.len() {
        return Err(format!(
            "proposal response must contain 2..={} proposals",
            admission.files.len()
        ));
    }
    let mut paths = BTreeSet::new();
    let mut proposals = Vec::with_capacity(values.len());
    for value in values {
        let proposal = exact_object(value, &["path", "before_sha256", "content"], "proposal")?;
        let path = string_field(proposal, "path", "proposal")?.to_owned();
        if !paths.insert(path.clone()) {
            return Err(format!("duplicate proposal path `{path}`"));
        }
        let admitted = admission
            .file(&path)
            .ok_or_else(|| format!("proposal path `{path}` was not admitted"))?;
        let before_sha256 = string_field(proposal, "before_sha256", "proposal")?.to_owned();
        if before_sha256 != admitted.before_sha256 {
            return Err(format!("proposal before digest mismatch for `{path}`"));
        }
        let content = string_field(proposal, "content", "proposal")?
            .as_bytes()
            .to_vec();
        if content.len() > MAX_SOURCE_BYTES {
            return Err(format!(
                "proposal for `{path}` exceeds {MAX_SOURCE_BYTES} bytes"
            ));
        }
        if content == admitted.content {
            return Err(format!("proposal for `{path}` does not change the file"));
        }
        proposals.push(Proposal {
            path,
            before_sha256,
            result_sha256: sha256(&content),
            before: admitted.content.clone(),
            content,
        });
    }
    Ok(proposals)
}

pub(crate) fn parse_revision_response(
    response: &[u8],
    revision: &Revision,
) -> Result<Proposal, String> {
    if response.is_empty() || response.len() > MAX_RESPONSE_BYTES {
        return Err(format!(
            "revision response must contain 1..={MAX_RESPONSE_BYTES} bytes"
        ));
    }
    let value: Value = serde_json::from_slice(response)
        .map_err(|error| format!("invalid revision response JSON: {error}"))?;
    let object = exact_object(&value, &["proposal"], "revision response")?;
    let proposal = exact_object(
        object.get("proposal").expect("required key checked"),
        &["path", "before_sha256", "content"],
        "revised proposal",
    )?;
    let path = string_field(proposal, "path", "revised proposal")?.to_owned();
    let before_sha256 = string_field(proposal, "before_sha256", "revised proposal")?.to_owned();
    if path != revision.rejected.path || before_sha256 != revision.rejected.before_sha256 {
        return Err("revision response changed the rejected proposal identity".into());
    }
    let content = string_field(proposal, "content", "revised proposal")?
        .as_bytes()
        .to_vec();
    if content.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "revised proposal for `{path}` exceeds {MAX_SOURCE_BYTES} bytes"
        ));
    }
    if content == revision.rejected.before {
        return Err(format!(
            "revised proposal for `{path}` does not change the admitted source"
        ));
    }
    if content == revision.rejected.content {
        return Err(format!(
            "revised proposal for `{path}` does not change the rejected generation"
        ));
    }
    Ok(Proposal {
        path,
        before_sha256,
        result_sha256: sha256(&content),
        before: revision.rejected.before.clone(),
        content,
    })
}

pub(crate) fn render_diff(proposal: &Proposal) -> String {
    let before =
        std::str::from_utf8(&proposal.before).expect("validated proposal source must remain UTF-8");
    let after = std::str::from_utf8(&proposal.content)
        .expect("validated proposal result must remain UTF-8");
    let before_lines: Vec<_> = before.split_inclusive('\n').collect();
    let after_lines: Vec<_> = after.split_inclusive('\n').collect();
    let prefix = before_lines
        .iter()
        .zip(&after_lines)
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = before_lines[prefix..]
        .iter()
        .rev()
        .zip(after_lines[prefix..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let before_change_end = before_lines.len() - suffix;
    let after_change_end = after_lines.len() - suffix;
    let context_before = prefix.min(3);
    let context_after = suffix.min(3);
    let before_start = prefix - context_before;
    let after_start = before_start;
    let before_end = before_change_end + context_after;
    let after_end = after_change_end + context_after;
    let mut output = format!(
        "--- a/{0}\n+++ b/{0}\n@@ -{1},{2} +{3},{4} @@\n",
        proposal.path,
        hunk_start(before_start, before_end),
        before_end - before_start,
        hunk_start(after_start, after_end),
        after_end - after_start,
    );
    for line in &before_lines[before_start..prefix] {
        push_diff_line(&mut output, ' ', line);
    }
    for line in &before_lines[prefix..before_change_end] {
        push_diff_line(&mut output, '-', line);
    }
    for line in &after_lines[prefix..after_change_end] {
        push_diff_line(&mut output, '+', line);
    }
    for line in &before_lines[before_change_end..before_end] {
        push_diff_line(&mut output, ' ', line);
    }
    output
}

fn hunk_start(start: usize, end: usize) -> usize {
    if start == end { start } else { start + 1 }
}

fn push_diff_line(output: &mut String, marker: char, line: &str) {
    output.push(marker);
    output.push_str(line);
    if !line.ends_with('\n') {
        output.push_str("\n\\ No newline at end of file\n");
    }
}

pub(crate) fn materialize(session: &Path, proposals: &[Proposal]) -> Result<Vec<PathBuf>, String> {
    fs::create_dir_all(session)
        .map_err(|error| format!("create proposal session `{}`: {error}", session.display()))?;
    let mut paths = Vec::with_capacity(proposals.len());
    for (index, proposal) in proposals.iter().enumerate() {
        let candidate = candidate_path(session, index);
        atomic_write(&candidate, &proposal.content)?;
        let metadata = serde_json::to_vec_pretty(&json!({
            "schema": 1,
            "index": index,
            "path": proposal.path,
            "before_sha256": proposal.before_sha256,
            "result_sha256": proposal.result_sha256,
            "candidate": candidate.file_name().expect("candidate file name").to_string_lossy(),
        }))
        .map_err(|error| format!("serialize proposal metadata: {error}"))?;
        atomic_write(&session.join(format!("proposal-{index}.json")), &metadata)?;
        paths.push(candidate);
    }
    Ok(paths)
}

pub(crate) fn candidate_path(session: &Path, index: usize) -> PathBuf {
    session.join(format!("proposal-{index}.candidate"))
}

pub(crate) fn revision_prompt_path(session: &Path) -> PathBuf {
    session.join("revision-1.prompt")
}

pub(crate) fn revision_journal_path(session: &Path) -> PathBuf {
    session.join("revision-1.qj")
}

pub(crate) fn materialize_revision_prompt(
    session: &Path,
    prompt: &[u8],
) -> Result<PathBuf, String> {
    if prompt.is_empty() || prompt.len() > MAX_REVISION_PROMPT_BYTES {
        return Err(format!(
            "revision prompt must contain 1..={MAX_REVISION_PROMPT_BYTES} bytes"
        ));
    }
    let path = revision_prompt_path(session);
    atomic_write(&path, prompt)?;
    Ok(path)
}

pub(crate) fn revision_candidate_path(session: &Path, proposal_index: usize) -> PathBuf {
    session.join(format!("proposal-{proposal_index}.revision-1.candidate"))
}

pub(crate) fn materialize_revision(
    session: &Path,
    revision: &Revision,
    proposal: &Proposal,
) -> Result<PathBuf, String> {
    let candidate = revision_candidate_path(session, revision.proposal_index);
    atomic_write(&candidate, &proposal.content)?;
    let metadata = serde_json::to_vec_pretty(&json!({
        "schema": 1,
        "revision": 1,
        "proposal_index": revision.proposal_index,
        "model": revision.model,
        "feedback_sha256": sha256(revision.feedback.as_bytes()),
        "path": proposal.path,
        "before_sha256": proposal.before_sha256,
        "rejected_result_sha256": revision.rejected.result_sha256,
        "result_sha256": proposal.result_sha256,
        "candidate": candidate.file_name().expect("candidate file name").to_string_lossy(),
    }))
    .map_err(|error| format!("serialize proposal revision metadata: {error}"))?;
    atomic_write(&session.join("revision-1.json"), &metadata)?;
    Ok(candidate)
}

pub(crate) fn continuation_prompt_path(session: &Path) -> PathBuf {
    session.join("continuation-1.prompt")
}

pub(crate) fn continuation_journal_path(session: &Path) -> PathBuf {
    session.join("continuation-1.qj")
}

pub(crate) fn continuation_candidate_path(session: &Path, proposal_index: usize) -> PathBuf {
    session.join(format!("proposal-{proposal_index}.revision-2.candidate"))
}

pub(crate) fn completion_summary_path(session: &Path) -> PathBuf {
    session.join("completion-1.txt")
}

pub(crate) fn materialize_continuation_prompt(
    session: &Path,
    prompt: &[u8],
) -> Result<PathBuf, String> {
    if prompt.is_empty() || prompt.len() > MAX_CONTINUATION_PROMPT_BYTES {
        return Err(format!(
            "continuation prompt must contain 1..={MAX_CONTINUATION_PROMPT_BYTES} bytes"
        ));
    }
    let path = continuation_prompt_path(session);
    atomic_write(&path, prompt)?;
    Ok(path)
}

pub(crate) fn materialize_continuation_response(
    session: &Path,
    continuation: &Continuation,
    response: &ContinuationResponse,
) -> Result<(), String> {
    let (metadata, obsolete) = match response {
        ContinuationResponse::Proposal {
            admitted_path_index,
            proposal_index,
            proposal,
        } => {
            let candidate = continuation_candidate_path(session, *proposal_index);
            atomic_write(&candidate, &proposal.content)?;
            (
                json!({
                    "schema": 1,
                    "outcome": "proposal",
                    "model": continuation.model,
                    "command_attempt": continuation.command.attempt,
                    "command_started_sha256": continuation.command.command_started_sha256,
                    "admitted_path_index": admitted_path_index,
                    "proposal_index": proposal_index,
                    "revision": 2,
                    "path": proposal.path,
                    "before_sha256": proposal.before_sha256,
                    "result_sha256": proposal.result_sha256,
                    "candidate": candidate.file_name().expect("candidate file name").to_string_lossy(),
                }),
                completion_summary_path(session),
            )
        }
        ContinuationResponse::Complete(summary) => {
            let path = completion_summary_path(session);
            atomic_write(&path, summary.as_bytes())?;
            (
                json!({
                    "schema": 1,
                    "outcome": "complete",
                    "model": continuation.model,
                    "command_attempt": continuation.command.attempt,
                    "command_started_sha256": continuation.command.command_started_sha256,
                    "summary_sha256": sha256(summary.as_bytes()),
                    "summary": path.file_name().expect("summary file name").to_string_lossy(),
                }),
                continuation
                    .current
                    .iter()
                    .map(|generation| {
                        continuation_candidate_path(session, generation.proposal_index)
                    })
                    .find(|path| path.exists())
                    .unwrap_or_else(|| session.join("no-continuation-candidate")),
            )
        }
    };
    if obsolete.exists() {
        fs::remove_file(&obsolete)
            .map_err(|error| format!("remove obsolete cache `{}`: {error}", obsolete.display()))?;
    }
    let metadata = serde_json::to_vec_pretty(&metadata)
        .map_err(|error| format!("serialize continuation metadata: {error}"))?;
    atomic_write(&session.join("continuation-1.json"), &metadata)?;
    Ok(())
}

pub(crate) fn resolve_source(repository_root: &Path, path: &str) -> Result<PathBuf, String> {
    validate_relative_path(path)?;
    let root = fs::canonicalize(repository_root)
        .map_err(|error| format!("canonicalize repository root: {error}"))?;
    let source = fs::canonicalize(root.join(path))
        .map_err(|error| format!("canonicalize admitted source `{path}`: {error}"))?;
    if !source.starts_with(&root) || !source.is_file() {
        return Err(format!("admitted source `{path}` left the repository root"));
    }
    Ok(source)
}

fn exact_object<'a>(
    value: &'a Value,
    expected: &[&str],
    label: &str,
) -> Result<&'a Map<String, Value>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{label} must be an object"))?;
    if object.len() != expected.len() || expected.iter().any(|key| !object.contains_key(*key)) {
        return Err(format!(
            "{label} keys must be exactly `{}`",
            expected.join(", ")
        ));
    }
    Ok(object)
}

fn string_field<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{label} `{field}` must be a string"))
}

fn validate_model(model: &str) -> Result<(), String> {
    if model.is_empty() || model.len() > 128 || model.chars().any(char::is_control) {
        return Err("revision model must contain 1..=128 non-control UTF-8 bytes".into());
    }
    Ok(())
}

fn validate_feedback(feedback: &[u8]) -> Result<String, String> {
    if feedback.is_empty() || feedback.len() > MAX_FEEDBACK_BYTES {
        return Err(format!(
            "revision feedback must contain 1..={MAX_FEEDBACK_BYTES} UTF-8 bytes"
        ));
    }
    let feedback =
        std::str::from_utf8(feedback).map_err(|_| "revision feedback is not UTF-8".to_owned())?;
    if feedback.trim().is_empty() {
        return Err("revision feedback must contain non-whitespace text".into());
    }
    Ok(feedback.to_owned())
}

fn validate_relative_path(path: &str) -> Result<(), String> {
    let parsed = Path::new(path);
    if path.is_empty()
        || parsed.is_absolute()
        || parsed
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "proposal path `{path}` is not a clean relative path"
        ));
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("{label} is not lowercase SHA-256"));
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if path.exists()
        && fs::read(path).map_err(|error| format!("read `{}`: {error}", path.display()))? == bytes
    {
        return Ok(());
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, bytes)
        .map_err(|error| format!("write `{}`: {error}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|error| format!("publish `{}`: {error}", path.display()))
}

fn sha256(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a string cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_CASE: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn prompt_round_trip_preserves_exact_admission() {
        let admission = fixture_admission();
        let prompt = admission.prompt_bytes().unwrap();
        assert_eq!(Admission::from_prompt(&prompt).unwrap(), admission);
    }

    #[test]
    fn response_requires_multiple_unique_admitted_changes() {
        let admission = fixture_admission();
        let response = json!({
            "proposals": [
                {
                    "path": "README.md",
                    "before_sha256": admission.files[0].before_sha256,
                    "content": "alpha revised\n"
                },
                {
                    "path": "lode/summary.md",
                    "before_sha256": admission.files[1].before_sha256,
                    "content": "beta revised\n"
                }
            ]
        });
        let proposals =
            parse_response(&serde_json::to_vec(&response).unwrap(), &admission).unwrap();
        assert_eq!(proposals.len(), 2);
        assert_eq!(proposals[0].path, "README.md");
        assert_eq!(proposals[1].content, b"beta revised\n");
    }

    #[test]
    fn revision_round_trip_binds_rejection_feedback_and_original_admission() {
        let admission = fixture_admission();
        let proposals = fixture_proposals(&admission);
        let revision = Revision::new(
            "gpt-test",
            b"Use a more precise label.\n",
            &admission,
            &proposals,
            1,
        )
        .unwrap();
        let prompt = revision.prompt_bytes().unwrap();
        assert_eq!(
            Revision::from_prompt(&prompt, &admission, &proposals).unwrap(),
            revision
        );

        let mut value: Value = serde_json::from_slice(&prompt).unwrap();
        value["admission"]["prompt_sha256"] = Value::String("0".repeat(64));
        assert!(
            Revision::from_prompt(&serde_json::to_vec(&value).unwrap(), &admission, &proposals)
                .is_err()
        );
    }

    #[test]
    fn revision_accepts_only_a_changed_replacement_for_the_rejected_path() {
        let root = temporary_directory("revision");
        let admission = fixture_admission();
        let proposals = fixture_proposals(&admission);
        let revision =
            Revision::new("gpt-test", b"Correct beta.", &admission, &proposals, 1).unwrap();
        let response = json!({
            "proposal": {
                "path": revision.rejected.path,
                "before_sha256": revision.rejected.before_sha256,
                "content": "beta corrected\n"
            }
        });
        let corrected =
            parse_revision_response(&serde_json::to_vec(&response).unwrap(), &revision).unwrap();
        assert_eq!(corrected.content, b"beta corrected\n");
        let path = materialize_revision(&root, &revision, &corrected).unwrap();
        fs::write(&path, b"corrupt").unwrap();
        materialize_revision(&root, &revision, &corrected).unwrap();
        assert_eq!(fs::read(path).unwrap(), corrected.content);

        for response in [
            json!({"proposal": {
                "path": "README.md",
                "before_sha256": revision.rejected.before_sha256,
                "content": "beta corrected\n"
            }}),
            json!({"proposal": {
                "path": revision.rejected.path,
                "before_sha256": revision.rejected.before_sha256,
                "content": "beta revised\n"
            }}),
            json!({"proposal": {
                "path": revision.rejected.path,
                "before_sha256": revision.rejected.before_sha256,
                "content": "beta\n"
            }}),
        ] {
            assert!(
                parse_revision_response(&serde_json::to_vec(&response).unwrap(), &revision)
                    .is_err()
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn diff_rendering_is_exact_and_marks_missing_final_newlines() {
        let proposal = Proposal {
            path: "Cargo.toml".into(),
            before_sha256: "0".repeat(64),
            result_sha256: "1".repeat(64),
            before: b"[package]\nname = \"quartz\"\npublish = false".to_vec(),
            content: b"[package]\nname = \"quartz\"\ndescription = \"Quartz\"\npublish = false\n"
                .to_vec(),
        };
        assert_eq!(
            render_diff(&proposal),
            concat!(
                "--- a/Cargo.toml\n",
                "+++ b/Cargo.toml\n",
                "@@ -1,3 +1,4 @@\n",
                " [package]\n",
                " name = \"quartz\"\n",
                "-publish = false\n",
                "\\ No newline at end of file\n",
                "+description = \"Quartz\"\n",
                "+publish = false\n",
            )
        );
    }

    #[test]
    fn response_rejects_unadmitted_duplicate_stale_and_unchanged_candidates() {
        let admission = fixture_admission();
        let cases = [
            json!({"proposals": [
                proposal("outside.md", &admission.files[0].before_sha256, "changed\n"),
                proposal("lode/summary.md", &admission.files[1].before_sha256, "changed\n")
            ]}),
            json!({"proposals": [
                proposal("README.md", &admission.files[0].before_sha256, "changed\n"),
                proposal("README.md", &admission.files[0].before_sha256, "changed again\n")
            ]}),
            json!({"proposals": [
                proposal("README.md", &"0".repeat(64), "changed\n"),
                proposal("lode/summary.md", &admission.files[1].before_sha256, "changed\n")
            ]}),
            json!({"proposals": [
                proposal("README.md", &admission.files[0].before_sha256, "alpha\n"),
                proposal("lode/summary.md", &admission.files[1].before_sha256, "changed\n")
            ]}),
        ];
        for case in cases {
            assert!(parse_response(&serde_json::to_vec(&case).unwrap(), &admission).is_err());
        }
    }

    #[test]
    fn materialization_is_reconstructed_from_validated_bytes() {
        let root = temporary_directory("materialize");
        let admission = fixture_admission();
        let response = json!({"proposals": [
            proposal("README.md", &admission.files[0].before_sha256, "alpha revised\n"),
            proposal("lode/summary.md", &admission.files[1].before_sha256, "beta revised\n")
        ]});
        let proposals =
            parse_response(&serde_json::to_vec(&response).unwrap(), &admission).unwrap();
        let paths = materialize(&root, &proposals).unwrap();
        fs::write(&paths[0], b"corrupt").unwrap();
        materialize(&root, &proposals).unwrap();
        assert_eq!(fs::read(&paths[0]).unwrap(), b"alpha revised\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn file_admission_rejects_duplicates_and_sources_outside_the_root() {
        let root = temporary_directory("admission");
        let repository = root.join("repository");
        fs::create_dir_all(&repository).unwrap();
        let task = root.join("task.txt");
        let source = repository.join("source.txt");
        let outside = root.join("outside.txt");
        fs::write(&task, b"change both files").unwrap();
        fs::write(&source, b"inside\n").unwrap();
        fs::write(&outside, b"outside\n").unwrap();
        assert!(
            Admission::from_files(&repository, &task, &[source.clone(), source.clone()]).is_err()
        );
        assert!(Admission::from_files(&repository, &task, &[source, outside]).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_command_accepts_only_a_corrected_proposal() {
        let (root, continuation) = fixture_continuation(7);
        let prompt = continuation.prompt_bytes().unwrap();
        assert_eq!(
            Continuation::from_prompt(
                &prompt,
                &continuation.admission,
                &continuation.current,
                &continuation.command,
            )
            .unwrap(),
            continuation
        );
        assert!(parse_continuation_response(b"COMPLETE\nTests passed.", &continuation).is_err());
        let response =
            parse_continuation_response(b"PROPOSE 1\nbeta corrected\n", &continuation).unwrap();
        let ContinuationResponse::Proposal {
            admitted_path_index,
            proposal_index,
            proposal,
        } = response
        else {
            panic!("expected corrected proposal");
        };
        assert_eq!(admitted_path_index, 1);
        assert_eq!(proposal_index, 1);
        assert_eq!(proposal.before, b"beta revised\n");
        assert_eq!(proposal.content, b"beta corrected\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn successful_command_requires_explicit_strict_complete_grammar() {
        let (root, continuation) = fixture_continuation(0);
        assert_eq!(
            parse_continuation_response(b"COMPLETE\nAll checks passed.", &continuation).unwrap(),
            ContinuationResponse::Complete("All checks passed.".into())
        );
        for invalid in [
            &b"COMPLETE"[..],
            &b"complete\nAll checks passed."[..],
            &b"COMPLETE\n   "[..],
            &b"PROPOSE 01\nchanged\n"[..],
            &b"PROPOSE 2\nchanged\n"[..],
            &b"PROPOSE 0\nalpha revised\n"[..],
        ] {
            assert!(parse_continuation_response(invalid, &continuation).is_err());
        }
        fs::remove_dir_all(root).unwrap();
    }

    fn fixture_continuation(exit_code: i32) -> (PathBuf, Continuation) {
        let root = temporary_directory("continuation");
        fs::create_dir_all(root.join("lode")).unwrap();
        fs::write(root.join("README.md"), b"alpha revised\n").unwrap();
        fs::write(root.join("lode/summary.md"), b"beta revised\n").unwrap();
        let admission = fixture_admission();
        let proposals = fixture_proposals(&admission);
        let current = proposals
            .into_iter()
            .enumerate()
            .map(|(index, proposal)| ProposalGeneration {
                proposal_index: index,
                admitted_path_index: index,
                revision: 0,
                proposal,
            })
            .collect::<Vec<_>>();
        let paths = admission
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let started = crate::commands::CommandStarted::new(
            1,
            vec![
                "/bin/sh".into(),
                "-c".into(),
                format!("printf command-result; exit {exit_code}"),
            ],
            &root,
            &paths,
        )
        .unwrap();
        let execution = crate::commands::execute(&started);
        let after = RepositoryIdentity::capture(&root, &paths).unwrap();
        let command = CommandFinished::new(&started, execution, after).unwrap();
        let continuation =
            Continuation::new("test-model", &admission, &current, &command, &root).unwrap();
        (root, continuation)
    }

    fn fixture_admission() -> Admission {
        Admission {
            task: "revise both files".into(),
            files: vec![
                admitted("README.md", b"alpha\n"),
                admitted("lode/summary.md", b"beta\n"),
            ],
        }
    }

    fn fixture_proposals(admission: &Admission) -> Vec<Proposal> {
        let response = json!({"proposals": [
            proposal("README.md", &admission.files[0].before_sha256, "alpha revised\n"),
            proposal(
                "lode/summary.md",
                &admission.files[1].before_sha256,
                "beta revised\n"
            )
        ]});
        parse_response(&serde_json::to_vec(&response).unwrap(), admission).unwrap()
    }

    fn admitted(path: &str, content: &[u8]) -> AdmittedFile {
        AdmittedFile {
            path: path.into(),
            before_sha256: sha256(content),
            content: content.to_vec(),
        }
    }

    fn proposal(path: &str, before_sha256: &str, content: &str) -> Value {
        json!({
            "path": path,
            "before_sha256": before_sha256,
            "content": content,
        })
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "quartz-proposals-{label}-{}-{}",
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
