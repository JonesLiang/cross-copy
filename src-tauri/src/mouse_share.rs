use crate::mouse_hook::SYNTHETIC_INPUT_MARKER;
use crate::{
    logger::Logger,
    model::ScreenPosition,
    mouse_hook::{
        recenter_cursor, run_mouse_hook, screen_size, set_cursor_visible, HookMouseButton,
        HookMouseEvent,
    },
};
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Mouse, Settings as EnigoSettings};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc as std_mpsc, Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;
use uuid::Uuid;

const NO_LATENCY: u64 = u64::MAX;
const PHYSICAL_INPUT_PRIORITY_MS: u64 = 180;
const HELD_BUTTON_SAFETY_TIMEOUT_MS: u64 = 10_000;
const LOGICAL_PIXEL_MILLI: i64 = 1_000;
const LOGICAL_500_DPI_GAIN_MILLI: i64 = 500;
const MAX_PHYSICAL_DELTA_PER_EVENT: i32 = 256;
const ENTER_RETRY_MS: u64 = 120;
const SESSION_TIMEOUT_MS: u64 = 5_000;
const KEEP_ALIVE_MS: u64 = 400;
const MOVE_SEND_INTERVAL_MS: u64 = 4;
const EDGE_INSET_PIXELS: i32 = 8;
const EDGE_TRANSITION_COOLDOWN_MS: u64 = 160;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SharedMouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MouseSignal {
    Enter {
        session_id: String,
        entry_edge: ScreenPosition,
        ratio: f64,
        sent_at: u64,
    },
    Move {
        session_id: String,
        sequence: u64,
        total_x_milli: i64,
        total_y_milli: i64,
    },
    Button {
        session_id: String,
        button: SharedMouseButton,
        pressed: bool,
    },
    Scroll {
        session_id: String,
        sequence: u64,
        total_x_milli: i64,
        total_y_milli: i64,
    },
    Return {
        session_id: String,
        ratio: f64,
    },
    Cancel {
        session_id: String,
    },
    Ack {
        session_id: String,
        sent_at: u64,
    },
    Latency {
        session_id: String,
        milliseconds: u64,
    },
    KeepAlive {
        session_id: String,
    },
}

#[derive(Clone, Debug)]
pub struct OutboundMouseSignal {
    pub peer_id: String,
    pub signal: MouseSignal,
}

struct OutgoingSession {
    peer_id: String,
    session_id: String,
    exit_edge: ScreenPosition,
    anchor_x: i32,
    anchor_y: i32,
    enter_ratio: f64,
    last_enter_retry_at: u64,
    acknowledged: bool,
    move_sequence: u64,
    total_x_milli: i64,
    total_y_milli: i64,
    last_move_sent_at: u64,
    last_sent_x_milli: i64,
    last_sent_y_milli: i64,
    scroll_sequence: u64,
    total_scroll_x_milli: i64,
    total_scroll_y_milli: i64,
    last_sent_scroll_x_milli: i64,
    last_sent_scroll_y_milli: i64,
    last_remote_at: u64,
}

struct IncomingSession {
    peer_id: String,
    session_id: String,
    return_edge: ScreenPosition,
    x_milli: i64,
    y_milli: i64,
    last_move_sequence: u64,
    last_total_x_milli: i64,
    last_total_y_milli: i64,
    scroll_x_milli: i64,
    scroll_y_milli: i64,
    last_scroll_sequence: u64,
    last_total_scroll_x_milli: i64,
    last_total_scroll_y_milli: i64,
    last_keep_alive_at: u64,
    held_buttons: [bool; 3],
    last_event_at: u64,
}

struct Runtime {
    target_peer: Option<String>,
    position: ScreenPosition,
    last_x: i32,
    last_y: i32,
    crossing_blocked_until: u64,
    outgoing: Option<OutgoingSession>,
    incoming: Option<IncomingSession>,
}

struct Inner {
    enabled: AtomicBool,
    listener_attempted: AtomicBool,
    listener_started: AtomicBool,
    latency_ms: AtomicU64,
    last_physical_at: AtomicU64,
    runtime: Mutex<Runtime>,
    outbound: mpsc::Sender<OutboundMouseSignal>,
    injector: std_mpsc::SyncSender<HookMouseEvent>,
    logger: Arc<Logger>,
    screen_width: i32,
    screen_height: i32,
}

pub struct MouseShare {
    inner: Arc<Inner>,
}

