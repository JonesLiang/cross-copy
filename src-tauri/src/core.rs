use crate::{
    crypto::{
        decode_secret, decrypt, encrypt, fingerprint, pairing_key, proof, random_secret, Envelope,
    },
    logger::{masked_ip, Logger},
    model::{
        Activity, ClipboardPayload, DiscoveryPacket, Peer, PeerView, TransferProgress, UiState,
    },
    store::Store,
};
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use clipboard_rs::{Clipboard, ClipboardContent, ClipboardContext, ContentFormat};
use enigo::{
    Direction::{Click, Press, Release},
    Enigo, Key, Keyboard, Settings as EnigoSettings,
};
use rand::RngCore;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tokio::{
    fs::{self, File},
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::Notify,
};
use uuid::Uuid;
use walkdir::WalkDir;

const DISCOVERY_PORT: u16 = 47653;
const TRANSFER_PORT: u16 = 47654;
const MULTICAST: Ipv4Addr = Ipv4Addr::new(239, 255, 67, 89);
const GLOBAL_BROADCAST: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 255);
const CHUNK_SIZE: usize = 1024 * 1024;
const ONLINE_WINDOW_MS: u64 = 35_000;
const CLIPBOARD_RETRY_ATTEMPTS: usize = 16;
const CLIPBOARD_RETRY_DELAY_MS: u64 = 50;
const ACTIVE_DISCOVERY_MS: u64 = 30_000;
const SYNTHETIC_INPUT_MARKER: usize = 0x4352_4f53_5343_4f50;

#[derive(Clone)]
struct SeenPeer {
    packet: DiscoveryPacket,
    host: IpAddr,
    last_seen: u64,
}

struct PairSession {
    code: String,
    salt: String,
    expires_at: u64,
    attempts: HashMap<IpAddr, u8>,
}

