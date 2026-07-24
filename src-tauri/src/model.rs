use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScreenPosition {
    Left,
    #[default]
    Right,
    Up,
    Down,
}

impl ScreenPosition {
    pub fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Up => Self::Down,
            Self::Down => Self::Up,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    pub id: String,
    pub name: String,
    pub secret: String,
    pub paired_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub device_id: String,
    pub device_name: String,
    pub peers: Vec<Peer>,
    pub sync_enabled: bool,
    pub launch_at_login: bool,
    #[serde(default = "default_copy_shortcut")]
    pub copy_shortcut: String,
    #[serde(default = "default_paste_shortcut")]
    pub paste_shortcut: String,
    #[serde(default)]
    pub mouse_share_enabled: bool,
    #[serde(default = "default_mouse_shortcut")]
    pub mouse_shortcut: String,
    #[serde(default)]
    pub mouse_position: ScreenPosition,
}

pub fn default_copy_shortcut() -> String {
    "Ctrl+Shift+C".into()
}

pub fn default_paste_shortcut() -> String {
    "Ctrl+Shift+V".into()
}

pub fn default_mouse_shortcut() -> String {
    "Ctrl+Shift+M".into()
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerView {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub last_seen: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub id: String,
    pub direction: String,
    pub label: String,
    pub detail: String,
    pub created_at: u64,
    pub status: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferProgress {
    pub id: String,
    pub label: String,
    pub direction: String,
    pub transferred: u64,
    pub total: u64,
    pub status: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiState {
    pub device_name: String,
    pub sync_enabled: bool,
    pub launch_at_login: bool,
    pub copy_shortcut: String,
    pub paste_shortcut: String,
    pub mouse_share_enabled: bool,
    pub mouse_shortcut: String,
    pub mouse_position: ScreenPosition,
    pub mouse_latency_ms: Option<u64>,
    pub mouse_session_active: bool,
    pub mouse_listener_started: bool,
    pub has_pending_clipboard: bool,
    pub transfer: Option<TransferProgress>,
    pub pairing_code: Option<String>,
    pub pairing_expires_at: Option<u64>,
    pub peers: Vec<PeerView>,
    pub activity: Vec<Activity>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ClipboardPayload {
    Text {
        text: String,
        fingerprint: String,
        created_at: u64,
    },
    Files {
        transfer_id: String,
        names: Vec<String>,
        bytes: u64,
        fingerprint: String,
        created_at: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryPacket {
    pub app: String,
    pub protocol: u8,
    pub id: String,
    pub name: String,
    pub port: u16,
    pub pairing_salt: Option<String>,
    pub pairing_expires_at: Option<u64>,
    #[serde(default)]
    pub mouse_share_enabled: bool,
    #[serde(default)]
    pub mouse_position: ScreenPosition,
}
