use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    path::{Component, Path},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(crate) const COMMAND_TIMEOUT_MS: u64 = 120_000;
pub(crate) const MAX_OUTPUT_BYTES: usize = 32 * 1024;
pub(crate) const MAX_ARG_BYTES: usize = 4 * 1024;
pub(crate) const MAX_ARGV_BYTES: usize = 32 * 1024;
const MAX_FACT_BYTES: usize = 256 * 1024;
const MAX_ERROR_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RepositoryIdentity {
    pub(crate) canonical_root: String,
    pub(crate) canonical_root_sha256: String,
    pub(crate) files: Vec<RepositoryFileIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RepositoryFileIdentity {
    pub(crate) path: String,
    pub(crate) status: String,
    pub(crate) byte_len: Option<u64>,
    pub(crate) sha256: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommandStarted {
    schema: u32,
    kind: String,
    pub(crate) attempt: u64,
    pub(crate) argv: Vec<String>,
    pub(crate) repository: RepositoryIdentity,
    pub(crate) cwd: String,
    pub(crate) timeout_ms: u64,
    pub(crate) stdout_limit_bytes: usize,
    pub(crate) stderr_limit_bytes: usize,
    approval: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BoundedOutput {
    pub(crate) encoding: String,
    pub(crate) content: String,
    pub(crate) truncated: bool,
    pub(crate) read_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommandFinished {
    schema: u32,
    kind: String,
    pub(crate) attempt: u64,
    pub(crate) command_started_sha256: String,
    pub(crate) argv: Vec<String>,
    pub(crate) repository: RepositoryIdentity,
    pub(crate) exit_code: Option<i32>,
    pub(crate) signal: Option<i32>,
    pub(crate) timed_out: bool,
    pub(crate) spawn_error: Option<String>,
    pub(crate) stdout: BoundedOutput,
    pub(crate) stderr: BoundedOutput,
    pub(crate) duration_ms: u64,
    pub(crate) repository_after: RepositoryIdentity,
}

#[derive(Debug)]
pub(crate) struct ExecutionResult {
    exit_code: Option<i32>,
    signal: Option<i32>,
    timed_out: bool,
    spawn_error: Option<String>,
    stdout: CapturedBytes,
    stderr: CapturedBytes,
    duration_ms: u64,
}

#[derive(Debug)]
struct CapturedBytes {
    bytes: Vec<u8>,
    truncated: bool,
    read_error: Option<String>,
}

impl RepositoryIdentity {
    pub(crate) fn capture(repository_root: &Path, paths: &[String]) -> Result<Self, String> {
        let root = fs::canonicalize(repository_root)
            .map_err(|error| format!("canonicalize repository root: {error}"))?;
        let canonical_root = root
            .to_str()
            .ok_or_else(|| "canonical repository root is not UTF-8".to_owned())?
            .to_owned();
        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            validate_relative_path(path)?;
            let requested = root.join(path);
            let identity = match fs::canonicalize(&requested) {
                Ok(canonical) if canonical.starts_with(&root) && canonical.is_file() => {
                    match fs::read(&canonical) {
                        Ok(bytes) => {
                            RepositoryFileIdentity {
                                path: path.clone(),
                                status: "regular".into(),
                                byte_len: Some(u64::try_from(bytes.len()).map_err(|_| {
                                    format!("repository file `{path}` is too large")
                                })?),
                                sha256: Some(sha256(&bytes)),
                            }
                        }
                        Err(error) => RepositoryFileIdentity {
                            path: path.clone(),
                            status: format!("unreadable:{error}"),
                            byte_len: None,
                            sha256: None,
                        },
                    }
                }
                Ok(_) => RepositoryFileIdentity {
                    path: path.clone(),
                    status: "outside-or-not-regular".into(),
                    byte_len: None,
                    sha256: None,
                },
                Err(error) if error.kind() == io::ErrorKind::NotFound => RepositoryFileIdentity {
                    path: path.clone(),
                    status: "missing".into(),
                    byte_len: None,
                    sha256: None,
                },
                Err(error) => RepositoryFileIdentity {
                    path: path.clone(),
                    status: format!("unreadable:{error}"),
                    byte_len: None,
                    sha256: None,
                },
            };
            files.push(identity);
        }
        Ok(Self {
            canonical_root_sha256: sha256(canonical_root.as_bytes()),
            canonical_root,
            files,
        })
    }

    pub(crate) fn require_regular(&self) -> Result<(), String> {
        if let Some(file) = self.files.iter().find(|file| file.status != "regular") {
            return Err(format!(
                "admitted repository file `{}` is {}",
                file.path, file.status
            ));
        }
        Ok(())
    }
}

impl RepositoryIdentity {
    pub(crate) fn to_value(&self) -> Value {
        json!({
            "canonical_root": self.canonical_root,
            "canonical_root_sha256": self.canonical_root_sha256,
            "files": self.files.iter().map(RepositoryFileIdentity::to_value).collect::<Vec<_>>(),
        })
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let object = exact_object(
            value,
            &["canonical_root", "canonical_root_sha256", "files"],
            "repository identity",
        )?;
        let files = object
            .get("files")
            .and_then(Value::as_array)
            .ok_or_else(|| "repository identity files must be an array".to_owned())?
            .iter()
            .map(RepositoryFileIdentity::from_value)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            canonical_root: string_field(object, "canonical_root", "repository identity")?.into(),
            canonical_root_sha256: string_field(
                object,
                "canonical_root_sha256",
                "repository identity",
            )?
            .into(),
            files,
        })
    }
}

impl RepositoryFileIdentity {
    fn to_value(&self) -> Value {
        json!({
            "path": self.path,
            "status": self.status,
            "byte_len": self.byte_len,
            "sha256": self.sha256,
        })
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let object = exact_object(
            value,
            &["path", "status", "byte_len", "sha256"],
            "repository file identity",
        )?;
        Ok(Self {
            path: string_field(object, "path", "repository file identity")?.into(),
            status: string_field(object, "status", "repository file identity")?.into(),
            byte_len: optional_u64_field(object, "byte_len", "repository file identity")?,
            sha256: optional_string_field(object, "sha256", "repository file identity")?,
        })
    }
}

impl CommandStarted {
    pub(crate) fn new(
        attempt: u64,
        argv: Vec<String>,
        repository_root: &Path,
        admitted_paths: &[String],
    ) -> Result<Self, String> {
        if attempt == 0 {
            return Err("command attempt must be positive".into());
        }
        validate_argv(&argv)?;
        let repository = RepositoryIdentity::capture(repository_root, admitted_paths)?;
        repository.require_regular()?;
        Ok(Self {
            schema: 1,
            kind: "CommandStarted".into(),
            attempt,
            argv,
            cwd: repository.canonical_root.clone(),
            repository,
            timeout_ms: COMMAND_TIMEOUT_MS,
            stdout_limit_bytes: MAX_OUTPUT_BYTES,
            stderr_limit_bytes: MAX_OUTPUT_BYTES,
            approval: "explicit-cli-invocation".into(),
        })
    }

    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>, String> {
        encode_fact(&self.to_value(), "CommandStarted")
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let value = decode_fact(bytes, "CommandStarted")?;
        let object = exact_object(
            &value,
            &[
                "schema",
                "kind",
                "attempt",
                "argv",
                "repository",
                "cwd",
                "timeout_ms",
                "stdout_limit_bytes",
                "stderr_limit_bytes",
                "approval",
            ],
            "CommandStarted",
        )?;
        let started = Self {
            schema: u32_field(object, "schema", "CommandStarted")?,
            kind: string_field(object, "kind", "CommandStarted")?.into(),
            attempt: u64_field(object, "attempt", "CommandStarted")?,
            argv: string_array_field(object, "argv", "CommandStarted")?,
            repository: RepositoryIdentity::from_value(
                object.get("repository").expect("required key checked"),
            )?,
            cwd: string_field(object, "cwd", "CommandStarted")?.into(),
            timeout_ms: u64_field(object, "timeout_ms", "CommandStarted")?,
            stdout_limit_bytes: usize_field(object, "stdout_limit_bytes", "CommandStarted")?,
            stderr_limit_bytes: usize_field(object, "stderr_limit_bytes", "CommandStarted")?,
            approval: string_field(object, "approval", "CommandStarted")?.into(),
        };
        started.validate()?;
        Ok(started)
    }

    fn to_value(&self) -> Value {
        json!({
            "schema": self.schema,
            "kind": self.kind,
            "attempt": self.attempt,
            "argv": self.argv,
            "repository": self.repository.to_value(),
            "cwd": self.cwd,
            "timeout_ms": self.timeout_ms,
            "stdout_limit_bytes": self.stdout_limit_bytes,
            "stderr_limit_bytes": self.stderr_limit_bytes,
            "approval": self.approval,
        })
    }

    pub(crate) fn sha256(&self) -> Result<String, String> {
        Ok(sha256(&self.to_bytes()?))
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema != 1 || self.kind != "CommandStarted" {
            return Err("unsupported CommandStarted fact".into());
        }
        if self.attempt == 0
            || self.timeout_ms != COMMAND_TIMEOUT_MS
            || self.stdout_limit_bytes != MAX_OUTPUT_BYTES
            || self.stderr_limit_bytes != MAX_OUTPUT_BYTES
            || self.approval != "explicit-cli-invocation"
            || self.cwd != self.repository.canonical_root
            || self.repository.canonical_root_sha256 != sha256(self.cwd.as_bytes())
        {
            return Err("CommandStarted policy fields changed".into());
        }
        self.repository.require_regular()?;
        validate_argv(&self.argv)
    }
}

impl CommandFinished {
    pub(crate) fn new(
        started: &CommandStarted,
        execution: ExecutionResult,
        repository_after: RepositoryIdentity,
    ) -> Result<Self, String> {
        let finished = Self {
            schema: 1,
            kind: "CommandFinished".into(),
            attempt: started.attempt,
            command_started_sha256: started.sha256()?,
            argv: started.argv.clone(),
            repository: started.repository.clone(),
            exit_code: execution.exit_code,
            signal: execution.signal,
            timed_out: execution.timed_out,
            spawn_error: execution.spawn_error,
            stdout: BoundedOutput::from_capture(execution.stdout),
            stderr: BoundedOutput::from_capture(execution.stderr),
            duration_ms: execution.duration_ms,
            repository_after,
        };
        finished.validate(started)?;
        Ok(finished)
    }

    pub(crate) fn to_bytes(&self) -> Result<Vec<u8>, String> {
        encode_fact(&self.to_value(), "CommandFinished")
    }

    pub(crate) fn from_bytes(bytes: &[u8], started: &CommandStarted) -> Result<Self, String> {
        let value = decode_fact(bytes, "CommandFinished")?;
        let object = exact_object(
            &value,
            &[
                "schema",
                "kind",
                "attempt",
                "command_started_sha256",
                "argv",
                "repository",
                "exit_code",
                "signal",
                "timed_out",
                "spawn_error",
                "stdout",
                "stderr",
                "duration_ms",
                "repository_after",
            ],
            "CommandFinished",
        )?;
        let finished = Self {
            schema: u32_field(object, "schema", "CommandFinished")?,
            kind: string_field(object, "kind", "CommandFinished")?.into(),
            attempt: u64_field(object, "attempt", "CommandFinished")?,
            command_started_sha256: string_field(
                object,
                "command_started_sha256",
                "CommandFinished",
            )?
            .into(),
            argv: string_array_field(object, "argv", "CommandFinished")?,
            repository: RepositoryIdentity::from_value(
                object.get("repository").expect("required key checked"),
            )?,
            exit_code: optional_i32_field(object, "exit_code", "CommandFinished")?,
            signal: optional_i32_field(object, "signal", "CommandFinished")?,
            timed_out: bool_field(object, "timed_out", "CommandFinished")?,
            spawn_error: optional_string_field(object, "spawn_error", "CommandFinished")?,
            stdout: BoundedOutput::from_value(object.get("stdout").expect("required key checked"))?,
            stderr: BoundedOutput::from_value(object.get("stderr").expect("required key checked"))?,
            duration_ms: u64_field(object, "duration_ms", "CommandFinished")?,
            repository_after: RepositoryIdentity::from_value(
                object
                    .get("repository_after")
                    .expect("required key checked"),
            )?,
        };
        finished.validate(started)?;
        Ok(finished)
    }

    pub(crate) fn to_value(&self) -> Value {
        json!({
            "schema": self.schema,
            "kind": self.kind,
            "attempt": self.attempt,
            "command_started_sha256": self.command_started_sha256,
            "argv": self.argv,
            "repository": self.repository.to_value(),
            "exit_code": self.exit_code,
            "signal": self.signal,
            "timed_out": self.timed_out,
            "spawn_error": self.spawn_error,
            "stdout": self.stdout.to_value(),
            "stderr": self.stderr.to_value(),
            "duration_ms": self.duration_ms,
            "repository_after": self.repository_after.to_value(),
        })
    }

    pub(crate) fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
            && self.signal.is_none()
            && !self.timed_out
            && self.spawn_error.is_none()
    }

    fn validate(&self, started: &CommandStarted) -> Result<(), String> {
        if self.schema != 1 || self.kind != "CommandFinished" {
            return Err("unsupported CommandFinished fact".into());
        }
        if self.attempt != started.attempt
            || self.argv != started.argv
            || self.repository != started.repository
            || self.command_started_sha256 != started.sha256()?
        {
            return Err("CommandFinished does not bind its CommandStarted fact".into());
        }
        if self.spawn_error.is_some()
            && (self.exit_code.is_some() || self.signal.is_some() || self.timed_out)
        {
            return Err("spawn failure cannot also contain process status".into());
        }
        if self.spawn_error.is_none() && self.exit_code.is_none() && self.signal.is_none() {
            return Err("CommandFinished has neither exit code, signal, nor spawn failure".into());
        }
        if self.duration_ms > COMMAND_TIMEOUT_MS.saturating_add(30_000) && !self.timed_out {
            return Err("CommandFinished duration exceeds the fixed command bound".into());
        }
        self.stdout.validate()?;
        self.stderr.validate()?;
        validate_argv(&self.argv)
    }
}

