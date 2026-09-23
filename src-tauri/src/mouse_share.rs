use crate::{
    logger::Logger,
    model::ScreenPosition,
    mouse_hook::{
        ensure_source_cursor_captured, recenter_cursor, run_keyboard_hook, run_mouse_hook,
        screen_bounds, set_realtime_priority, set_source_cursor_captured, DesktopBounds, HookKey,
        HookMouseButton, HookMouseEvent, SYNTHETIC_INPUT_MARKER,
    },
};
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
use enigo::Coordinate;
use enigo::{Axis, Button, Direction, Enigo, Keyboard, Mouse, Settings as EnigoSettings};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashSet, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;
use uuid::Uuid;

const NO_LATENCY: u64 = u64::MAX;
const PHYSICAL_INPUT_PRIORITY_MS: u64 = 180;
const HELD_INPUT_SAFETY_TIMEOUT_MS: u64 = 10_000;
const LOGICAL_PIXEL_MILLI: i64 = 1_000;
const MAX_PHYSICAL_DELTA_PER_EVENT: i32 = 256;
const EDGE_REARM_PIXELS: i32 = 32;
const TAKEOVER_DISTANCE: u32 = 6;
const TAKEOVER_WINDOW_MS: u64 = 150;
const ENTER_RETRY_MS: u64 = 120;
const SESSION_TIMEOUT_MS: u64 = 5_000;
const KEEP_ALIVE_MS: u64 = 1_000;
const EXTREME_MOVE_SEND_INTERVAL_MS: u64 = 2;
const BALANCED_MOVE_SEND_INTERVAL_MS: u64 = 4;
const SESSION_MAINTENANCE_INTERVAL_MS: u64 = 50;
const EDGE_INSET_PIXELS: i32 = 8;
const RETURN_ARM_DISTANCE_PIXELS: i32 = 32;
// Returning control requires a deliberate sustained push against the return
// edge.  Jitter in the physical delta stream (a few px back and forth) must
// never be enough to hand control back.
const RETURN_PUSH_THRESHOLD_MILLI: i64 = 48_000;
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
    Key {
        session_id: String,
        key: HookKey,
        pressed: bool,
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
    first_move_logged: bool,
    last_input_at: u64,
    last_gap_log_at: u64,
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
    receive_dpi: u16,
    last_injected_x: i32,
    last_injected_y: i32,
    last_move_sequence: u64,
    last_gap_log_at: u64,
    last_total_x_milli: i64,
    last_total_y_milli: i64,
    scroll_x_milli: i64,
    scroll_y_milli: i64,
    last_scroll_sequence: u64,
    last_total_scroll_x_milli: i64,
    last_total_scroll_y_milli: i64,
    last_keep_alive_at: u64,
    return_armed: bool,
    return_push_milli: i64,
    held_buttons: [bool; 3],
    held_keys: HashSet<HookKey>,
    last_event_at: u64,
    last_input_at: u64,
}

#[derive(Clone)]
struct MouseTarget {
    peer_id: String,
    position: ScreenPosition,
    screen_number: u8,
}

#[derive(Clone, Copy)]
struct QueuedInput {
    event: HookMouseEvent,
    generation: u64,
    queued_at: std::time::Instant,
}

fn is_release(event: HookMouseEvent) -> bool {
    matches!(
        event,
        HookMouseEvent::Button { pressed: false, .. } | HookMouseEvent::Key { pressed: false, .. }
    )
}

struct InjectionQueue {
    generation: AtomicU64,
    pending: Mutex<VecDeque<QueuedInput>>,
    ready: Condvar,
}

impl InjectionQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            generation: AtomicU64::new(0),
            pending: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
        })
    }

    fn push(&self, event: HookMouseEvent) {
        let mut pending = self.pending.lock().expect("input injection queue lock");
        let queued = QueuedInput {
            event,
            generation: self.generation.load(Ordering::Acquire),
            queued_at: std::time::Instant::now(),
        };
        if matches!(event, HookMouseEvent::Move { .. })
            && pending
                .back()
                .is_some_and(|queued| matches!(queued.event, HookMouseEvent::Move { .. }))
        {
            if let Some(latest) = pending.back_mut() {
                *latest = queued;
            }
        } else {
            pending.push_back(queued);
        }
        self.ready.notify_one();
    }

    fn invalidate(&self) {
        let mut pending = self.pending.lock().expect("input injection queue lock");
        self.generation.fetch_add(1, Ordering::AcqRel);
        // Releases must survive ownership changes, even if the matching press
        // has already left this queue.
        pending.retain(|queued| is_release(queued.event));
    }

    fn pop(&self) -> QueuedInput {
        let mut pending = self.pending.lock().expect("input injection queue lock");
        loop {
            if let Some(event) = pending.pop_front() {
                return event;
            }
            pending = self
                .ready
                .wait(pending)
                .expect("input injection queue wait");
        }
    }
}

struct Runtime {
    targets: Vec<MouseTarget>,
    receive_dpi: Vec<(String, u16)>,
    last_x: i32,
    last_y: i32,
    crossing_blocked_until: u64,
    edge_armed: bool,
    takeover_delta: (i32, i32),
    takeover_started_at: u64,
    retired_sessions: VecDeque<(String, String)>,
    outgoing: Option<OutgoingSession>,
    incoming: Option<IncomingSession>,
    local_held_keys: HashSet<HookKey>,
    suppressed_shortcut_keys: HashSet<HookKey>,
}

impl Runtime {
    fn retire_incoming(&mut self) -> Option<IncomingSession> {
        let incoming = self.incoming.take()?;
        self.retired_sessions
            .push_back((incoming.peer_id.clone(), incoming.session_id.clone()));
        while self.retired_sessions.len() > 64 {
            self.retired_sessions.pop_front();
        }
        self.edge_armed = false;
        self.takeover_delta = (0, 0);
        Some(incoming)
    }

    fn expire_incoming(&mut self, now: u64) -> Option<IncomingSession> {
        if self.incoming.as_ref().is_some_and(|incoming| {
            now.saturating_sub(incoming.last_event_at) >= SESSION_TIMEOUT_MS
        }) {
            self.retire_incoming()
        } else {
            None
        }
    }

    fn release_idle_input(&mut self, now: u64) -> Vec<HookMouseEvent> {
        let Some(incoming) = self.incoming.as_mut() else {
            return Vec::new();
        };
        if now.saturating_sub(incoming.last_input_at) < HELD_INPUT_SAFETY_TIMEOUT_MS {
            return Vec::new();
        }
        let releases = release_held_input(incoming);
        incoming.held_buttons = [false; 3];
        incoming.held_keys.clear();
        releases
    }

    fn local_intent(&mut self, event: HookMouseEvent, now: u64) -> bool {
        match event {
            HookMouseEvent::Move {
                native_delta: Some((dx, dy)),
                ..
            } => {
                if now.saturating_sub(self.takeover_started_at) > TAKEOVER_WINDOW_MS {
                    self.takeover_started_at = now;
                    self.takeover_delta = (0, 0);
                }
                self.takeover_delta.0 = self.takeover_delta.0.saturating_add(dx);
                self.takeover_delta.1 = self.takeover_delta.1.saturating_add(dy);
                self.takeover_delta
                    .0
                    .unsigned_abs()
                    .max(self.takeover_delta.1.unsigned_abs())
                    >= TAKEOVER_DISTANCE
            }
            HookMouseEvent::Button { pressed, .. } | HookMouseEvent::Key { pressed, .. } => pressed,
            HookMouseEvent::Scroll {
                delta_x_milli,
                delta_y_milli,
            } => delta_x_milli != 0 || delta_y_milli != 0,
            _ => false,
        }
    }
}

struct Inner {
    enabled: AtomicBool,
    extreme_performance: Arc<AtomicBool>,
    listener_attempted: AtomicBool,
    listener_started: AtomicBool,
    keyboard_listener_attempted: AtomicBool,
    keyboard_listener_started: AtomicBool,
    source_control_active: Arc<AtomicBool>,
    latency_ms: AtomicU64,
    last_physical_at: AtomicU64,
    runtime: Mutex<Runtime>,
    // OS hooks must never wait for a network consumer. Consecutive moves are
    // coalesced by the consumer; button/key ordering is preserved.
    outbound: mpsc::UnboundedSender<OutboundMouseSignal>,
    injector: Arc<InjectionQueue>,
    logger: Arc<Logger>,
    bounds: Mutex<DesktopBounds>,
}

pub struct MouseShare {
    inner: Arc<Inner>,
}

