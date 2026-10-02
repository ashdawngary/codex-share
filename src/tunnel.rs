use std::{env, path::PathBuf, process::Stdio, time::Duration};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use regex::Regex;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};

#[async_trait]
pub trait ExposureProvider: Send + Sync {
    async fn expose(&self, local_url: &str) -> Result<Exposure>;
}

#[derive(Clone, Copy)]
pub enum TunnelKind {
    None,
    Ngrok,
    Cloudflare,
}

impl TunnelKind {
    pub fn preflight(self) -> Result<()> {
        match self {
            Self::None => Ok(()),
            Self::Ngrok => {
                let executable = executable_on_path("ngrok")
                    .context("ngrok is not on PATH; install it or use --tunnel cloudflare/none")?;
                if ngrok_has_auth_token() {
                    Ok(())
                } else {
                    bail!(
                        "ngrok was found at {}, but no auth token was detected. Run `ngrok config add-authtoken <token>` or set NGROK_AUTHTOKEN before sharing",
                        executable.display()
                    )
                }
            }
            Self::Cloudflare => {
                executable_on_path("cloudflared")
                    .context("cloudflared is not on PATH; install it or use --tunnel ngrok/none")?;
                Ok(())
            }
        }
    }

    pub fn provider(self) -> Box<dyn ExposureProvider> {
        match self {
            Self::None => Box::new(NoTunnel),
            Self::Ngrok => Box::new(CommandTunnel::ngrok()),
            Self::Cloudflare => Box::new(CommandTunnel::cloudflare()),
        }
    }
}

pub fn print_availability() {
    println!("Tunnel availability (local preflight)");
    for (name, kind) in [
        ("ngrok", TunnelKind::Ngrok),
        ("cloudflared", TunnelKind::Cloudflare),
    ] {
        match kind.preflight() {
            Ok(()) => println!("  ready      {name}"),
            Err(error) => println!("  unavailable {name} — {error:#}"),
        }
    }
}

fn executable_on_path(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

fn ngrok_has_auth_token() -> bool {
    env::var("NGROK_AUTHTOKEN").is_ok_and(|token| !token.trim().is_empty())
        || ngrok_config_paths().iter().any(|path| {
            std::fs::read_to_string(path).is_ok_and(|config| config_has_auth_token(&config))
        })
}

fn ngrok_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(config) = env::var_os("XDG_CONFIG_HOME") {
        paths.push(PathBuf::from(config).join("ngrok/ngrok.yml"));
    }
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        paths.extend([
            home.join(".config/ngrok/ngrok.yml"),
            home.join("Library/Application Support/ngrok/ngrok.yml"),
            home.join(".ngrok2/ngrok.yml"),
        ]);
    }
    paths
}

fn config_has_auth_token(config: &str) -> bool {
    config.lines().any(|line| {
        let line = line.trim_start();
        line.strip_prefix("authtoken:")
            .is_some_and(|token| !token.trim().is_empty())
    })
}

pub struct Exposure {
    public_url: Option<String>,
    child: Option<Child>,
}

impl Exposure {
    pub fn public_url(&self) -> Option<&str> {
        self.public_url.as_deref()
    }

    pub async fn stop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill().await;
        }
    }
}

struct NoTunnel;

#[async_trait]
impl ExposureProvider for NoTunnel {
    async fn expose(&self, _local_url: &str) -> Result<Exposure> {
        Ok(Exposure {
            public_url: None,
            child: None,
        })
    }
}

struct CommandTunnel {
    executable: &'static str,
    args: fn(&str) -> Vec<String>,
    url_pattern: Regex,
}

impl CommandTunnel {
    fn ngrok() -> Self {
        Self {
            executable: "ngrok",
            args: |url| {
                vec![
                    "http".into(),
                    url.into(),
                    "--log".into(),
                    "stdout".into(),
                    "--log-format".into(),
                    "json".into(),
                ]
            },
            url_pattern: Regex::new(
                r#"https://[a-zA-Z0-9.-]+\.(?:ngrok\.app|ngrok-free\.(?:app|dev))"#,
            )
            .unwrap(),
        }
    }

    fn cloudflare() -> Self {
        Self {
            executable: "cloudflared",
            args: |url| {
                vec![
                    "tunnel".into(),
                    "--url".into(),
                    url.into(),
                    "--no-autoupdate".into(),
                ]
            },
            url_pattern: Regex::new(r#"https://[a-zA-Z0-9-]+\.trycloudflare\.com"#).unwrap(),
        }
    }
}

#[async_trait]
impl ExposureProvider for CommandTunnel {
    async fn expose(&self, local_url: &str) -> Result<Exposure> {
        let mut child = Command::new(self.executable)
            .args((self.args)(local_url))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| {
                format!(
                    "could not start {}; install it or use --tunnel none",
                    self.executable
                )
            })?;

        let stdout = child.stdout.take().context("tunnel stdout unavailable")?;
        let stderr = child.stderr.take().context("tunnel stderr unavailable")?;
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<String>(32);
        for stream in [
            Box::new(stdout) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
            Box::new(stderr),
        ] {
            let sender = sender.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stream).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let _ = sender.send(line).await;
                }
            });
        }
        drop(sender);

        let public_url = tokio::time::timeout(Duration::from_secs(20), async {
            while let Some(line) = receiver.recv().await {
                if let Some(found) = self.url_pattern.find(&line) {
                    return Some(found.as_str().to_owned());
                }
            }
            None
        })
        .await
        .ok()
        .flatten();

        let Some(public_url) = public_url else {
            let _ = child.kill().await;
            bail!(
                "{} did not report a public URL within 20 seconds",
                self.executable
            );
        };
        Ok(Exposure {
            public_url: Some(public_url),
            child: Some(child),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_an_ngrok_auth_token_in_yaml() {
        assert!(config_has_auth_token("version: 3\nauthtoken: abc123\n"));
        assert!(config_has_auth_token("  authtoken: abc123"));
        assert!(!config_has_auth_token("version: 3\nauthtoken:   \n"));
        assert!(!config_has_auth_token("api_key: abc123"));
    }

    #[test]
    fn recognizes_current_ngrok_public_domains() {
        let pattern = CommandTunnel::ngrok().url_pattern;
        assert!(pattern.is_match("https://example.ngrok.app"));
        assert!(pattern.is_match("https://example.ngrok-free.app"));
        assert!(pattern.is_match("https://example.ngrok-free.dev"));
    }
}
