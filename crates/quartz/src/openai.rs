use std::{
    thread,
    time::{Duration, Instant},
};

use quartz_kernel::{
    ExchangeAdapter, ExchangeFailure, ExchangeResponse, ExchangeTerminalMetadata, IncompleteReason,
    RemoteErrorCode,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) struct OpenAiResponses {
    api_key: String,
    model: String,
}

impl OpenAiResponses {
    pub(crate) fn new(api_key: String, model: String) -> Result<Self, &'static str> {
        if api_key.is_empty() {
            return Err("OPENAI_API_KEY is empty");
        }
        if model.is_empty() {
            return Err("production model is empty");
        }
        Ok(Self { api_key, model })
    }
}

impl ExchangeAdapter for OpenAiResponses {
    fn identity(&self) -> &str {
        "openai-responses"
    }

    fn exchange(
        &self,
        request: &[u8],
        timeout: Duration,
        max_response_bytes: usize,
    ) -> Result<ExchangeResponse, ExchangeFailure> {
        let prompt = std::str::from_utf8(request).map_err(|_| ExchangeFailure::Protocol)?;
        let envelope_limit = max_response_bytes
            .saturating_mul(8)
            .clamp(1024 * 1024, 8 * 1024 * 1024);
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(ExchangeFailure::Protocol)?;
        let mut response = request_agent(timeout)
            .post("https://api.openai.com/v1/responses")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send_json(request_body(&self.model, prompt))
            .map_err(|_| ExchangeFailure::Ambiguous)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(http_failure(status));
        }
        let mut envelope = read_envelope(&mut response, envelope_limit)?;
        loop {
            match envelope.get("status").and_then(Value::as_str) {
                Some("completed") => {
                    return normalize_response(envelope, max_response_bytes);
                }
                Some("queued" | "in_progress") => {}
                Some("failed" | "cancelled" | "incomplete") => {
                    return Err(terminal_response_failure(&envelope)?);
                }
                Some(_) | None => return Err(ExchangeFailure::Protocol),
            }

            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(ExchangeFailure::Ambiguous)?;
            thread::sleep(remaining.min(Duration::from_millis(250)));
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(ExchangeFailure::Ambiguous)?;
            let id = envelope
                .get("id")
                .and_then(Value::as_str)
                .ok_or(ExchangeFailure::Protocol)?;
            let mut response = request_agent(remaining)
                .get(format!("https://api.openai.com/v1/responses/{id}"))
                .header("Authorization", format!("Bearer {}", self.api_key))
                .call()
                .map_err(|_| ExchangeFailure::Ambiguous)?;
            let status = response.status().as_u16();
            if !(200..300).contains(&status) {
                return Err(http_failure(status));
            }
            envelope = read_envelope(&mut response, envelope_limit)?;
        }
    }
}

fn request_body(model: &str, prompt: &str) -> Value {
    json!({
        "model": model,
        "input": prompt,
        "background": true,
        "store": false,
        "max_output_tokens": 4096
    })
}

fn request_agent(timeout: Duration) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build();
    ureq::Agent::new_with_config(config)
}

fn http_failure(status: u16) -> ExchangeFailure {
    match status {
        401 | 403 => ExchangeFailure::Authentication,
        400..=499 => ExchangeFailure::RequestRejected,
        _ => ExchangeFailure::remote_failed_other(),
    }
}

fn terminal_response_failure(response: &Value) -> Result<ExchangeFailure, ExchangeFailure> {
    match response.get("status").and_then(Value::as_str) {
        Some("failed") => failed_response(response),
        Some("cancelled") => Ok(ExchangeFailure::RemoteCancelled {
            terminal: terminal_metadata(response)?,
        }),
        Some("incomplete") => incomplete_response(response),
        _ => Err(ExchangeFailure::Protocol),
    }
}

