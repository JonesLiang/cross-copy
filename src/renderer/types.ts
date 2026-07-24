export type ScreenPosition = "left" | "right" | "up" | "down";

export type UiState = {
  deviceName: string;
  syncEnabled: boolean;
  launchAtLogin: boolean;
  copyShortcut: string;
  pasteShortcut: string;
  mouseShareEnabled: boolean;
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