impl MouseShare {
    pub fn new(logger: Arc<Logger>, outbound: mpsc::Sender<OutboundMouseSignal>) -> Arc<Self> {
        let (injector, injection_receiver) = std_mpsc::sync_channel(512);
        let mut enigo = Enigo::new(&mouse_input_settings()).ok();
        let (screen_width, screen_height) = screen_size();
        let injection_logger = Arc::clone(&logger);
        let _ = std::thread::Builder::new()
            .name("crosscopy-mouse-injector".into())
            .spawn(move || {
                let Some(mut enigo) = enigo.take() else {
                    injection_logger.error(
                        "mouse_injector_failed",
                        "provider=enigo initialization_failed=true",
                    );
                    return;
                };
                let mut last_error_log = 0_u64;
                while let Ok(event) = injection_receiver.recv() {
                    if let Err(error) = inject_mouse_event(&mut enigo, event) {
                        let now = now_ms();
                        if now.saturating_sub(last_error_log) >= 1_000 {
                            last_error_log = now;
                            injection_logger.warn("mouse_simulation_failed", error);
                        }
                    }
                }
            });
        let mouse_share = Arc::new(Self {
            inner: Arc::new(Inner {
                enabled: AtomicBool::new(false),
                listener_attempted: AtomicBool::new(false),
                listener_started: AtomicBool::new(false),
                latency_ms: AtomicU64::new(NO_LATENCY),
                last_physical_at: AtomicU64::new(0),
                runtime: Mutex::new(Runtime {
                    target_peer: None,
                    position: ScreenPosition::Right,
                    last_x: 0,
                    last_y: 0,
                    crossing_blocked_until: 0,
                    outgoing: None,
                    incoming: None,
                }),
                outbound,
                injector,
                logger,
                screen_width,
                screen_height,
            }),
        });
        mouse_share.start_session_maintenance();
        mouse_share
    }

    pub fn configure(&self, enabled: bool, position: ScreenPosition, target_peer: Option<String>) {
        let was_enabled = self.inner.enabled.swap(enabled, Ordering::AcqRel);
        if enabled && !was_enabled {
            self.inner
                .listener_attempted
                .store(false, Ordering::Release);
        }
        let mut runtime = self.inner.runtime.lock().expect("mouse runtime lock");
        let target_changed = runtime.target_peer != target_peer;
        runtime.position = position;
        runtime.target_peer = target_peer;
        let should_start_listener = enabled && runtime.target_peer.is_some();
        let mut release_events = Vec::new();
        if !enabled || target_changed {
            if runtime.outgoing.take().is_some() {
                release_events.push(HookMouseEvent::CursorVisible(true));
            }
            if let Some(incoming) = runtime.incoming.take() {
                release_events = release_held_buttons(&incoming);
            }
            self.inner.latency_ms.store(NO_LATENCY, Ordering::Relaxed);
        }
        drop(runtime);
        for event in release_events {
            self.inject(event);
        }
        if should_start_listener {
            self.ensure_listener_started();
        }
    }

    pub fn listener_started(&self) -> bool {
        self.inner.listener_started.load(Ordering::Acquire)
    }

    pub fn latency_ms(&self) -> Option<u64> {
        match self.inner.latency_ms.load(Ordering::Relaxed) {
            NO_LATENCY => None,
            value => Some(value),
        }
    }

    pub fn session_active(&self) -> bool {
        let runtime = self.inner.runtime.lock().expect("mouse runtime lock");
        runtime.outgoing.is_some() || runtime.incoming.is_some()
    }

    pub fn expire_unresponsive_outgoing(&self) {
        let mut runtime = self.inner.runtime.lock().expect("mouse runtime lock");
        let expired_edge = runtime
            .outgoing
            .as_ref()
            .filter(|session| now_ms().saturating_sub(session.last_remote_at) >= SESSION_TIMEOUT_MS)
            .map(|session| session.exit_edge);
        let Some(exit_edge) = expired_edge else {
            return;
        };
        runtime.outgoing = None;
        let point = safe_source_point(
            exit_edge,
            runtime.last_x,
            runtime.last_y,
            self.inner.screen_width,
            self.inner.screen_height,
        );
        runtime.last_x = point.0;
        runtime.last_y = point.1;
        runtime.crossing_blocked_until = now_ms() + EDGE_TRANSITION_COOLDOWN_MS;
        drop(runtime);
        self.inject(HookMouseEvent::CursorVisible(true));
        self.inject(absolute_move(point.0, point.1));
        self.inner
            .logger
            .warn("mouse_session_cancelled", "reason=peer_unresponsive");
    }

    pub fn force_stop(&self) {
        self.inner.enabled.store(false, Ordering::Release);
        let mut runtime = self.inner.runtime.lock().expect("mouse runtime lock");
        let outgoing_active = runtime.outgoing.take().is_some();
        let releases = runtime
            .incoming
            .take()
            .map(|incoming| release_held_buttons(&incoming))
            .unwrap_or_default();
        drop(runtime);
        if outgoing_active {
            self.inject(HookMouseEvent::CursorVisible(true));
        }
        for event in releases {
            self.inject(event);
        }
    }