impl BoundedOutput {
    fn from_capture(captured: CapturedBytes) -> Self {
        let (encoding, content) = match String::from_utf8(captured.bytes) {
            Ok(text) => ("utf-8".to_owned(), text),
            Err(error) => ("hex".to_owned(), hex(error.as_bytes())),
        };
        Self {
            encoding,
            content,
            truncated: captured.truncated,
            read_error: captured.read_error,
        }
    }

    fn to_value(&self) -> Value {
        json!({
            "encoding": self.encoding,
            "content": self.content,
            "truncated": self.truncated,
            "read_error": self.read_error,
        })
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let object = exact_object(
            value,
            &["encoding", "content", "truncated", "read_error"],
            "bounded command output",
        )?;
        Ok(Self {
            encoding: string_field(object, "encoding", "bounded command output")?.into(),
            content: string_field(object, "content", "bounded command output")?.into(),
            truncated: bool_field(object, "truncated", "bounded command output")?,
            read_error: optional_string_field(object, "read_error", "bounded command output")?,
        })
    }

    pub(crate) fn bytes(&self) -> Result<Vec<u8>, String> {
        match self.encoding.as_str() {
            "utf-8" => Ok(self.content.as_bytes().to_vec()),
            "hex" => decode_hex(&self.content),
            _ => Err("command output has an unsupported encoding".into()),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.bytes()?.len() > MAX_OUTPUT_BYTES {
            return Err("command output exceeds its retained byte bound".into());
        }
        if self
            .read_error
            .as_ref()
            .is_some_and(|error| error.len() > MAX_ERROR_BYTES)
        {
            return Err("command output read error exceeds its bound".into());
        }
        Ok(())
    }
}

pub(crate) fn execute(started: &CommandStarted) -> ExecutionResult {
    let began = Instant::now();
    let mut command = Command::new(&started.argv[0]);
    command
        .args(&started.argv[1..])
        .current_dir(&started.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ExecutionResult {
                exit_code: None,
                signal: None,
                timed_out: false,
                spawn_error: Some(bound_error(error.to_string())),
                stdout: CapturedBytes::empty(),
                stderr: CapturedBytes::empty(),
                duration_ms: elapsed_millis(began),
            };
        }
    };
    let stdout = child.stdout.take().expect("piped stdout must be present");
    let stderr = child.stderr.take().expect("piped stderr must be present");
    let stdout_reader = thread::spawn(move || capture(stdout));
    let stderr_reader = thread::spawn(move || capture(stderr));
    let deadline = Duration::from_millis(started.timeout_ms);
    let (status, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (Some(status), false),
            Ok(None) if began.elapsed() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                break (child.wait().ok(), true);
            }
            Err(_) => break (child.wait().ok(), false),
        }
    };
    let stdout = stdout_reader
        .join()
        .unwrap_or_else(|_| CapturedBytes::reader_panicked());
    let stderr = stderr_reader
        .join()
        .unwrap_or_else(|_| CapturedBytes::reader_panicked());
    let exit_code = status.as_ref().and_then(std::process::ExitStatus::code);
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.as_ref().and_then(std::process::ExitStatus::signal)
    };
    #[cfg(not(unix))]
    let signal = None;
    ExecutionResult {
        exit_code,
        signal,
        timed_out,
        spawn_error: None,
        stdout,
        stderr,
        duration_ms: elapsed_millis(began),
    }
}

