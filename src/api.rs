//! Model API clients: one streaming request per worker thread. Conversations
//! are kept in Anthropic's block format; the OpenAI-style client translates.

use crate::provider::{Kind, Provider};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::sync::mpsc::Sender;
use std::time::Duration;

const MAX_TOKENS: u32 = 8192;
const MAX_ATTEMPTS: u32 = 3;

pub struct Request {
    pub provider: Provider,
    pub system: String,
    pub tools: Value,
    pub messages: Vec<Value>,
}

#[derive(Debug)]
pub struct Reply {
    pub content: Vec<Value>,
    pub stop_reason: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

pub enum StreamEvent {
    Delta(String),
    Done(Result<Reply, String>),
}

/// Runs the request on a new thread. Every event carries `turn_id` so the UI
/// can drop events from a request it no longer cares about.
pub fn spawn(req: Request, turn_id: u64, tx: Sender<(u64, StreamEvent)>) {
    std::thread::spawn(move || {
        let result = run(&req, |delta| {
            let _ = tx.send((turn_id, StreamEvent::Delta(delta)));
        });
        let _ = tx.send((turn_id, StreamEvent::Done(result)));
    });
}

fn run(req: &Request, mut on_delta: impl FnMut(String)) -> Result<Reply, String> {
    let provider = &req.provider;
    let base = provider.base_url.trim_end_matches('/');
    if provider.kind == Kind::Anthropic {
        let headers = [
            ("x-api-key", provider.key.clone()),
            ("anthropic-version", "2023-06-01".to_string()),
        ];
        let response = post(
            provider,
            &format!("{base}/messages"),
            &headers,
            &anthropic_body(req),
        )?;
        let mut acc = Accumulator::default();
        for event in events(response) {
            if let Some(delta) = acc.feed(&event?)? {
                on_delta(delta);
            }
        }
        acc.finish()
    } else {
        let mut headers = Vec::new();
        if !provider.key.is_empty() {
            headers.push(("authorization", format!("Bearer {}", provider.key)));
        }
        let url = format!("{base}/chat/completions");
        let response = post(provider, &url, &headers, &openai_body(req))?;
        let mut acc = OpenAiAccumulator::default();
        for event in events(response) {
            if let Some(delta) = acc.feed(&event?)? {
                on_delta(delta);
            }
        }
        acc.finish()
    }
}

/// Services report errors as `{"error": {"message": ..}}`, `{"error": ".."}`
/// or `{"message": ".."}`.
fn error_message(v: &Value) -> Option<String> {
    [&v["error"]["message"], &v["error"], &v["message"]]
        .into_iter()
        .find_map(|m| m.as_str().map(str::to_string))
}

fn post(
    provider: &Provider,
    url: &str,
    headers: &[(&str, String)],
    body: &Value,
) -> Result<ureq::Response, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        // A local model can take minutes to read a long prompt before it answers.
        .timeout_read(Duration::from_secs(600))
        .build();
    let mut attempt = 1;
    loop {
        let mut request = agent.post(url);
        for (name, value) in headers {
            request = request.set(name, value);
        }
        match request.send_json(body) {
            Ok(response) => return Ok(response),
            Err(ureq::Error::Status(code, response)) => {
                let retryable = code == 429 || code >= 500;
                if retryable && attempt < MAX_ATTEMPTS {
                    std::thread::sleep(Duration::from_secs(2 * attempt as u64));
                    attempt += 1;
                    continue;
                }
                let text = response.into_string().unwrap_or_default();
                let message = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| error_message(&v))
                    .unwrap_or(text);
                return Err(format!("{} error {code}: {message}", provider.kind.label()));
            }
            Err(e) if provider.kind == Kind::Local => {
                return Err(format!("Could not reach the model server at {url}: {e}"));
            }
            Err(e) => return Err(format!("Network error: {e}")),
        }
    }
}