    pub fn apply_remote(&self, peer_id: &str, signal: MouseSignal) -> Vec<OutboundMouseSignal> {
        let mut responses = Vec::new();
        if !self.inner.enabled.load(Ordering::Acquire) {
            return responses;
        }
        let (width, height) = (self.inner.screen_width, self.inner.screen_height);
        let mut runtime = self.inner.runtime.lock().expect("mouse runtime lock");
        let mut simulated_events = Vec::new();
        match signal {
            MouseSignal::Enter {
                session_id,
                entry_edge,
                ratio,
                sent_at,
            } => {
                if incoming_matches(&runtime, peer_id, &session_id) {
                    responses.push(outbound(
                        peer_id,
                        MouseSignal::Ack {
                            session_id,
                            sent_at,
                        },
                    ));
                    return responses;
                }
                if now_ms().saturating_sub(self.inner.last_physical_at.load(Ordering::Relaxed))
                    < PHYSICAL_INPUT_PRIORITY_MS
                {
                    responses.push(outbound(peer_id, MouseSignal::Cancel { session_id }));
                    return responses;
                }
                runtime.outgoing = None;
                let (x, y) = edge_point(entry_edge, ratio, width, height);
                runtime.incoming = Some(IncomingSession {
                    peer_id: peer_id.to_string(),
                    session_id: session_id.clone(),
                    return_edge: entry_edge,
                    x_milli: i64::from(x) * LOGICAL_PIXEL_MILLI,
                    y_milli: i64::from(y) * LOGICAL_PIXEL_MILLI,
                    last_move_sequence: 0,
                    last_total_x_milli: 0,
                    last_total_y_milli: 0,
                    scroll_x_milli: 0,
                    scroll_y_milli: 0,
                    last_scroll_sequence: 0,
                    last_total_scroll_x_milli: 0,
                    last_total_scroll_y_milli: 0,
                    last_keep_alive_at: now_ms(),
                    held_buttons: [false; 3],
                    last_event_at: now_ms(),
                });
                simulated_events.push(absolute_move(x, y));
                responses.push(outbound(
                    peer_id,
                    MouseSignal::Ack {
                        session_id,
                        sent_at,
                    },
                ));
                self.inner.logger.info(
                    "mouse_remote_enter",
                    format!("edge={entry_edge:?} ratio={ratio:.3}"),
                );
            }
            MouseSignal::Move {
                session_id,
                sequence,
                total_x_milli,
                total_y_milli,
            } => {
                let Some(incoming) = runtime.incoming.as_mut() else {
                    return responses;
                };
                if incoming.peer_id != peer_id || incoming.session_id != session_id {
                    return responses;
                }
                if sequence <= incoming.last_move_sequence {
                    return responses;
                }
                incoming.last_event_at = now_ms();
                let delta_x_milli = total_x_milli.saturating_sub(incoming.last_total_x_milli);
                let delta_y_milli = total_y_milli.saturating_sub(incoming.last_total_y_milli);
                incoming.last_move_sequence = sequence;
                incoming.last_total_x_milli = total_x_milli;
                incoming.last_total_y_milli = total_y_milli;
                let next_x_milli = incoming
                    .x_milli
                    .saturating_add(delta_x_milli)
                    .clamp(0, i64::from(width - 1) * LOGICAL_PIXEL_MILLI);
                let next_y_milli = incoming
                    .y_milli
                    .saturating_add(delta_y_milli)
                    .clamp(0, i64::from(height - 1) * LOGICAL_PIXEL_MILLI);
                let next_x = milli_to_pixel(next_x_milli);
                let next_y = milli_to_pixel(next_y_milli);
                if crossed_return_edge(
                    incoming.return_edge,
                    next_x,
                    next_y,
                    delta_x_milli,
                    delta_y_milli,
                    width,
                    height,
                ) {
                    let ratio = edge_ratio(incoming.return_edge, next_x, next_y, width, height);
                    let session_id = incoming.session_id.clone();
                    simulated_events.extend(release_held_buttons(incoming));
                    runtime.incoming = None;
                    responses.push(outbound(peer_id, MouseSignal::Return { session_id, ratio }));
                    self.inner
                        .logger
                        .info("mouse_remote_return", format!("ratio={ratio:.3}"));
                } else {
                    incoming.x_milli = next_x_milli;
                    incoming.y_milli = next_y_milli;
                    simulated_events.push(absolute_move(next_x, next_y));
                }
            }
            MouseSignal::Button {
                session_id,
                button,
                pressed,
            } => {
                if let Some(incoming) = matching_incoming_mut(&mut runtime, peer_id, &session_id) {
                    incoming.last_event_at = now_ms();
                    incoming.held_buttons[button_index(button)] = pressed;
                    simulated_events.push(HookMouseEvent::Button {
                        button: to_hook_button(button),
                        pressed,
                    });
                }
            }
            MouseSignal::Scroll {
                session_id,
                sequence,
                total_x_milli,
                total_y_milli,
            } => {
                if let Some(incoming) = matching_incoming_mut(&mut runtime, peer_id, &session_id) {
                    if sequence <= incoming.last_scroll_sequence {
                        return responses;
                    }
                    incoming.last_event_at = now_ms();
                    incoming.scroll_x_milli = incoming.scroll_x_milli.saturating_add(
                        total_x_milli.saturating_sub(incoming.last_total_scroll_x_milli),
                    );
                    incoming.scroll_y_milli = incoming.scroll_y_milli.saturating_add(
                        total_y_milli.saturating_sub(incoming.last_total_scroll_y_milli),
                    );
                    incoming.last_scroll_sequence = sequence;
                    incoming.last_total_scroll_x_milli = total_x_milli;
                    incoming.last_total_scroll_y_milli = total_y_milli;
                    let delta_x_milli = take_complete_scroll_lines(&mut incoming.scroll_x_milli);
                    let delta_y_milli = take_complete_scroll_lines(&mut incoming.scroll_y_milli);
                    if delta_x_milli != 0 || delta_y_milli != 0 {
                        simulated_events.push(HookMouseEvent::Scroll {
                            delta_x_milli,
                            delta_y_milli,
                        });
                    }
                }
            }
            MouseSignal::Return { session_id, ratio } => {
                let Some(outgoing_session) = runtime.outgoing.as_ref() else {
                    return responses;
                };
                if outgoing_session.peer_id != peer_id || outgoing_session.session_id != session_id
                {
                    return responses;
                }
                let point = edge_point(outgoing_session.exit_edge, ratio, width, height);
                runtime.outgoing = None;
                runtime.last_x = point.0;
                runtime.last_y = point.1;
                runtime.crossing_blocked_until = now_ms() + EDGE_TRANSITION_COOLDOWN_MS;
                simulated_events.push(HookMouseEvent::CursorVisible(true));
                simulated_events.push(absolute_move(point.0, point.1));
            }
            MouseSignal::Cancel { session_id } => {
                let cancelled_edge = runtime
                    .outgoing
                    .as_ref()
                    .filter(|session| {
                        session.peer_id == peer_id && session.session_id == session_id
                    })
                    .map(|session| session.exit_edge);
                if let Some(exit_edge) = cancelled_edge {
                    runtime.outgoing = None;
                    let point =
                        safe_source_point(exit_edge, runtime.last_x, runtime.last_y, width, height);
                    runtime.last_x = point.0;
                    runtime.last_y = point.1;
                    runtime.crossing_blocked_until = now_ms() + EDGE_TRANSITION_COOLDOWN_MS;
                    simulated_events.push(HookMouseEvent::CursorVisible(true));
                    simulated_events.push(absolute_move(point.0, point.1));
                }
                if runtime.incoming.as_ref().is_some_and(|session| {
                    session.peer_id == peer_id && session.session_id == session_id
                }) {
                    if let Some(incoming) = runtime.incoming.take() {
                        simulated_events.extend(release_held_buttons(&incoming));
                    }
                }
            }
            MouseSignal::Ack {
                session_id,
                sent_at,
            } => {
                if let Some(outgoing) = runtime.outgoing.as_mut().filter(|session| {
                    session.peer_id == peer_id && session.session_id == session_id
                }) {
                    let was_acknowledged = outgoing.acknowledged;
                    outgoing.acknowledged = true;
                    outgoing.last_remote_at = now_ms();
                    let latency = now_ms().saturating_sub(sent_at).div_ceil(2);
                    self.inner.latency_ms.store(latency, Ordering::Relaxed);
                    responses.push(outbound(
                        peer_id,
                        MouseSignal::Latency {
                            session_id,
                            milliseconds: latency,
                        },
                    ));
                    if !was_acknowledged
                        && (outgoing.total_x_milli != 0 || outgoing.total_y_milli != 0)
                    {
                        outgoing.move_sequence = outgoing.move_sequence.saturating_add(1);
                        outgoing.last_move_sent_at = now_ms();
                        responses.push(outbound(
                            peer_id,
                            MouseSignal::Move {
                                session_id: outgoing.session_id.clone(),
                                sequence: outgoing.move_sequence,
                                total_x_milli: outgoing.total_x_milli,
                                total_y_milli: outgoing.total_y_milli,
                            },
                        ));
                    }
                    if !was_acknowledged
                        && (outgoing.total_scroll_x_milli != 0
                            || outgoing.total_scroll_y_milli != 0)
                    {
                        outgoing.scroll_sequence = outgoing.scroll_sequence.saturating_add(1);
                        responses.push(outbound(
                            peer_id,
                            MouseSignal::Scroll {
                                session_id: outgoing.session_id.clone(),
                                sequence: outgoing.scroll_sequence,
                                total_x_milli: outgoing.total_scroll_x_milli,
                                total_y_milli: outgoing.total_scroll_y_milli,
                            },
                        ));
                    }
                }
            }
            MouseSignal::Latency {
                session_id,
                milliseconds,
            } => {
                if runtime.incoming.as_ref().is_some_and(|session| {
                    session.peer_id == peer_id && session.session_id == session_id
                }) {
                    self.inner.latency_ms.store(milliseconds, Ordering::Relaxed);
                }
            }
            MouseSignal::KeepAlive { session_id } => {
                if let Some(outgoing) = runtime.outgoing.as_mut().filter(|session| {
                    session.peer_id == peer_id && session.session_id == session_id
                }) {
                    let was_acknowledged = outgoing.acknowledged;
                    outgoing.acknowledged = true;
                    outgoing.last_remote_at = now_ms();
                    if !was_acknowledged
                        && (outgoing.total_x_milli != 0 || outgoing.total_y_milli != 0)
                    {
                        outgoing.move_sequence = outgoing.move_sequence.saturating_add(1);
                        responses.push(outbound(
                            peer_id,
                            MouseSignal::Move {
                                session_id: outgoing.session_id.clone(),
                                sequence: outgoing.move_sequence,
                                total_x_milli: outgoing.total_x_milli,
                                total_y_milli: outgoing.total_y_milli,
                            },
                        ));
                    }
                }
            }
        }
        drop(runtime);
        for event in simulated_events {
            self.inject(event);
        }
        responses
    }

