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

const INSTRUCTIONS: &str = "Edit only the admitted files needed for the task. Return only the required JSON object. Every proposal must use one admitted path and matching before_sha256, and content must be the complete replacement file. Return at least two proposals. Do not use Markdown fences or commentary.";

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

    fn fixture_admission() -> Admission {
        Admission {
            task: "revise both files".into(),
            files: vec![
                admitted("README.md", b"alpha\n"),
                admitted("lode/summary.md", b"beta\n"),
            ],
        }
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
