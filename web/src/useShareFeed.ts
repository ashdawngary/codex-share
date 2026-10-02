import { useCallback, useEffect, useRef, useState } from "react";
import type { PublicEvent, ShareMeta, SnapshotPage } from "./types";

const PAGE_SIZE = 40;

function mergeEvents(current: PublicEvent[], incoming: PublicEvent[]): PublicEvent[] {
  const merged = new Map(current.map((event) => [event.id, event]));
  for (const event of incoming) merged.set(event.id, event);
  return [...merged.values()].sort((left, right) => left.id - right.id);
}

function tokenFromLocation(): string {
  const parts = window.location.pathname.split("/").filter(Boolean);
  return parts.at(-1) ?? "";
}

export function useShareFeed() {
  const token = tokenFromLocation();
  const [meta, setMeta] = useState<ShareMeta | null>(null);
  const [events, setEvents] = useState<PublicEvent[]>([]);
  const [hasMore, setHasMore] = useState(true);
  const [isWorking, setIsWorking] = useState(false);
  const [connection, setConnection] = useState<"connecting" | "live" | "offline">("connecting");
  const [ready, setReady] = useState(false);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const snapshotReady = useRef(false);
  const liveBuffer = useRef<PublicEvent[]>([]);
  const eventsRef = useRef<PublicEvent[]>([]);
  const loadingOlderRef = useRef(false);

  useEffect(() => {
    eventsRef.current = events;
  }, [events]);

  const fetchPage = useCallback(async (before?: number): Promise<SnapshotPage> => {
    const query = new URLSearchParams({ limit: String(PAGE_SIZE) });
    if (before !== undefined) query.set("before", String(before));
    const response = await fetch(`/s/${token}/snapshot?${query}`);
    if (!response.ok) throw new Error("snapshot unavailable");
    return response.json() as Promise<SnapshotPage>;
  }, [token]);

  useEffect(() => {
    let cancelled = false;
    Promise.all([
      fetch(`/s/${token}/meta`).then((response) => {
        if (!response.ok) throw new Error("share unavailable");
        return response.json() as Promise<ShareMeta>;
      }),
      fetchPage(),
    ]).then(([nextMeta, page]) => {
      if (cancelled) return;
      const buffered = liveBuffer.current.splice(0);
      snapshotReady.current = true;
      setMeta(nextMeta);
      setHasMore(page.hasMore);
      setIsWorking(
        buffered.reduce(
          (working, event) => event.kind === "status" ? event.status === "working" : working,
          page.isWorking,
        ),
      );
      setEvents(mergeEvents(page.events, buffered));
      setReady(true);
    }).catch(() => {
      if (cancelled) return;
      snapshotReady.current = true;
      setConnection("offline");
      setReady(true);
    });
    return () => {
      cancelled = true;
    };
  }, [fetchPage, token]);

  useEffect(() => {
    let cancelled = false;
    let socket: WebSocket | undefined;
    let retry: number | undefined;

    function connect() {
      if (cancelled) return;
      setConnection("connecting");
      const scheme = window.location.protocol === "https:" ? "wss" : "ws";
      socket = new WebSocket(`${scheme}://${window.location.host}/s/${token}/events/ws`);
      socket.onopen = () => setConnection("live");
      socket.onerror = () => socket?.close();
      socket.onclose = () => {
        if (cancelled) return;
        setConnection("offline");
        retry = window.setTimeout(connect, 1000);
      };
      socket.onmessage = (message) => {
        const event = JSON.parse(String(message.data)) as PublicEvent;
        if (!snapshotReady.current) {
          liveBuffer.current.push(event);
          return;
        }
        if (event.kind === "status") setIsWorking(event.status === "working");
        setEvents((current) => mergeEvents(current, [event]));
      };
    }

    connect();
    return () => {
      cancelled = true;
      if (retry !== undefined) window.clearTimeout(retry);
      socket?.close();
    };
  }, [token]);

  const loadOlder = useCallback(async () => {
    const first = eventsRef.current[0];
    if (!snapshotReady.current || loadingOlderRef.current || !hasMore || !first) return false;
    loadingOlderRef.current = true;
    setLoadingOlder(true);
    try {
      const page = await fetchPage(first.id);
      setHasMore(page.hasMore);
      setEvents((current) => mergeEvents(current, page.events));
      return true;
    } finally {
      loadingOlderRef.current = false;
      setLoadingOlder(false);
    }
  }, [fetchPage, hasMore]);

  return { meta, events, hasMore, isWorking, connection, ready, loadingOlder, loadOlder };
}