#[derive(Clone)]
struct Transfer {
    roots: Vec<PathBuf>,
    expires_at: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum WireMessage {
    PairRequest {
        id: String,
        name: String,
        proof: String,
    },
    PairAccepted {
        id: String,
        name: String,
        envelope: Envelope,
    },
    PairRejected,
    Secure {
        sender_id: String,
        envelope: Envelope,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum UdpPairMessage {
    PairRequest {
        app: String,
        protocol: u8,
        id: String,
        name: String,
        code_hash: String,
    },
    PairAccepted {
        app: String,
        protocol: u8,
        id: String,
        name: String,
        salt: String,
        envelope: Envelope,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum SecureMessage {
    Clipboard { payload: ClipboardPayload },
    Pull { transfer_id: String },
    Manifest { entries: Vec<FileEntry> },
    Error { message: String },
}

#[derive(Clone, Serialize, Deserialize)]
struct FileEntry {
    path: String,
    size: u64,
    directory: bool,
}

enum LocalClipboard {
    Text(String),
    Files(Vec<PathBuf>),
}

#[derive(Clone)]
enum PendingClipboard {
    Text(String),
    Files(Vec<String>),
}

enum ClipboardSnapshot {
    Contents(Vec<ClipboardContent>),
    Empty,
}

struct ClipboardOperationGuard<'a> {
    active: &'a AtomicBool,
}

impl<'a> ClipboardOperationGuard<'a> {
    fn acquire(active: &'a AtomicBool) -> Option<Self> {
        active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self { active })
    }
}

impl Drop for ClipboardOperationGuard<'_> {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

pub struct Core {
    pub store: Arc<Store>,
    app: AppHandle,
    logger: Arc<Logger>,
    discovered: Mutex<HashMap<String, SeenPeer>>,
    pairing: Mutex<Option<PairSession>>,
    transfers: Mutex<HashMap<String, Transfer>>,
    activities: Mutex<Vec<Activity>>,
    transfer_progress: Mutex<Option<TransferProgress>>,
    pending_clipboard: Mutex<Option<PendingClipboard>>,
    clipboard_operation_active: AtomicBool,
    awake_until: AtomicU64,
    last_discovery_response: AtomicU64,
    last_progress_emit: AtomicU64,
    discovery_wake: Notify,
    port: AtomicU64,
}

impl Core {
    pub fn new(store: Arc<Store>, logger: Arc<Logger>, app: AppHandle) -> Arc<Self> {
        Arc::new(Self {
            store,
            app,
            logger,
            discovered: Mutex::new(HashMap::new()),
            pairing: Mutex::new(None),
            transfers: Mutex::new(HashMap::new()),
            activities: Mutex::new(Vec::new()),
            transfer_progress: Mutex::new(None),
            pending_clipboard: Mutex::new(None),
            clipboard_operation_active: AtomicBool::new(false),
            awake_until: AtomicU64::new(now_ms() + ACTIVE_DISCOVERY_MS),
            last_discovery_response: AtomicU64::new(0),
            last_progress_emit: AtomicU64::new(0),
            discovery_wake: Notify::new(),
            port: AtomicU64::new(0),
        })
    }

    pub async fn start(self: &Arc<Self>) -> Result<(), String> {
        self.logger.info(
            "service_start",
            format!(
                "version={} platform={}/{}",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        );
        let listener = TcpListener::bind(("0.0.0.0", TRANSFER_PORT))
            .await
            .map_err(|e| format!("无法监听局域网端口 {TRANSFER_PORT}：{e}"))?;
        self.port.store(
            listener.local_addr().map_err(|e| e.to_string())?.port() as u64,
            Ordering::Relaxed,
        );
        self.logger.info(
            "tcp_listen",
            format!("port={}", self.port.load(Ordering::Relaxed)),
        );
        let server_core = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, address)) => {
                        let core = Arc::clone(&server_core);
                        tauri::async_runtime::spawn(async move {
                            if let Err(error) =
                                Arc::clone(&core).handle_connection(stream, address).await
                            {
                                core.logger.error(
                                    "tcp_connection_failed",
                                    format!("source={} error={error}", masked_ip(address.ip())),
                                );
                            }
                        });
                    }
                    Err(error) => {
                        server_core
                            .logger
                            .error("tcp_accept_failed", error.to_string());
                        tokio::time::sleep(Duration::from_millis(250)).await
                    }
                }
            }
        });
        self.start_discovery().await?;
        Ok(())
    }

    pub fn ui_state(&self) -> UiState {
        let settings = self.store.get();
        let now = now_ms();
        let discovered = self.discovered.lock().expect("discovery lock");
        let pairing = self.pairing.lock().expect("pairing lock");
        UiState {
            device_name: settings.device_name,
            sync_enabled: settings.sync_enabled,
            launch_at_login: settings.launch_at_login,
            copy_shortcut: settings.copy_shortcut,
            paste_shortcut: settings.paste_shortcut,
            has_pending_clipboard: self
                .pending_clipboard
                .lock()
                .expect("pending clipboard lock")
                .is_some(),
            transfer: self
                .transfer_progress
                .lock()
                .expect("transfer progress lock")
                .clone(),
            pairing_code: pairing.as_ref().map(|session| session.code.clone()),
            pairing_expires_at: pairing.as_ref().map(|session| session.expires_at),
            peers: settings
                .peers
                .iter()
                .map(|peer| {
                    let seen = discovered.get(&peer.id);
                    PeerView {
                        id: peer.id.clone(),
                        name: peer.name.clone(),
                        online: seen.is_some_and(|value| {
                            now.saturating_sub(value.last_seen) < ONLINE_WINDOW_MS
                        }),
                        last_seen: seen.map(|value| value.last_seen),
                    }
                })
                .collect(),
            activity: self.activities.lock().expect("activity lock").clone(),
        }
    }

    pub fn publish(&self) {
        let _ = self.app.emit("state", self.ui_state());
    }

    pub fn begin_pairing(&self) {
        self.wake_network();
        let code = format!("{:06}", rand::random_range(0..1_000_000));
        let mut salt = [0_u8; 16];
        rand::rng().fill_bytes(&mut salt);
        *self.pairing.lock().expect("pairing lock") = Some(PairSession {
            code,
            salt: URL_SAFE_NO_PAD.encode(salt),
            expires_at: now_ms() + 120_000,
            attempts: HashMap::new(),
        });
        self.logger.info("pairing_opened", "expires_in_seconds=120");
        self.publish();
    }

    pub fn cancel_pairing(&self) {
        *self.pairing.lock().expect("pairing lock") = None;
        self.logger.info("pairing_cancelled", "by_user=true");
        self.publish();
    }

    pub async fn pair_with_code(self: &Arc<Self>, code: String) -> Result<(), String> {
        if code.len() != 6 || !code.chars().all(|value| value.is_ascii_digit()) {
            return Err("请输入 6 位数字验证码".into());
        }
        self.wake_network();
        let candidates: Vec<SeenPeer> = self
            .discovered
            .lock()
            .expect("discovery lock")
            .values()
            .filter(|seen| {
                seen.packet.pairing_salt.is_some()
                    && seen.packet.pairing_expires_at.unwrap_or(0) > now_ms()
            })
            .cloned()
            .collect();
        self.logger.info(
            "pairing_submit",
            format!("discovered_candidates={}", candidates.len()),
        );
        let mut last_error = None;
        for seen in candidates {
            match self.try_pair(&seen, &code).await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    self.logger.warn("pairing_tcp_failed", &error);
                    last_error = Some(error);
                }
            }
        }
        self.try_pair_udp(&code).await.map_err(|udp_error| {
            self.logger.error("pairing_all_methods_failed", &udp_error);
            format!(
                "连接失败：{}；热点模式也失败：{udp_error}",
                last_error.unwrap_or_else(|| "没有收到对方的设备广播".into())
            )
        })
    }

    pub fn set_sync(&self, value: bool) -> Result<(), String> {
        self.store
            .update(|settings| settings.sync_enabled = value)
            .map_err(|e| e.to_string())?;
        self.publish();
        if value {
            self.wake_network();
        }
        Ok(())
    }

    pub fn set_shortcuts(&self, copy: String, paste: String) -> Result<(), String> {
        self.store
            .update(|settings| {
                settings.copy_shortcut = copy;
                settings.paste_shortcut = paste;
            })
            .map_err(|error| error.to_string())?;
        self.publish();
        Ok(())
    }

    pub async fn trigger_copy(self: &Arc<Self>) {
        let Some(_operation) = ClipboardOperationGuard::acquire(&self.clipboard_operation_active)
        else {
            self.logger.info(
                "clipboard_shortcut_ignored",
                "reason=operation_in_progress kind=copy",
            );
            return;
        };
        if !self.store.get().sync_enabled {
            self.add_activity("system", "同步已暂停", "请先开启同步", "error");
            return;
        }
        self.wake_network();
        let original = match capture_clipboard(&self.logger).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.add_activity("system", "无法保护本机剪贴板", &error, "error");
                return;
            }
        };
        if let Err(error) = simulate_native_shortcut('c') {
            self.logger.error("shortcut_copy_simulation_failed", &error);
            self.add_activity("system", "无法复制", &error, "error");
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let event = read_current_clipboard(&self.logger).await;
        if let Err(error) = restore_clipboard(original, &self.logger).await {
            self.logger.error("clipboard_restore_failed", &error);
            self.add_activity("system", "恢复本机剪贴板失败", &error, "error");
            return;
        }
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                self.add_activity("system", "未读取到内容", &error, "error");
                return;
            }
        };
        let peer_is_known_online = self
            .discovered
            .lock()
            .expect("discovery lock")
            .values()
            .any(|seen| now_ms().saturating_sub(seen.last_seen) < ONLINE_WINDOW_MS);
        // Known peers send immediately. A cold or sleeping peer gets one LAN round trip.
        tokio::time::sleep(Duration::from_millis(if peer_is_known_online {
            20
        } else {
            350
        }))
        .await;
        if let Err(error) = self.handle_local_clipboard(event).await {
            self.logger.error("clipboard_local_failed", &error);
            self.add_activity("system", "发送失败", &error, "error");
        }
    }

    pub async fn trigger_paste(self: &Arc<Self>) {
        let Some(_operation) = ClipboardOperationGuard::acquire(&self.clipboard_operation_active)
        else {
            self.logger.info(
                "clipboard_shortcut_ignored",
                "reason=operation_in_progress kind=paste",
            );
            return;
        };
        if !self.store.get().sync_enabled {
            self.add_activity("system", "同步已暂停", "请先开启同步", "error");
            return;
        }
        self.wake_network();
        let original = match capture_clipboard(&self.logger).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.add_activity("system", "无法保护本机剪贴板", &error, "error");
                return;
            }
        };
        let pending = self
            .pending_clipboard
            .lock()
            .expect("pending clipboard lock")
            .clone();
        let result = match pending {
            Some(PendingClipboard::Text(text)) => write_clipboard_text(&text, &self.logger).await,
            Some(PendingClipboard::Files(files)) => {
                write_clipboard_files(&files, &self.logger).await
            }
            None => {
                self.add_activity(
                    "system",
                    "没有待粘贴内容",
                    "请先在另一台电脑触发跨设备复制",
                    "error",
                );
                return;
            }
        };
        if let Err(error) = result {
            let _ = restore_clipboard(original, &self.logger).await;
            self.add_activity("system", "写入剪贴板失败", &error, "error");
            return;
        }
        if let Err(error) = simulate_native_shortcut('v') {
            self.logger
                .error("shortcut_paste_simulation_failed", &error);
            self.add_activity("system", "无法粘贴", &error, "error");
            let _ = restore_clipboard(original, &self.logger).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(600)).await;
        if let Err(error) = restore_clipboard(original, &self.logger).await {
            self.logger.error("clipboard_restore_failed", &error);
            self.add_activity("system", "恢复本机剪贴板失败", &error, "error");
            return;
        }
        self.add_activity("system", "已触发跨设备粘贴", "内容已粘贴到当前应用", "done");
    }

    pub fn set_launch_at_login(&self, value: bool) -> Result<(), String> {
        self.store
            .update(|settings| settings.launch_at_login = value)
            .map_err(|e| e.to_string())?;
        self.publish();
        Ok(())
    }

    pub fn unpair(&self, peer_id: &str) -> Result<(), String> {
        self.store
            .update(|settings| settings.peers.retain(|peer| peer.id != peer_id))
            .map_err(|e| e.to_string())?;
        self.publish();
        self.logger.info("peer_removed", "by_user=true");
        Ok(())
    }

    pub fn export_diagnostics(&self) -> Result<String, String> {
        let settings = self.store.get();
        let now = now_ms();
        let online = self
            .discovered
            .lock()
            .expect("discovery lock")
            .values()
            .filter(|peer| now.saturating_sub(peer.last_seen) < ONLINE_WINDOW_MS)
            .count();
        let summary = format!(
            "sync_enabled={}\npaired_peers={}\nonline_peers={}\ndiscovery_port={}\ntransfer_port={}",
            settings.sync_enabled,
            settings.peers.len(),
            online,
            DISCOVERY_PORT,
            TRANSFER_PORT
        );
        let directory = dirs::download_dir()
            .ok_or("无法找到下载目录")?
            .join("CrossCopy");
        let path = self
            .logger
            .export(&directory, &summary)
            .map_err(|e| e.to_string())?;
        reveal_file(&path);
        self.logger
            .info("diagnostics_exported", "destination=downloads/CrossCopy");
        Ok(path.to_string_lossy().into_owned())
    }

    async fn start_discovery(self: &Arc<Self>) -> Result<(), String> {
        let socket = UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT))
            .await
            .map_err(|e| e.to_string())?;
        socket
            .join_multicast_v4(MULTICAST, Ipv4Addr::UNSPECIFIED)
            .map_err(|e| e.to_string())?;
        socket.set_multicast_ttl_v4(1).map_err(|e| e.to_string())?;
        socket
            .set_multicast_loop_v4(false)
            .map_err(|e| e.to_string())?;
        socket.set_broadcast(true).map_err(|e| e.to_string())?;
        self.logger.info(
            "discovery_started",
            format!("port={DISCOVERY_PORT} multicast={MULTICAST} broadcast=true"),
        );
        let socket = Arc::new(socket);

        let receive_core = Arc::clone(self);
        let receive_socket = Arc::clone(&socket);
        tauri::async_runtime::spawn(async move {
            let mut buffer = [0_u8; 4096];
            loop {
                let Ok((size, source)) = receive_socket.recv_from(&mut buffer).await else {
                    continue;
                };
                if let Ok(packet) = serde_json::from_slice::<DiscoveryPacket>(&buffer[..size]) {
                    if packet.app != "crosscopy"
                        || packet.protocol != 1
                        || packet.id == receive_core.store.get().device_id
                    {
                        continue;
                    }
                    let packet_port = packet.port;
                    let packet_pairing = packet.pairing_salt.is_some();
                    let packet_id = packet.id.clone();
                    let is_paired = receive_core
                        .store
                        .get()
                        .peers
                        .iter()
                        .any(|peer| peer.id == packet_id);
                    let previous = receive_core
                        .discovered
                        .lock()
                        .expect("discovery lock")
                        .insert(
                            packet_id,
                            SeenPeer {
                                packet,
                                host: source.ip(),
                                last_seen: now_ms(),
                            },
                        );
                    if previous.is_none() {
                        receive_core.logger.info(
                            "peer_discovered",
                            format!(
                                "source={} port={} pairing={}",
                                masked_ip(source.ip()),
                                packet_port,
                                packet_pairing
                            ),
                        );
                    }
                    if is_paired {
                        if now_ms() > receive_core.awake_until.load(Ordering::Relaxed) {
                            receive_core.wake_network();
                        } else {
                            let last = receive_core.last_discovery_response.load(Ordering::Relaxed);
                            if now_ms().saturating_sub(last) > 2_000 {
                                receive_core
                                    .last_discovery_response
                                    .store(now_ms(), Ordering::Relaxed);
                                receive_core.discovery_wake.notify_one();
                            }
                        }
                    }
                    receive_core.publish();
                    continue;
                }
                if let Ok(request) = serde_json::from_slice::<UdpPairMessage>(&buffer[..size]) {
                    let core = Arc::clone(&receive_core);
                    let socket = Arc::clone(&receive_socket);
                    tauri::async_runtime::spawn(async move {
                        let _ = core.handle_udp_pair_request(&socket, source, request).await;
                    });
                }
            }
        });

        let beacon_core = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let multicast_target = SocketAddr::new(IpAddr::V4(MULTICAST), DISCOVERY_PORT);
            loop {
                let packet = beacon_core.discovery_packet();
                if let Ok(bytes) = serde_json::to_vec(&packet) {
                    let _ = socket.send_to(&bytes, multicast_target).await;
                    for target in discovery_broadcast_targets() {
                        let _ = socket.send_to(&bytes, target).await;
                    }
                }
                let delay = if now_ms() < beacon_core.awake_until.load(Ordering::Relaxed) {
                    Duration::from_secs(1)
                } else {
                    Duration::from_secs(15)
                };
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = beacon_core.discovery_wake.notified() => {}
                }
            }
        });
        Ok(())
    }

    pub fn wake_network(&self) {
        self.awake_until
            .store(now_ms() + ACTIVE_DISCOVERY_MS, Ordering::Relaxed);
        self.discovery_wake.notify_one();
        self.logger.info("network_wake", "active_for_seconds=30");
    }

    fn discovery_packet(&self) -> DiscoveryPacket {
        let settings = self.store.get();
        let mut pairing = self.pairing.lock().expect("pairing lock");
        if pairing
            .as_ref()
            .is_some_and(|value| value.expires_at <= now_ms())
        {
            *pairing = None;
        }
        DiscoveryPacket {
            app: "crosscopy".into(),
            protocol: 1,
            id: settings.device_id,
            name: settings.device_name,
            port: self.port.load(Ordering::Relaxed) as u16,
            pairing_salt: pairing.as_ref().map(|value| value.salt.clone()),
            pairing_expires_at: pairing.as_ref().map(|value| value.expires_at),
        }
    }

    async fn handle_local_clipboard(self: &Arc<Self>, event: LocalClipboard) -> Result<(), String> {
        let payload = match event {
            LocalClipboard::Text(text) => {
                if text.is_empty() {
                    return Ok(());
                }
                self.logger.info(
                    "clipboard_local_text",
                    format!("characters={}", text.chars().count()),
                );
                ClipboardPayload::Text {
                    fingerprint: fingerprint(&text),
                    created_at: now_ms(),
                    text,
                }
            }
            LocalClipboard::Files(paths) => {
                if paths.is_empty() {
                    return Ok(());
                }
                self.logger
                    .info("clipboard_local_files", format!("items={}", paths.len()));
                let transfer_id = Uuid::new_v4().to_string();
                let (bytes, names) = scan_roots(&paths)?;
                self.transfers.lock().expect("transfer lock").insert(
                    transfer_id.clone(),
                    Transfer {
                        roots: paths.clone(),
                        expires_at: now_ms() + 600_000,
                    },
                );
                ClipboardPayload::Files {
                    transfer_id,
                    names,
                    bytes,
                    fingerprint: fingerprint(
                        paths
                            .iter()
                            .map(|path| path.to_string_lossy())
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    created_at: now_ms(),
                }
            }
        };
        let delivered = self.broadcast(&payload).await;
        if delivered == 0 {
            self.add_activity(
                "system",
                "未发送",
                "暂未发现在线设备，请点击连接后重试",
                "error",
            );
            return Ok(());
        }
        match &payload {
            ClipboardPayload::Text { text, .. } => self.add_activity(
                "sent",
                &ellipsize(text, 42),
                &format!("{} 个字符", text.chars().count()),
                "done",
            ),
            ClipboardPayload::Files { names, bytes, .. } => {
                self.add_activity("sent", &summary_names(names), &format_bytes(*bytes), "done")
            }
        }
        Ok(())
    }

    async fn broadcast(&self, payload: &ClipboardPayload) -> u32 {
        let settings = self.store.get();
        let discovered = self.discovered.lock().expect("discovery lock").clone();
        let kind = match payload {
            ClipboardPayload::Text { .. } => "text",
            ClipboardPayload::Files { .. } => "files",
        };
        let mut online = 0_u32;
        let mut delivered = 0_u32;
        for peer in settings.peers {
            let Some(seen) = discovered.get(&peer.id) else {
                self.logger.info(
                    "clipboard_peer_skipped",
                    format!("kind={kind} reason=not_discovered"),
                );
                continue;
            };
            if now_ms().saturating_sub(seen.last_seen) >= ONLINE_WINDOW_MS {
                self.logger.info(
                    "clipboard_peer_skipped",
                    format!("kind={kind} reason=discovery_stale"),
                );
                continue;
            }
            online += 1;
            let address = SocketAddr::new(seen.host, seen.packet.port);
            let key = match decode_secret(&peer.secret) {
                Ok(value) => value,
                Err(error) => {
                    self.logger.error(
                        "clipboard_secret_failed",
                        format!("kind={kind} error={error}"),
                    );
                    continue;
                }
            };
            match TcpStream::connect(address).await {
                Ok(mut stream) => {
                    let secure = SecureMessage::Clipboard {
                        payload: payload.clone(),
                    };
                    if let Ok(envelope) = encrypt(&key, &secure) {
                        let message = WireMessage::Secure {
                            sender_id: settings.device_id.clone(),
                            envelope,
                        };
                        if let Err(error) = write_json(&mut stream, &message).await {
                            self.logger.warn(
                                "clipboard_announce_failed",
                                format!("target={} error={error}", masked_ip(address.ip())),
                            );
                        } else {
                            delivered += 1;
                        }
                    }
                }
                Err(error) => {
                    self.logger.warn(
                        "peer_connect_failed",
                        format!("target={} error={error}", masked_ip(address.ip())),
                    );
                }
            }
        }
        self.logger.info(
            "clipboard_broadcast_completed",
            format!("kind={kind} online={online} delivered={delivered}"),
        );
        delivered
    }

    async fn try_pair(&self, seen: &SeenPeer, code: &str) -> Result<(), String> {
        self.logger.info(
            "pairing_tcp_start",
            format!("target={} port={}", masked_ip(seen.host), seen.packet.port),
        );
        let settings = self.store.get();
        let salt = seen.packet.pairing_salt.as_deref().ok_or("配对信息无效")?;
        let key = pairing_key(code, salt)?;
        let mut stream = tokio::time::timeout(
            Duration::from_secs(5),
            TcpStream::connect((seen.host, seen.packet.port)),
        )
        .await
        .map_err(|_| "连接超时。请允许 CrossCopy 通过 Windows 防火墙的“专用网络”".to_string())?
        .map_err(|e| format!("无法连接配对设备：{e}"))?;
        write_json(
            &mut stream,
            &WireMessage::PairRequest {
                id: settings.device_id.clone(),
                name: settings.device_name.clone(),
                proof: proof(&key, &settings.device_id, &seen.packet.id),
            },
        )
        .await?;
        let response: WireMessage =
            tokio::time::timeout(Duration::from_secs(5), read_json(&mut stream))
                .await
                .map_err(|_| "等待配对设备响应超时".to_string())??;
        let WireMessage::PairAccepted { id, name, envelope } = response else {
            return Err("验证码不正确".into());
        };
        let secret: String = decrypt(&key, &envelope)?;
        self.upsert_peer(Peer {
            id,
            name: name.clone(),
            secret,
            paired_at: now_ms(),
        })?;
        self.add_activity(
            "system",
            &format!("已连接 {name}"),
            "配对完成，可以开始复制",
            "done",
        );
        self.logger.info("pairing_tcp_succeeded", "peer_saved=true");
        Ok(())
    }

    async fn try_pair_udp(&self, code: &str) -> Result<(), String> {
        self.logger
            .info("pairing_hotspot_fallback_start", "timeout_seconds=6");
        let socket = UdpSocket::bind(("0.0.0.0", 0))
            .await
            .map_err(|e| e.to_string())?;
        socket.set_broadcast(true).map_err(|e| e.to_string())?;
        socket.set_multicast_ttl_v4(1).map_err(|e| e.to_string())?;
        let settings = self.store.get();
        let request = UdpPairMessage::PairRequest {
            app: "crosscopy".into(),
            protocol: 1,
            id: settings.device_id,
            name: settings.device_name,
            code_hash: fingerprint(code),
        };
        let bytes = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        socket
            .send_to(
                &bytes,
                SocketAddr::new(IpAddr::V4(MULTICAST), DISCOVERY_PORT),
            )
            .await
            .map_err(|e| e.to_string())?;
        let _ = socket
            .send_to(
                &bytes,
                SocketAddr::new(IpAddr::V4(GLOBAL_BROADCAST), DISCOVERY_PORT),
            )
            .await;

        let mut response_buffer = [0_u8; 4096];
        let (size, source) = tokio::time::timeout(
            Duration::from_secs(6),
            socket.recv_from(&mut response_buffer),
        )
        .await
        .map_err(|_| "未收到验证码显示方的响应，请检查两台电脑是否在同一热点".to_string())?
        .map_err(|e| e.to_string())?;
        let response: UdpPairMessage =
            serde_json::from_slice(&response_buffer[..size]).map_err(|e| e.to_string())?;
        let UdpPairMessage::PairAccepted {
            app,
            protocol,
            id,
            name,
            salt,
            envelope,
        } = response
        else {
            return Err("收到的热点配对响应无效".into());
        };
        if app != "crosscopy" || protocol != 1 {
            return Err("热点配对协议不兼容".into());
        }
        let key = pairing_key(code, &salt)?;
        let secret: String = decrypt(&key, &envelope)?;
        self.upsert_peer(Peer {
            id,
            name: name.clone(),
            secret,
            paired_at: now_ms(),
        })?;
        self.add_activity(
            "system",
            &format!("已连接 {name}"),
            &format!("通过热点完成配对 ({})", source.ip()),
            "done",
        );
        self.logger.info(
            "pairing_hotspot_fallback_succeeded",
            format!("source={}", masked_ip(source.ip())),
        );
        Ok(())
    }

    async fn handle_udp_pair_request(
        &self,
        socket: &UdpSocket,
        source: SocketAddr,
        request: UdpPairMessage,
    ) -> Result<(), String> {
        let UdpPairMessage::PairRequest {
            app,
            protocol,
            id,
            name,
            code_hash,
        } = request
        else {
            return Ok(());
        };
        if app != "crosscopy" || protocol != 1 || id == self.store.get().device_id {
            return Ok(());
        }
        self.logger.info(
            "pairing_hotspot_request_received",
            format!("source={}", masked_ip(source.ip())),
        );
        let pairing_data = {
            let mut pairing = self.pairing.lock().expect("pairing lock");
            let Some(session) = pairing.as_mut() else {
                return Ok(());
            };
            let attempts = session.attempts.entry(source.ip()).or_default();
            *attempts += 1;
            if *attempts > 5
                || session.expires_at <= now_ms()
                || fingerprint(&session.code) != code_hash
            {
                return Ok(());
            }
            Some((session.code.clone(), session.salt.clone()))
        };
        let Some((code, salt)) = pairing_data else {
            return Ok(());
        };
        let key = pairing_key(&code, &salt)?;
        let secret = random_secret();
        let settings = self.store.get();
        let response = UdpPairMessage::PairAccepted {
            app: "crosscopy".into(),
            protocol: 1,
            id: settings.device_id,
            name: settings.device_name,
            salt,
            envelope: encrypt(&key, &secret)?,
        };
        self.upsert_peer(Peer {
            id,
            name: name.clone(),
            secret,
            paired_at: now_ms(),
        })?;
        socket
            .send_to(
                &serde_json::to_vec(&response).map_err(|e| e.to_string())?,
                source,
            )
            .await
            .map_err(|e| e.to_string())?;
        *self.pairing.lock().expect("pairing lock") = None;
        self.add_activity(
            "system",
            &format!("已连接 {name}"),
            "已通过热点完成配对",
            "done",
        );
        self.logger.info(
            "pairing_hotspot_response_sent",
            format!("target={}", masked_ip(source.ip())),
        );
        Ok(())
    }

    async fn handle_connection(
        self: Arc<Self>,
        mut stream: TcpStream,
        source: SocketAddr,
    ) -> Result<(), String> {
        self.logger.info(
            "tcp_connection_received",
            format!("source={}", masked_ip(source.ip())),
        );
        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        let message: WireMessage = read_json(&mut stream).await?;
        match message {
            WireMessage::PairRequest {
                id,
                name,
                proof: received,
            } => {
                self.handle_pair_request(&mut stream, source.ip(), id, name, received)
                    .await
            }
            WireMessage::Secure {
                sender_id,
                envelope,
            } => {
                let peer = self
                    .store
                    .get()
                    .peers
                    .into_iter()
                    .find(|peer| peer.id == sender_id)
                    .ok_or("设备未配对")?;
                self.discovered.lock().expect("discovery lock").insert(
                    peer.id.clone(),
                    SeenPeer {
                        packet: DiscoveryPacket {
                            app: "crosscopy".into(),
                            protocol: 1,
                            id: peer.id.clone(),
                            name: peer.name.clone(),
                            port: TRANSFER_PORT,
                            pairing_salt: None,
                            pairing_expires_at: None,
                        },
                        host: source.ip(),
                        last_seen: now_ms(),
                    },
                );
                let key = decode_secret(&peer.secret)?;
                let secure: SecureMessage = decrypt(&key, &envelope)?;
                match secure {
                    SecureMessage::Clipboard { payload } => {
                        self.handle_remote_clipboard(peer, payload).await
                    }
                    SecureMessage::Pull { transfer_id } => {
                        let result = self.send_transfer(&mut stream, &key, &transfer_id).await;
                        if let Err(error) = &result {
                            self.fail_transfer_progress(error);
                        }
                        result
                    }
                    _ => Err("无效请求".into()),
                }
            }
            _ => Err("无效请求".into()),
        }
    }

    async fn handle_pair_request(
        &self,
        stream: &mut TcpStream,
        source: IpAddr,
        id: String,
        name: String,
        received: String,
    ) -> Result<(), String> {
        let pairing_data = {
            let settings = self.store.get();
            let mut pairing = self.pairing.lock().expect("pairing lock");
            if let Some(session) = pairing.as_mut() {
                let attempts = session.attempts.entry(source).or_default();
                *attempts += 1;
                if *attempts > 5 || session.expires_at <= now_ms() {
                    return Err("配对尝试次数过多".into());
                }
                Some((
                    pairing_key(&session.code, &session.salt)?,
                    settings.device_id,
                ))
            } else {
                None
            }
        };
        let Some((key, target_id)) = pairing_data else {
            write_json(stream, &WireMessage::PairRejected).await?;
            return Ok(());
        };
        if proof(&key, &id, &target_id) != received {
            write_json(stream, &WireMessage::PairRejected).await?;
            return Ok(());
        }
        let secret = random_secret();
        let settings = self.store.get();
        write_json(
            stream,
            &WireMessage::PairAccepted {
                id: settings.device_id.clone(),
                name: settings.device_name.clone(),
                envelope: encrypt(&key, &secret)?,
            },
        )
        .await?;
        self.upsert_peer(Peer {
            id,
            name: name.clone(),
            secret,
            paired_at: now_ms(),
        })?;
        *self.pairing.lock().expect("pairing lock") = None;
        self.add_activity(
            "system",
            &format!("已连接 {name}"),
            "配对完成，可以开始复制",
            "done",
        );
        Ok(())
    }

    async fn handle_remote_clipboard(
        self: &Arc<Self>,
        peer: Peer,
        payload: ClipboardPayload,
    ) -> Result<(), String> {
        if !self.store.get().sync_enabled {
            return Ok(());
        }
        match payload {
            ClipboardPayload::Text { text, .. } => {
                self.logger.info(
                    "clipboard_text_received",
                    format!("characters={}", text.chars().count()),
                );
                *self
                    .pending_clipboard
                    .lock()
                    .expect("pending clipboard lock") = Some(PendingClipboard::Text(text.clone()));
                self.add_activity(
                    "received",
                    &ellipsize(&text, 42),
                    &format!("来自 {}，等待快捷键粘贴", peer.name),
                    "done",
                );
                Ok(())
            }
            ClipboardPayload::Files {
                transfer_id,
                names,
                bytes,
                ..
            } => {
                self.logger.info(
                    "clipboard_files_announced",
                    format!("items={} bytes={bytes}", names.len()),
                );
                let label = summary_names(&names);
                self.add_activity(
                    "received",
                    &label,
                    &format!("正在从 {} 接收", peer.name),
                    "working",
                );
                let result = self.pull_transfer(peer, transfer_id, label, bytes).await;
                if let Err(error) = &result {
                    self.fail_transfer_progress(error);
                    self.add_activity("system", "文件接收失败", error, "error");
                }
                result
            }
        }
    }

    async fn pull_transfer(
        &self,
        peer: Peer,
        transfer_id: String,
        label: String,
        _bytes: u64,
    ) -> Result<(), String> {
        self.logger.info(
            "file_transfer_start",
            format!("bytes={_bytes} peer_online_lookup=true"),
        );
        self.begin_transfer_progress(&transfer_id, &label, "received", _bytes);
        let seen = self
            .discovered
            .lock()
            .expect("discovery lock")
            .get(&peer.id)
            .cloned()
            .ok_or("发送设备已经离线")?;
        let key = decode_secret(&peer.secret)?;
        let mut stream = TcpStream::connect((seen.host, seen.packet.port))
            .await
            .map_err(|e| e.to_string())?;
        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        write_json(
            &mut stream,
            &WireMessage::Secure {
                sender_id: self.store.get().device_id,
                envelope: encrypt(&key, &SecureMessage::Pull { transfer_id })?,
            },
        )
        .await?;
        let manifest_bytes = read_secure_frame(&mut stream, &key).await?;
        let manifest: SecureMessage =
            serde_json::from_slice(&manifest_bytes).map_err(|e| e.to_string())?;
        let SecureMessage::Manifest { entries } = manifest else {
            return Err("文件清单无效".into());
        };
        let destination = unique_destination().await?;
        fs::create_dir_all(&destination)
            .await
            .map_err(|e| e.to_string())?;
        let mut top_level = Vec::new();
        let mut transferred = 0_u64;
        for entry in entries {
            let relative = safe_relative_path(&entry.path)?;
            let target = destination.join(&relative);
            if let Some(first) = relative.components().next() {
                let root = destination.join(first.as_os_str());
                if !top_level.contains(&root) {
                    top_level.push(root);
                }
            }
            if entry.directory {
                fs::create_dir_all(&target)
                    .await
                    .map_err(|e| e.to_string())?;
                continue;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            let mut file = File::create(&target).await.map_err(|e| e.to_string())?;
            let mut remaining = entry.size;
            while remaining > 0 {
                let chunk = read_secure_frame(&mut stream, &key).await?;
                if chunk.is_empty() || chunk.len() as u64 > remaining {
                    return Err("文件数据损坏".into());
                }
                file.write_all(&chunk).await.map_err(|e| e.to_string())?;
                remaining -= chunk.len() as u64;
                transferred += chunk.len() as u64;
                self.update_transfer_progress(transferred);
            }
            file.flush().await.map_err(|e| e.to_string())?;
        }
        let clipboard_files: Vec<String> = top_level
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        *self
            .pending_clipboard
            .lock()
            .expect("pending clipboard lock") = Some(PendingClipboard::Files(clipboard_files));
        self.add_activity(
            "received",
            &label,
            &format!("来自 {}，等待快捷键粘贴", peer.name),
            "done",
        );
        self.logger.info(
            "file_transfer_completed",
            format!("bytes={_bytes} output_items={}", top_level.len()),
        );
        self.complete_transfer_progress();
        Ok(())
    }

    async fn send_transfer(
        &self,
        stream: &mut TcpStream,
        key: &[u8],
        transfer_id: &str,
    ) -> Result<(), String> {
        let transfer = self
            .transfers
            .lock()
            .expect("transfer lock")
            .get(transfer_id)
            .cloned()
            .filter(|value| value.expires_at > now_ms())
            .ok_or("传输已过期")?;
        let entries = build_manifest(&transfer.roots)?;
        let total = entries.iter().map(|entry| entry.size).sum();
        let label = summary_names(
            &transfer
                .roots
                .iter()
                .map(|path| {
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect::<Vec<_>>(),
        );
        self.begin_transfer_progress(transfer_id, &label, "sent", total);
        write_secure_frame(
            stream,
            key,
            &serde_json::to_vec(&SecureMessage::Manifest {
                entries: entries.clone(),
            })
            .map_err(|e| e.to_string())?,
        )
        .await?;
        let root_map = root_map(&transfer.roots);
        let mut buffer = vec![0_u8; CHUNK_SIZE];
        let mut transferred = 0_u64;
        for entry in entries.into_iter().filter(|entry| !entry.directory) {
            let source = resolve_manifest_source(&entry.path, &root_map)?;
            let mut file = File::open(source).await.map_err(|e| e.to_string())?;
            loop {
                let size = file.read(&mut buffer).await.map_err(|e| e.to_string())?;
                if size == 0 {
                    break;
                }
                write_secure_frame(stream, key, &buffer[..size]).await?;
                transferred += size as u64;
                self.update_transfer_progress(transferred);
            }
        }
        self.complete_transfer_progress();
        Ok(())
    }

    fn upsert_peer(&self, peer: Peer) -> Result<(), String> {
        self.store
            .update(|settings| {
                settings.peers.retain(|value| value.id != peer.id);
                settings.peers.push(peer);
            })
            .map_err(|e| e.to_string())?;
        self.publish();
        Ok(())
    }

    fn add_activity(&self, direction: &str, label: &str, detail: &str, status: &str) {
        let mut activities = self.activities.lock().expect("activity lock");
        activities.insert(
            0,
            Activity {
                id: Uuid::new_v4().to_string(),
                direction: direction.into(),
                label: label.into(),
                detail: detail.into(),
                created_at: now_ms(),
                status: status.into(),
            },
        );
        activities.truncate(20);
        drop(activities);
        self.publish();
    }

    fn begin_transfer_progress(&self, id: &str, label: &str, direction: &str, total: u64) {
        *self
            .transfer_progress
            .lock()
            .expect("transfer progress lock") = Some(TransferProgress {
            id: id.into(),
            label: label.into(),
            direction: direction.into(),
            transferred: 0,
            total,
            status: "working".into(),
        });
        if let Some(window) = self.app.get_webview_window("transfer") {
            let _ = window.show();
        } else {
            let _ = WebviewWindowBuilder::new(
                &self.app,
                "transfer",
                WebviewUrl::App("index.html?transfer=1".into()),
            )
            .title("CrossCopy 传输")
            .inner_size(420.0, 132.0)
            .resizable(false)
            .always_on_top(true)
            .center()
            .build();
        }
        self.publish();
    }

    fn update_transfer_progress(&self, transferred: u64) {
        let mut completed = false;
        if let Some(progress) = self
            .transfer_progress
            .lock()
            .expect("transfer progress lock")
            .as_mut()
        {
            progress.transferred = transferred.min(progress.total);
            completed = progress.transferred >= progress.total;
        }
        let now = now_ms();
        let last = self.last_progress_emit.load(Ordering::Relaxed);
        if completed || now.saturating_sub(last) >= 100 {
            self.last_progress_emit.store(now, Ordering::Relaxed);
            self.publish();
        }
    }

    fn complete_transfer_progress(&self) {
        if let Some(progress) = self
            .transfer_progress
            .lock()
            .expect("transfer progress lock")
            .as_mut()
        {
            progress.transferred = progress.total;
            progress.status = "done".into();
        }
        self.publish();
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1_500)).await;
            if let Some(window) = app.get_webview_window("transfer") {
                let _ = window.close();
            }
        });
    }

    fn fail_transfer_progress(&self, error: &str) {
        if let Some(progress) = self
            .transfer_progress
            .lock()
            .expect("transfer progress lock")
            .as_mut()
        {
            progress.status = "error".into();
        }
        self.logger.error("file_transfer_failed", error);
        self.publish();
    }
}