/// The JSON payloads of a server-sent event stream.
fn events(response: ureq::Response) -> impl Iterator<Item = Result<Value, String>> {
    BufReader::new(response.into_reader())
        .lines()
        .filter_map(|line| match line {
            Err(e) => Some(Err(format!("Stream read failed: {e}"))),
            Ok(line) => serde_json::from_str(line.strip_prefix("data:")?.trim())
                .ok()
                .map(Ok),
        })
}

// ---- Anthropic ----

fn anthropic_body(req: &Request) -> Value {
    let mut messages = req.messages.clone();
    // Cache breakpoint on the newest message, so each turn reuses the prefix
    // (tools, system prompt and earlier turns) that the previous turn wrote.
    if let Some(block) = messages
        .last_mut()
        .and_then(|m| m["content"].as_array_mut())
        .and_then(|blocks| blocks.last_mut())
    {
        block["cache_control"] = json!({"type": "ephemeral"});
    }
    json!({
        "model": req.provider.model,
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "system": [{"type": "text", "text": req.system}],
        "tools": req.tools,
        "messages": messages,
    })
}

/// Rebuilds the assistant message from server-sent events. Blocks it does not
/// know (thinking, for example) pass through untouched, because the API wants
/// them back verbatim on the next request of a tool loop.
#[derive(Default)]
pub struct Accumulator {
    blocks: Vec<Value>,
    partial_json: Vec<String>,
    stop_reason: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    stopped: bool,
}

impl Accumulator {
    /// Returns the new visible text, if the event carried any.
    pub fn feed(&mut self, event: &Value) -> Result<Option<String>, String> {
        let index = event["index"].as_u64().unwrap_or(0) as usize;
        match event["type"].as_str().unwrap_or("") {
            "message_start" => {
                let usage = &event["message"]["usage"];
                self.input_tokens = [
                    "input_tokens",
                    "cache_creation_input_tokens",
                    "cache_read_input_tokens",
                ]
                .iter()
                .filter_map(|k| usage[*k].as_u64())
                .sum();
            }
            "content_block_start" => {
                self.blocks.push(event["content_block"].clone());
                self.partial_json.push(String::new());
            }
            "content_block_delta" => {
                let (Some(block), Some(buf)) =
                    (self.blocks.get_mut(index), self.partial_json.get_mut(index))
                else {
                    return Ok(None);
                };
                let delta = &event["delta"];
                match delta["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        let text = delta["text"].as_str().unwrap_or("");
                        append(block, "text", text);
                        return Ok(Some(text.to_string()));
                    }
                    "input_json_delta" => {
                        buf.push_str(delta["partial_json"].as_str().unwrap_or(""))
                    }
                    "thinking_delta" => {
                        append(block, "thinking", delta["thinking"].as_str().unwrap_or(""))
                    }
                    "signature_delta" => append(
                        block,
                        "signature",
                        delta["signature"].as_str().unwrap_or(""),
                    ),
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let (Some(block), Some(buf)) =
                    (self.blocks.get_mut(index), self.partial_json.get(index))
                    && block["type"] == "tool_use"
                {
                    block["input"] = serde_json::from_str(buf).unwrap_or_else(|_| json!({}));
                }
            }
            "message_delta" => {
                if let Some(reason) = event["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(reason.to_string());
                }
                if let Some(n) = event["usage"]["output_tokens"].as_u64() {
                    self.output_tokens = n;
                }
            }
            "message_stop" => self.stopped = true,
            "error" => {
                let message = event["error"]["message"]
                    .as_str()
                    .unwrap_or("unknown error");
                return Err(format!("API error: {message}"));
            }
            _ => {}
        }
        Ok(None)
    }

