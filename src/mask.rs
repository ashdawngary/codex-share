use anyhow::Result;
use regex::{Captures, Regex};

#[derive(Clone)]
pub struct Masker {
    home: Option<String>,
    literals: Vec<String>,
    patterns: Vec<Regex>,
    assignment: Regex,
}

impl Masker {
    pub fn new(mut literals: Vec<String>) -> Result<Self> {
        if let Ok(extra) = std::env::var("CODEX_SHARE_REDACT") {
            literals.extend(
                extra
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned),
            );
        }
        literals.sort_by_key(|value| std::cmp::Reverse(value.len()));

        Ok(Self {
            home: std::env::var("HOME").ok().filter(|v| !v.is_empty()),
            literals,
            patterns: [
                r"\bsk-[A-Za-z0-9_-]{16,}\b",
                r"\b(?:ghp|gho|ghu|ghs|github_pat)_[A-Za-z0-9_]{16,}\b",
                r"\bglpat-[A-Za-z0-9_-]{16,}\b",
                r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b",
                r"\bAKIA[A-Z0-9]{16}\b",
                r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{12,}",
                r"(?i)https?://[^\s/@:]+:[^\s/@]+@",
            ]
            .into_iter()
            .map(Regex::new)
            .collect::<std::result::Result<_, _>>()?,
            assignment: Regex::new(
                r#"(?i)\b(api[_-]?key|access[_-]?token|auth[_-]?token|password|passwd|secret)\b(\s*[:=]\s*)('[^']*'|\"[^\"]*\"|[^\s,;]+)"#,
            )?,
        })
    }

    pub fn mask(&self, value: &str) -> String {
        let mut result = value.to_owned();
        if let Some(home) = &self.home {
            result = result.replace(home, "~");
        }
        for literal in &self.literals {
            result = result.replace(literal, "[REDACTED]");
        }
        for pattern in &self.patterns {
            result = pattern.replace_all(&result, "[REDACTED]").into_owned();
        }
        self.assignment
            .replace_all(&result, |caps: &Captures<'_>| {
                format!("{}{}[REDACTED]", &caps[1], &caps[2])
            })
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_common_secrets_and_literals() {
        let masker = Masker::new(vec!["private-name".into()]).unwrap();
        let input =
            "api_key=supersecret password: \"hunter2\" sk-abcdefghijklmnopqrstuvwxyz private-name";
        let output = masker.mask(input);
        assert!(!output.contains("supersecret"));
        assert!(!output.contains("hunter2"));
        assert!(!output.contains("sk-abcdef"));
        assert!(!output.contains("private-name"));
    }
}
