//! Talking to the brain.
//!
//! Generation stays in Python, where the model and the training code live. The core reaches it
//! over loopback and streams the answer back token by token. This is deliberately the only
//! place in the core that does network input and output on behalf of a turn, and it runs in a
//! connection task, never in the hub, so a slow or wedged model cannot stall anything else.

use futures::StreamExt;
use groow_proto::turn::{Message, ToolSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What the core asks the brain for.
#[derive(Debug, Clone, Serialize)]
pub struct GenRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub max_new_tokens: usize,
    pub temperature: f64,
    pub top_p: f64,
    pub top_k: usize,
    pub enable_thinking: bool,
    /// Lower goes first. The conscious turn preempts inner thoughts and background play.
    pub priority: u8,
}

/// One piece of an answer as it arrives.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Chunk {
    Delta { delta: String },
    Done { done: bool, text: String, #[serde(default)] tokens: usize, #[serde(default)] seconds: f64 },
    Error { error: String },
}

#[derive(Debug, Clone, Default)]
pub struct Generated {
    pub text: String,
    pub tokens: usize,
    pub seconds: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum BrainError {
    #[error("the brain is not answering at {0}: {1}")]
    Unreachable(String, String),
    #[error("the brain refused: {0}")]
    Refused(String),
    #[error("the brain sent something unreadable: {0}")]
    Garbled(String),
}

#[derive(Clone)]
pub struct Brain {
    url: String,
    client: reqwest::Client,
}

impl Brain {
    pub fn new(url: impl Into<String>) -> Brain {
        Brain {
            url: url.into(),
            // No overall timeout: a long generation is normal. The connect timeout is short,
            // so a brain that is simply not there is reported at once rather than hanging.
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(3))
                .build()
                .unwrap_or_default(),
        }
    }

    pub async fn healthy(&self) -> bool {
        self.health().await.is_some()
    }

    /// What the brain says about itself, including which creature its weights are.
    pub async fn health(&self) -> Option<Value> {
        let r = self
            .client
            .get(format!("{}/health", self.url))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .ok()?;
        if !r.status().is_success() {
            return None;
        }
        r.json::<Value>().await.ok()
    }

    /// Generate, calling `on_delta` for each piece as it arrives.
    ///
    /// The deltas are advisory: they exist so a person can watch the answer appear. The value
    /// that matters is the final text, which the brain sends complete at the end, so a dropped
    /// delta cannot corrupt what gets journalled.
    pub async fn complete(
        &self,
        req: &GenRequest,
        mut on_delta: impl FnMut(&str),
    ) -> Result<Generated, BrainError> {
        let resp = self
            .client
            .post(format!("{}/generate", self.url))
            .json(req)
            .send()
            .await
            .map_err(|e| BrainError::Unreachable(self.url.clone(), e.to_string()))?;

        if !resp.status().is_success() {
            let code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(BrainError::Refused(format!("{code}: {}", body.chars().take(400).collect::<String>())));
        }

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut out = Generated::default();
        let mut finished = false;

        while let Some(part) = stream.next().await {
            let bytes = part.map_err(|e| BrainError::Unreachable(self.url.clone(), e.to_string()))?;
            buf.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(i) = buf.find('\n') {
                let line: String = buf.drain(..=i).collect();
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Chunk>(line) {
                    Ok(Chunk::Delta { delta }) => {
                        out.text.push_str(&delta);
                        on_delta(&delta);
                    }
                    Ok(Chunk::Done { text, tokens, seconds, .. }) => {
                        // The complete text is authoritative; the accumulated deltas are not.
                        out.text = text;
                        out.tokens = tokens;
                        out.seconds = seconds;
                        finished = true;
                    }
                    Ok(Chunk::Error { error }) => return Err(BrainError::Refused(error)),
                    Err(_) => return Err(BrainError::Garbled(line.chars().take(200).collect())),
                }
            }
        }
        if !finished && out.text.is_empty() {
            return Err(BrainError::Garbled("the stream ended without an answer".into()));
        }
        Ok(out)
    }
}

/// Everything the brain needs, assembled from the configuration.
pub fn request_from(
    cfg: &crate::config::Config,
    messages: Vec<Message>,
    tools: Vec<ToolSchema>,
    priority: u8,
) -> GenRequest {
    GenRequest {
        messages,
        tools,
        max_new_tokens: cfg.max_new_tokens,
        temperature: cfg.temperature,
        top_p: cfg.top_p,
        top_k: cfg.top_k,
        enable_thinking: cfg.enable_thinking,
        priority,
    }
}

/// Parse a generation argument blob that may itself be a JSON string.
pub fn loose_object(v: &Value) -> Value {
    match v {
        Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Object(Default::default())),
        Value::Object(_) => v.clone(),
        _ => Value::Object(Default::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_are_told_apart() {
        let d: Chunk = serde_json::from_str(r#"{"delta":"hel"}"#).unwrap();
        assert!(matches!(d, Chunk::Delta { .. }));
        let done: Chunk = serde_json::from_str(r#"{"done":true,"text":"hello","tokens":2,"seconds":0.5}"#).unwrap();
        match done {
            Chunk::Done { text, tokens, .. } => {
                assert_eq!(text, "hello");
                assert_eq!(tokens, 2);
            }
            other => panic!("wrong chunk: {other:?}"),
        }
        let err: Chunk = serde_json::from_str(r#"{"error":"out of memory"}"#).unwrap();
        assert!(matches!(err, Chunk::Error { .. }));
    }

    #[test]
    fn a_request_carries_the_sampling_settings() {
        let cfg = crate::config::Config::default();
        let r = request_from(&cfg, vec![Message::user("hi")], vec![], 0);
        assert_eq!(r.temperature, cfg.temperature);
        assert_eq!(r.max_new_tokens, cfg.max_new_tokens);
        assert_eq!(r.priority, 0);
    }

    #[test]
    fn a_missing_brain_is_reported_clearly_not_as_a_hang() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        // Port zero is never listening.
        let b = Brain::new("http://127.0.0.1:1");
        rt.block_on(async {
            assert!(!b.healthy().await);
            let req = request_from(&crate::config::Config::default(), vec![Message::user("hi")], vec![], 0);
            let e = b.complete(&req, |_| {}).await.unwrap_err();
            assert!(matches!(e, BrainError::Unreachable(_, _)), "got {e}");
        });
    }

    #[test]
    fn a_json_string_of_arguments_is_unwrapped() {
        assert_eq!(loose_object(&serde_json::json!("{\"a\":1}"))["a"], 1);
        assert_eq!(loose_object(&serde_json::json!({"a": 2}))["a"], 2);
        assert!(loose_object(&serde_json::json!(7)).as_object().unwrap().is_empty());
        assert!(loose_object(&serde_json::json!("not json")).as_object().unwrap().is_empty());
    }
}
