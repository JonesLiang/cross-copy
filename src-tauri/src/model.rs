use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    #[serde(default = "default_true")]
    pub direct: bool,
    #[serde(default = "default_true")]
    pub clipboard_allowed: bool,
    #[serde(default = "default_true")]
    pub mouse_allowed: bool,
    #[serde(default)]
    pub filesystem_allowed: bool,
    #[serde(default = "default_mouse_receive_dpi")]
    pub mouse_receive_dpi: u16,
    #[serde(default)]
    pub screen_number: u8,
    #[serde(default)]
    pub screen_position: ScreenPosition,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub device_id: String,
    pub device_name: String,
    #[serde(default)]
    pub group_id: String,
    #[serde(default)]
    pub group_secret: String,
    pub peers: Vec<Peer>,
    pub sync_enabled: bool,
    pub launch_at_login: bool,
    #[serde(default = "default_copy_shortcut")]
    pub copy_shortcut: String,
    #[serde(default = "default_paste_shortcut")]
    pub paste_shortcut: String,
    #[serde(default)]
    pub mouse_share_enabled: bool,
    #[serde(default)]
    pub mouse_extreme_performance: bool,
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

pub fn default_mouse_receive_dpi() -> u16 {
    500
}

pub fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerView {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub last_seen: Option<u64>,
    pub direct: bool,
    pub clipboard_allowed: bool,
    pub mouse_allowed: bool,
    pub filesystem_allowed: bool,
    pub mouse_receive_dpi: u16,
    pub mouse_share_enabled: bool,
    pub screen_number: u8,
    pub screen_position: ScreenPosition,
    pub displays: Vec<DisplayView>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayView {
    pub id: String,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
    pub mirrored_count: u32,
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
    pub displays: Vec<DisplayView>,
    pub sync_enabled: bool,
    pub launch_at_login: bool,
    pub copy_shortcut: String,
    pub paste_shortcut: String,
    pub mouse_share_enabled: bool,
    pub mouse_extreme_performance: bool,
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
    #[serde(default)]
    pub instance_id: String,
    #[serde(default)]
    pub displays: Vec<DisplayView>,
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
