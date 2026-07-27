export type ScreenPosition = "left" | "right" | "up" | "down";

export type DisplayInfo = {
  id: string;
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  primary: boolean;
  mirroredCount: number;
};

export type FsEntry = {
  name: string;
  path: string;
  directory: boolean;
  symlink: boolean;
  size: number;
  modifiedAt: number | null;
  readonly: boolean;
  hidden: boolean;
};

export type FsRequest =
  | { type: "roots" }
  | { type: "list"; path: string }
  | { type: "read"; path: string }
  | {
      type: "write";
      path: string;
      data: string;
      expectedModifiedAt: number | null;
    }
  | { type: "createDirectory"; path: string }
  | { type: "createFile"; path: string }
  | { type: "rename"; path: string; destination: string }
  | { type: "remove"; path: string; recursive: boolean };

export type FsResponse =
  | { type: "entries"; entries: FsEntry[] }
  | {
      type: "file";
      path: string;
      data: string;
      modifiedAt: number | null;
      size: number;
    }
  | { type: "done"; entry: FsEntry | null }
  | { type: "error"; message: string };

export type UiState = {
  deviceName: string;
  displays: DisplayInfo[];
  syncEnabled: boolean;
  launchAtLogin: boolean;
  copyShortcut: string;
  pasteShortcut: string;
  mouseShareEnabled: boolean;
  mouseExtremePerformance: boolean;
  mouseShortcut: string;
  mousePosition: ScreenPosition;
  mouseLatencyMs: number | null;
  mouseSessionActive: boolean;
  mouseListenerStarted: boolean;
  hasPendingClipboard: boolean;
  pairingCode: string | null;
  pairingExpiresAt: number | null;
  peers: Array<{
    id: string;
    name: string;
    online: boolean;
    lastSeen?: number;
    direct: boolean;
    clipboardAllowed: boolean;
    mouseAllowed: boolean;
    filesystemAllowed: boolean;
    mouseReceiveDpi: number;
    mouseShareEnabled: boolean;
    screenNumber: number;
    screenPosition: ScreenPosition;
    displays: DisplayInfo[];
  }>;
  activity: Array<{
    id: string;
    direction: "sent" | "received" | "system";
    label: string;
    detail: string;
    createdAt: number;
    status: "done" | "working" | "error";
  }>;
  transfer: {
    id: string;
    label: string;
    direction: "sent" | "received";
    transferred: number;
    total: number;
    status: "working" | "done" | "error";
  } | null;
};
