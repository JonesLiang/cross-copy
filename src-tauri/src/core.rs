use crate::{
    crypto::{decode_secret, decrypt, encrypt, fingerprint, pairing_key, proof, random_secret, Envelope},
    model::{Activity, ClipboardPayload, DiscoveryPacket, Peer, PeerView, UiState},
    store::Store,
};
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use clipboard_rs::{
    Clipboard, ClipboardContext, ClipboardHandler, ClipboardWatcher, ClipboardWatcherContext,
};
use rand::RngCore;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter};
use tokio::{
    fs::{self, File},
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::mpsc,
};
use uuid::Uuid;
use walkdir::WalkDir;

const DISCOVERY_PORT: u16 = 47653;
const TRANSFER_PORT: u16 = 47654;
const MULTICAST: Ipv4Addr = Ipv4Addr::new(239, 255, 67, 89);
const GLOBAL_BROADCAST: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 255);
const CHUNK_SIZE: usize = 1024 * 1024;
const ONLINE_WINDOW_MS: u64 = 8_000;

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

pub struct Core {
    pub store: Arc<Store>,
    app: AppHandle,
    discovered: Mutex<HashMap<String, SeenPeer>>,
    pairing: Mutex<Option<PairSession>>,
    transfers: Mutex<HashMap<String, Transfer>>,
    activities: Mutex<Vec<Activity>>,
    suppress_until: AtomicU64,
    port: AtomicU64,
}

impl Core {
    pub fn new(store: Arc<Store>, app: AppHandle) -> Arc<Self> {
        Arc::new(Self {
            store,
            app,
            discovered: Mutex::new(HashMap::new()),
            pairing: Mutex::new(None),
            transfers: Mutex::new(HashMap::new()),
            activities: Mutex::new(Vec::new()),
            suppress_until: AtomicU64::new(0),
            port: AtomicU64::new(0),
        })
    }

