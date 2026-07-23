use serde::{Deserialize, Serialize};
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
pub struct UiState {
    pub device_name: String,
    pub sync_enabled: bool,
    pub launch_at_login: bool,
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
}