    fn ensure_listener_started(&self) {
        if self
            .inner
            .listener_attempted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        self.inner.listener_started.store(true, Ordering::Release);
        let inner = Arc::clone(&self.inner);
        if let Err(error) = std::thread::Builder::new()
            .name("crosscopy-mouse-hook".into())
            .spawn(move || {
                inner
                    .logger
                    .info("mouse_listener_started", "provider=native_mouse_only");
                let callback_inner = Arc::clone(&inner);
                if let Err(error) =
                    run_mouse_hook(move |event| callback_inner.handle_local_event(event))
                {
                    inner.listener_started.store(false, Ordering::Release);
                    inner
                        .logger
                        .error("mouse_listener_failed", format!("{error:?}"));
                }
            })
        {
            self.inner.listener_started.store(false, Ordering::Release);
            self.inner
                .logger
                .error("mouse_listener_thread_failed", error.to_string());
        }
    }

    fn inject(&self, event: HookMouseEvent) {
        self.inner.inject(event);
    }

    fn start_session_maintenance(self: &Arc<Self>) {
        let mouse_share = Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("crosscopy-mouse-maintenance".into())
            .spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(50));
                let Some(mouse_share) = mouse_share.upgrade() else {
                    return;
                };
                mouse_share.expire_unresponsive_outgoing();

