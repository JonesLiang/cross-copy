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

export type UiState = {
  deviceName: string;
  displays: DisplayInfo[];
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
    direct: boolean;
    clipboardAllowed: boolean;
    mouseAllowed: boolean;
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
