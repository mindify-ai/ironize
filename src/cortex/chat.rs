//! OpenAI Chat Completions over the Cortex gateway, with SSE streaming.

use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};

/// One conversation message in OpenAI wire format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Message::System {
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Message::User {
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "function_type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON-encoded arguments, exactly as the model produced them.
    pub arguments: String,
}

fn function_type() -> String {
    "function".into()
}

/// A tool the model may call.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: FunctionDefinition,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolDefinition {
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> Self {
        Self {
            kind: "function",
            function: FunctionDefinition {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct Usage {
    #[serde(default, rename = "prompt_tokens")]
    pub input_tokens: u64,
    #[serde(default, rename = "completion_tokens")]
    pub output_tokens: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: &'a [Message],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub tools: &'a [ToolDefinition],
    pub stream: bool,
    pub stream_options: StreamOptions,
}

#[derive(Debug, Serialize)]
pub(crate) struct StreamOptions {
    pub include_usage: bool,
}

/// An incremental piece of the assistant's reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    Text(String),
    /// The first delta of a tool call: its id and name.
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },
    /// More JSON for a tool call's arguments.
    ToolCallArgs {
        index: usize,
        delta: String,
    },
    Usage(Usage),
    Finish(String),
}

/// Incremental SSE decoder for `chat.completion.chunk` frames. Bytes may arrive
/// split anywhere, including inside a UTF-8 sequence or a line.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
    done: bool,
}

impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<StreamEvent>> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(newline) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line);
            self.line(line.trim_end_matches(['\r', '\n']), &mut events)?;
        }
        Ok(events)
    }

    /// Flushes a final line that arrived without a trailing newline.
    pub fn finish(&mut self) -> Result<Vec<StreamEvent>> {
        let rest = std::mem::take(&mut self.buffer);
        let mut events = Vec::new();
        if !rest.is_empty() {
            self.line(String::from_utf8_lossy(&rest).trim_end(), &mut events)?;
        }
        Ok(events)
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    fn line(&mut self, line: &str, events: &mut Vec<StreamEvent>) -> Result<()> {
        let Some(data) = line.strip_prefix("data:") else {
            return Ok(()); // `event:`, `id:`, comments and blank separators
        };
        let data = data.trim();
        if data == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        if data.is_empty() {
            return Ok(());
        }
        let chunk: Value = serde_json::from_str(data).map_err(|_| Error::UnexpectedResponse {
            operation: "read the model stream".into(),
        })?;
        chunk_events(&chunk, events)
    }
}

fn chunk_events(chunk: &Value, events: &mut Vec<StreamEvent>) -> Result<()> {
    if let Some(error) = chunk.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| error.as_str())
            .unwrap_or("the model stream failed");
        return Err(Error::Api {
            status: 200,
            message: message.to_string(),
        });
    }
    for choice in chunk
        .get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        // Streaming chunks carry `delta`; a non-streaming completion carries `message`.
        let delta = choice.get("delta").or_else(|| choice.get("message"));
        if let Some(delta) = delta {
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    events.push(StreamEvent::Text(text.to_string()));
                }
            }
            let calls = delta.get("tool_calls").and_then(Value::as_array);
            for (position, call) in calls.into_iter().flatten().enumerate() {
                let index = call
                    .get("index")
                    .and_then(Value::as_u64)
                    .map_or(position, |i| i as usize);
                let function = call.get("function");
                let name = function.and_then(|f| f.get("name")).and_then(Value::as_str);
                let id = call.get("id").and_then(Value::as_str);
                if let (Some(id), Some(name)) = (id, name) {
                    events.push(StreamEvent::ToolCallStart {
                        index,
                        id: id.to_string(),
                        name: name.to_string(),
                    });
                }
                if let Some(args) = function
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                {
                    if !args.is_empty() {
                        events.push(StreamEvent::ToolCallArgs {
                            index,
                            delta: args.to_string(),
                        });
                    }
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            events.push(StreamEvent::Finish(reason.to_string()));
        }
    }
    if let Some(usage) = chunk.get("usage").filter(|u| !u.is_null()) {
        if let Ok(usage) = serde_json::from_value::<Usage>(usage.clone()) {
            events.push(StreamEvent::Usage(usage));
        }
    }
    Ok(())
}

/// A streaming model reply.
pub struct ChatStream {
    inner: std::pin::Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    decoder: SseDecoder,
    pending: std::collections::VecDeque<StreamEvent>,
    ended: bool,
    /// The server answered with a plain JSON completion instead of SSE.
    json_body: Option<Vec<u8>>,
}