                let now = now_ms();
                let mut runtime = mouse_share
                    .inner
                    .runtime
                    .lock()
                    .expect("mouse runtime lock");
                let mut signals = Vec::new();
                let mut releases = Vec::new();
                if let Some(outgoing) = runtime.outgoing.as_mut() {
                    if !outgoing.acknowledged
                        && now.saturating_sub(outgoing.last_enter_retry_at) >= ENTER_RETRY_MS
                    {
                        outgoing.last_enter_retry_at = now;
                        signals.push(outbound(
                            &outgoing.peer_id,
                            MouseSignal::Enter {
                                session_id: outgoing.session_id.clone(),
                                entry_edge: outgoing.exit_edge.opposite(),
                                ratio: outgoing.enter_ratio,
                                sent_at: now,
                            },
                        ));
                    }
                    if outgoing.acknowledged
                        && (outgoing.total_x_milli != outgoing.last_sent_x_milli
                            || outgoing.total_y_milli != outgoing.last_sent_y_milli)
                        && now.saturating_sub(outgoing.last_move_sent_at) >= MOVE_SEND_INTERVAL_MS
                    {
                        outgoing.move_sequence = outgoing.move_sequence.saturating_add(1);
                        outgoing.last_move_sent_at = now;
                        outgoing.last_sent_x_milli = outgoing.total_x_milli;
                        outgoing.last_sent_y_milli = outgoing.total_y_milli;
                        signals.push(outbound(
                            &outgoing.peer_id,
                            MouseSignal::Move {
                                session_id: outgoing.session_id.clone(),
                                sequence: outgoing.move_sequence,
                                total_x_milli: outgoing.total_x_milli,
                                total_y_milli: outgoing.total_y_milli,
                            },
                        ));
                    }
                    if outgoing.acknowledged
                        && (outgoing.total_scroll_x_milli != outgoing.last_sent_scroll_x_milli
                            || outgoing.total_scroll_y_milli != outgoing.last_sent_scroll_y_milli)
                    {
                        outgoing.scroll_sequence = outgoing.scroll_sequence.saturating_add(1);
                        outgoing.last_sent_scroll_x_milli = outgoing.total_scroll_x_milli;
                        outgoing.last_sent_scroll_y_milli = outgoing.total_scroll_y_milli;
                        signals.push(outbound(
                            &outgoing.peer_id,
                            MouseSignal::Scroll {
                                session_id: outgoing.session_id.clone(),
                                sequence: outgoing.scroll_sequence,
                                total_x_milli: outgoing.total_scroll_x_milli,
                                total_y_milli: outgoing.total_scroll_y_milli,
                            },
                        ));
                    }
                }
                if let Some(incoming) = runtime.incoming.as_mut() {
                    if now.saturating_sub(incoming.last_keep_alive_at) >= KEEP_ALIVE_MS {
                        incoming.last_keep_alive_at = now;
                        signals.push(outbound(
                            &incoming.peer_id,
                            MouseSignal::KeepAlive {
                                session_id: incoming.session_id.clone(),
                            },
                        ));
                    }
                    if now.saturating_sub(incoming.last_event_at) >= HELD_BUTTON_SAFETY_TIMEOUT_MS
                        && incoming.held_buttons.iter().any(|pressed| *pressed)
                    {
                        releases = release_held_buttons(incoming);
                        incoming.held_buttons = [false; 3];
                    }
                }
                drop(runtime);
                for signal in signals {
                    let _ = mouse_share.inner.outbound.blocking_send(signal);
                }
                let released_any = !releases.is_empty();
                for event in releases {
                    mouse_share.inject(event);
                }
                if released_any {
                    mouse_share.inner.logger.warn(
                        "mouse_buttons_safety_released",
                        "reason=remote_session_idle",
                    );
                }
            });
    }
}