impl CapturedBytes {
    fn empty() -> Self {
        Self {
            bytes: Vec::new(),
            truncated: false,
            read_error: None,
        }
    }

    fn reader_panicked() -> Self {
        Self {
            bytes: Vec::new(),
            truncated: false,
            read_error: Some("output reader panicked".into()),
        }
    }
}

fn capture(mut reader: impl Read) -> CapturedBytes {
    let mut output = Vec::with_capacity(MAX_OUTPUT_BYTES.min(8 * 1024));
    let mut truncated = false;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                return CapturedBytes {
                    bytes: output,
                    truncated,
                    read_error: None,
                };
            }
            Ok(length) => {
                let remaining = MAX_OUTPUT_BYTES.saturating_sub(output.len());
                let retained = remaining.min(length);
                output.extend_from_slice(&buffer[..retained]);
                truncated |= retained != length;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                return CapturedBytes {
                    bytes: output,
                    truncated,
                    read_error: Some(bound_error(error.to_string())),
                };
            }
        }
    }
}

fn encode_fact(value: &Value, label: &str) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("serialize {label} fact: {error}"))?;
    if bytes.is_empty() || bytes.len() > MAX_FACT_BYTES {
        return Err(format!(
            "{label} fact must contain 1..={MAX_FACT_BYTES} bytes"
        ));
    }
    Ok(bytes)
}