fn failed_response(response: &Value) -> Result<ExchangeFailure, ExchangeFailure> {
    let code = match response.pointer("/error/code").and_then(Value::as_str) {
        Some("server_error") => RemoteErrorCode::ServerError,
        Some("rate_limit_exceeded") => RemoteErrorCode::RateLimitExceeded,
        Some("invalid_prompt") => RemoteErrorCode::InvalidPrompt,
        Some("vector_store_timeout") => RemoteErrorCode::VectorStoreTimeout,
        Some(_) | None => RemoteErrorCode::Other,
    };
    Ok(ExchangeFailure::RemoteFailed {
        code,
        terminal: terminal_metadata(response)?,
    })
}

fn incomplete_response(response: &Value) -> Result<ExchangeFailure, ExchangeFailure> {
    let reason = match response
        .pointer("/incomplete_details/reason")
        .and_then(Value::as_str)
    {
        Some("max_output_tokens") => IncompleteReason::MaxOutputTokens,
        Some("content_filter") => IncompleteReason::ContentFilter,
        Some(_) | None => IncompleteReason::Other,
    };
    Ok(ExchangeFailure::Incomplete {
        reason,
        terminal: terminal_metadata(response)?,
    })
}

fn terminal_metadata(response: &Value) -> Result<ExchangeTerminalMetadata, ExchangeFailure> {
    let usage = response
        .pointer("/usage/total_tokens")
        .and_then(Value::as_u64);
    if usage.is_some_and(|usage| usage > i64::MAX as u64) {
        return Err(ExchangeFailure::Protocol);
    }
    let response_id_sha256 = response
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(|id| {
            let mut output = String::with_capacity(64);
            for byte in Sha256::digest(id.as_bytes()) {
                use std::fmt::Write as _;
                write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
            }
            output
        });
    Ok(ExchangeTerminalMetadata {
        usage,
        response_id_sha256,
    })
}

fn read_envelope(
    response: &mut ureq::http::Response<ureq::Body>,
    limit: usize,
) -> Result<Value, ExchangeFailure> {
    let body = response
        .body_mut()
        .with_config()
        .limit(limit as u64)
        .read_to_vec()
        .map_err(|error| match error {
            ureq::Error::BodyExceedsLimit(_) => ExchangeFailure::ResponseLimit,
            _ => ExchangeFailure::Ambiguous,
        })?;
    serde_json::from_slice(&body).map_err(|_| ExchangeFailure::Protocol)
}