async fn read_current_clipboard(logger: &Logger) -> Result<LocalClipboard, String> {
    let mut last_error = String::new();
    for attempt in 1..=CLIPBOARD_RETRY_ATTEMPTS {
        let result = (|| {
            let context = ClipboardContext::new().map_err(|error| error.to_string())?;
            if context.has(ContentFormat::Files) {
                let files = context.get_files().map_err(|error| error.to_string())?;
                if files.is_empty() {
                    return Err("文件列表为空".into());
                }
                return Ok(LocalClipboard::Files(
                    files.into_iter().map(PathBuf::from).collect(),
                ));
            }
            if context.has(ContentFormat::Text) {
                let text = context.get_text().map_err(|error| error.to_string())?;
                if text.is_empty() {
                    return Err("文字内容为空".into());
                }
                return Ok(LocalClipboard::Text(text));
            }
            Err("当前内容不是受支持的文字、文件或文件夹".into())
        })();
        match result {
            Ok(event) => {
                logger.info(
                    "clipboard_shortcut_read",
                    format!(
                        "kind={} attempt={attempt}",
                        match &event {
                            LocalClipboard::Text(_) => "text",
                            LocalClipboard::Files(_) => "files",
                        }
                    ),
                );
                return Ok(event);
            }
            Err(error) => last_error = error,
        }
        if attempt < CLIPBOARD_RETRY_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(CLIPBOARD_RETRY_DELAY_MS)).await;
        }
    }
    logger.warn(
        "clipboard_shortcut_read_failed",
        format!("attempts={} error={last_error}", CLIPBOARD_RETRY_ATTEMPTS),
    );
    Err(last_error)
}