impl Inner {
    fn handle_local_event(&self, event: HookMouseEvent) -> bool {
        self.last_physical_at.store(now_ms(), Ordering::Relaxed);
        if !self.enabled.load(Ordering::Acquire) {
            return false;
        }

        let (width, height) = (self.screen_width, self.screen_height);
        let mut runtime = self.runtime.lock().expect("mouse runtime lock");
        if let Some(incoming) = runtime.incoming.take() {
            if let HookMouseEvent::Move { x, y, .. } = event {
                runtime.last_x = x;
                runtime.last_y = y;
            }
            let release_events = release_held_buttons(&incoming);
            let _ = self.outbound.blocking_send(outbound(
                &incoming.peer_id,
                MouseSignal::Cancel {
                    session_id: incoming.session_id,
                },
            ));
            drop(runtime);
            for event in release_events {
                self.inject(event);
            }
            return false;
        }

        if let Some(outgoing) = runtime.outgoing.as_mut() {
            let peer_id = outgoing.peer_id.clone();
            let session_id = outgoing.session_id.clone();
            let mut should_recenter = false;
            match event {
                HookMouseEvent::Move { x, y, native_delta } => {
                    let (raw_delta_x, raw_delta_y) =
                        native_delta.unwrap_or((x - outgoing.anchor_x, y - outgoing.anchor_y));
                    let delta_x = clamp_physical_delta(raw_delta_x);
                    let delta_y = clamp_physical_delta(raw_delta_y);
                    should_recenter = true;
                    if delta_x != 0 || delta_y != 0 {
                        outgoing.total_x_milli = outgoing
                            .total_x_milli
                            .saturating_add(i64::from(delta_x) * LOGICAL_500_DPI_GAIN_MILLI);
                        outgoing.total_y_milli = outgoing
                            .total_y_milli
                            .saturating_add(i64::from(delta_y) * LOGICAL_500_DPI_GAIN_MILLI);
                        let now = now_ms();
                        if outgoing.acknowledged
                            && now.saturating_sub(outgoing.last_move_sent_at)
                                >= MOVE_SEND_INTERVAL_MS
                        {
                            outgoing.move_sequence = outgoing.move_sequence.saturating_add(1);
                            if self
                                .outbound
                                .try_send(outbound(
                                    &peer_id,
                                    MouseSignal::Move {
                                        session_id,
                                        sequence: outgoing.move_sequence,
                                        total_x_milli: outgoing.total_x_milli,
                                        total_y_milli: outgoing.total_y_milli,
                                    },
                                ))
                                .is_ok()
                            {
                                outgoing.last_move_sent_at = now;
                                outgoing.last_sent_x_milli = outgoing.total_x_milli;
                                outgoing.last_sent_y_milli = outgoing.total_y_milli;
                            }
                        }
                    }
                }
                HookMouseEvent::Button { button, pressed } => {
                    if outgoing.acknowledged {
                        let _ = self.outbound.blocking_send(outbound(
                            &peer_id,
                            MouseSignal::Button {
                                session_id,
                                button: from_hook_button(button),
                                pressed,
                            },
                        ));
                    }
                }
                HookMouseEvent::Scroll {
                    delta_x_milli,
                    delta_y_milli,
                } => {
                    outgoing.total_scroll_x_milli =
                        outgoing.total_scroll_x_milli.saturating_add(delta_x_milli);
                    outgoing.total_scroll_y_milli =
                        outgoing.total_scroll_y_milli.saturating_add(delta_y_milli);
                    if outgoing.acknowledged {
                        outgoing.scroll_sequence = outgoing.scroll_sequence.saturating_add(1);
                        if self
                            .outbound
                            .try_send(outbound(
                                &peer_id,
                                MouseSignal::Scroll {
                                    session_id,
                                    sequence: outgoing.scroll_sequence,
                                    total_x_milli: outgoing.total_scroll_x_milli,
                                    total_y_milli: outgoing.total_scroll_y_milli,
                                },
                            ))
                            .is_ok()
                        {
                            outgoing.last_sent_scroll_x_milli = outgoing.total_scroll_x_milli;
                            outgoing.last_sent_scroll_y_milli = outgoing.total_scroll_y_milli;
                        }
                    }
                }
                _ => {}
            }
            let anchor = (outgoing.anchor_x, outgoing.anchor_y);
            drop(runtime);
            if should_recenter {
                let _ = recenter_cursor(anchor.0, anchor.1, width, height);
            }
            return true;
        }

        if let HookMouseEvent::Move { x, y, .. } = event {
            let previous_x = runtime.last_x;
            let previous_y = runtime.last_y;
            runtime.last_x = x;
            runtime.last_y = y;
            let position = runtime.position;
            let target = runtime.target_peer.clone();
            if let Some(peer_id) = target {
                if now_ms() >= runtime.crossing_blocked_until
                    && reached_exit_edge(position, x, y, previous_x, previous_y, width, height)
                {
                    let session_id = Uuid::new_v4().to_string();
                    let ratio = edge_ratio(position, x, y, width, height);
                    let anchor_x = width / 2;
                    let anchor_y = height / 2;
                    let sent_at = now_ms();
                    runtime.outgoing = Some(OutgoingSession {
                        peer_id: peer_id.clone(),
                        session_id: session_id.clone(),
                        exit_edge: position,
                        anchor_x,
                        anchor_y,
                        enter_ratio: ratio,
                        last_enter_retry_at: sent_at,
                        acknowledged: false,
                        move_sequence: 0,
                        total_x_milli: 0,
                        total_y_milli: 0,
                        last_move_sent_at: 0,
                        last_sent_x_milli: 0,
                        last_sent_y_milli: 0,
                        scroll_sequence: 0,
                        total_scroll_x_milli: 0,
                        total_scroll_y_milli: 0,
                        last_sent_scroll_x_milli: 0,
                        last_sent_scroll_y_milli: 0,
                        last_remote_at: sent_at,
                    });
                    let _ = self.outbound.blocking_send(outbound(
                        &peer_id,
                        MouseSignal::Enter {
                            session_id,
                            entry_edge: position.opposite(),
                            ratio,
                            sent_at,
                        },
                    ));
                    drop(runtime);
                    self.inject(HookMouseEvent::CursorVisible(false));
                    let _ = recenter_cursor(anchor_x, anchor_y, width, height);
                    return true;
                }
            }
        }
        false
    }