impl MouseShare {
    pub fn new(
        logger: Arc<Logger>,
        outbound: mpsc::UnboundedSender<OutboundMouseSignal>,
    ) -> Arc<Self> {
        let injector = InjectionQueue::new();
        let injection_receiver = Arc::clone(&injector);
        let extreme_performance = Arc::new(AtomicBool::new(false));
        let injector_extreme_performance = Arc::clone(&extreme_performance);
        let source_control_active = Arc::new(AtomicBool::new(false));
        let injector_source_control_active = Arc::clone(&source_control_active);
        let bounds = screen_bounds();
        let injection_logger = Arc::clone(&logger);
        let _ = std::thread::Builder::new()
            .name("crosscopy-mouse-injector".into())
            .spawn(move || {
                // Initialize on the thread that owns native input resources. A
                // permission failure at startup must not kill injection forever.
                let mut enigo = None;
                let mut last_error_log = 0_u64;
                let mut high_priority = false;
                let mut last_stall_log = 0;
                loop {
                    let queued = injection_receiver.pop();
                    let event = queued.event;
                    if queued.generation != injection_receiver.generation.load(Ordering::Acquire)
                        && !is_release(event)
                    {
                        continue;
                    }
                    if matches!(event, HookMouseEvent::Move { .. })
                        && injector_source_control_active.load(Ordering::Acquire)
                    {
                        continue;
                    }
                    let requested = injector_extreme_performance.load(Ordering::Acquire);
                    if requested != high_priority {
                        set_realtime_priority(requested);
                        high_priority = requested;
                    }
                    if enigo.is_none() {
                        match Enigo::new(&mouse_input_settings()) {
                            Ok(value) => enigo = Some(value),
                            Err(error) => {
                                injection_logger.warn("mouse_injector_retry", error.to_string());
                                // Do not replay old clicks/keys after permission
                                // is granted seconds later.
                                injection_receiver
                                    .pending
                                    .lock()
                                    .expect("input injection queue lock")
                                    .clear();
                                std::thread::sleep(std::time::Duration::from_secs(1));
                                continue;
                            }
                        }
                    }
                    if queued.generation != injection_receiver.generation.load(Ordering::Acquire)
                        && !is_release(event)
                    {
                        continue;
                    }
                    let queue_ms = queued.queued_at.elapsed().as_millis();
                    let started = std::time::Instant::now();
                    if let Err(error) = inject_mouse_event(enigo.as_mut().unwrap(), event) {
                        let now = now_ms();
                        if now.saturating_sub(last_error_log) >= 1_000 {
                            last_error_log = now;
                            injection_logger.warn("mouse_simulation_failed", error);
                        }
                    }
                    let inject_ms = started.elapsed().as_millis();
                    if (queue_ms >= 32 || inject_ms >= 16)
                        && now_ms().saturating_sub(last_stall_log) >= 1_000
                    {
                        last_stall_log = now_ms();
                        injection_logger.warn(
                            "mouse_injection_stall",
                            format!("queue_ms={queue_ms} inject_ms={inject_ms}"),
                        );
                    }
                }
            });
        let mouse_share = Arc::new(Self {
            inner: Arc::new(Inner {
                enabled: AtomicBool::new(false),
                extreme_performance,
                listener_attempted: AtomicBool::new(false),
                listener_started: AtomicBool::new(false),
                keyboard_listener_attempted: AtomicBool::new(false),
                keyboard_listener_started: AtomicBool::new(false),
                source_control_active,
                latency_ms: AtomicU64::new(NO_LATENCY),
                last_physical_at: AtomicU64::new(0),
                runtime: Mutex::new(Runtime {
                    targets: Vec::new(),
                    receive_dpi: Vec::new(),
                    last_x: 0,
                    last_y: 0,
                    crossing_blocked_until: 0,
                    edge_armed: false,
                    takeover_delta: (0, 0),
                    takeover_started_at: 0,
                    retired_sessions: VecDeque::new(),
                    outgoing: None,
                    incoming: None,
                    local_held_keys: HashSet::new(),
                    suppressed_shortcut_keys: HashSet::new(),
                }),
                outbound,
                injector,
                logger,
                bounds: Mutex::new(bounds),
            }),
        });
        mouse_share.start_session_maintenance();
        mouse_share
    }

    pub fn configure(
        &self,
        enabled: bool,
        extreme_performance: bool,
        targets: Vec<(String, ScreenPosition, u8)>,
        receive_dpi: Vec<(String, u16)>,
    ) {
        self.inner
            .extreme_performance
            .store(extreme_performance, Ordering::Release);
        let latest_bounds = screen_bounds();
        let bounds_changed = {
            let mut bounds = self.inner.bounds.lock().expect("desktop bounds lock");
            let changed = *bounds != latest_bounds;
            *bounds = latest_bounds;
            changed
        };
        self.inner.enabled.store(enabled, Ordering::Release);
        let targets = targets
            .into_iter()
            .map(|(peer_id, position, screen_number)| MouseTarget {
                peer_id,
                position,
                screen_number,
            })
            .collect::<Vec<_>>();
        let mut runtime = self.inner.runtime.lock().expect("mouse runtime lock");
        let outgoing_invalid = !enabled
            || bounds_changed
            || runtime.outgoing.as_ref().is_some_and(|session| {
                !targets
                    .iter()
                    .any(|target| target.peer_id == session.peer_id)
            });
        let incoming_invalid = !enabled
            || bounds_changed
            || runtime.incoming.as_ref().is_some_and(|session| {
                !targets
                    .iter()
                    .any(|target| target.peer_id == session.peer_id)
            });
        runtime.targets = targets;
        runtime.receive_dpi = receive_dpi;
        let active_receive_dpi = runtime.incoming.as_ref().map(|incoming| {
            (
                incoming.peer_id.clone(),
                runtime
                    .receive_dpi
                    .iter()
                    .find(|(peer_id, _)| peer_id == &incoming.peer_id)
                    .map(|(_, dpi)| *dpi)
                    .unwrap_or(500),
            )
        });
        if let (Some(incoming), Some((peer_id, dpi))) =
            (runtime.incoming.as_mut(), active_receive_dpi)
        {
            if incoming.peer_id == peer_id {
                incoming.receive_dpi = dpi;
            }
        }
        let should_start_listener = enabled && !runtime.targets.is_empty();
        let mut release_events = Vec::new();
        let mut cancelled = Vec::new();
        if outgoing_invalid {
            if let Some(session) = runtime.outgoing.take() {
                cancelled.push((session.peer_id, session.session_id));
            }
        }
        if incoming_invalid {
            if let Some(session) = runtime.retire_incoming() {
                cancelled.push((session.peer_id.clone(), session.session_id.clone()));
                release_events.extend(release_held_input(&session));
            }
        }
        if !cancelled.is_empty() {
            runtime.edge_armed = false;
            self.inner.injector.invalidate();
        }
        if outgoing_invalid || incoming_invalid {
            self.inner.latency_ms.store(NO_LATENCY, Ordering::Relaxed);
        }
        for event in release_events {
            self.inject(event);
        }
        drop(runtime);
        for (peer_id, session_id) in cancelled {
            let _ = self
                .inner
                .outbound
                .send(outbound(&peer_id, MouseSignal::Cancel { session_id }));
        }
        if outgoing_invalid {
            self.inner.reconcile_source_cursor_capture();
        }
        if should_start_listener {
            self.ensure_listener_started();
        }
    }

