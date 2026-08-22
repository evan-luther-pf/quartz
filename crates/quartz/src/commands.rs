use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::Value;
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
const MAX_FACT_BYTES: usize = 3 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RepositoryIdentity {
    pub(crate) canonical_root: String,
    pub(crate) canonical_root_sha256: String,
    pub(crate) files: Vec<RepositoryFileIdentity>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RepositoryFileIdentity {
    pub(crate) byte_len: Option<u64>,
    pub(crate) content: Option<String>,
    pub(crate) path: String,
    pub(crate) sha256: Option<String>,
    pub(crate) status: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandStarted {
    approval: String,
    pub(crate) argv: Vec<String>,
    pub(crate) attempt: u64,
    pub(crate) cwd: String,
    kind: String,
    pub(crate) repository: RepositoryIdentity,
    schema: u32,
    pub(crate) stderr_limit_bytes: usize,
    pub(crate) stdout_limit_bytes: usize,
    pub(crate) timeout_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BoundedOutput {
    pub(crate) content: String,
    pub(crate) encoding: String,
    pub(crate) read_error: Option<String>,
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandFinished {
    pub(crate) argv: Vec<String>,
    pub(crate) attempt: u64,
    pub(crate) command_started_sha256: String,
    pub(crate) duration_ms: u64,
    pub(crate) exit_code: Option<i32>,
    kind: String,
    pub(crate) repository: RepositoryIdentity,
    pub(crate) repository_after: RepositoryIdentity,
    schema: u32,
    pub(crate) signal: Option<i32>,
    pub(crate) spawn_error: Option<String>,
    pub(crate) stderr: BoundedOutput,
    pub(crate) stdout: BoundedOutput,
    pub(crate) timed_out: bool,
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
                            let content = String::from_utf8(bytes.clone()).ok();
                            RepositoryFileIdentity {
                                path: path.clone(),
                                status: "regular".into(),
                                byte_len: Some(u64::try_from(bytes.len()).map_err(|_| {
                                    format!("repository file `{path}` is too large")
                                })?),
                                sha256: Some(sha256(&bytes)),
                                content,
                            }
                        }
                        Err(error) => RepositoryFileIdentity {
                            path: path.clone(),
                            status: format!("unreadable:{error}"),
                            byte_len: None,
                            sha256: None,
                            content: None,
                        },
                    }
                }
                Ok(_) => RepositoryFileIdentity {
                    path: path.clone(),
                    status: "outside-or-not-regular".into(),
                    byte_len: None,
                    sha256: None,
                    content: None,
                },
                Err(error) if error.kind() == io::ErrorKind::NotFound => RepositoryFileIdentity {
                    path: path.clone(),
                    status: "missing".into(),
                    byte_len: None,
                    sha256: None,
                    content: None,
                },
                Err(error) => RepositoryFileIdentity {
                    path: path.clone(),
                    status: format!("unreadable:{error}"),
                    byte_len: None,
                    sha256: None,
                    content: None,
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
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| format!("serialize CommandStarted fact: {error}"))?;
        if bytes.is_empty() || bytes.len() > MAX_FACT_BYTES {
            return Err(format!(
                "CommandStarted fact must contain 1..={MAX_FACT_BYTES} bytes"
            ));
        }
        Ok(bytes)
    }

    #[cfg(test)]
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.is_empty() || bytes.len() > MAX_FACT_BYTES {
            return Err(format!(
                "durable CommandStarted fact must contain 1..={MAX_FACT_BYTES} bytes"
            ));
        }
        let started: Self = serde_json::from_slice(bytes)
            .map_err(|error| format!("invalid durable CommandStarted fact: {error}"))?;
        started.validate()?;
        Ok(started)
    }

    pub(crate) fn sha256(&self) -> Result<String, String> {
        Ok(sha256(&self.to_bytes()?))
    }

    #[cfg(test)]
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
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| format!("serialize CommandFinished fact: {error}"))?;
        if bytes.is_empty() || bytes.len() > MAX_FACT_BYTES {
            return Err(format!(
                "CommandFinished fact must contain 1..={MAX_FACT_BYTES} bytes"
            ));
        }
        Ok(bytes)
    }

    #[cfg(test)]
    pub(crate) fn from_bytes(bytes: &[u8], started: &CommandStarted) -> Result<Self, String> {
        if bytes.is_empty() || bytes.len() > MAX_FACT_BYTES {
            return Err(format!(
                "durable CommandFinished fact must contain 1..={MAX_FACT_BYTES} bytes"
            ));
        }
        let finished: Self = serde_json::from_slice(bytes)
            .map_err(|error| format!("invalid durable CommandFinished fact: {error}"))?;
        finished.validate(started)?;
        Ok(finished)
    }

    #[cfg(test)]
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

pub(crate) fn validate_argv(argv: &[String]) -> Result<(), String> {
    if argv.first().is_none_or(String::is_empty) {
        return Err("approved command requires a non-empty executable argument".into());
    }
    let mut total = 0_usize;
    for argument in argv {
        if argument.len() > MAX_ARG_BYTES {
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
    if !input.len().is_multiple_of(2) {
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

    #[test]
    fn serde_rejects_invalid_command_facts() {
        let root = temporary_repository("invalid-fact");
        let started =
            CommandStarted::new(1, vec!["/bin/true".into()], &root, &["source.txt".into()])
                .unwrap();
        let mut value = serde_json::to_value(&started).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), Value::Bool(true));
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(CommandStarted::from_bytes(&bytes).is_err());
        let mut value = serde_json::to_value(&started).unwrap();
        value.as_object_mut().unwrap().remove("approval");
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(CommandStarted::from_bytes(&bytes).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn serialization_matches_legacy_bytes() {
        let started = CommandStarted {
            approval: "explicit-cli-invocation".into(),
            argv: vec!["/bin/echo".into(), "hello".into()],
            attempt: 1,
            cwd: "/fixed/repository".into(),
            kind: "CommandStarted".into(),
            repository: RepositoryIdentity {
                canonical_root: "/fixed/repository".into(),
                canonical_root_sha256: sha256(b"/fixed/repository"),
                files: vec![RepositoryFileIdentity {
                    byte_len: Some(7),
                    content: Some("content".into()),
                    path: "source.txt".into(),
                    sha256: Some(
                        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
                    ),
                    status: "regular".into(),
                }],
            },
            schema: 1,
            stderr_limit_bytes: MAX_OUTPUT_BYTES,
            stdout_limit_bytes: MAX_OUTPUT_BYTES,
            timeout_ms: COMMAND_TIMEOUT_MS,
        };
        let expected = b"{\n  \"approval\": \"explicit-cli-invocation\",\n  \"argv\": [\n    \"/bin/echo\",\n    \"hello\"\n  ],\n  \"attempt\": 1,\n  \"cwd\": \"/fixed/repository\",\n  \"kind\": \"CommandStarted\",\n  \"repository\": {\n    \"canonical_root\": \"/fixed/repository\",\n    \"canonical_root_sha256\": \"15250920c2ea0f032234df7baf7a6737a6f3587be102adb71b08e0d56ae47af8\",\n    \"files\": [\n      {\n        \"byte_len\": 7,\n        \"content\": \"content\",\n        \"path\": \"source.txt\",\n        \"sha256\": \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\",\n        \"status\": \"regular\"\n      }\n    ]\n  },\n  \"schema\": 1,\n  \"stderr_limit_bytes\": 32768,\n  \"stdout_limit_bytes\": 32768,\n  \"timeout_ms\": 120000\n}";
        assert_eq!(started.to_bytes().unwrap(), expected);
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
