export type UiState = {
  deviceName: string;
  syncEnabled: boolean;
  launchAtLogin: boolean;
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
};