async fn capture_clipboard(logger: &Logger) -> Result<ClipboardSnapshot, String> {
    let mut last_error = String::new();
    for attempt in 1..=CLIPBOARD_RETRY_ATTEMPTS {
        let result = (|| {
            let context = ClipboardContext::new().map_err(|error| error.to_string())?;
            let mut formats = vec![
                ContentFormat::Image,
                ContentFormat::Files,
                ContentFormat::Text,
                ContentFormat::Rtf,
                ContentFormat::Html,
            ];
            let available = context
                .available_formats()
                .map_err(|error| error.to_string())?;
            #[cfg(target_os = "macos")]
            for format in &available {
                if format != "unknown format" {
                    formats.push(ContentFormat::Other(format.clone()));
                }
            }
            let contents = context.get(&formats).map_err(|error| error.to_string())?;
            if !contents.is_empty() {
                return Ok(ClipboardSnapshot::Contents(contents));
            }
            if available.is_empty() {
                Ok(ClipboardSnapshot::Empty)
            } else {
                Err("本机剪贴板暂时被其他程序占用".into())
            }
        })();
        match result {
            Ok(snapshot) => {
                logger.info("clipboard_snapshot_captured", format!("attempt={attempt}"));
                return Ok(snapshot);
            }
            Err(error) => last_error = error,
        }
        if attempt < CLIPBOARD_RETRY_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(CLIPBOARD_RETRY_DELAY_MS)).await;
        }
    }
    logger.warn(
        "clipboard_snapshot_failed",
        format!("attempts={} error={last_error}", CLIPBOARD_RETRY_ATTEMPTS),
    );
    Err(format!("保护本机剪贴板失败：{last_error}"))
}