fn decode_fact(bytes: &[u8], label: &str) -> Result<Value, String> {
    if bytes.is_empty() || bytes.len() > MAX_FACT_BYTES {
        return Err(format!(
            "durable {label} fact must contain 1..={MAX_FACT_BYTES} bytes"
        ));
    }
    serde_json::from_slice(bytes).map_err(|error| format!("invalid durable {label} fact: {error}"))
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

fn optional_string_field(
    object: &Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<Option<String>, String> {
    match object.get(field).expect("required key checked") {
        Value::Null => Ok(None),
        Value::String(value) => Ok(Some(value.clone())),
        _ => Err(format!("{label} `{field}` must be a string or null")),
    }
}

fn u64_field(object: &Map<String, Value>, field: &str, label: &str) -> Result<u64, String> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{label} `{field}` must be a non-negative integer"))
}

fn optional_u64_field(
    object: &Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<Option<u64>, String> {
    match object.get(field).expect("required key checked") {
        Value::Null => Ok(None),
        value => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{label} `{field}` must be a non-negative integer or null")),
    }
}

fn u32_field(object: &Map<String, Value>, field: &str, label: &str) -> Result<u32, String> {
    u32::try_from(u64_field(object, field, label)?)
        .map_err(|_| format!("{label} `{field}` exceeds u32"))
}

fn usize_field(object: &Map<String, Value>, field: &str, label: &str) -> Result<usize, String> {
    usize::try_from(u64_field(object, field, label)?)
        .map_err(|_| format!("{label} `{field}` exceeds usize"))
}

fn optional_i32_field(
    object: &Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<Option<i32>, String> {
    match object.get(field).expect("required key checked") {
        Value::Null => Ok(None),
        value => value
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| format!("{label} `{field}` must be an i32 or null")),
    }
}

fn bool_field(object: &Map<String, Value>, field: &str, label: &str) -> Result<bool, String> {
    object
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{label} `{field}` must be a boolean"))
}

fn string_array_field(
    object: &Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<Vec<String>, String> {
    object
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{label} `{field}` must be an array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{label} `{field}` values must be strings"))
        })
        .collect()
}

fn validate_argv(argv: &[String]) -> Result<(), String> {
    if argv.first().is_none_or(String::is_empty) {
        return Err("approved command requires a non-empty executable argument".into());
    }
    let mut total = 0_usize;
    for argument in argv {
        if argument.as_bytes().len() > MAX_ARG_BYTES {
            return Err(format!(
                "approved command argument exceeds {MAX_ARG_BYTES} bytes"
            ));
        }
        total = total
            .checked_add(argument.len())
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| "approved command argv size overflow".to_owned())?;
    }
    if total > MAX_ARGV_BYTES {
        return Err(format!(
            "approved command argv exceeds {MAX_ARGV_BYTES} bytes"
        ));
    }
    Ok(())
}