impl ChatStream {
    pub(crate) fn new(response: reqwest::Response) -> Self {
        let is_json = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("application/json"));
        Self {
            inner: Box::pin(response.bytes_stream()),
            decoder: SseDecoder::default(),
            pending: Default::default(),
            ended: false,
            json_body: is_json.then(Vec::new),
        }
    }

    /// The next event, or `None` once the reply is complete.
    pub async fn next(&mut self) -> Option<Result<StreamEvent>> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(Ok(event));
            }
            if self.ended {
                return None;
            }
            match self.inner.next().await {
                Some(Ok(bytes)) => {
                    let decoded = match &mut self.json_body {
                        Some(body) => {
                            body.extend_from_slice(&bytes);
                            Ok(Vec::new())
                        }
                        None => self.decoder.push(&bytes),
                    };
                    match decoded {
                        Ok(events) => self.pending.extend(events),
                        Err(e) => return Some(Err(self.fail(e))),
                    }
                }
                Some(Err(e)) => return Some(Err(self.fail(e.into()))),
                None => {
                    self.ended = true;
                    let tail = match self.json_body.take() {
                        Some(body) => serde_json::from_slice::<Value>(&body)
                            .map_err(|_| Error::UnexpectedResponse {
                                operation: "read the model reply".into(),
                            })
                            .and_then(|v| {
                                let mut events = Vec::new();
                                chunk_events(&v, &mut events).map(|()| events)
                            }),
                        None => self.decoder.finish(),
                    };
                    match tail {
                        Ok(events) => self.pending.extend(events),
                        Err(e) => return Some(Err(e)),
                    }
                }
            }
        }
    }

    fn fail(&mut self, error: Error) -> Error {
        self.ended = true;
        self.pending.clear();
        error
    }
}

/// Assembles stream events into a complete assistant message.
#[derive(Debug, Default)]
pub struct ReplyBuilder {
    pub text: String,
    calls: Vec<Option<ToolCall>>,
    pub usage: Option<Usage>,
    pub finish_reason: Option<String>,
}

impl ReplyBuilder {
    pub fn apply(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::Text(text) => self.text.push_str(text),
            StreamEvent::ToolCallStart { index, id, name } => {
                let slot = self.slot(*index);
                match slot {
                    Some(call) => {
                        // Some providers repeat the id/name; keep the first.
                        if call.id.is_empty() {
                            call.id = id.clone();
                        }
                    }
                    None => {
                        *slot = Some(ToolCall {
                            id: id.clone(),
                            kind: function_type(),
                            function: FunctionCall {
                                name: name.clone(),
                                arguments: String::new(),
                            },
                        })
                    }
                }
            }
            StreamEvent::ToolCallArgs { index, delta } => {
                if let Some(call) = self.slot(*index) {
                    call.function.arguments.push_str(delta);
                }
            }
            StreamEvent::Usage(usage) => self.usage = Some(*usage),
            StreamEvent::Finish(reason) => self.finish_reason = Some(reason.clone()),
        }
    }

    fn slot(&mut self, index: usize) -> &mut Option<ToolCall> {
        if self.calls.len() <= index {
            self.calls.resize(index + 1, None);
        }
        &mut self.calls[index]
    }

    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.calls.iter().flatten().cloned().collect()
    }

    pub fn into_message(self) -> Message {
        let tool_calls = self.tool_calls();
        Message::Assistant {
            content: (!self.text.is_empty()).then_some(self.text),
            tool_calls,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(input: &str, split: usize) -> Vec<StreamEvent> {
        let mut decoder = SseDecoder::default();
        let mut events = Vec::new();
        for chunk in input.as_bytes().chunks(split) {
            events.extend(decoder.push(chunk).unwrap());
        }
        events.extend(decoder.finish().unwrap());
        events
    }

    const STREAM: &str = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Héllo \"},\"finish_reason\":null}]}\n\n",
        ": keep-alive\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\":\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"a.rs\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":7}}\n\n",
        "data: [DONE]\n\n",
    );

    #[test]
    fn decodes_regardless_of_chunk_boundaries() {
        let whole = decode_all(STREAM, STREAM.len());
        for split in [1, 2, 3, 7, 64] {
            assert_eq!(decode_all(STREAM, split), whole, "split {split}");
        }
        let mut reply = ReplyBuilder::default();
        whole.iter().for_each(|e| reply.apply(e));
        assert_eq!(reply.text, "Héllo ");
        assert_eq!(
            reply.usage,
            Some(Usage {
                input_tokens: 12,
                output_tokens: 7
            })
        );
        assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
        let calls = reply.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[0].function.arguments, r#"{"path":"a.rs"}"#);
    }

    #[test]
    fn stream_errors_surface() {
        let mut decoder = SseDecoder::default();
        let err = decoder
            .push(
                b"data: {\"error\":{\"message\":\"rate limited\",\"type\":\"rate_limit_error\"}}\n",
            )
            .unwrap_err();
        assert!(err.to_string().contains("rate limited"));
    }

    #[test]
    fn assistant_message_serializes_like_openai() {
        let message = Message::Assistant {
            content: None,
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "ls".into(),
                    arguments: "{}".into(),
                },
            }],
        };
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            serde_json::json!({
                "role": "assistant",
                "tool_calls": [{ "id": "c1", "type": "function", "function": { "name": "ls", "arguments": "{}" } }]
            })
        );
        let tool = Message::Tool {
            tool_call_id: "c1".into(),
            content: "ok".into(),
        };
        assert_eq!(
            serde_json::to_value(&tool).unwrap(),
            serde_json::json!({ "role": "tool", "tool_call_id": "c1", "content": "ok" })
        );
    }
}