async fn restore_clipboard(snapshot: ClipboardSnapshot, logger: &Logger) -> Result<(), String> {
    match snapshot {
        ClipboardSnapshot::Contents(contents) => {
            let context = ClipboardContext::new().map_err(|error| error.to_string())?;
            context.set(contents).map_err(|error| error.to_string())?;
            logger.info("clipboard_snapshot_restored", "verified=true");
            Ok(())
        }
        ClipboardSnapshot::Empty => ClipboardContext::new()
            .map_err(|error| error.to_string())?
            .clear()
            .map_err(|error| error.to_string()),
    }
}

async fn write_clipboard_text(text: &str, logger: &Logger) -> Result<(), String> {
    let mut last_error = String::new();
    for attempt in 1..=CLIPBOARD_RETRY_ATTEMPTS {
        let result = (|| {
            let context = ClipboardContext::new().map_err(|error| error.to_string())?;
            context
                .set_text(text.to_owned())
                .map_err(|error| error.to_string())?;
            let actual = context.get_text().map_err(|error| error.to_string())?;
            if actual != text {
                return Err("clipboard verification mismatch".into());
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                logger.info(
                    "clipboard_text_written",
                    format!("verified=true attempt={attempt}"),
                );
                return Ok(());
            }
            Err(error) => last_error = error,
        }
        if attempt < CLIPBOARD_RETRY_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(CLIPBOARD_RETRY_DELAY_MS)).await;
        }
    }
    logger.error(
        "clipboard_text_write_failed",
        format!("attempts={} error={last_error}", CLIPBOARD_RETRY_ATTEMPTS),
    );
    Err(format!("写入系统文字剪贴板失败：{last_error}"))
}

