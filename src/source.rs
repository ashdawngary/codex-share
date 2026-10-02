use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, AsyncSeekExt, BufReader, SeekFrom},
    sync::{broadcast, RwLock},
};

use crate::mask::Masker;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicEvent {
    pub id: u64,
    pub timestamp: Option<String>,
    #[serde(flatten)]
    pub body: PublicEventBody,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PublicEventBody {
    Message {
        role: String,
        text: String,
    },
    Activity {
        name: String,
        status: String,
    },
    Diff {
        files: Vec<PublicDiffFile>,
        truncated: bool,
    },
    Status {
        status: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicDiffFile {
    pub path: String,
    pub kind: String,
    pub patch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_path: Option<String>,
}

pub struct SharedFeed {
    history: RwLock<VecDeque<PublicEvent>>,
    capacity: usize,
    sender: broadcast::Sender<PublicEvent>,
}

impl SharedFeed {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (sender, _) = broadcast::channel(capacity.max(16));
        Self {
            history: RwLock::new(VecDeque::with_capacity(capacity)),
            capacity,
            sender,
        }
    }

    pub async fn publish(&self, event: PublicEvent) {
        let mut history = self.history.write().await;
        if history.len() == self.capacity {
            history.pop_front();
        }
        history.push_back(event.clone());
        drop(history);
        let _ = self.sender.send(event);
    }

    pub async fn snapshot(&self) -> Vec<PublicEvent> {
        self.history.read().await.iter().cloned().collect()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<PublicEvent> {
        self.sender.subscribe()
    }
}

pub struct JsonlSource {
    path: PathBuf,
    masker: Masker,
    feed: std::sync::Arc<SharedFeed>,
}

impl JsonlSource {
    pub fn new(path: PathBuf, masker: Masker, feed: std::sync::Arc<SharedFeed>) -> Self {
        Self { path, masker, feed }
    }

    pub async fn run(self) -> Result<()> {
        let file = File::open(&self.path).await?;
        let mut reader = BufReader::new(file);
        let mut chunk = String::new();
        let mut pending = String::new();
        let mut next_id = 1_u64;

        loop {
            chunk.clear();
            match reader.read_line(&mut chunk).await? {
                0 => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    let length = tokio::fs::metadata(&self.path).await?.len();
                    let position = reader.stream_position().await?;
                    if length < position {
                        reader.seek(SeekFrom::Start(0)).await?;
                        pending.clear();
                    }
                }
                _ => {
                    pending.push_str(&chunk);
                    if !pending.ends_with('\n') {
                        continue;
                    }
                    let line = std::mem::take(&mut pending);
                    match serde_json::from_str::<Value>(&line) {
                        Ok(value) => {
                            for body in project(&value, &self.masker) {
                                let event = PublicEvent {
                                    id: next_id,
                                    timestamp: value
                                        .get("timestamp")
                                        .and_then(Value::as_str)
                                        .map(str::to_owned),
                                    body,
                                };
                                next_id += 1;
                                self.feed.publish(event).await;
                            }
                        }
                        Err(error) => tracing::warn!(%error, "skipping malformed rollout line"),
                    }
                }
            }
        }
    }
}

fn project(record: &Value, masker: &Masker) -> Vec<PublicEventBody> {
    let Some(payload) = record.get("payload") else {
        return vec![];
    };
    let Some(record_type) = record.get("type").and_then(Value::as_str) else {
        return vec![];
    };
    let Some(payload_type) = payload.get("type").and_then(Value::as_str) else {
        return vec![];
    };
    match (record_type, payload_type) {
        ("response_item", "message") => {
            let Some(role) = payload.get("role").and_then(Value::as_str) else {
                return vec![];
            };
            if role != "user" && role != "assistant" {
                return vec![];
            }
            let Some(content) = payload.get("content") else {
                return vec![];
            };
            let mut text = extract_text(content);
            if role == "user" {
                text = strip_internal_user_blocks(&text);
            }
            (!text.is_empty())
                .then(|| PublicEventBody::Message {
                    role: role.to_owned(),
                    text: masker.mask(&text),
                })
                .into_iter()
                .collect()
        }
        ("response_item", "function_call" | "custom_tool_call") => {
            vec![PublicEventBody::Activity {
                name: safe_tool_name(payload.get("name").and_then(Value::as_str)),
                status: "started".into(),
            }]
        }
        ("response_item", "function_call_output" | "custom_tool_call_output") => {
            vec![PublicEventBody::Activity {
                name: "tool".into(),
                status: "completed".into(),
            }]
        }
        ("event_msg", "patch_apply_end") => project_diff(payload, masker).into_iter().collect(),
        ("event_msg", "task_started") => vec![PublicEventBody::Status {
            status: "working".into(),
        }],
        ("event_msg", "task_complete" | "turn_aborted") => vec![PublicEventBody::Status {
            status: "idle".into(),
        }],
        _ => vec![],
    }
}

fn project_diff(payload: &Value, masker: &Masker) -> Option<PublicEventBody> {
    const MAX_DIFF_BYTES: usize = 256 * 1024;
    const MAX_DIFF_FILES: usize = 100;

    let changes = payload.get("changes")?.as_object()?;
    let mut files = Vec::with_capacity(changes.len().min(MAX_DIFF_FILES));
    let mut remaining = MAX_DIFF_BYTES;
    let mut truncated = changes.len() > MAX_DIFF_FILES;

    for (path, change) in changes.iter().take(MAX_DIFF_FILES) {
        let kind = change
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("update");
        let raw_patch = if let Some(diff) = change.get("unified_diff").and_then(Value::as_str) {
            diff.to_owned()
        } else if let Some(content) = change.get("content").and_then(Value::as_str) {
            let prefix = if kind == "delete" { '-' } else { '+' };
            content
                .split_inclusive('\n')
                .map(|line| format!("{prefix}{line}"))
                .collect()
        } else {
            String::new()
        };
        let masked = masker.mask(&raw_patch);
        let (patch, was_truncated) = truncate_utf8(&masked, remaining);
        remaining = remaining.saturating_sub(patch.len());
        truncated |= was_truncated;
        files.push(PublicDiffFile {
            path: masker.mask(path),
            kind: kind.to_owned(),
            patch,
            move_path: change
                .get("move_path")
                .and_then(Value::as_str)
                .map(|path| masker.mask(path)),
        });
        if remaining == 0 {
            truncated |= files.len() < changes.len();
            break;
        }
    }

    (!files.is_empty()).then_some(PublicEventBody::Diff { files, truncated })
}

fn truncate_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let end = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= max_bytes)
        .last()
        .unwrap_or(0);
    (value[..end].to_owned(), true)
}