    pub async fn start(self: &Arc<Self>) -> Result<(), String> {
        let listener = TcpListener::bind(("0.0.0.0", TRANSFER_PORT))
            .await
            .map_err(|e| format!("无法监听局域网端口 {TRANSFER_PORT}：{e}"))?;
        self.port.store(
            listener.local_addr().map_err(|e| e.to_string())?.port() as u64,
            Ordering::Relaxed,
        );
        let server_core = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, address)) => {
                        let core = Arc::clone(&server_core);
                        tauri::async_runtime::spawn(async move {
                            let _ = core.handle_connection(stream, address).await;
                        });
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
                }
            }
        });
        self.start_discovery().await?;
        self.start_clipboard_watcher()?;
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
                        online: seen
                            .is_some_and(|value| now.saturating_sub(value.last_seen) < ONLINE_WINDOW_MS),
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
        let code = format!("{:06}", rand::random_range(0..1_000_000));
        let mut salt = [0_u8; 16];
        rand::rng().fill_bytes(&mut salt);
        *self.pairing.lock().expect("pairing lock") = Some(PairSession {
            code,
            salt: URL_SAFE_NO_PAD.encode(salt),
            expires_at: now_ms() + 120_000,
            attempts: HashMap::new(),
        });
        self.publish();
    }

    pub fn cancel_pairing(&self) {
        *self.pairing.lock().expect("pairing lock") = None;
        self.publish();
    }

    pub async fn pair_with_code(self: &Arc<Self>, code: String) -> Result<(), String> {
        if code.len() != 6 || !code.chars().all(|value| value.is_ascii_digit()) {
            return Err("请输入 6 位数字验证码".into());
        }
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
        let mut last_error = None;
        for seen in candidates {
            match self.try_pair(&seen, &code).await {
                Ok(()) => return Ok(()),
                Err(error) => last_error = Some(error),
            }
        }
        self.try_pair_udp(&code).await.map_err(|udp_error| {
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
        Ok(())
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
        Ok(())
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
        let socket = Arc::new(socket);

        let receive_core = Arc::clone(self);
        let receive_socket = Arc::clone(&socket);
        tauri::async_runtime::spawn(async move {
            let mut buffer = [0_u8; 4096];
            loop {
                let Ok((size, source)) = receive_socket.recv_from(&mut buffer).await else {
                    continue;
                };
                if let Ok(packet) =
                    serde_json::from_slice::<DiscoveryPacket>(&buffer[..size])
                {
                    if packet.app != "crosscopy"
                        || packet.protocol != 1
                        || packet.id == receive_core.store.get().device_id
                    {
                        continue;
                    }
                    receive_core.discovered.lock().expect("discovery lock").insert(
                        packet.id.clone(),
                        SeenPeer {
                            packet,
                            host: source.ip(),
                            last_seen: now_ms(),
                        },
                    );
                    receive_core.publish();
                    continue;
                }
                if let Ok(request) =
                    serde_json::from_slice::<UdpPairMessage>(&buffer[..size])
                {
                    let core = Arc::clone(&receive_core);
                    let socket = Arc::clone(&receive_socket);
                    tauri::async_runtime::spawn(async move {
                        let _ = core
                            .handle_udp_pair_request(&socket, source, request)
                            .await;
                    });
                }
            }
        });

        let beacon_core = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let multicast_target =
                SocketAddr::new(IpAddr::V4(MULTICAST), DISCOVERY_PORT);
            let broadcast_target =
                SocketAddr::new(IpAddr::V4(GLOBAL_BROADCAST), DISCOVERY_PORT);
            loop {
                let packet = beacon_core.discovery_packet();
                if let Ok(bytes) = serde_json::to_vec(&packet) {
                    let _ = socket.send_to(&bytes, multicast_target).await;
                    let _ = socket.send_to(&bytes, broadcast_target).await;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
        Ok(())
    }

    fn discovery_packet(&self) -> DiscoveryPacket {
        let settings = self.store.get();
        let mut pairing = self.pairing.lock().expect("pairing lock");
        if pairing.as_ref().is_some_and(|value| value.expires_at <= now_ms()) {
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

    fn start_clipboard_watcher(self: &Arc<Self>) -> Result<(), String> {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("crosscopy-clipboard".into())
            .spawn(move || {
                let Ok(context) = ClipboardContext::new() else {
                    return;
                };
                let handler = ClipboardChangeHandler { context, sender };
                let Ok(mut watcher) = ClipboardWatcherContext::new() else {
                    return;
                };
                watcher.add_handler(handler);
                watcher.start_watch();
            })
            .map_err(|e| e.to_string())?;

        let core = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            while let Some(event) = receiver.recv().await {
                if now_ms() < core.suppress_until.load(Ordering::Relaxed)
                    || !core.store.get().sync_enabled
                {
                    continue;
                }
                if let Err(error) = core.handle_local_clipboard(event).await {
                    core.add_activity("system", "发送失败", &error, "error");
                }
            }
        });
        Ok(())
    }

    async fn handle_local_clipboard(self: &Arc<Self>, event: LocalClipboard) -> Result<(), String> {
        let payload = match event {
            LocalClipboard::Text(text) => {
                if text.is_empty() {
                    return Ok(());
                }
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
        self.broadcast(&payload).await;
        match &payload {
            ClipboardPayload::Text { text, .. } => self.add_activity(
                "sent",
                &ellipsize(text, 42),
                &format!("{} 个字符", text.chars().count()),
                "done",
            ),
            ClipboardPayload::Files { names, bytes, .. } => self.add_activity(
                "sent",
                &summary_names(names),
                &format_bytes(*bytes),
                "done",
            ),
        }
        Ok(())
    }

    async fn broadcast(&self, payload: &ClipboardPayload) {
        let settings = self.store.get();
        let discovered = self.discovered.lock().expect("discovery lock").clone();
        for peer in settings.peers {
            let Some(seen) = discovered.get(&peer.id) else {
                continue;
            };
            if now_ms().saturating_sub(seen.last_seen) >= ONLINE_WINDOW_MS {
                continue;
            }
            let address = SocketAddr::new(seen.host, seen.packet.port);
            let key = match decode_secret(&peer.secret) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if let Ok(mut stream) = TcpStream::connect(address).await {
                let secure = SecureMessage::Clipboard {
                    payload: payload.clone(),
                };
                if let Ok(envelope) = encrypt(&key, &secure) {
                    let message = WireMessage::Secure {
                        sender_id: settings.device_id.clone(),
                        envelope,
                    };
                    let _ = write_json(&mut stream, &message).await;
                }
            }
        }
    }

    async fn try_pair(&self, seen: &SeenPeer, code: &str) -> Result<(), String> {
        let settings = self.store.get();
        let salt = seen
            .packet
            .pairing_salt
            .as_deref()
            .ok_or("配对信息无效")?;
        let key = pairing_key(code, salt)?;
        let mut stream = tokio::time::timeout(
            Duration::from_secs(5),
            TcpStream::connect((seen.host, seen.packet.port)),
        )
        .await
        .map_err(|_| {
            "连接超时。请允许 CrossCopy 通过 Windows 防火墙的“专用网络”"
                .to_string()
        })?
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
        let WireMessage::PairAccepted {
            id,
            name,
            envelope,
        } = response
        else {
            return Err("验证码不正确".into());
        };
        let secret: String = decrypt(&key, &envelope)?;
        self.upsert_peer(Peer {
            id,
            name: name.clone(),
            secret,
            paired_at: now_ms(),
        })?;
        self.add_activity("system", &format!("已连接 {name}"), "配对完成，可以开始复制", "done");
        Ok(())
    }

    async fn try_pair_udp(&self, code: &str) -> Result<(), String> {
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
        .map_err(|_| {
            "未收到验证码显示方的响应，请检查两台电脑是否在同一热点"
                .to_string()
        })?
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
        Ok(())
    }

    async fn handle_connection(
        self: Arc<Self>,
        mut stream: TcpStream,
        source: SocketAddr,
    ) -> Result<(), String> {
        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        let message: WireMessage = read_json(&mut stream).await?;
        match message {
            WireMessage::PairRequest {
                id,
                name,
                proof: received,
            } => self
                .handle_pair_request(&mut stream, source.ip(), id, name, received)
                .await,
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
                let key = decode_secret(&peer.secret)?;
                let secure: SecureMessage = decrypt(&key, &envelope)?;
                match secure {
                    SecureMessage::Clipboard { payload } => {
                        self.handle_remote_clipboard(peer, payload).await
                    }
                    SecureMessage::Pull { transfer_id } => {
                        self.send_transfer(&mut stream, &key, &transfer_id).await
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
        self.add_activity("system", &format!("已连接 {name}"), "配对完成，可以开始复制", "done");
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
                self.suppress_until.store(now_ms() + 1_500, Ordering::Relaxed);
                ClipboardContext::new()
                    .map_err(|e| e.to_string())?
                    .set_text(text.clone())
                    .map_err(|e| e.to_string())?;
                self.add_activity("received", &ellipsize(&text, 42), &format!("来自 {}", peer.name), "done");
                Ok(())
            }
            ClipboardPayload::Files {
                transfer_id,
                names,
                bytes,
                ..
            } => {
                let label = summary_names(&names);
                self.add_activity("received", &label, &format!("正在从 {} 接收", peer.name), "working");
                self.pull_transfer(peer, transfer_id, label, bytes).await
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
                fs::create_dir_all(&target).await.map_err(|e| e.to_string())?;
                continue;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
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
            }
            file.flush().await.map_err(|e| e.to_string())?;
        }
        self.suppress_until.store(now_ms() + 1_500, Ordering::Relaxed);
        ClipboardContext::new()
            .map_err(|e| e.to_string())?
            .set_files(
                top_level
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
            )
            .map_err(|e| e.to_string())?;
        self.add_activity("received", &label, &format!("来自 {}，已放入剪贴板", peer.name), "done");
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
        for entry in entries.into_iter().filter(|entry| !entry.directory) {
            let source = resolve_manifest_source(&entry.path, &root_map)?;
            let mut file = File::open(source).await.map_err(|e| e.to_string())?;
            loop {
                let size = file.read(&mut buffer).await.map_err(|e| e.to_string())?;
                if size == 0 {
                    break;
                }
                write_secure_frame(stream, key, &buffer[..size]).await?;
            }
        }
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
}

struct ClipboardChangeHandler {
    context: ClipboardContext,
    sender: mpsc::UnboundedSender<LocalClipboard>,
}

impl ClipboardHandler for ClipboardChangeHandler {
    fn on_clipboard_change(&mut self) {
        if let Ok(files) = self.context.get_files() {
            if !files.is_empty() {
                let _ = self
                    .sender
                    .send(LocalClipboard::Files(files.into_iter().map(PathBuf::from).collect()));
                return;
            }
        }
        if let Ok(text) = self.context.get_text() {
            if !text.is_empty() {
                let _ = self.sender.send(LocalClipboard::Text(text));
            }
        }
    }
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
    stream.read_exact(&mut bytes).await.map_err(|e| e.to_string())?;
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
                    size: if metadata.is_file() { metadata.len() } else { 0 },
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

fn resolve_manifest_source(path: &str, roots: &HashMap<String, PathBuf>) -> Result<PathBuf, String> {
    let relative = safe_relative_path(path)?;
    let mut components = relative.components();
    let root_name = components.next().ok_or("文件路径无效")?.as_os_str().to_string_lossy();
    let mut source = roots.get(root_name.as_ref()).cloned().ok_or("文件路径无效")?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_paths() {
        assert!(safe_relative_path("../secret").is_err());
        assert!(safe_relative_path("/absolute").is_err());
        assert!(safe_relative_path("folder/file.txt").is_ok());
    }

    #[test]
    fn secure_frame_payload_limit_has_headroom_for_manifest() {
        assert!(CHUNK_SIZE > 512 * 1024);
    }
}
