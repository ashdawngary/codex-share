import { memo, useEffect, useMemo, useRef, useState } from "react";
import { Markdown } from "./markdown";
import type { PublicDiffFile, PublicEvent } from "./types";
import { useShareFeed } from "./useShareFeed";

function formatTime(timestamp?: string): string {
  if (!timestamp) return "";
  const date = new Date(timestamp);
  return Number.isNaN(date.valueOf())
    ? ""
    : date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

const MessageCard = memo(function MessageCard({ event }: {
  event: Extract<PublicEvent, { kind: "message" }>;
}) {
  const assistant = event.role === "assistant";
  return (
    <article className={`message ${event.role}`}>
      <div className="message-header">
        <div className="message-author">
          <span className="author-mark">{assistant ? "C" : "Y"}</span>
          <span className="message-role">{assistant ? "Codex" : "You"}</span>
        </div>
        <time className="message-time">{formatTime(event.timestamp)}</time>
      </div>
      <div className="message-text"><Markdown>{event.text}</Markdown></div>
    </article>
  );
});

const ActivityGroup = memo(function ActivityGroup({ events }: {
  events: Extract<PublicEvent, { kind: "activity" }>[];
}) {
  const latest = events.at(-1);
  return (
    <details className="activity-group">
      <summary className="activity-summary">
        <span className="activity-glyph" aria-hidden="true">&gt;_</span>
        <span className="chevron" aria-hidden="true" />
        <span className="activity-title">Agent activity</span>
        <span className="activity-latest">{latest ? `${latest.name} ${latest.status}` : ""}</span>
        <span className="activity-count">{events.length} {events.length === 1 ? "event" : "events"}</span>
      </summary>
      <div className="activity-list">
        {events.map((event) => (
          <div className="activity-row" data-status={event.status} key={event.id}>
            <span>{event.name}</span><span className="activity-status">{event.status}</span>
          </div>
        ))}
      </div>
    </details>
  );
});

function DiffFile({ file, open }: { file: PublicDiffFile; open: boolean }) {
  return (
    <details className="diff-file" open={open}>
      <summary className="diff-file-summary">
        <span className="diff-kind">{file.kind}</span>
        <span className="diff-path">{file.path}</span>
        {file.movePath ? <span className="diff-move">→ {file.movePath}</span> : null}
      </summary>
      <pre className="diff-patch">{file.patch || "(no textual changes)"}</pre>
    </details>
  );
}

const DiffCard = memo(function DiffCard({ event }: {
  event: Extract<PublicEvent, { kind: "diff" }>;
}) {
  return (
    <section className="diff-card">
      <div className="diff-header">
        <span className="diff-heading">{event.files.length} {event.files.length === 1 ? "file changed" : "files changed"}</span>
        {event.truncated ? <span className="diff-truncated">Preview truncated</span> : null}
      </div>
      <div className="diff-files">
        {event.files.map((file, index) => <DiffFile file={file} open={event.files.length === 1 || index === 0} key={`${file.path}:${index}`} />)}
      </div>
    </section>
  );
});

function Thinking() {
  return (
    <div className="thinking" role="status">
      <div className="thinking-card">
        <span className="thinking-mark" aria-hidden="true"><span /><span /><span /></span>
        <span className="thinking-title">Codex is working</span>
        <span className="thinking-detail">Live</span>
      </div>
    </div>
  );
}

function Empty() {
  return (
    <div className="empty">
      <div className="empty-mark" />
      <p className="empty-title">Waiting for thread activity</p>
      <p className="empty-copy">Messages will appear here as the conversation begins.</p>
    </div>
  );
}

type TimelineItem =
  | { kind: "event"; event: Exclude<PublicEvent, { kind: "status" | "activity" }> }
  | { kind: "activity"; events: Extract<PublicEvent, { kind: "activity" }>[] };

function groupTimeline(events: PublicEvent[]): TimelineItem[] {
  const result: TimelineItem[] = [];
  for (const event of events) {
    if (event.kind === "status") continue;
    if (event.kind === "activity") {
      const last = result.at(-1);
      if (last?.kind === "activity") last.events.push(event);
      else result.push({ kind: "activity", events: [event] });
    } else {
      result.push({ kind: "event", event });
    }
  }
  return result;
}

export default function App() {
  const { meta, events, isWorking, connection, ready, loadingOlder, loadOlder } = useShareFeed();
  const [pinned, setPinned] = useState(true);
  const [copied, setCopied] = useState(false);
  const initiallyPositioned = useRef(false);
  const pinnedRef = useRef(true);
  const feedRef = useRef<HTMLElement>(null);
  const timeline = useMemo(() => groupTimeline(events), [events]);

  function scrollToLatest(behavior: ScrollBehavior = "smooth") {
    window.scrollTo({ top: document.documentElement.scrollHeight, behavior });
  }

  useEffect(() => {
    if (!ready || initiallyPositioned.current) return;
    window.history.scrollRestoration = "manual";
    const frame = requestAnimationFrame(() => {
      requestAnimationFrame(() => scrollToLatest("auto"));
    });
    const settle = window.setTimeout(() => {
      if (pinnedRef.current) scrollToLatest("auto");
      initiallyPositioned.current = true;
    }, 250);
    return () => {
      cancelAnimationFrame(frame);
      window.clearTimeout(settle);
    };
  }, [ready, events.length]);

  useEffect(() => {
    if (initiallyPositioned.current && pinned) requestAnimationFrame(() => scrollToLatest("auto"));
  }, [events.length, isWorking, pinned]);

  useEffect(() => {
    const feed = feedRef.current;
    if (!feed) return;
    const observer = new ResizeObserver(() => {
      if (pinnedRef.current) scrollToLatest("auto");
    });
    observer.observe(feed);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    async function onScroll() {
      const root = document.documentElement;
      const distance = root.scrollHeight - window.scrollY - window.innerHeight;
      pinnedRef.current = distance < 120;
      setPinned(pinnedRef.current);
      if (window.scrollY >= 220 || loadingOlder) return;
      const previousHeight = root.scrollHeight;
      const previousY = window.scrollY;
      if (await loadOlder()) {
        requestAnimationFrame(() => {
          const addedHeight = document.documentElement.scrollHeight - previousHeight;
          window.scrollTo({ top: previousY + addedHeight, behavior: "auto" });
        });
      }
    }
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => window.removeEventListener("scroll", onScroll);
  }, [loadOlder, loadingOlder]);

  async function copyLink() {
    try {
      await navigator.clipboard.writeText(window.location.href);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1400);
    } catch {
      setCopied(false);
    }
  }

  const connectionLabel = connection === "live" ? "Live" : connection === "connecting" ? "Connecting" : "Reconnecting";
  return (
    <>
      <header className="topbar">
        <div className="topbar-inner">
          <div className="identity">
            <div className="mark" aria-hidden="true" />
            <div className="identity-copy">
              <div className="brand-line">
                <h1>Codex Share</h1>
                <div className="scope"><span>scope</span><strong>{meta?.permission ?? "loading"}</strong></div>
              </div>
              <div className="thread-name" title={meta?.source}>{meta?.source ?? "Loading thread…"}</div>
            </div>
          </div>
          <div className="header-actions">
            <div className="connection" data-state={connection} role="status"><span className="connection-dot" /><span>{connectionLabel}</span></div>
            <button className="copy-button" type="button" onClick={copyLink}>{copied ? "Copied" : "Copy link"}</button>
          </div>
        </div>
      </header>

      <div className="shell">
        <div className="history-loader" hidden={!loadingOlder}><span className="history-spinner" />Loading earlier messages</div>
        <main ref={feedRef} className="feed" aria-live={ready ? "polite" : "off"}>
          {timeline.map((item) => {
            if (item.kind === "activity") return <ActivityGroup events={item.events} key={`activity:${item.events[0]?.id}`} />;
            if (item.event.kind === "message") return <MessageCard event={item.event} key={item.event.id} />;
            return <DiffCard event={item.event} key={item.event.id} />;
          })}
          {ready && timeline.length === 0 ? <Empty /> : null}
          {isWorking ? <Thinking /> : null}
        </main>
      </div>

      <button className={`jump${pinned ? "" : " visible"}`} type="button" onClick={() => { pinnedRef.current = true; setPinned(true); scrollToLatest(); }}>Jump to latest ↓</button>
    </>
  );
}