    pub fn finish(self) -> Result<Reply, String> {
        if !self.stopped {
            return Err("The connection closed before the reply finished.".to_string());
        }
        let stop_reason = self.stop_reason.unwrap_or_else(|| "end_turn".to_string());
        let mut content: Vec<Value> = self
            .blocks
            .into_iter()
            // The API rejects empty text blocks when they are sent back.
            .filter(|b| !(b["type"] == "text" && b["text"].as_str().unwrap_or("").is_empty()))
            // Tool calls and thinking only need to go back to the API inside a
            // tool loop. Outside one they may be cut off, so keep the text only.
            .filter(|b| stop_reason == "tool_use" || b["type"] == "text")
            .collect();
        if !content
            .iter()
            .any(|b| b["type"] == "text" || b["type"] == "tool_use")
        {
            content.push(json!({"type": "text", "text": "(no reply)"}));
        }
        Ok(Reply {
            content,
            stop_reason,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
        })
    }
}

fn append(block: &mut Value, key: &str, extra: &str) {
    let current = block[key].as_str().unwrap_or("");
    block[key] = Value::String(format!("{current}{extra}"));
}

// ---- OpenAI-style chat completions (OpenAI, Hugging Face, llama.cpp) ----

fn joined_text(blocks: &[Value]) -> String {
    blocks
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn openai_messages(system: &str, messages: &[Value]) -> Vec<Value> {
    let mut out = vec![json!({"role": "system", "content": system})];
    for message in messages {
        let blocks = message["content"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let text = joined_text(blocks);
        if message["role"] == "assistant" {
            let calls: Vec<Value> = blocks
                .iter()
                .filter(|b| b["type"] == "tool_use")
                .map(|b| {
                    json!({
                        "id": b["id"],
                        "type": "function",
                        "function": {"name": b["name"], "arguments": b["input"].to_string()},
                    })
                })
                .collect();
            let mut msg = json!({"role": "assistant", "content": text});
            if !calls.is_empty() {
                msg["tool_calls"] = Value::Array(calls);
            }
            out.push(msg);
        } else {
            // Tool results are separate messages here, and must come first.
            for b in blocks.iter().filter(|b| b["type"] == "tool_result") {
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": b["tool_use_id"],
                    "content": b["content"],
                }));
            }
            if !text.is_empty() {
                out.push(json!({"role": "user", "content": text}));
            }
        }
    }
    out
}

fn openai_body(req: &Request) -> Value {
    let tools: Vec<Value> = req
        .tools
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t["name"],
                    "description": t["description"],
                    "parameters": t["input_schema"],
                },
            })
        })
        .collect();
    json!({
        "model": req.provider.model,
        "stream": true,
        "stream_options": {"include_usage": true},
        "tools": tools,
        "messages": openai_messages(&req.system, &req.messages),
    })
}

#[derive(Default)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String,
}

/// Rebuilds a chat completions stream into the same reply shape the
/// Anthropic client produces.
#[derive(Default)]
pub struct OpenAiAccumulator {
    text: String,
    calls: Vec<ToolCall>,
    finish_reason: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
}