    fn inject(&self, event: HookMouseEvent) {
        if matches!(event, HookMouseEvent::Move { .. }) {
            // Movement is absolute and cumulative, so a saturated injector may
            // drop an intermediate frame; the next frame catches up instantly.
            let _ = self.injector.try_send(event);
        } else {
            // Cursor visibility, buttons and scrolling are stateful and must
            // never be discarded, otherwise the source can retain a ghost
            // cursor or a target can retain a pressed button.
            let _ = self.injector.send(event);
        }
    }
}

fn outbound(peer_id: &str, signal: MouseSignal) -> OutboundMouseSignal {
    OutboundMouseSignal {
        peer_id: peer_id.to_string(),
        signal,
    }
}

fn incoming_matches(runtime: &Runtime, peer_id: &str, session_id: &str) -> bool {
    runtime
        .incoming
        .as_ref()
        .is_some_and(|session| session.peer_id == peer_id && session.session_id == session_id)
}

fn matching_incoming_mut<'a>(
    runtime: &'a mut Runtime,
    peer_id: &str,
    session_id: &str,
) -> Option<&'a mut IncomingSession> {
    runtime
        .incoming
        .as_mut()
        .filter(|session| session.peer_id == peer_id && session.session_id == session_id)
}

fn release_held_buttons(session: &IncomingSession) -> Vec<HookMouseEvent> {
    [
        HookMouseButton::Left,
        HookMouseButton::Right,
        HookMouseButton::Middle,
    ]
    .into_iter()
    .enumerate()
    .filter(|(index, _)| session.held_buttons[*index])
    .map(|(_, button)| HookMouseEvent::Button {
        button,
        pressed: false,
    })
    .collect()
}

fn button_index(button: SharedMouseButton) -> usize {
    match button {
        SharedMouseButton::Left => 0,
        SharedMouseButton::Right => 1,
        SharedMouseButton::Middle => 2,
    }
}

fn edge_point(edge: ScreenPosition, ratio: f64, width: i32, height: i32) -> (i32, i32) {
    let ratio = ratio.clamp(0.0, 1.0);
    let horizontal_inset = EDGE_INSET_PIXELS.min((width - 1) / 2);
    let vertical_inset = EDGE_INSET_PIXELS.min((height - 1) / 2);
    match edge {
        ScreenPosition::Left => (
            horizontal_inset,
            (ratio * f64::from(height - 1)).round() as i32,
        ),
        ScreenPosition::Right => (
            width - 1 - horizontal_inset,
            (ratio * f64::from(height - 1)).round() as i32,
        ),
        ScreenPosition::Up => (
            (ratio * f64::from(width - 1)).round() as i32,
            vertical_inset,
        ),
        ScreenPosition::Down => (
            (ratio * f64::from(width - 1)).round() as i32,
            height - 1 - vertical_inset,
        ),
    }
}

