use std::{
    fs::File,
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::source;

struct SessionOption {
    path: PathBuf,
    title: String,
    preview: String,
    modified: SystemTime,
}

pub fn choose_recent_session(limit: usize) -> Result<Option<PathBuf>> {
    let sessions = source::recent_rollouts(limit)?;
    if sessions.is_empty() {
        bail!("no rollout JSONL files found")
    }
    let options: Vec<_> = sessions.iter().map(|path| describe_session(path)).collect();

    println!("\nChoose a Codex thread to share\n");
    for (index, option) in options.iter().enumerate() {
        println!(
            "  {:>2}. {}  {}",
            index + 1,
            option.title,
            ago(option.modified)
        );
        println!("      {}", option.preview);
    }
    println!("\n   q. Cancel");
    print!("\nSelect a thread [1-{}]: ", options.len());
    io::stdout().flush().context("could not draw picker")?;

    let mut selection = String::new();
    io::stdin()
        .read_line(&mut selection)
        .context("could not read picker selection")?;
    let selection = selection.trim();
    if selection.eq_ignore_ascii_case("q") || selection.is_empty() {
        return Ok(None);
    }
    let index: usize = selection
        .parse()
        .context("enter a thread number, or q to cancel")?;
    options
        .get(index.saturating_sub(1))
        .map(|option| Some(option.path.clone()))
        .context("thread number is out of range")
}

fn describe_session(path: &Path) -> SessionOption {
    let modified = path
        .metadata()
        .and_then(|metadata| metadata.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let fallback = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Untitled thread")
        .to_owned();
    let mut title = fallback;
    let mut preview = "No user message yet".to_owned();

    if let Ok(file) = File::open(path) {
        for line in BufReader::new(file).lines().take(120).flatten() {
            let Ok(record) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if record.get("type").and_then(Value::as_str) == Some("session_meta") {
                if let Some(id) = record
                    .get("payload")
                    .and_then(|payload| payload.get("id"))
                    .and_then(Value::as_str)
                {
                    title = compact(id, 42);
                }
            }
            if record.get("type").and_then(Value::as_str) == Some("response_item")
                && record
                    .get("payload")
                    .and_then(|payload| payload.get("type"))
                    .and_then(Value::as_str)
                    == Some("message")
                && record
                    .get("payload")
                    .and_then(|payload| payload.get("role"))
                    .and_then(Value::as_str)
                    == Some("user")
            {
                if let Some(text) = message_text(
                    record
                        .get("payload")
                        .and_then(|payload| payload.get("content")),
                ) {
                    let text = source::strip_internal_user_blocks(&text);
                    if !text.is_empty() {
                        preview = compact(&text, 92);
                        break;
                    }
                }
            }
        }
    }

    SessionOption {
        path: path.to_owned(),
        title,
        preview,
        modified,
    }
}

fn message_text(content: Option<&Value>) -> Option<String> {
    let text = content?
        .as_array()?
        .iter()
        .filter_map(|item| {
            item.get("text")
                .and_then(Value::as_str)
                .or_else(|| item.get("input_text").and_then(Value::as_str))
        })
        .collect::<Vec<_>>()
        .join(" ");
    (!text.trim().is_empty()).then_some(text)
}

fn compact(value: &str, limit: usize) -> String {
    let mut text = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > limit {
        text = text
            .chars()
            .take(limit.saturating_sub(1))
            .collect::<String>();
        text.push('…');
    }
    text
}

fn ago(time: SystemTime) -> String {
    let elapsed = SystemTime::now()
        .duration_since(time)
        .unwrap_or(Duration::ZERO);
    match elapsed.as_secs() {
        0..=59 => "just now".into(),
        60..=3_599 => format!("{}m ago", elapsed.as_secs() / 60),
        3_600..=86_399 => format!("{}h ago", elapsed.as_secs() / 3_600),
        seconds => format!("{}d ago", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_preview_uses_the_first_user_message() {
        let option = describe_session(Path::new("tests/fixtures/session.jsonl"));
        assert_eq!(option.title, "thread-test");
        assert_eq!(option.preview, "Please check api_key=supersecret");
    }

    #[test]
    fn compact_makes_one_line_labels() {
        assert_eq!(compact("one\n two\tthree", 20), "one two three");
        assert_eq!(compact("123456", 5), "1234…");
    }
}