async fn write_clipboard_files(files: &[String], logger: &Logger) -> Result<(), String> {
    let mut last_error = String::new();
    for attempt in 1..=CLIPBOARD_RETRY_ATTEMPTS {
        let result = (|| {
            let context = ClipboardContext::new().map_err(|error| error.to_string())?;
            context
                .set_files(files.to_vec())
                .map_err(|error| error.to_string())?;
            let actual = context.get_files().map_err(|error| error.to_string())?;
            if actual.len() != files.len()
                || !files
                    .iter()
                    .all(|expected| actual.iter().any(|value| value == expected))
            {
                return Err("clipboard verification mismatch".into());
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                logger.info(
                    "clipboard_files_written",
                    format!("verified=true items={} attempt={attempt}", files.len()),
                );
                return Ok(());
            }
            Err(error) => last_error = error,
        }
        if attempt < CLIPBOARD_RETRY_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(CLIPBOARD_RETRY_DELAY_MS)).await;
        }
    }
    logger.error(
        "clipboard_files_write_failed",
        format!("attempts={} error={last_error}", CLIPBOARD_RETRY_ATTEMPTS),
    );
    Err(format!("写入系统文件剪贴板失败：{last_error}"))
}

fn native_input_settings() -> EnigoSettings {
    let mut settings = EnigoSettings::default();
    // Permission prompts are an explicit, one-time product action. Asking from
    // every shortcut invocation makes macOS display the same dialog forever.
    settings.open_prompt_to_get_permissions = false;
    settings.event_source_user_data = Some(SYNTHETIC_INPUT_MARKER as i64);
    settings.windows_dw_extra_info = Some(SYNTHETIC_INPUT_MARKER);
    settings
}

