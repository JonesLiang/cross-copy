use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use serde::Serialize;
use std::{net::UdpSocket, time::{Duration, SystemTime, UNIX_EPOCH}};

const PEER_ID: &str = "ce792739-d889-47ef-90ee-847fe97a88b3";
const PEER_SECRET: &str = "IgfwbjqQ2aon__wp5evnSCay0yjHUq5RdIT7401QuX0";
const TARGET: &str = "192.168.137.102:47655";

#[derive(Serialize)]
struct Envelope {
    nonce: String,
    data: String,
}

#[derive(Serialize)]
struct Packet<'a> {
    app: &'a str,
    protocol: u8,
    senderId: &'a str,
    envelope: Envelope,
}

fn encrypt<T: Serialize>(key: &[u8], value: &T) -> Envelope {
    let cipher = Aes256Gcm::new_from_slice(key).unwrap();
    let mut nonce = [0_u8; 12];
    rand::rng().fill_bytes(&mut nonce);
    let plain = serde_json::to_vec(value).unwrap();
    let data = cipher
        .encrypt(Nonce::from_slice(&nonce), plain.as_ref())
        .unwrap();
    Envelope {
        nonce: URL_SAFE_NO_PAD.encode(nonce),
        data: URL_SAFE_NO_PAD.encode(data),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn send(socket: &UdpSocket, key: &[u8], signal: serde_json::Value) {
    let envelope = encrypt(key, &signal);
    let packet = Packet {
        app: "crosscopy",
        protocol: 6,
        senderId: PEER_ID,
        envelope,
    };
    let bytes = serde_json::to_vec(&packet).unwrap();
    socket.send_to(&bytes, TARGET).unwrap();
}

/// Run one incoming session against the receiver: Enter from the right edge,
/// then replay per-frame deltas (milli-pixels), then Cancel to clean up.
fn run_session(socket: &UdpSocket, key: &[u8], session_id: &str, frames: &[(i64, i64)]) {
    send(
        socket,
        key,
        serde_json::json!({
            "type": "enter",
            "session_id": session_id,
            "entry_edge": "right",
            "ratio": 0.5,
            "sent_at": now_ms(),
        }),
    );
    std::thread::sleep(Duration::from_millis(200));

    let mut total_x_milli = 0_i64;
    let mut total_y_milli = 0_i64;
    for (index, (dx, dy)) in frames.iter().enumerate() {
        total_x_milli += dx;
        total_y_milli += dy;
        send(
            socket,
            key,
            serde_json::json!({
                "type": "move",
                "session_id": session_id,
                "sequence": index as u64 + 1,
                "total_x_milli": total_x_milli,
                "total_y_milli": total_y_milli,
            }),
        );
        std::thread::sleep(Duration::from_millis(16));
    }
    std::thread::sleep(Duration::from_millis(400));

    send(
        socket,
        key,
        serde_json::json!({ "type": "cancel", "session_id": session_id }),
    );
    std::thread::sleep(Duration::from_millis(200));
}

fn main() {
    let key = URL_SAFE_NO_PAD.decode(PEER_SECRET).unwrap();
    assert_eq!(key.len(), 32);
    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();

    // Scenario A: arm (40px inward), walk back to the edge, then 10 frames of
    // +2px outward jitter (20px accumulated push, below the 48px threshold).
    // Expected: NO mouse_remote_return in the receiver log.
    let mut jitter = Vec::new();
    jitter.extend(std::iter::repeat((-1000_i64, 0_i64)).take(40));
    jitter.extend(std::iter::repeat((1000_i64, 0_i64)).take(40));
    jitter.extend(std::iter::repeat((2000_i64, 0_i64)).take(10));
    println!("scenario A (jitter): {} frames — expect NO remote_return", jitter.len());
    run_session(&socket, &key, "probe-jitter-0001", &jitter);

    std::thread::sleep(Duration::from_millis(500));

    // Scenario B: same walk, then 30 frames of sustained +2px outward push
    // (61px accumulated, above the 48px threshold).
    // Expected: mouse_remote_return DOES appear in the receiver log.
    let mut push = Vec::new();
    push.extend(std::iter::repeat((-1000_i64, 0_i64)).take(40));
    push.extend(std::iter::repeat((1000_i64, 0_i64)).take(40));
    push.extend(std::iter::repeat((2000_i64, 0_i64)).take(30));
    println!("scenario B (sustained push): {} frames — expect remote_return", push.len());
    run_session(&socket, &key, "probe-push-00001", &push);

    println!("done — check mouse.log for mouse_remote_return lines");
}