fn normalize_response(
    response: Value,
    max_response_bytes: usize,
) -> Result<ExchangeResponse, ExchangeFailure> {
    let id = response
        .get("id")
        .and_then(Value::as_str)
        .ok_or(ExchangeFailure::Protocol)?;
    let usage = response
        .pointer("/usage/total_tokens")
        .and_then(Value::as_u64)
        .ok_or(ExchangeFailure::Protocol)?;
    let mut output = String::new();
    for item in response
        .get("output")
        .and_then(Value::as_array)
        .ok_or(ExchangeFailure::Protocol)?
    {
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in content {
            if part.get("type").and_then(Value::as_str) == Some("output_text") {
                let text = part
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or(ExchangeFailure::Protocol)?;
                output.push_str(text);
                if output.len() > max_response_bytes {
                    return Err(ExchangeFailure::ResponseLimit);
                }
            }
        }
    }
    if output.is_empty() {
        return Err(ExchangeFailure::EmptyResponse);
    }
    Ok(ExchangeResponse {
        bytes: output.into_bytes(),
        provenance: format!("openai:{id}"),
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_bounded_stateless_background_execution() {
        let body = request_body("gpt-test", "prompt");
        assert_eq!(body["model"], "gpt-test");
        assert_eq!(body["input"], "prompt");
        assert_eq!(body["background"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["max_output_tokens"], 4096);
    }

    #[test]
    fn normalizes_completed_background_response() {
        let response = normalize_response(
            json!({
                "id": "resp_123",
                "status": "completed",
                "usage": {"total_tokens": 12},
                "output": [
                    {"type": "reasoning", "content": []},
                    {"type": "message", "content": [
                        {"type": "output_text", "text": "Quartz "},
                        {"type": "output_text", "text": "works."}
                    ]}
                ]
            }),
            64,
        )
        .unwrap();
        assert_eq!(response.bytes, b"Quartz works.");
        assert_eq!(response.provenance, "openai:resp_123");
        assert_eq!(response.usage, 12);
    }

    #[test]
    fn rejects_response_text_over_bound() {
        let result = normalize_response(
            json!({
                "id": "resp_123",
                "status": "completed",
                "usage": {"total_tokens": 12},
                "output": [{"content": [
                    {"type": "output_text", "text": "too long"}
                ]}]
            }),
            4,
        );
        assert!(matches!(result, Err(ExchangeFailure::ResponseLimit)));
    }

    #[test]
    fn classifies_non_secret_terminal_failures() {
        assert_eq!(http_failure(401), ExchangeFailure::Authentication);
        assert_eq!(http_failure(403), ExchangeFailure::Authentication);
        assert_eq!(http_failure(400), ExchangeFailure::RequestRejected);
        assert_eq!(http_failure(429), ExchangeFailure::RequestRejected);
        assert_eq!(http_failure(500), ExchangeFailure::remote_failed_other());

        let empty = normalize_response(
            json!({
                "id": "resp_123",
                "usage": {"total_tokens": 0},
                "output": []
            }),
            64,
        );
        assert_eq!(empty, Err(ExchangeFailure::EmptyResponse));
        assert_eq!(
            normalize_response(json!({"output": []}), 64),
            Err(ExchangeFailure::Protocol)
        );
    }

    #[test]
    fn retains_only_safe_terminal_response_details() {
        let failed = terminal_response_failure(&json!({
            "id": "resp_sensitive_identifier",
            "status": "failed",
            "error": {
                "code": "server_error",
                "message": "unsafe provider message"
            },
            "usage": {"total_tokens": 321}
        }))
        .unwrap();
        let debug = format!("{failed:?}");
        assert!(!debug.contains("unsafe provider message"));
        assert!(!debug.contains("resp_sensitive_identifier"));
        let ExchangeFailure::RemoteFailed { code, terminal } = failed else {
            panic!("expected failed response")
        };
        assert_eq!(code, RemoteErrorCode::ServerError);
        assert_eq!(terminal.usage, Some(321));
        assert_eq!(terminal.response_id_sha256.as_deref().unwrap().len(), 64);

        assert!(matches!(
            terminal_response_failure(&json!({
                "id": "resp_cancelled",
                "status": "cancelled",
                "usage": {"total_tokens": 12}
            })),
            Ok(ExchangeFailure::RemoteCancelled {
                terminal: ExchangeTerminalMetadata {
                    usage: Some(12),
                    response_id_sha256: Some(_),
                }
            })
        ));

        for (reason, expected) in [
            ("max_output_tokens", IncompleteReason::MaxOutputTokens),
            ("content_filter", IncompleteReason::ContentFilter),
            ("future_reason", IncompleteReason::Other),
        ] {
            assert!(matches!(
                terminal_response_failure(&json!({
                    "id": "resp_incomplete",
                    "status": "incomplete",
                    "incomplete_details": {"reason": reason},
                    "usage": {"total_tokens": 456}
                })),
                Ok(ExchangeFailure::Incomplete {
                    reason: actual,
                    terminal: ExchangeTerminalMetadata {
                        usage: Some(456),
                        response_id_sha256: Some(_),
                    },
                }) if actual == expected
            ));
        }

        assert!(matches!(
            terminal_response_failure(&json!({
                "status": "failed",
                "error": {"code": "future_error", "message": "not retained"}
            })),
            Ok(ExchangeFailure::RemoteFailed {
                code: RemoteErrorCode::Other,
                terminal: ExchangeTerminalMetadata {
                    usage: None,
                    response_id_sha256: None,
                }
            })
        ));
    }
}
