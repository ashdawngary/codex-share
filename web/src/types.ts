export interface ShareMeta {
  source: string;
  permission: string;
  activityAllowed: boolean;
  diffsAllowed: boolean;
}

export interface PublicDiffFile {
  path: string;
  kind: string;
  patch: string;
  movePath?: string;
}

interface EventBase {
  id: number;
  timestamp?: string;
}

export type PublicEvent =
  | (EventBase & { kind: "message"; role: "user" | "assistant"; text: string })
  | (EventBase & { kind: "activity"; name: string; status: string })
  | (EventBase & { kind: "diff"; files: PublicDiffFile[]; truncated: boolean })
  | (EventBase & { kind: "status"; status: string });

export interface SnapshotPage {
  events: PublicEvent[];
  hasMore: boolean;
  isWorking: boolean;
}