fn validate_relative_path(path: &str) -> Result<(), String> {
    let parsed = Path::new(path);
    if path.is_empty()
        || parsed.is_absolute()
        || parsed
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        return Err(format!(
            "admitted path `{path}` is not a clean relative path"
        ));
    }
    Ok(())
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn bound_error(error: String) -> String {
    if error.len() <= MAX_ERROR_BYTES {
        return error;
    }
    let mut end = MAX_ERROR_BYTES;
    while !error.is_char_boundary(end) {
        end -= 1;
    }
    error[..end].to_owned()
}

fn sha256(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a string cannot fail");
    }
    output
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a string cannot fail");
    }
    output
}

fn decode_hex(input: &str) -> Result<Vec<u8>, String> {
    if input.len() % 2 != 0 {
        return Err("hex command output has odd length".into());
    }
    input
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digit = |byte| match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                _ => None,
            };
            let high = digit(pair[0]).ok_or_else(|| "invalid hex command output".to_owned())?;
            let low = digit(pair[1]).ok_or_else(|| "invalid hex command output".to_owned())?;
            Ok((high << 4) | low)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_CASE: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn command_facts_round_trip_exact_output_and_identity() {
        let root = temporary_repository("round-trip");
        let started = CommandStarted::new(
            1,
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf 'ok'; printf '\\377' >&2".into(),
            ],
            &root,
            &["source.txt".into()],
        )
        .unwrap();
        let reconstructed = CommandStarted::from_bytes(&started.to_bytes().unwrap()).unwrap();
        assert_eq!(reconstructed, started);
        let execution = execute(&started);
        let after = RepositoryIdentity::capture(&root, &["source.txt".into()]).unwrap();
        let finished = CommandFinished::new(&started, execution, after).unwrap();
        assert_eq!(finished.stdout.bytes().unwrap(), b"ok");
        assert_eq!(finished.stderr.bytes().unwrap(), [0xff]);
        assert_eq!(
            CommandFinished::from_bytes(&finished.to_bytes().unwrap(), &started).unwrap(),
            finished
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_command_is_terminal_and_not_successful() {
        let root = temporary_repository("failure");
        let started = CommandStarted::new(
            1,
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf failed >&2; exit 7".into(),
            ],
            &root,
            &["source.txt".into()],
        )
        .unwrap();
        let execution = execute(&started);
        let after = RepositoryIdentity::capture(&root, &["source.txt".into()]).unwrap();
        let finished = CommandFinished::new(&started, execution, after).unwrap();
        assert_eq!(finished.exit_code, Some(7));
        assert_eq!(finished.stderr.bytes().unwrap(), b"failed");
        assert!(!finished.succeeded());
        fs::remove_dir_all(root).unwrap();
    }

    fn temporary_repository(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "quartz-command-{label}-{}-{}",
            std::process::id(),
            NEXT_CASE.fetch_add(1, Ordering::Relaxed)
        ));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("source.txt"), b"source\n").unwrap();
        root
    }
}