fn safe_source_point(edge: ScreenPosition, x: i32, y: i32, width: i32, height: i32) -> (i32, i32) {
    edge_point(edge, edge_ratio(edge, x, y, width, height), width, height)
}

fn edge_ratio(edge: ScreenPosition, x: i32, y: i32, width: i32, height: i32) -> f64 {
    match edge {
        ScreenPosition::Left | ScreenPosition::Right => {
            f64::from(y.clamp(0, height - 1)) / f64::from(height - 1)
        }
        ScreenPosition::Up | ScreenPosition::Down => {
            f64::from(x.clamp(0, width - 1)) / f64::from(width - 1)
        }
    }
}

fn reached_exit_edge(
    edge: ScreenPosition,
    x: i32,
    y: i32,
    previous_x: i32,
    previous_y: i32,
    width: i32,
    height: i32,
) -> bool {
    match edge {
        ScreenPosition::Left => x <= 0 && x <= previous_x,
        ScreenPosition::Right => x >= width - 1 && x >= previous_x,
        ScreenPosition::Up => y <= 0 && y <= previous_y,
        ScreenPosition::Down => y >= height - 1 && y >= previous_y,
    }
}

fn crossed_return_edge(
    edge: ScreenPosition,
    x: i32,
    y: i32,
    delta_x_milli: i64,
    delta_y_milli: i64,
    width: i32,
    height: i32,
) -> bool {
    match edge {
        ScreenPosition::Left => x <= 0 && delta_x_milli < 0,
        ScreenPosition::Right => x >= width - 1 && delta_x_milli > 0,
        ScreenPosition::Up => y <= 0 && delta_y_milli < 0,
        ScreenPosition::Down => y >= height - 1 && delta_y_milli > 0,
    }
}

fn absolute_move(x: i32, y: i32) -> HookMouseEvent {
    HookMouseEvent::Move {
        x,
        y,
        native_delta: None,
    }
}

fn clamp_physical_delta(value: i32) -> i32 {
    value.clamp(-MAX_PHYSICAL_DELTA_PER_EVENT, MAX_PHYSICAL_DELTA_PER_EVENT)
}

fn milli_to_pixel(value: i64) -> i32 {
    ((value + LOGICAL_PIXEL_MILLI / 2) / LOGICAL_PIXEL_MILLI)
        .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn take_complete_scroll_lines(value: &mut i64) -> i64 {
    let complete = (*value / LOGICAL_PIXEL_MILLI) * LOGICAL_PIXEL_MILLI;
    *value -= complete;
    complete
}

fn from_hook_button(button: HookMouseButton) -> SharedMouseButton {
    match button {
        HookMouseButton::Left => SharedMouseButton::Left,
        HookMouseButton::Right => SharedMouseButton::Right,
        HookMouseButton::Middle => SharedMouseButton::Middle,
    }
}

fn to_hook_button(button: SharedMouseButton) -> HookMouseButton {
    match button {
        SharedMouseButton::Left => HookMouseButton::Left,
        SharedMouseButton::Right => HookMouseButton::Right,
        SharedMouseButton::Middle => HookMouseButton::Middle,
    }
}

fn inject_mouse_event(enigo: &mut Enigo, event: HookMouseEvent) -> Result<(), String> {
    match event {
        HookMouseEvent::Move { x, y, .. } => enigo
            .move_mouse(x, y, Coordinate::Abs)
            .map_err(|error| error.to_string()),
        HookMouseEvent::Button { button, pressed } => enigo
            .button(
                match button {
                    HookMouseButton::Left => Button::Left,
                    HookMouseButton::Right => Button::Right,
                    HookMouseButton::Middle => Button::Middle,
                },
                if pressed {
                    Direction::Press
                } else {
                    Direction::Release
                },
            )
            .map_err(|error| error.to_string()),
        HookMouseEvent::Scroll {
            delta_x_milli,
            delta_y_milli,
        } => {
            let delta_x = delta_x_milli / LOGICAL_PIXEL_MILLI;
            let delta_y = delta_y_milli / LOGICAL_PIXEL_MILLI;
            if delta_x != 0 {
                enigo
                    .scroll(clamp_i64(delta_x), Axis::Horizontal)
                    .map_err(|error| error.to_string())?;
            }
            if delta_y != 0 {
                enigo
                    .scroll(clamp_i64(-delta_y), Axis::Vertical)
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        }
        HookMouseEvent::CursorVisible(visible) => set_cursor_visible(visible),
    }
}

fn mouse_input_settings() -> EnigoSettings {
    let mut settings = EnigoSettings::default();
    settings.open_prompt_to_get_permissions = false;
    settings.event_source_user_data = Some(SYNTHETIC_INPUT_MARKER as i64);
    settings.windows_dw_extra_info = Some(SYNTHETIC_INPUT_MARKER);
    settings
}

fn clamp_i64(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