impl OpenAiAccumulator {
    pub fn feed(&mut self, chunk: &Value) -> Result<Option<String>, String> {
        if let Some(message) = error_message(chunk) {
            return Err(format!("API error: {message}"));
        }
        if let Some(n) = chunk["usage"]["prompt_tokens"].as_u64() {
            self.input_tokens = n;
        }
        if let Some(n) = chunk["usage"]["completion_tokens"].as_u64() {
            self.output_tokens = n;
        }
        let choice = &chunk["choices"][0];
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.finish_reason = Some(reason.to_string());
        }
        let delta = &choice["delta"];
        for call in delta["tool_calls"].as_array().into_iter().flatten() {
            let index = call["index"].as_u64().unwrap_or(0) as usize;
            if self.calls.len() <= index {
                self.calls.resize_with(index + 1, ToolCall::default);
            }
            let slot = &mut self.calls[index];
            for (target, part) in [
                (&mut slot.id, &call["id"]),
                (&mut slot.name, &call["function"]["name"]),
                (&mut slot.arguments, &call["function"]["arguments"]),
            ] {
                target.push_str(part.as_str().unwrap_or(""));
            }
        }
        match delta["content"].as_str() {
            Some(text) if !text.is_empty() => {
                self.text.push_str(text);
                Ok(Some(text.to_string()))
            }
            _ => Ok(None),
        }
    }

    pub fn finish(self) -> Result<Reply, String> {
        let Some(finish_reason) = self.finish_reason else {
            return Err("The connection closed before the reply finished.".to_string());
        };
        let mut content = Vec::new();
        if !self.text.trim().is_empty() {
            content.push(json!({"type": "text", "text": self.text}));
        }
        // A reply cut off by the length limit may hold half a tool call.
        if finish_reason != "length" {
            for (i, call) in self.calls.into_iter().enumerate() {
                if call.name.is_empty() {
                    continue;
                }
                content.push(json!({
                    "type": "tool_use",
                    // Some local servers leave the id empty.
                    "id": if call.id.is_empty() { format!("call_{i}") } else { call.id },
                    "name": call.name,
                    "input": serde_json::from_str::<Value>(&call.arguments).unwrap_or_else(|_| json!({})),
                }));
            }
        }
        let stop_reason = if content.iter().any(|b| b["type"] == "tool_use") {
            "tool_use"
        } else {
            match finish_reason.as_str() {
                "length" => "max_tokens",
                "content_filter" => "refusal",
                _ => "end_turn",
            }
        };
        if content.is_empty() {
            content.push(json!({"type": "text", "text": "(no reply)"}));
        }
        Ok(Reply {
            content,
            stop_reason: stop_reason.to_string(),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(kind: Kind, messages: Vec<Value>) -> Request {
        Request {
            provider: Provider::new(kind, "key"),
            system: "be strict".into(),
            tools: json!([{"name": "save_notes", "description": "d", "input_schema": {"type": "object"}}]),
            messages,
        }
    }

    fn replay(events: &[Value]) -> (Accumulator, String) {
        let mut acc = Accumulator::default();
        let mut text = String::new();
        for e in events {
            if let Some(d) = acc.feed(e).unwrap() {
                text.push_str(&d);
            }
        }
        (acc, text)
    }

    #[test]
    fn rebuilds_text_thinking_and_tool_calls() {
        let (acc, text) = replay(&[
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 7, "cache_read_input_tokens": 100}}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "hm"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Why "}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "so?"}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "t1", "name": "save_notes", "input": {}}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"notes\":"}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "\"x\"}"}}),
            json!({"type": "content_block_stop", "index": 2}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 42}}),
            json!({"type": "message_stop"}),
        ]);
        assert_eq!(text, "Why so?");
        let reply = acc.finish().unwrap();
        assert_eq!(reply.stop_reason, "tool_use");
        assert_eq!((reply.input_tokens, reply.output_tokens), (107, 42));
        assert_eq!(
            reply.content,
            vec![
                json!({"type": "thinking", "thinking": "hm", "signature": "sig"}),
                json!({"type": "text", "text": "Why so?"}),
                json!({"type": "tool_use", "id": "t1", "name": "save_notes", "input": {"notes": "x"}}),
            ]
        );
    }

    #[test]
    fn drops_tool_calls_cut_off_by_the_token_limit() {
        let (acc, _) = replay(&[
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "t1", "name": "x", "input": {}}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"no"}}),
            json!({"type": "message_delta", "delta": {"stop_reason": "max_tokens"}, "usage": {"output_tokens": 1}}),
            json!({"type": "message_stop"}),
        ]);
        let reply = acc.finish().unwrap();
        assert_eq!(
            reply.content,
            vec![json!({"type": "text", "text": "(no reply)"})]
        );
    }

    #[test]
    fn reports_stream_errors_and_early_close() {
        let mut acc = Accumulator::default();
        let err = acc
            .feed(&json!({"type": "error", "error": {"message": "Overloaded"}}))
            .unwrap_err();
        assert!(err.contains("Overloaded"));
        assert!(Accumulator::default().finish().is_err());
    }

    #[test]
    fn puts_the_cache_breakpoint_on_the_last_message() {
        let req = request(
            Kind::Anthropic,
            vec![
                json!({"role": "user", "content": [{"type": "text", "text": "a"}]}),
                json!({"role": "assistant", "content": [{"type": "text", "text": "b"}]}),
                json!({"role": "user", "content": [{"type": "text", "text": "c"}]}),
            ],
        );
        let body = anthropic_body(&req);
        assert!(
            body["messages"][0]["content"][0]
                .get("cache_control")
                .is_none()
        );
        assert_eq!(
            body["messages"][2]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn translates_a_tool_loop_to_chat_completions() {
        let req = request(
            Kind::OpenAi,
            vec![
                json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
                json!({"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "hm", "signature": "s"},
                    {"type": "text", "text": "ok"},
                    {"type": "tool_use", "id": "t1", "name": "save_notes", "input": {"notes": "x"}}
                ]}),
                json!({"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "Notes saved.", "is_error": false},
                    {"type": "text", "text": "next"}
                ]}),
            ],
        );
        let body = openai_body(&req);
        assert_eq!(
            body["tools"][0]["function"]["parameters"],
            json!({"type": "object"})
        );
        assert_eq!(
            body["messages"],
            json!([
                {"role": "system", "content": "be strict"},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "ok", "tool_calls": [
                    {"id": "t1", "type": "function", "function": {"name": "save_notes", "arguments": "{\"notes\":\"x\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "t1", "content": "Notes saved."},
                {"role": "user", "content": "next"},
            ])
        );
    }

    fn replay_openai(chunks: &[Value]) -> (Reply, String) {
        let mut acc = OpenAiAccumulator::default();
        let mut text = String::new();
        for c in chunks {
            if let Some(d) = acc.feed(c).unwrap() {
                text.push_str(&d);
            }
        }
        (acc.finish().unwrap(), text)
    }

    #[test]
    fn rebuilds_a_chat_completions_stream() {
        let (reply, text) = replay_openai(&[
            json!({"choices": [{"delta": {"role": "assistant", "content": "Why "}}]}),
            json!({"choices": [{"delta": {"content": "so?"}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "c1", "function": {"name": "save_notes", "arguments": "{\"no"}}]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "tes\":\"x\"}"}}]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 1, "function": {"name": "complete_topic", "arguments": "{}"}}]}}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
            json!({"choices": [], "usage": {"prompt_tokens": 50, "completion_tokens": 9}}),
        ]);
        assert_eq!(text, "Why so?");
        // Counts as a tool call even though the server said "stop".
        assert_eq!(reply.stop_reason, "tool_use");
        assert_eq!((reply.input_tokens, reply.output_tokens), (50, 9));
        assert_eq!(
            reply.content,
            vec![
                json!({"type": "text", "text": "Why so?"}),
                json!({"type": "tool_use", "id": "c1", "name": "save_notes", "input": {"notes": "x"}}),
                json!({"type": "tool_use", "id": "call_1", "name": "complete_topic", "input": {}}),
            ]
        );
    }

    #[test]
    fn chat_completions_length_limit_and_errors() {
        let (reply, _) = replay_openai(&[
            json!({"choices": [{"delta": {"content": "half"}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "c1", "function": {"name": "x", "arguments": "{\"a"}}]}}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "length"}]}),
        ]);
        assert_eq!(reply.stop_reason, "max_tokens");
        assert_eq!(reply.content, vec![json!({"type": "text", "text": "half"})]);

        let mut acc = OpenAiAccumulator::default();
        assert!(
            acc.feed(&json!({"error": {"message": "bad key"}}))
                .unwrap_err()
                .contains("bad key")
        );
        assert!(acc.feed(&json!({"error": "Model not supported"})).is_err());
        assert!(OpenAiAccumulator::default().finish().is_err());
    }
}