fn simulate_native_shortcut(key: char) -> Result<(), String> {
    let mut enigo = match Enigo::new(&native_input_settings()) {
        Ok(enigo) => enigo,
        Err(error) => {
            #[cfg(target_os = "macos")]
            {
                return Err(format!(
                    "CrossCopy 当前版本未获得辅助功能权限。请在“系统设置 → 隐私与安全性 → 辅助功能”中允许 CrossCopy；如果已经开启，请关闭后重新开启一次：{error}"
                ));
            }
            #[cfg(not(target_os = "macos"))]
            {
                return Err(format!("无法控制当前应用的键盘输入：{error}"));
            }
        }
    };
    #[cfg(target_os = "macos")]
    let modifier = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let modifier = Key::Control;

    enigo
        .key(modifier, Press)
        .map_err(|error| error.to_string())?;
    let click_result = enigo
        .key(Key::Unicode(key), Click)
        .map_err(|error| error.to_string());
    let release_result = enigo
        .key(modifier, Release)
        .map_err(|error| error.to_string());
    click_result?;
    release_result
}

fn discovery_broadcast_targets() -> Vec<SocketAddr> {
    let mut targets = vec![SocketAddr::new(
        IpAddr::V4(GLOBAL_BROADCAST),
        DISCOVERY_PORT,
    )];
    if let Ok(interfaces) = get_if_addrs::get_if_addrs() {
        for interface in interfaces {
            if let get_if_addrs::IfAddr::V4(address) = interface.addr {
                if address.ip.is_loopback() {
                    continue;
                }
                if let Some(broadcast) = address.broadcast {
                    let target = SocketAddr::new(IpAddr::V4(broadcast), DISCOVERY_PORT);
                    if !targets.contains(&target) {
                        targets.push(target);
                    }
                }
            }
        }
    }
    targets
}