    pub fn listener_started(&self) -> bool {
        self.inner.listener_started.load(Ordering::Acquire)
            && self.inner.keyboard_listener_started.load(Ordering::Acquire)
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

    pub fn switch_to_peer(&self, peer_id: String, position: ScreenPosition) -> Result<(), String> {
        self.inner.switch_to_peer(peer_id, position)
    }

    pub fn focus_local(&self) {
        self.inner.focus_local();
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
        let bounds = self.inner.desktop_bounds();
        let point = safe_source_point(
            exit_edge,
            runtime.last_x,
            runtime.last_y,
            bounds.width,
            bounds.height,
        );
        runtime.last_x = point.0;
        runtime.last_y = point.1;
        runtime.edge_armed = false;
        runtime.crossing_blocked_until = now_ms() + EDGE_TRANSITION_COOLDOWN_MS;
        drop(runtime);
        self.inner.reconcile_source_cursor_capture();
        self.inject(absolute_move(point.0, point.1));
        self.inner
            .logger
            .warn("mouse_session_cancelled", "reason=peer_unresponsive");
    }

    pub fn force_stop(&self) {
        self.inner.enabled.store(false, Ordering::Release);
        let mut runtime = self.inner.runtime.lock().expect("mouse runtime lock");
        runtime.outgoing.take();
        self.inner.injector.invalidate();
        let releases = runtime
            .retire_incoming()
            .map(|incoming| release_held_input(&incoming))
            .unwrap_or_default();
        for event in releases {
            self.inject(event);
        }
        drop(runtime);
        self.inner.reconcile_source_cursor_capture();
    }

    pub fn apply_remote(&self, peer_id: &str, signal: MouseSignal) -> Vec<OutboundMouseSignal> {
        let mut responses = Vec::new();
        if !self.inner.enabled.load(Ordering::Acquire) {
            return responses;
        }
        let bounds = self.inner.desktop_bounds();
        let (width, height) = (bounds.width, bounds.height);
        let mut runtime = self.inner.runtime.lock().expect("mouse runtime lock");
        let mut simulated_events = Vec::new();
        let mut source_ownership_changed = false;
        match signal {
            MouseSignal::Enter {
                session_id,
                entry_edge,
                ratio,
                sent_at,
            } => {
                if runtime
                    .retired_sessions
                    .iter()
                    .any(|(peer, id)| peer == peer_id && id == &session_id)
                {
                    responses.push(outbound(peer_id, MouseSignal::Cancel { session_id }));
                    return responses;
                }
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
                let physical_input_age =
                    now_ms().saturating_sub(self.inner.last_physical_at.load(Ordering::Relaxed));
                if physical_input_age < PHYSICAL_INPUT_PRIORITY_MS {
                    self.inner.logger.info(
                        "mouse_remote_enter_rejected",
                        format!("reason=local_physical_input age_ms={physical_input_age}"),
                    );
                    responses.push(outbound(peer_id, MouseSignal::Cancel { session_id }));
                    return responses;
                }
                if let Some(previous) = runtime.retire_incoming() {
                    simulated_events.extend(release_held_input(&previous));
                    responses.push(outbound(
                        &previous.peer_id,
                        MouseSignal::Cancel {
                            session_id: previous.session_id,
                        },
                    ));
                    self.inner.logger.info(
                        "mouse_control_preempted",
                        format!("new_controller={peer_id}"),
                    );
                }
                if let Some(previous) = runtime.outgoing.take() {
                    responses.push(outbound(
                        &previous.peer_id,
                        MouseSignal::Cancel {
                            session_id: previous.session_id,
                        },
                    ));
                    source_ownership_changed = true;
                }
                let (x, y) = edge_point(entry_edge, ratio, width, height);
                let receive_dpi = runtime
                    .receive_dpi
                    .iter()
                    .find(|(configured_peer, _)| configured_peer == peer_id)
                    .map(|(_, dpi)| *dpi)
                    .unwrap_or(500);
                self.inner.injector.invalidate();
                runtime.edge_armed = false;
                runtime.takeover_delta = (0, 0);
                runtime.takeover_started_at = now_ms();
                runtime.incoming = Some(IncomingSession {
                    peer_id: peer_id.to_string(),
                    session_id: session_id.clone(),
                    return_edge: entry_edge,
                    x_milli: i64::from(x) * LOGICAL_PIXEL_MILLI,
                    y_milli: i64::from(y) * LOGICAL_PIXEL_MILLI,
                    receive_dpi,
                    last_injected_x: x,
                    last_injected_y: y,
                    last_move_sequence: 0,
                    last_gap_log_at: 0,
                    last_total_x_milli: 0,
                    last_total_y_milli: 0,
                    scroll_x_milli: 0,
                    scroll_y_milli: 0,
                    last_scroll_sequence: 0,
                    last_total_scroll_x_milli: 0,
                    last_total_scroll_y_milli: 0,
                    last_keep_alive_at: now_ms(),
                    return_armed: false,
                    return_push_milli: 0,
                    held_buttons: [false; 3],
                    held_keys: HashSet::new(),
                    last_event_at: now_ms(),
                    last_input_at: now_ms(),
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
                    format!("edge={entry_edge:?} ratio={ratio:.3} dpi={receive_dpi}"),
                );
            }
            MouseSignal::Move {
                session_id,
                sequence,
                total_x_milli,
                total_y_milli,
            } => {
                let Some(incoming) = runtime.incoming.as_mut() else {
                    responses.push(outbound(peer_id, MouseSignal::Cancel { session_id }));
                    return responses;
                };
                if incoming.peer_id != peer_id || incoming.session_id != session_id {
                    responses.push(outbound(peer_id, MouseSignal::Cancel { session_id }));
                    return responses;
                }
                if sequence <= incoming.last_move_sequence {
                    return responses;
                }
                let is_first_move = incoming.last_move_sequence == 0;
                let gap_ms = now_ms().saturating_sub(incoming.last_event_at);
                if !is_first_move
                    && (80..1_000).contains(&gap_ms)
                    && now_ms().saturating_sub(incoming.last_gap_log_at) >= 1_000
                {
                    incoming.last_gap_log_at = now_ms();
                    self.inner.logger.info(
                        "mouse_receive_gap",
                        format!(
                            "gap_ms={gap_ms} sequence_gap={}",
                            sequence.saturating_sub(incoming.last_move_sequence)
                        ),
                    );
                }
                incoming.last_event_at = now_ms();
                incoming.last_input_at = incoming.last_event_at;
                let delta_x_milli = scale_receive_delta(
                    total_x_milli.saturating_sub(incoming.last_total_x_milli),
                    incoming.receive_dpi,
                );
                let delta_y_milli = scale_receive_delta(
                    total_y_milli.saturating_sub(incoming.last_total_y_milli),
                    incoming.receive_dpi,
                );
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
                if is_first_move {
                    self.inner.logger.info(
                        "mouse_incoming_first_move",
                        format!(
                            "delta_x_milli={delta_x_milli} delta_y_milli={delta_y_milli} dpi={} next_x={next_x} next_y={next_y}",
                            incoming.receive_dpi
                        ),
                    );
                }
                if distance_from_edge(incoming.return_edge, next_x, next_y, width, height)
                    >= RETURN_ARM_DISTANCE_PIXELS
                {
                    incoming.return_armed = true;
                }
                // Control is handed back only when the remote controller
                // keeps pushing against the return edge: outward deltas
                // accumulate while the cursor is pinned at the edge, and any
                // inward motion resets the gesture.  Single-frame jitter can
                // no longer cancel the session.
                let outward_milli = match incoming.return_edge {
                    ScreenPosition::Right => delta_x_milli.max(0),
                    ScreenPosition::Left => (-delta_x_milli).max(0),
                    ScreenPosition::Down => delta_y_milli.max(0),
                    ScreenPosition::Up => (-delta_y_milli).max(0),
                };
                let pinned_at_return_edge =
                    distance_from_edge(incoming.return_edge, next_x, next_y, width, height) == 0;
                if pinned_at_return_edge && outward_milli > 0 {
                    incoming.return_push_milli =
                        incoming.return_push_milli.saturating_add(outward_milli);
                } else {
                    incoming.return_push_milli = 0;
                }
                if incoming.return_armed
                    && incoming.return_push_milli >= RETURN_PUSH_THRESHOLD_MILLI
                {
                    let ratio = edge_ratio(incoming.return_edge, next_x, next_y, width, height);
                    let session_id = incoming.session_id.clone();
                    simulated_events.extend(release_held_input(incoming));
                    runtime.retire_incoming();
                    self.inner.injector.invalidate();
                    responses.push(outbound(peer_id, MouseSignal::Return { session_id, ratio }));
                    self.inner
                        .logger
                        .info("mouse_remote_return", format!("ratio={ratio:.3}"));
                } else {
                    incoming.x_milli = next_x_milli;
                    incoming.y_milli = next_y_milli;
                    if next_x != incoming.last_injected_x || next_y != incoming.last_injected_y {
                        incoming.last_injected_x = next_x;
                        incoming.last_injected_y = next_y;
                        simulated_events.push(absolute_move(next_x, next_y));
                    }
                }
            }
            MouseSignal::Button {
                session_id,
                button,
                pressed,
            } => {
                if let Some(incoming) = matching_incoming_mut(&mut runtime, peer_id, &session_id) {
                    incoming.last_event_at = now_ms();
                    incoming.last_input_at = incoming.last_event_at;
                    let x = milli_to_pixel(incoming.x_milli);
                    let y = milli_to_pixel(incoming.y_milli);
                    if x != incoming.last_injected_x || y != incoming.last_injected_y {
                        incoming.last_injected_x = x;
                        incoming.last_injected_y = y;
                        simulated_events.push(absolute_move(x, y));
                    }
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
                    incoming.last_input_at = incoming.last_event_at;
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
            MouseSignal::Key {
                session_id,
                key,
                pressed,
            } => {
                if let Some(incoming) = matching_incoming_mut(&mut runtime, peer_id, &session_id) {
                    incoming.last_event_at = now_ms();
                    incoming.last_input_at = incoming.last_event_at;
                    if pressed {
                        incoming.held_keys.insert(key);
                    } else {
                        incoming.held_keys.remove(&key);
                    }
                    simulated_events.push(HookMouseEvent::Key { key, pressed });
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
                source_ownership_changed = true;
                runtime.last_x = point.0;
                runtime.last_y = point.1;
                runtime.edge_armed = false;
                runtime.crossing_blocked_until = now_ms() + EDGE_TRANSITION_COOLDOWN_MS;
                simulated_events.push(absolute_move(point.0, point.1));
                self.inner.logger.info(
                    "mouse_outgoing_return_received",
                    format!("reason=remote_crossed_return_edge ratio={ratio:.3}"),
                );
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
                    self.inner
                        .logger
                        .warn("mouse_outgoing_cancelled", "reason=remote_cancel");
                    runtime.outgoing = None;
                    source_ownership_changed = true;
                    let point =
                        safe_source_point(exit_edge, runtime.last_x, runtime.last_y, width, height);
                    runtime.last_x = point.0;
                    runtime.last_y = point.1;
                    runtime.edge_armed = false;
                    runtime.crossing_blocked_until = now_ms() + EDGE_TRANSITION_COOLDOWN_MS;
                    simulated_events.push(absolute_move(point.0, point.1));
                }
                if runtime.incoming.as_ref().is_some_and(|session| {
                    session.peer_id == peer_id && session.session_id == session_id
                }) {
                    if let Some(incoming) = runtime.retire_incoming() {
                        self.inner.injector.invalidate();
                        simulated_events.extend(release_held_input(&incoming));
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
                    if !was_acknowledged {
                        self.inner
                            .logger
                            .info("mouse_outgoing_ack", format!("latency_ms={latency}"));
                    }
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
                if let Some(incoming) = matching_incoming_mut(&mut runtime, peer_id, &session_id) {
                    incoming.last_event_at = now_ms();
                } else if !runtime
                    .outgoing
                    .as_ref()
                    .is_some_and(|s| s.peer_id == peer_id && s.session_id == session_id)
                {
                    responses.push(outbound(peer_id, MouseSignal::Cancel { session_id }));
                    return responses;
                }
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
        // Queue incoming frames under the state lock, so local takeover cannot
        // invalidate the session and then receive a late enqueue from this frame.
        let injection_generation = self.inner.injector.generation.load(Ordering::Acquire);
        if source_ownership_changed {
            drop(runtime);
            self.inner.reconcile_source_cursor_capture();
            runtime = self.inner.runtime.lock().expect("mouse runtime lock");
        }
        for event in simulated_events {
            if injection_generation != self.inner.injector.generation.load(Ordering::Acquire)
                && !is_release(event)
            {
                continue;
            }
            if !matches!(event, HookMouseEvent::Move { .. }) || runtime.outgoing.is_none() {
                self.inject(event);
            }
        }
        drop(runtime);
        responses
    }

    fn ensure_listener_started(&self) {
        let start_mouse = self
            .inner
            .listener_attempted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if start_mouse {
            self.inner.listener_started.store(true, Ordering::Release);
            let inner = Arc::clone(&self.inner);
            if let Err(error) = std::thread::Builder::new()
                .name("crosscopy-mouse-hook".into())
                .spawn(move || {
                    inner
                        .logger
                        .info("mouse_listener_started", "provider=native_mouse");
                    let callback_inner = Arc::clone(&inner);
                    if let Err(error) =
                        run_mouse_hook(move |event| callback_inner.handle_local_event(event))
                    {
                        inner.listener_started.store(false, Ordering::Release);
                        inner
                            .logger
                            .error("mouse_listener_failed", format!("{error:?}"));
                    }
                    inner.focus_local();
                    inner.listener_started.store(false, Ordering::Release);
                    inner.listener_attempted.store(false, Ordering::Release);
                })
            {
                self.inner
                    .listener_attempted
                    .store(false, Ordering::Release);
                self.inner.listener_started.store(false, Ordering::Release);
                self.inner
                    .logger
                    .error("mouse_listener_thread_failed", error.to_string());
            }
        }

        let start_keyboard = self
            .inner
            .keyboard_listener_attempted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if start_keyboard {
            self.inner
                .keyboard_listener_started
                .store(true, Ordering::Release);
            let keyboard_inner = Arc::clone(&self.inner);
            if let Err(error) = std::thread::Builder::new()
                .name("crosscopy-keyboard-hook".into())
                .spawn(move || {
                    keyboard_inner
                        .logger
                        .info("keyboard_listener_started", "provider=native_keyboard");
                    let callback_inner = Arc::clone(&keyboard_inner);
                    if let Err(error) = run_keyboard_hook(move |key, pressed| {
                        callback_inner.handle_local_key_event(key, pressed)
                    }) {
                        keyboard_inner
                            .keyboard_listener_started
                            .store(false, Ordering::Release);
                        keyboard_inner
                            .logger
                            .error("keyboard_listener_failed", error);
                    }
                    keyboard_inner.focus_local();
                    keyboard_inner
                        .keyboard_listener_started
                        .store(false, Ordering::Release);
                    keyboard_inner
                        .keyboard_listener_attempted
                        .store(false, Ordering::Release);
                })
            {
                self.inner
                    .keyboard_listener_attempted
                    .store(false, Ordering::Release);
                self.inner
                    .keyboard_listener_started
                    .store(false, Ordering::Release);
                self.inner
                    .logger
                    .error("keyboard_listener_thread_failed", error.to_string());
            }
        }
    }

    fn inject(&self, event: HookMouseEvent) {
        self.inner.inject(event);
    }

    fn start_session_maintenance(self: &Arc<Self>) {
        let mouse_share = Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("crosscopy-mouse-maintenance".into())
            .spawn(move || {
                let mut high_priority = false;
                let mut last_listener_retry = 0;
                loop {
                    let Some(mouse_share) = mouse_share.upgrade() else {
                        return;
                    };
                    let extreme = mouse_share
                        .inner
                        .extreme_performance
                        .load(Ordering::Acquire);
                    if extreme != high_priority {
                        set_realtime_priority(extreme);
                        high_priority = extreme;
                    }
                    // Flush the trailing throttled move within a frame while
                    // active; a fixed 50 ms tick made slow motion visibly step.
                    // Idle sessions retain the inexpensive maintenance tick.
                    let delay = if mouse_share.session_active() {
                        if extreme {
                            EXTREME_MOVE_SEND_INTERVAL_MS
                        } else {
                            BALANCED_MOVE_SEND_INTERVAL_MS
                        }
                    } else {
                        SESSION_MAINTENANCE_INTERVAL_MS
                    };
                    std::thread::sleep(std::time::Duration::from_millis(delay));
                    mouse_share.expire_unresponsive_outgoing();

                    let now = now_ms();
                    if now.saturating_sub(last_listener_retry) >= 1_000 {
                        last_listener_retry = now;
                        if mouse_share.inner.enabled.load(Ordering::Acquire) {
                            mouse_share.ensure_listener_started();
                        }
                        // Retry failed native capture/release transitions; a
                        // transient OS failure must not leave a hidden cursor.
                        mouse_share.inner.reconcile_source_cursor_capture();
                    }
                    let mut runtime = mouse_share
                        .inner
                        .runtime
                        .lock()
                        .expect("mouse runtime lock");
                    let mut signals = Vec::new();
                    let mut releases = Vec::new();
                    if let Some(outgoing) = runtime.outgoing.as_mut() {
                        if outgoing.acknowledged
                            && now.saturating_sub(outgoing.last_enter_retry_at) >= KEEP_ALIVE_MS
                        {
                            outgoing.last_enter_retry_at = now;
                            signals.push(outbound(
                                &outgoing.peer_id,
                                MouseSignal::KeepAlive {
                                    session_id: outgoing.session_id.clone(),
                                },
                            ));
                        }
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
                            && now.saturating_sub(outgoing.last_move_sent_at)
                                >= if extreme {
                                    EXTREME_MOVE_SEND_INTERVAL_MS
                                } else {
                                    BALANCED_MOVE_SEND_INTERVAL_MS
                                }
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
                                || outgoing.total_scroll_y_milli
                                    != outgoing.last_sent_scroll_y_milli)
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
                    if let Some(incoming) = runtime.expire_incoming(now) {
                        mouse_share.inner.injector.invalidate();
                        releases.extend(release_held_input(&incoming));
                        signals.push(outbound(
                            &incoming.peer_id,
                            MouseSignal::Cancel {
                                session_id: incoming.session_id,
                            },
                        ));
                        mouse_share
                            .inner
                            .logger
                            .warn("mouse_incoming_expired", "reason=controller_unresponsive");
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
                    }
                    releases.extend(runtime.release_idle_input(now));
                    let released_any = !releases.is_empty();
                    for event in releases {
                        mouse_share.inject(event);
                    }
                    drop(runtime);
                    for signal in signals {
                        let _ = mouse_share.inner.outbound.send(signal);
                    }
                    if released_any {
                        mouse_share
                            .inner
                            .logger
                            .warn("remote_input_safety_released", "reason=remote_session_idle");
                    }
                }
            });
    }
}

impl Inner {
    fn desktop_bounds(&self) -> DesktopBounds {
        *self.bounds.lock().expect("desktop bounds lock")
    }

    fn reconcile_source_cursor_capture(&self) {
        if let Err(error) = self.reconcile_source_cursor_capture_result() {
            self.logger.warn("mouse_source_capture_failed", error);
        }
    }

    fn reconcile_source_cursor_capture_result(&self) -> Result<(), String> {
        let runtime = self.runtime.lock().expect("mouse runtime lock");
        let captured = runtime.outgoing.is_some();
        let previous = self.source_control_active.load(Ordering::Acquire);
        if captured {
            self.source_control_active.store(true, Ordering::Release);
        }
        if let Err(error) = set_source_cursor_captured(captured) {
            self.source_control_active
                .store(previous, Ordering::Release);
            return Err(format!("captured={captured} error={error}"));
        }
        self.source_control_active
            .store(captured, Ordering::Release);
        if previous != captured {
            self.logger.info(
                "mouse_source_capture_changed",
                format!("captured={captured}"),
            );
        }
        Ok(())
    }

    fn switch_to_peer(&self, peer_id: String, position: ScreenPosition) -> Result<(), String> {
        if !self.enabled.load(Ordering::Acquire) {
            return Err("请先开启鼠标与键盘共享".into());
        }
        let bounds = self.desktop_bounds();
        let (width, height) = (bounds.width, bounds.height);
        let mut runtime = self.runtime.lock().expect("mouse runtime lock");
        if !runtime
            .targets
            .iter()
            .any(|target| target.peer_id == peer_id)
        {
            return Err("目标屏幕当前不可用".into());
        }
        if let Some(previous) = runtime.outgoing.take() {
            let _ = self.outbound.send(outbound(
                &previous.peer_id,
                MouseSignal::Cancel {
                    session_id: previous.session_id,
                },
            ));
        }
        let releases = if let Some(previous) = runtime.retire_incoming() {
            let _ = self.outbound.send(outbound(
                &previous.peer_id,
                MouseSignal::Cancel {
                    session_id: previous.session_id.clone(),
                },
            ));
            release_held_input(&previous)
        } else {
            Vec::new()
        };
        self.injector.invalidate();
        runtime.edge_armed = false;
        let session_id = Uuid::new_v4().to_string();
        let ratio = edge_ratio(position, runtime.last_x, runtime.last_y, width, height);
        let sent_at = now_ms();
        let anchor_x = width / 2;
        let anchor_y = height / 2;
        runtime.outgoing = Some(new_outgoing_session(
            peer_id.clone(),
            session_id.clone(),
            position,
            ratio,
            sent_at,
            anchor_x,
            anchor_y,
        ));
        for event in releases {
            self.inject(event);
        }
        drop(runtime);
        if let Err(error) = self.reconcile_source_cursor_capture_result() {
            self.focus_local();
            return Err(error);
        }
        if let Err(error) = recenter_cursor(anchor_x, anchor_y, bounds) {
            self.focus_local();
            return Err(error);
        }
        let _ = self.outbound.send(outbound(
            &peer_id,
            MouseSignal::Enter {
                session_id,
                entry_edge: position.opposite(),
                ratio,
                sent_at,
            },
        ));
        self.logger.info(
            "mouse_screen_switched",
            format!("target={peer_id} position={position:?}"),
        );
        Ok(())
    }

    fn focus_local(&self) {
        let mut runtime = self.runtime.lock().expect("mouse runtime lock");
        let outgoing = runtime.outgoing.take();
        let incoming = runtime.retire_incoming();
        self.injector.invalidate();
        if let Some(session) = &outgoing {
            let _ = self.outbound.send(outbound(
                &session.peer_id,
                MouseSignal::Cancel {
                    session_id: session.session_id.clone(),
                },
            ));
        }
        if let Some(session) = &incoming {
            let _ = self.outbound.send(outbound(
                &session.peer_id,
                MouseSignal::Cancel {
                    session_id: session.session_id.clone(),
                },
            ));
        }
        let releases = incoming
            .as_ref()
            .map(release_held_input)
            .unwrap_or_default();
        runtime.edge_armed = false;
        runtime.crossing_blocked_until = now_ms() + EDGE_TRANSITION_COOLDOWN_MS;
        for event in releases {
            self.inject(event);
        }
        drop(runtime);
        self.reconcile_source_cursor_capture();
        if outgoing.is_some() {
            let bounds = self.desktop_bounds();
            self.inject(absolute_move(bounds.width / 2, bounds.height / 2));
        }
        self.logger.info("mouse_screen_switched", "target=local");
    }

    fn take_over_incoming(&self, runtime: &mut Runtime, now: u64) {
        if let Some(incoming) = runtime.retire_incoming() {
            self.last_physical_at.store(now, Ordering::Relaxed);
            runtime.crossing_blocked_until = now + EDGE_TRANSITION_COOLDOWN_MS;
            self.injector.invalidate();
            for event in release_held_input(&incoming) {
                self.inject(event);
            }
            let _ = self.outbound.send(outbound(
                &incoming.peer_id,
                MouseSignal::Cancel {
                    session_id: incoming.session_id,
                },
            ));
            self.logger.info(
                "mouse_local_takeover",
                "reason=physical_input edge_rearm_required=true",
            );
        }
    }

    fn handle_local_event(&self, event: HookMouseEvent) -> bool {
        let now = now_ms();
        if !self.enabled.load(Ordering::Acquire) {
            return false;
        }

        let bounds = self.desktop_bounds();
        let (width, height) = (bounds.width, bounds.height);
        let event = localize_move(event, bounds);
        let mut runtime = self.runtime.lock().expect("mouse runtime lock");
        if runtime.incoming.is_some() {
            if runtime.local_intent(event, now) {
                self.take_over_incoming(&mut runtime, now);
                if let HookMouseEvent::Move { x, y, .. } = event {
                    runtime.last_x = x;
                    runtime.last_y = y;
                }
                return false;
            }
            return true;
        }

        self.last_physical_at.store(now, Ordering::Relaxed);

        if let Some(outgoing) = runtime.outgoing.as_mut() {
            let peer_id = outgoing.peer_id.clone();
            let session_id = outgoing.session_id.clone();
            match event {
                HookMouseEvent::Move { x, y, native_delta } => {
                    let (raw_delta_x, raw_delta_y) =
                        native_delta.unwrap_or((x - outgoing.anchor_x, y - outgoing.anchor_y));
                    let (delta_x, delta_y) = (
                        clamp_physical_delta(raw_delta_x),
                        clamp_physical_delta(raw_delta_y),
                    );
                    // Suppressed Windows movement never updates the OS cursor.
                    // Keep the capture anchor fixed; the hook provides each
                    // physical delta relative to the actual cursor position.
                    if delta_x != 0 || delta_y != 0 {
                        let gap_ms = now.saturating_sub(outgoing.last_input_at);
                        if outgoing.last_input_at != 0
                            && (80..1_000).contains(&gap_ms)
                            && now.saturating_sub(outgoing.last_gap_log_at) >= 1_000
                        {
                            outgoing.last_gap_log_at = now;
                            self.logger
                                .info("mouse_source_gap", format!("gap_ms={gap_ms}"));
                        }
                        outgoing.last_input_at = now;
                        if !outgoing.first_move_logged {
                            outgoing.first_move_logged = true;
                            self.logger.info(
                                "mouse_outgoing_first_move",
                                format!("delta_x={delta_x} delta_y={delta_y}"),
                            );
                        }
                        outgoing.total_x_milli = outgoing
                            .total_x_milli
                            .saturating_add(scaled_pointer_delta(delta_x));
                        outgoing.total_y_milli = outgoing
                            .total_y_milli
                            .saturating_add(scaled_pointer_delta(delta_y));
                        let extreme = self.extreme_performance.load(Ordering::Acquire);
                        let send_interval = if extreme {
                            0
                        } else {
                            BALANCED_MOVE_SEND_INTERVAL_MS
                        };
                        let now = now_ms();
                        if outgoing.acknowledged
                            && now.saturating_sub(outgoing.last_move_sent_at) >= send_interval
                        {
                            outgoing.move_sequence = outgoing.move_sequence.saturating_add(1);
                            let signal = outbound(
                                &peer_id,
                                MouseSignal::Move {
                                    session_id: session_id.clone(),
                                    sequence: outgoing.move_sequence,
                                    total_x_milli: outgoing.total_x_milli,
                                    total_y_milli: outgoing.total_y_milli,
                                },
                            );
                            if self.outbound.send(signal).is_ok() {
                                outgoing.last_move_sent_at = now;
                                outgoing.last_sent_x_milli = outgoing.total_x_milli;
                                outgoing.last_sent_y_milli = outgoing.total_y_milli;
                            }
                        }
                    }
                }
                HookMouseEvent::Button { button, pressed } => {
                    if outgoing.acknowledged {
                        let _ = self.outbound.send(outbound(
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
                            .send(outbound(
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
            drop(runtime);
            if let Err(error) = ensure_source_cursor_captured() {
                self.logger.warn("mouse_cursor_guard_failed", error);
            }
            return true;
        }

        if let HookMouseEvent::Move { x, y, .. } = event {
            let previous_x = runtime.last_x;
            let previous_y = runtime.last_y;
            runtime.last_x = x;
            runtime.last_y = y;
            if x >= EDGE_REARM_PIXELS
                && x < width - EDGE_REARM_PIXELS
                && y >= EDGE_REARM_PIXELS
                && y < height - EDGE_REARM_PIXELS
            {
                runtime.edge_armed = true;
            }
            let target = runtime
                .targets
                .iter()
                .find(|target| {
                    reached_exit_edge(target.position, x, y, previous_x, previous_y, width, height)
                })
                .cloned();
            if let Some(target) = target {
                if runtime.edge_armed && now_ms() >= runtime.crossing_blocked_until {
                    let peer_id = target.peer_id;
                    let position = target.position;
                    runtime.local_held_keys.clear();
                    runtime.suppressed_shortcut_keys.clear();
                    let session_id = Uuid::new_v4().to_string();
                    let ratio = edge_ratio(position, x, y, width, height);
                    let anchor_x = width / 2;
                    let anchor_y = height / 2;
                    let sent_at = now_ms();
                    runtime.edge_armed = false;
                    self.injector.invalidate();
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
                        first_move_logged: false,
                        last_input_at: 0,
                        last_gap_log_at: 0,
                        scroll_sequence: 0,
                        total_scroll_x_milli: 0,
                        total_scroll_y_milli: 0,
                        last_sent_scroll_x_milli: 0,
                        last_sent_scroll_y_milli: 0,
                        last_remote_at: sent_at,
                    });
                    let enter = outbound(
                        &peer_id,
                        MouseSignal::Enter {
                            session_id,
                            entry_edge: position.opposite(),
                            ratio,
                            sent_at,
                        },
                    );
                    self.logger.info(
                        "mouse_outgoing_enter",
                        format!("edge={position:?} ratio={ratio:.3}"),
                    );
                    drop(runtime);
                    if let Err(error) = self.reconcile_source_cursor_capture_result() {
                        self.logger.warn("mouse_cursor_hide_failed", error);
                        self.focus_local();
                        return true;
                    }
                    if let Err(error) = recenter_cursor(anchor_x, anchor_y, bounds) {
                        self.logger.warn("mouse_cursor_recenter_failed", error);
                        self.focus_local();
                        return true;
                    }
                    let _ = self.outbound.send(enter);
                    return true;
                }
            }
        }
        false
    }

    fn handle_local_key_event(&self, key: HookKey, pressed: bool) -> bool {
        if !self.enabled.load(Ordering::Acquire) {
            return false;
        }
        let mut runtime = self.runtime.lock().expect("mouse runtime lock");
        if runtime.incoming.is_some() {
            if pressed {
                self.take_over_incoming(&mut runtime, now_ms());
            } else {
                return true;
            }
        }
        if runtime.outgoing.is_none() {
            runtime.local_held_keys.clear();
            runtime.suppressed_shortcut_keys.clear();
            return false;
        }
        if runtime.suppressed_shortcut_keys.contains(&key) {
            if !pressed {
                runtime.local_held_keys.remove(&key);
                runtime.suppressed_shortcut_keys.remove(&key);
            }
            return true;
        }
        if pressed {
            runtime.local_held_keys.insert(key);
        } else {
            runtime.local_held_keys.remove(&key);
        }
        let shortcut_screen = pressed
            .then(|| shortcut_screen_number(key))
            .flatten()
            .filter(|_| {
                runtime.local_held_keys.iter().any(|held| is_control(*held))
                    && runtime.local_held_keys.iter().any(|held| is_alt(*held))
            });
        if let Some(screen_number) = shortcut_screen {
            let target = (screen_number != 1)
                .then(|| {
                    runtime
                        .targets
                        .iter()
                        .find(|target| target.screen_number == screen_number)
                        .cloned()
                })
                .flatten();
            let suppressed = runtime
                .local_held_keys
                .iter()
                .copied()
                .filter(|held| is_control(*held) || is_alt(*held))
                .collect::<Vec<_>>();
            runtime.suppressed_shortcut_keys.extend(suppressed);
            runtime.suppressed_shortcut_keys.insert(key);
            drop(runtime);
            let result = if screen_number == 1 {
                self.focus_local();
                Ok(())
            } else if let Some(target) = target {
                self.switch_to_peer(target.peer_id, target.position)
            } else {
                Err("目标屏幕当前不可用".to_string())
            };
            if let Err(error) = result {
                self.logger.warn(
                    "keyboard_screen_shortcut_failed",
                    format!("screen={screen_number} error={error}"),
                );
            }
            return true;
        }
        let Some(outgoing) = runtime.outgoing.as_ref() else {
            return true;
        };
        if !outgoing.acknowledged {
            return true;
        }
        let peer_id = outgoing.peer_id.clone();
        let session_id = outgoing.session_id.clone();
        drop(runtime);
        let _ = self.outbound.send(outbound(
            &peer_id,
            MouseSignal::Key {
                session_id,
                key,
                pressed,
            },
        ));
        true
    }

    fn inject(&self, event: HookMouseEvent) {
        let event = globalize_move(event, self.desktop_bounds());
        self.injector.push(event);
    }
}

fn outbound(peer_id: &str, signal: MouseSignal) -> OutboundMouseSignal {
    OutboundMouseSignal {
        peer_id: peer_id.to_string(),
        signal,
    }
}

fn new_outgoing_session(
    peer_id: String,
    session_id: String,
    exit_edge: ScreenPosition,
    enter_ratio: f64,
    sent_at: u64,
    anchor_x: i32,
    anchor_y: i32,
) -> OutgoingSession {
    OutgoingSession {
        peer_id,
        session_id,
        exit_edge,
        anchor_x,
        anchor_y,
        enter_ratio,
        last_enter_retry_at: sent_at,
        acknowledged: false,
        move_sequence: 0,
        total_x_milli: 0,
        total_y_milli: 0,
        last_move_sent_at: 0,
        last_sent_x_milli: 0,
        last_sent_y_milli: 0,
        first_move_logged: false,
        last_input_at: 0,
        last_gap_log_at: 0,
        scroll_sequence: 0,
        total_scroll_x_milli: 0,
        total_scroll_y_milli: 0,
        last_sent_scroll_x_milli: 0,
        last_sent_scroll_y_milli: 0,
        last_remote_at: sent_at,
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

fn release_held_keys(session: &IncomingSession) -> Vec<HookMouseEvent> {
    session
        .held_keys
        .iter()
        .copied()
        .map(|key| HookMouseEvent::Key {
            key,
            pressed: false,
        })
        .collect()
}

fn release_held_input(session: &IncomingSession) -> Vec<HookMouseEvent> {
    let mut events = release_held_buttons(session);
    events.extend(release_held_keys(session));
    events
}

fn button_index(button: SharedMouseButton) -> usize {
    match button {
        SharedMouseButton::Left => 0,
        SharedMouseButton::Right => 1,
        SharedMouseButton::Middle => 2,
    }
}

fn shortcut_screen_number(key: HookKey) -> Option<u8> {
    let HookKey::Character(value @ '1'..='9') = key else {
        return None;
    };
    value.to_digit(10).map(|value| value as u8)
}

fn is_control(key: HookKey) -> bool {
    matches!(key, HookKey::LeftControl | HookKey::RightControl)
}

fn is_alt(key: HookKey) -> bool {
    matches!(key, HookKey::LeftAlt | HookKey::RightAlt)
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

fn distance_from_edge(edge: ScreenPosition, x: i32, y: i32, width: i32, height: i32) -> i32 {
    match edge {
        ScreenPosition::Left => x,
        ScreenPosition::Right => width - 1 - x,
        ScreenPosition::Up => y,
        ScreenPosition::Down => height - 1 - y,
    }
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
        ScreenPosition::Left => x <= 0 && x < previous_x,
        ScreenPosition::Right => x >= width - 1 && x > previous_x,
        ScreenPosition::Up => y <= 0 && y < previous_y,
        ScreenPosition::Down => y >= height - 1 && y > previous_y,
    }
}

fn absolute_move(x: i32, y: i32) -> HookMouseEvent {
    HookMouseEvent::Move {
        x,
        y,
        native_delta: None,
    }
}

fn localize_move(event: HookMouseEvent, bounds: DesktopBounds) -> HookMouseEvent {
    match event {
        HookMouseEvent::Move { x, y, native_delta } => HookMouseEvent::Move {
            x: x.saturating_sub(bounds.x),
            y: y.saturating_sub(bounds.y),
            native_delta,
        },
        other => other,
    }
}

fn globalize_move(event: HookMouseEvent, bounds: DesktopBounds) -> HookMouseEvent {
    match event {
        HookMouseEvent::Move { x, y, native_delta } => HookMouseEvent::Move {
            x: bounds.x.saturating_add(x),
            y: bounds.y.saturating_add(y),
            native_delta,
        },
        other => other,
    }
}

fn clamp_physical_delta(value: i32) -> i32 {
    value.clamp(-MAX_PHYSICAL_DELTA_PER_EVENT, MAX_PHYSICAL_DELTA_PER_EVENT)
}

fn scaled_pointer_delta(value: i32) -> i64 {
    let magnitude = value.unsigned_abs();
    let gain_milli = match magnitude {
        0..=2 => 1_000,
        3..=8 => 750,
        _ => 500,
    };
    i64::from(value) * gain_milli
}

fn scale_receive_delta(value: i64, dpi: u16) -> i64 {
    let scaled = i128::from(value) * i128::from(dpi) / 500;
    scaled.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
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
    let result = inject_mouse_event_impl(enigo, event);
    #[cfg(target_os = "macos")]
    if result.is_ok() {
        crate::mouse_hook::record_injected_event(event);
    }
    result
}

fn inject_mouse_event_impl(enigo: &mut Enigo, event: HookMouseEvent) -> Result<(), String> {
    match event {
        HookMouseEvent::Move { x, y, .. } => {
            // Injection-queue move events are already in global desktop
            // coordinates (Inner::inject globalizes them), so they are passed
            // to the platform cursor APIs unchanged.
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            {
                crate::mouse_hook::move_cursor_absolute(x, y)
            }
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            {
                enigo
                    .move_mouse(x, y, Coordinate::Abs)
                    .map_err(|error| error.to_string())
            }
        }
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
        HookMouseEvent::Key { key, pressed } => {
            let Some(key) = key.to_enigo() else {
                return Ok(());
            };
            enigo
                .key(
                    key,
                    if pressed {
                        Direction::Press
                    } else {
                        Direction::Release
                    },
                )
                .map_err(|error| error.to_string())
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injection_coalesces_motion_without_reordering_buttons() {
        let queue = InjectionQueue::new();
        queue.push(absolute_move(1, 2));
        queue.push(absolute_move(3, 4));
        let button = HookMouseEvent::Button {
            button: HookMouseButton::Left,
            pressed: true,
        };
        queue.push(button);
        queue.push(absolute_move(5, 6));
        queue.push(absolute_move(7, 8));
        assert_eq!(queue.pop().event, absolute_move(3, 4));
        assert_eq!(queue.pop().event, button);
        assert_eq!(queue.pop().event, absolute_move(7, 8));
    }

    struct Fixture {
        share: MouseShare,
        receiver: mpsc::UnboundedReceiver<OutboundMouseSignal>,
        directory: std::path::PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
    fn fixture(outgoing: bool) -> Fixture {
        let directory = std::env::temp_dir().join(format!("crosscopy-test-{}", Uuid::new_v4()));
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut session = new_outgoing_session(
            "peer".into(),
            "session".into(),
            ScreenPosition::Right,
            0.5,
            now_ms(),
            500,
            400,
        );
        session.acknowledged = true;
        let inner = Inner {
            enabled: AtomicBool::new(true),
            extreme_performance: Arc::new(AtomicBool::new(true)),
            listener_attempted: AtomicBool::new(false),
            listener_started: AtomicBool::new(false),
            keyboard_listener_attempted: AtomicBool::new(false),
            keyboard_listener_started: AtomicBool::new(false),
            source_control_active: Arc::new(AtomicBool::new(outgoing)),
            latency_ms: AtomicU64::new(NO_LATENCY),
            last_physical_at: AtomicU64::new(0),
            runtime: Mutex::new(Runtime {
                targets: Vec::new(),
                receive_dpi: Vec::new(),
                last_x: 0,
                last_y: 0,
                crossing_blocked_until: 0,
                edge_armed: false,
                takeover_delta: (0, 0),
                takeover_started_at: 0,
                retired_sessions: VecDeque::new(),
                outgoing: outgoing.then_some(session),
                incoming: None,
                local_held_keys: HashSet::new(),
                suppressed_shortcut_keys: HashSet::new(),
            }),
            outbound: sender,
            injector: InjectionQueue::new(),
            logger: Arc::new(Logger::new(&directory).unwrap()),
            bounds: Mutex::new(DesktopBounds {
                x: 0,
                y: 0,
                width: 1000,
                height: 800,
            }),
        };
        Fixture {
            share: MouseShare {
                inner: Arc::new(inner),
            },
            receiver,
            directory,
        }
    }
    fn enter(share: &MouseShare) {
        let responses = share.apply_remote(
            "peer",
            MouseSignal::Enter {
                session_id: "incoming".into(),
                entry_edge: ScreenPosition::Left,
                ratio: 0.5,
                sent_at: now_ms(),
            },
        );
        assert!(matches!(responses[0].signal, MouseSignal::Ack { .. }));
    }

    #[test]
    fn repeated_suppressed_moves_keep_accumulating_from_fixed_anchor() {
        let mut fixture = fixture(true);
        let inner = &fixture.share.inner;
        let receiver = &mut fixture.receiver;
        for _ in 0..10 {
            assert!(inner.handle_local_event(HookMouseEvent::Move {
                x: 505,
                y: 400,
                native_delta: None
            }));
        }
        for expected in 1..=10 {
            let signal = receiver.try_recv().unwrap().signal;
            match signal {
                MouseSignal::Move { total_x_milli, .. } => {
                    assert_eq!(total_x_milli, expected * scaled_pointer_delta(5))
                }
                _ => panic!("expected motion"),
            }
        }
        assert_eq!(
            inner
                .runtime
                .lock()
                .unwrap()
                .outgoing
                .as_ref()
                .unwrap()
                .anchor_x,
            500
        );
    }
    #[test]
    fn local_click_revokes_remote_session_and_discards_old_motion() {
        let mut fixture = fixture(false);
        enter(&fixture.share);
        fixture.share.apply_remote(
            "peer",
            MouseSignal::Key {
                session_id: "incoming".into(),
                key: HookKey::LeftShift,
                pressed: true,
            },
        );
        assert!(!fixture
            .share
            .inner
            .handle_local_event(HookMouseEvent::Button {
                button: HookMouseButton::Left,
                pressed: true
            }));
        assert!(fixture
            .share
            .inner
            .runtime
            .lock()
            .unwrap()
            .incoming
            .is_none());
        assert!(matches!(
            fixture.receiver.try_recv().unwrap().signal,
            MouseSignal::Cancel { .. }
        ));
        let pending = fixture.share.inner.injector.pending.lock().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].event,
            HookMouseEvent::Key {
                key: HookKey::LeftShift,
                pressed: false
            }
        );
    }

    #[test]
    fn deliberate_local_motion_takes_over_but_balanced_sensor_jitter_does_not() {
        let fixture = fixture(false);
        enter(&fixture.share);
        for dx in [1, -1, 1, -1, 1, -1] {
            assert!(fixture
                .share
                .inner
                .handle_local_event(HookMouseEvent::Move {
                    x: 20,
                    y: 400,
                    native_delta: Some((dx, 0))
                }));
        }
        assert!(!fixture
            .share
            .inner
            .handle_local_event(HookMouseEvent::Move {
                x: 35,
                y: 400,
                native_delta: Some((16, 0))
            }));
        assert!(fixture
            .share
            .inner
            .runtime
            .lock()
            .unwrap()
            .incoming
            .is_none());
    }

    #[test]
    fn physical_keyboard_also_takes_back_control() {
        let fixture = fixture(false);
        enter(&fixture.share);
        assert!(!fixture
            .share
            .inner
            .handle_local_key_event(HookKey::Character('a'), true));
        assert!(fixture
            .share
            .inner
            .runtime
            .lock()
            .unwrap()
            .incoming
            .is_none());
    }

    #[test]
    fn retired_session_cannot_be_revived_by_a_delayed_enter() {
        let fixture = fixture(false);
        enter(&fixture.share);
        fixture
            .share
            .inner
            .handle_local_key_event(HookKey::Escape, true);
        let responses = fixture.share.apply_remote(
            "peer",
            MouseSignal::Enter {
                session_id: "incoming".into(),
                entry_edge: ScreenPosition::Left,
                ratio: 0.5,
                sent_at: now_ms(),
            },
        );
        assert!(matches!(responses[0].signal, MouseSignal::Cancel { .. }));
        assert!(fixture
            .share
            .inner
            .runtime
            .lock()
            .unwrap()
            .incoming
            .is_none());
        assert!(fixture
            .share
            .inner
            .runtime
            .lock()
            .unwrap()
            .retired_sessions
            .iter()
            .any(|(_, id)| id == "incoming"));
    }

    #[test]
    fn abandoned_stream_repeats_cancel_even_when_first_cancel_was_lost() {
        let fixture = fixture(false);
        for signal in [
            MouseSignal::Move {
                session_id: "old".into(),
                sequence: 5,
                total_x_milli: 1000,
                total_y_milli: 0,
            },
            MouseSignal::KeepAlive {
                session_id: "old".into(),
            },
        ] {
            let responses = fixture.share.apply_remote("peer", signal);
            assert!(
                matches!(&responses[0].signal, MouseSignal::Cancel { session_id } if session_id == "old")
            );
        }
    }

    #[test]
    fn edge_stays_disarmed_after_takeover_until_pointer_moves_inside() {
        let fixture = fixture(false);
        enter(&fixture.share);
        fixture
            .share
            .inner
            .handle_local_key_event(HookKey::Escape, true);
        {
            let mut runtime = fixture.share.inner.runtime.lock().unwrap();
            runtime.crossing_blocked_until = 0; // Time alone must never rearm it.
            runtime.targets.push(MouseTarget {
                peer_id: "peer".into(),
                position: ScreenPosition::Right,
                screen_number: 2,
            });
        }
        for _ in 0..20 {
            assert!(!fixture
                .share
                .inner
                .handle_local_event(HookMouseEvent::Move {
                    x: 999,
                    y: 400,
                    native_delta: Some((1, 0))
                }));
        }
        assert!(fixture
            .share
            .inner
            .runtime
            .lock()
            .unwrap()
            .outgoing
            .is_none());
        assert!(!fixture.share.inner.runtime.lock().unwrap().edge_armed);
        fixture
            .share
            .inner
            .handle_local_event(HookMouseEvent::Move {
                x: 900,
                y: 400,
                native_delta: Some((-99, 0)),
            });
        assert!(fixture.share.inner.runtime.lock().unwrap().edge_armed);
    }

    #[test]
    fn returning_cursor_is_queued_only_after_source_capture_is_released() {
        let fixture = fixture(true);
        fixture.share.apply_remote(
            "peer",
            MouseSignal::Return {
                session_id: "session".into(),
                ratio: 0.5,
            },
        );
        assert!(!fixture
            .share
            .inner
            .source_control_active
            .load(Ordering::Acquire));
        assert!(!fixture.share.inner.runtime.lock().unwrap().edge_armed);
        assert!(matches!(
            fixture.share.inner.injector.pop().event,
            HookMouseEvent::Move { .. }
        ));
    }

    #[test]
    fn fast_native_motion_is_not_silently_discarded() {
        let mut fixture = fixture(true);
        fixture
            .share
            .inner
            .handle_local_event(HookMouseEvent::Move {
                x: 500,
                y: 400,
                native_delta: Some((200, 0)),
            });
        assert!(
            matches!(fixture.receiver.try_recv().unwrap().signal, MouseSignal::Move { total_x_milli, .. } if total_x_milli == scaled_pointer_delta(200))
        );
    }

    #[test]
    fn queue_invalidation_preserves_releases_and_revokes_dequeued_motion() {
        let queue = InjectionQueue::new();
        queue.push(absolute_move(10, 20));
        let in_flight = queue.pop();
        queue.push(HookMouseEvent::Button {
            button: HookMouseButton::Left,
            pressed: false,
        });
        queue.push(HookMouseEvent::Key {
            key: HookKey::LeftShift,
            pressed: true,
        });
        queue.invalidate();
        assert_ne!(
            in_flight.generation,
            queue.generation.load(Ordering::Acquire)
        );
        assert!(is_release(queue.pop().event));
        assert!(queue.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn controller_heartbeat_refreshes_incoming_lease() {
        let fixture = fixture(false);
        enter(&fixture.share);
        fixture
            .share
            .inner
            .runtime
            .lock()
            .unwrap()
            .incoming
            .as_mut()
            .unwrap()
            .last_event_at = 0;
        fixture.share.apply_remote(
            "peer",
            MouseSignal::KeepAlive {
                session_id: "incoming".into(),
            },
        );
        assert!(
            fixture
                .share
                .inner
                .runtime
                .lock()
                .unwrap()
                .incoming
                .as_ref()
                .unwrap()
                .last_event_at
                > 0
        );
    }
    #[test]
    fn missing_controller_heartbeat_expires_and_retires_the_session() {
        let fixture = fixture(false);
        enter(&fixture.share);
        let mut runtime = fixture.share.inner.runtime.lock().unwrap();
        let last = runtime.incoming.as_ref().unwrap().last_event_at;
        assert!(runtime
            .expire_incoming(last + SESSION_TIMEOUT_MS - 1)
            .is_none());
        assert!(runtime.expire_incoming(last + SESSION_TIMEOUT_MS).is_some());
        assert!(runtime.incoming.is_none());
        assert!(!runtime.edge_armed);
        assert!(runtime
            .retired_sessions
            .iter()
            .any(|(_, id)| id == "incoming"));
    }

    #[test]
    fn resting_or_sliding_along_an_edge_is_not_an_outward_crossing() {
        assert!(!reached_exit_edge(
            ScreenPosition::Right,
            999,
            400,
            999,
            390,
            1000,
            800
        ));
        assert!(reached_exit_edge(
            ScreenPosition::Right,
            999,
            400,
            998,
            400,
            1000,
            800
        ));
        assert!(!reached_exit_edge(
            ScreenPosition::Left,
            0,
            400,
            0,
            390,
            1000,
            800
        ));
        assert!(reached_exit_edge(
            ScreenPosition::Left,
            0,
            400,
            1,
            400,
            1000,
            800
        ));
    }
    #[test]
    fn heartbeat_does_not_disable_lost_keyup_safety_release() {
        let fixture = fixture(false);
        enter(&fixture.share);
        fixture.share.apply_remote(
            "peer",
            MouseSignal::Key {
                session_id: "incoming".into(),
                key: HookKey::LeftShift,
                pressed: true,
            },
        );
        let last_input = fixture
            .share
            .inner
            .runtime
            .lock()
            .unwrap()
            .incoming
            .as_ref()
            .unwrap()
            .last_input_at;
        fixture.share.apply_remote(
            "peer",
            MouseSignal::KeepAlive {
                session_id: "incoming".into(),
            },
        );
        let mut runtime = fixture.share.inner.runtime.lock().unwrap();
        assert_eq!(runtime.incoming.as_ref().unwrap().last_input_at, last_input);
        assert_eq!(
            runtime.release_idle_input(last_input + HELD_INPUT_SAFETY_TIMEOUT_MS),
            vec![HookMouseEvent::Key {
                key: HookKey::LeftShift,
                pressed: false
            }]
        );
        assert!(runtime.incoming.as_ref().unwrap().held_keys.is_empty());
    }
}
