use std::{convert::Infallible, path::PathBuf, sync::Arc, time::Duration};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, HeaderValue, StatusCode},
    response::{
        sse::{Event, KeepAlive},
        Html, IntoResponse, Response, Sse,
    },
    routing::get,
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use futures_util::StreamExt;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio_stream::wrappers::BroadcastStream;

use crate::source::{PublicEvent, PublicEventBody, SharedFeed};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SharePermission {
    Conversation,
    Activity,
    Diffs,
    ActivityDiffs,
}

impl SharePermission {
    fn code(self) -> &'static str {
        match self {
            Self::Conversation => "c",
            Self::Activity => "ca",
            Self::Diffs => "cd",
            Self::ActivityDiffs => "cad",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Conversation => "conversation only",
            Self::Activity => "conversation + activity",
            Self::Diffs => "conversation + diffs",
            Self::ActivityDiffs => "conversation + activity + diffs",
        }
    }

    fn includes_activity(self) -> bool {
        matches!(self, Self::Activity | Self::ActivityDiffs)
    }

    fn includes_diffs(self) -> bool {
        matches!(self, Self::Diffs | Self::ActivityDiffs)
    }

    fn allows(self, event: &PublicEvent) -> bool {
        match event.body {
            PublicEventBody::Message { .. } => true,
            PublicEventBody::Status { .. } => true,
            PublicEventBody::Activity { .. } => self.includes_activity(),
            PublicEventBody::Diff { .. } => self.includes_diffs(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotQuery {
    before: Option<u64>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotPage {
    events: Vec<PublicEvent>,
    has_more: bool,
    is_working: bool,
}

#[derive(Clone)]
pub struct ShareGrant {
    token: String,
    permission: SharePermission,
}

impl ShareGrant {
    pub fn mint(permission: SharePermission) -> Self {
        let mut bytes = [0_u8; 24];
        rand::rng().fill_bytes(&mut bytes);
        let secret = URL_SAFE_NO_PAD.encode(bytes);
        Self {
            token: format!("cs1_{}_{}", permission.code(), secret),
            permission,
        }
    }

    pub fn permission(&self) -> SharePermission {
        self.permission
    }
}

#[derive(Clone)]
struct AppState {
    feed: Arc<SharedFeed>,
    source: String,
    grant: ShareGrant,
    shutdown: watch::Receiver<bool>,
}

pub fn router(
    feed: Arc<SharedFeed>,
    source: PathBuf,
    grant: ShareGrant,
    shutdown: watch::Receiver<bool>,
) -> Router {
    Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/s/{token}", get(index))
        .route("/s/{token}/snapshot", get(snapshot))
        .route("/s/{token}/events", get(events))
        .route("/s/{token}/events/ws", get(events_ws))
        .with_state(AppState {
            feed,
            source: source
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            grant,
            shutdown,
        })
}

pub fn share_url(base: &str, grant: &ShareGrant) -> String {
    format!("{}/s/{}", base.trim_end_matches('/'), grant.token)
}

fn authorized(candidate: &str, grant: &ShareGrant) -> bool {
    constant_time_eq(candidate.as_bytes(), grant.token.as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

async fn index(Path(candidate): Path<String>, State(state): State<AppState>) -> Response {
    if !authorized(&candidate, &state.grant) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let page = INDEX
        .replace("__TOKEN__", &candidate)
        .replace("__SOURCE__", &escape_html(&state.source))
        .replace("__PERMISSION__", state.grant.permission.label())
        .replace(
            "__ACTIVITY_ALLOWED__",
            if state.grant.permission.includes_activity() {
                "true"
            } else {
                "false"
            },
        )
        .replace(
            "__DIFFS_ALLOWED__",
            if state.grant.permission.includes_diffs() {
                "true"
            } else {
                "false"
            },
        );
    secure(Html(page)).into_response()
}

async fn snapshot(
    Path(candidate): Path<String>,
    Query(query): Query<SnapshotQuery>,
    State(state): State<AppState>,
) -> Response {
    if !authorized(&candidate, &state.grant) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let page = paginate_events(
        state.feed.snapshot().await,
        state.grant.permission,
        query.before,
        query.limit.unwrap_or(40),
    );
    secure(Json(page)).into_response()
}

fn paginate_events(
    events: Vec<PublicEvent>,
    permission: SharePermission,
    before: Option<u64>,
    limit: usize,
) -> SnapshotPage {
    let is_working = events
        .iter()
        .rev()
        .find_map(|event| match &event.body {
            PublicEventBody::Status { status } => Some(status == "working"),
            _ => None,
        })
        .unwrap_or(false);
    let mut allowed: Vec<_> = events
        .into_iter()
        .filter(|event| permission.allows(event))
        .filter(|event| before.is_none_or(|cursor| event.id < cursor))
        .collect();
    let split_at = allowed.len().saturating_sub(limit.clamp(1, 100));
    let has_more = split_at > 0;
    let events = allowed.split_off(split_at);
    SnapshotPage {
        events,
        has_more,
        is_working,
    }
}

async fn events(Path(candidate): Path<String>, State(state): State<AppState>) -> Response {
    if !authorized(&candidate, &state.grant) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let permission = state.grant.permission;
    let mut shutdown = state.shutdown.clone();
    let events = BroadcastStream::new(state.feed.subscribe())
        .filter_map(move |item| async move {
            match item {
                Ok(event) if permission.allows(&event) => {
                    Some(Ok::<Event, Infallible>(to_sse(event)))
                }
                Ok(_) | Err(_) => None,
            }
        })
        .take_until(async move {
            let _ = shutdown.changed().await;
        });
    secure(Sse::new(events).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
        .into_response()
}

async fn events_ws(
    Path(candidate): Path<String>,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> Response {
    if !authorized(&candidate, &state.grant) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let permission = state.grant.permission;
    let feed = state.feed.clone();
    let shutdown = state.shutdown.clone();
    ws.on_upgrade(move |socket| ws_session(socket, feed, permission, shutdown))
        .into_response()
}

async fn ws_session(
    mut socket: WebSocket,
    feed: Arc<SharedFeed>,
    permission: SharePermission,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut events = feed.subscribe();
    loop {
        tokio::select! {
            update = events.recv() => match update {
                Ok(event) if permission.allows(&event) => {
                    let Ok(json) = serde_json::to_string(&event) else { continue; };
                    if socket.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            _ = shutdown.changed() => break,
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}

fn to_sse(event: PublicEvent) -> Event {
    Event::default()
        .id(event.id.to_string())
        .json_data(event)
        .unwrap_or_default()
}

fn secure<T: IntoResponse>(value: T) -> Response {
    let mut response = value.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-store, no-transform"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"));
    response
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

const INDEX: &str = include_str!("../static/index.html");

#[cfg(test)]
mod tests {
    use super::*;

    fn event_with_id(id: u64, body: PublicEventBody) -> PublicEvent {
        PublicEvent {
            id,
            timestamp: None,
            body,
        }
    }

    fn event(body: PublicEventBody) -> PublicEvent {
        event_with_id(1, body)
    }

    #[test]
    fn conversation_grant_excludes_tool_activity_but_keeps_turn_state() {
        let permission = SharePermission::Conversation;
        assert!(permission.allows(&event(PublicEventBody::Message {
            role: "assistant".into(),
            text: "hello".into(),
        })));
        assert!(!permission.allows(&event(PublicEventBody::Activity {
            name: "exec".into(),
            status: "started".into(),
        })));
        assert!(permission.allows(&event(PublicEventBody::Status {
            status: "working".into(),
        })));
    }

    #[test]
    fn snapshot_pages_backward_from_the_newest_allowed_event() {
        let events = (1..=5)
            .map(|id| {
                event_with_id(
                    id,
                    PublicEventBody::Message {
                        role: "assistant".into(),
                        text: id.to_string(),
                    },
                )
            })
            .collect();

        let newest = paginate_events(events, SharePermission::Conversation, None, 2);
        assert_eq!(
            newest
                .events
                .iter()
                .map(|event| event.id)
                .collect::<Vec<_>>(),
            vec![4, 5]
        );
        assert!(newest.has_more);
        assert!(!newest.is_working);

        let older = paginate_events(newest.events, SharePermission::Conversation, Some(4), 2);
        assert!(older.events.is_empty());
        assert!(!older.has_more);

        let events = (1..=5)
            .map(|id| {
                event_with_id(
                    id,
                    PublicEventBody::Message {
                        role: "assistant".into(),
                        text: id.to_string(),
                    },
                )
            })
            .collect();
        let older = paginate_events(events, SharePermission::Conversation, Some(4), 2);
        assert_eq!(
            older
                .events
                .iter()
                .map(|event| event.id)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!(older.has_more);
    }

    #[test]
    fn activity_grant_still_exposes_no_tool_details() {
        let permission = SharePermission::Activity;
        let value = serde_json::to_value(event(PublicEventBody::Activity {
            name: "exec".into(),
            status: "started".into(),
        }))
        .unwrap();
        assert!(permission.allows(&serde_json::from_value(value.clone()).unwrap()));
        assert!(value.get("arguments").is_none());
        assert!(value.get("output").is_none());
    }

    #[test]
    fn diff_grant_is_independent_from_activity() {
        let diff = event(PublicEventBody::Diff {
            files: vec![],
            truncated: false,
        });
        let activity = event(PublicEventBody::Activity {
            name: "exec".into(),
            status: "started".into(),
        });
        assert!(SharePermission::Diffs.allows(&diff));
        assert!(!SharePermission::Diffs.allows(&activity));
        assert!(SharePermission::Activity.allows(&activity));
        assert!(!SharePermission::Activity.allows(&diff));
        assert!(SharePermission::ActivityDiffs.allows(&activity));
        assert!(SharePermission::ActivityDiffs.allows(&diff));
    }
}