fn safe_tool_name(name: Option<&str>) -> String {
    let name = name.unwrap_or("tool");
    if name.len() <= 80
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.'))
    {
        name.to_owned()
    } else {
        "tool".into()
    }
}

fn extract_text(content: &Value) -> String {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            item.get("text")
                .and_then(Value::as_str)
                .or_else(|| item.get("input_text").and_then(Value::as_str))
                .or_else(|| item.get("output_text").and_then(Value::as_str))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn strip_internal_user_blocks(input: &str) -> String {
    let mut result = input.to_owned();
    for tag in ["environment_context", "recommended_plugins"] {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        while let Some(start) = result.find(&open) {
            let search_from = start + open.len();
            let Some(relative_end) = result[search_from..].find(&close) else {
                result.truncate(start);
                break;
            };
            let end = search_from + relative_end + close.len();
            result.replace_range(start..end, "");
        }
    }
    result.trim().to_owned()
}

pub fn newest_rollout() -> Result<PathBuf> {
    recent_rollouts(1)?
        .into_iter()
        .next()
        .context("no rollout JSONL files found")
}

pub fn recent_rollouts(limit: usize) -> Result<Vec<PathBuf>> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let sessions = PathBuf::from(home).join(".codex/sessions");
    let mut rollouts = Vec::new();
    visit(&sessions, &mut |path| {
        if path.extension().is_some_and(|ext| ext == "jsonl") {
            if let Ok(modified) = path.metadata().and_then(|m| m.modified()) {
                rollouts.push((modified, path.to_owned()));
            }
        }
    })?;
    rollouts.sort_by(|(left, _), (right, _)| right.cmp(left));
    Ok(rollouts
        .into_iter()
        .take(limit.max(1))
        .map(|(_, path)| path)
        .collect())
}

fn visit(dir: &Path, callback: &mut impl FnMut(&Path)) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            visit(&path, callback)?;
        } else {
            callback(&path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_messages_but_not_internal_data() {
        let masker = Masker::new(vec![]).unwrap();
        let message = serde_json::json!({
            "type": "response_item",
            "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "done"}]}
        });
        assert!(matches!(
            project(&message, &masker).as_slice(),
            [PublicEventBody::Message { text, .. }] if text == "done"
        ));

        let reasoning = serde_json::json!({
            "type": "response_item",
            "payload": {"type": "reasoning", "encrypted_content": "secret", "summary": []}
        });
        assert!(project(&reasoning, &masker).is_empty());
    }

    #[test]
    fn drops_tool_arguments_and_outputs() {
        let masker = Masker::new(vec![]).unwrap();
        let call = serde_json::json!({
            "type": "response_item",
            "payload": {"type": "function_call", "name": "exec_command", "arguments": "TOP SECRET"}
        });
        let serialized = serde_json::to_string(&project(&call, &masker)).unwrap();
        assert!(!serialized.contains("TOP SECRET"));
    }

    #[test]
    fn strips_codex_metadata_from_user_messages() {
        let masker = Masker::new(vec![]).unwrap();
        let message = serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "<recommended_plugins>private plugin list</recommended_plugins>\n<environment_context><cwd>/private/repo</cwd></environment_context>\nActual request"
                }]
            }
        });
        assert!(matches!(
            project(&message, &masker).as_slice(),
            [PublicEventBody::Message { text, .. }] if text == "Actual request"
        ));

        let metadata_only = serde_json::json!({
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "<environment_context>private</environment_context>"}]
            }
        });
        assert!(project(&metadata_only, &masker).is_empty());
    }

    #[test]
    fn projects_masked_structured_diffs_without_command_output() {
        let masker = Masker::new(vec![]).unwrap();
        let patch = serde_json::json!({
            "type": "event_msg",
            "payload": {
                "type": "patch_apply_end",
                "stdout": "PRIVATE COMMAND OUTPUT",
                "stderr": "PRIVATE STDERR",
                "changes": {
                    "src/new.rs": {"type": "add", "content": "const KEY: &str = \"api_key=supersecret\";\n"},
                    "src/old.rs": {"type": "update", "unified_diff": "@@ -1 +1 @@\n-old\n+new\n", "move_path": null}
                }
            }
        });
        let serialized = serde_json::to_string(&project(&patch, &masker)).unwrap();
        assert!(serialized.contains("src/new.rs"));
        assert!(serialized.contains("@@ -1 +1 @@"));
        assert!(!serialized.contains("supersecret"));
        assert!(!serialized.contains("PRIVATE COMMAND OUTPUT"));
        assert!(!serialized.contains("PRIVATE STDERR"));
    }
}
