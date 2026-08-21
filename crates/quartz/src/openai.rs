use std::{
    thread,
    time::{Duration, Instant},
};

use quartz_kernel::{ExchangeAdapter, ExchangeFailure, ExchangeResponse};
use serde_json::{Value, json};

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
        let prompt = std::str::from_utf8(request).map_err(|_| ExchangeFailure::Rejected)?;
        let envelope_limit = max_response_bytes
            .saturating_mul(8)
            .clamp(1024 * 1024, 8 * 1024 * 1024);
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(ExchangeFailure::Rejected)?;
        let mut response = request_agent(timeout)
            .post("https://api.openai.com/v1/responses")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send_json(request_body(&self.model, prompt))
            .map_err(|_| ExchangeFailure::Ambiguous)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return if (400..500).contains(&status) {
                Err(ExchangeFailure::Rejected)
            } else {
                Err(ExchangeFailure::Ambiguous)
            };
        }
        let mut envelope = read_envelope(&mut response, envelope_limit)?;
        loop {
            match envelope.get("status").and_then(Value::as_str) {
                Some("completed") => {
                    return normalize_response(envelope, max_response_bytes);
                }
                Some("queued" | "in_progress") => {}
                Some(_) => return Err(ExchangeFailure::Rejected),
                None => return Err(ExchangeFailure::Ambiguous),
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
                .ok_or(ExchangeFailure::Ambiguous)?;
            let mut response = request_agent(remaining)
                .get(format!("https://api.openai.com/v1/responses/{id}"))
                .header("Authorization", format!("Bearer {}", self.api_key))
                .call()
                .map_err(|_| ExchangeFailure::Ambiguous)?;
            if !(200..300).contains(&response.status().as_u16()) {
                return Err(ExchangeFailure::Ambiguous);
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
        "max_output_tokens": 1024
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

fn read_envelope(
    response: &mut ureq::http::Response<ureq::Body>,
    limit: usize,
) -> Result<Value, ExchangeFailure> {
    let body = response
        .body_mut()
        .with_config()
        .limit(limit as u64)
        .read_to_vec()
        .map_err(|_| ExchangeFailure::Ambiguous)?;
    serde_json::from_slice(&body).map_err(|_| ExchangeFailure::Ambiguous)
}

fn normalize_response(
    response: Value,
    max_response_bytes: usize,
) -> Result<ExchangeResponse, ExchangeFailure> {
    let id = response
        .get("id")
        .and_then(Value::as_str)
        .ok_or(ExchangeFailure::Ambiguous)?;
    let usage = response
        .pointer("/usage/total_tokens")
        .and_then(Value::as_u64)
        .ok_or(ExchangeFailure::Ambiguous)?;
    let mut output = String::new();
    for item in response
        .get("output")
        .and_then(Value::as_array)
        .ok_or(ExchangeFailure::Ambiguous)?
    {
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in content {
            if part.get("type").and_then(Value::as_str) == Some("output_text") {
                let text = part
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or(ExchangeFailure::Ambiguous)?;
                output.push_str(text);
                if output.len() > max_response_bytes {
                    return Err(ExchangeFailure::Rejected);
                }
            }
        }
    }
    if output.is_empty() {
        return Err(ExchangeFailure::Rejected);
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
        assert_eq!(body["max_output_tokens"], 1024);
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
        assert!(matches!(result, Err(ExchangeFailure::Rejected)));
    }
}