async fn write_json<T: Serialize>(stream: &mut TcpStream, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("消息过大".into());
    }
    stream
        .write_u32(bytes.len() as u32)
        .await
        .map_err(|e| e.to_string())?;
    stream.write_all(&bytes).await.map_err(|e| e.to_string())
}

async fn read_json<T: DeserializeOwned>(stream: &mut TcpStream) -> Result<T, String> {
    let size = stream.read_u32().await.map_err(|e| e.to_string())? as usize;
    if size > 2 * 1024 * 1024 {
        return Err("消息过大".into());
    }
    let mut bytes = vec![0_u8; size];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

async fn write_secure_frame(
    stream: &mut TcpStream,
    key: &[u8],
    plain: &[u8],
) -> Result<(), String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| e.to_string())?;
    let mut nonce = [0_u8; 12];
    rand::rng().fill_bytes(&mut nonce);
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce), plain)
        .map_err(|e| e.to_string())?;
    stream
        .write_u32((nonce.len() + encrypted.len()) as u32)
        .await
        .map_err(|e| e.to_string())?;
    stream.write_all(&nonce).await.map_err(|e| e.to_string())?;
    stream
        .write_all(&encrypted)
        .await
        .map_err(|e| e.to_string())
}

async fn read_secure_frame(stream: &mut TcpStream, key: &[u8]) -> Result<Vec<u8>, String> {
    let size = stream.read_u32().await.map_err(|e| e.to_string())? as usize;
    if !(28..=CHUNK_SIZE + 64 * 1024).contains(&size) {
        return Err("数据帧大小无效".into());
    }
    let mut frame = vec![0_u8; size];
    stream
        .read_exact(&mut frame)
        .await
        .map_err(|e| e.to_string())?;
    Aes256Gcm::new_from_slice(key)
        .map_err(|e| e.to_string())?
        .decrypt(Nonce::from_slice(&frame[..12]), &frame[12..])
        .map_err(|e| e.to_string())
}

fn build_manifest(roots: &[PathBuf]) -> Result<Vec<FileEntry>, String> {
    let mut entries = Vec::new();
    for root in roots {
        let name = root
            .file_name()
            .ok_or("文件路径无效")?
            .to_string_lossy()
            .into_owned();
        if root.is_dir() {
            for item in WalkDir::new(root).follow_links(false) {
                let item = item.map_err(|e| e.to_string())?;
                let suffix = item.path().strip_prefix(root).map_err(|e| e.to_string())?;
                let relative = if suffix.as_os_str().is_empty() {
                    PathBuf::from(&name)
                } else {
                    PathBuf::from(&name).join(suffix)
                };
                let metadata = item.metadata().map_err(|e| e.to_string())?;
                entries.push(FileEntry {
                    path: relative.to_string_lossy().replace('\\', "/"),
                    size: if metadata.is_file() {
                        metadata.len()
                    } else {
                        0
                    },
                    directory: metadata.is_dir(),
                });
            }
        } else {
            entries.push(FileEntry {
                path: name,
                size: root.metadata().map_err(|e| e.to_string())?.len(),
                directory: false,
            });
        }
    }
    Ok(entries)
}

fn root_map(roots: &[PathBuf]) -> HashMap<String, PathBuf> {
    roots
        .iter()
        .filter_map(|path| {
            path.file_name()
                .map(|name| (name.to_string_lossy().into_owned(), path.clone()))
        })
        .collect()
}

fn resolve_manifest_source(
    path: &str,
    roots: &HashMap<String, PathBuf>,
) -> Result<PathBuf, String> {
    let relative = safe_relative_path(path)?;
    let mut components = relative.components();
    let root_name = components
        .next()
        .ok_or("文件路径无效")?
        .as_os_str()
        .to_string_lossy();
    let mut source = roots
        .get(root_name.as_ref())
        .cloned()
        .ok_or("文件路径无效")?;
    for component in components {
        source.push(component.as_os_str());
    }
    Ok(source)
}

fn safe_relative_path(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("文件路径不安全".into());
    }
    Ok(path.to_path_buf())
}

fn scan_roots(paths: &[PathBuf]) -> Result<(u64, Vec<String>), String> {
    let mut bytes = 0;
    let mut names = Vec::new();
    for path in paths {
        names.push(
            path.file_name()
                .ok_or("文件路径无效")?
                .to_string_lossy()
                .into_owned(),
        );
        if path.is_dir() {
            for item in WalkDir::new(path).follow_links(false) {
                let item = item.map_err(|e| e.to_string())?;
                if item.file_type().is_file() {
                    bytes += item.metadata().map_err(|e| e.to_string())?.len();
                }
            }
        } else {
            bytes += path.metadata().map_err(|e| e.to_string())?.len();
        }
    }
    Ok((bytes, names))
}

async fn unique_destination() -> Result<PathBuf, String> {
    let root = dirs::download_dir()
        .ok_or("无法找到下载目录")?
        .join("CrossCopy");
    let destination = root.join(format!("{}", now_ms()));
    fs::create_dir_all(&destination)
        .await
        .map_err(|e| e.to_string())?;
    Ok(destination)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn ellipsize(value: &str, max: usize) -> String {
    let mut chars = value.chars();
    let prefix: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn summary_names(names: &[String]) -> String {
    match names {
        [] => "文件".into(),
        [only] => only.clone(),
        many => format!("{} 等 {} 项", many[0], many.len()),
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    const GB: f64 = 1024.0 * MB;
    let value = bytes as f64;
    if value < KB {
        format!("{bytes} B")
    } else if value < MB {
        format!("{:.1} KB", value / KB)
    } else if value < GB {
        format!("{:.1} MB", value / MB)
    } else {
        format!("{:.1} GB", value / GB)
    }
}

fn reveal_file(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn rejects_traversal_paths() {
        assert!(safe_relative_path("../secret").is_err());
        assert!(safe_relative_path("/absolute").is_err());
        assert!(safe_relative_path("folder/file.txt").is_ok());
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn secure_frame_payload_limit_has_headroom_for_manifest() {
        assert!(CHUNK_SIZE > 512 * 1024);
    }

    #[test]
    fn clipboard_operation_guard_prevents_reentrant_shortcuts() {
        let active = AtomicBool::new(false);
        let first = ClipboardOperationGuard::acquire(&active).expect("first operation");
        assert!(ClipboardOperationGuard::acquire(&active).is_none());
        drop(first);
        assert!(ClipboardOperationGuard::acquire(&active).is_some());
    }

    #[test]
    fn native_input_never_prompts_from_a_shortcut() {
        let settings = native_input_settings();
        assert!(!settings.open_prompt_to_get_permissions);
        assert_eq!(
            settings.event_source_user_data,
            Some(SYNTHETIC_INPUT_MARKER as i64)
        );
        assert_eq!(settings.windows_dw_extra_info, Some(SYNTHETIC_INPUT_MARKER));
    }
}
