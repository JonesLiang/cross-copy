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
const PHYSICAL_TAKEOVER_WINDOW_MS: u64 = 150;
const PHYSICAL_TAKEOVER_DISTANCE: u32 = 12;
const HELD_INPUT_SAFETY_TIMEOUT_MS: u64 = 10_000;
const LOGICAL_PIXEL_MILLI: i64 = 1_000;
const MAX_PHYSICAL_DELTA_PER_EVENT: i32 = 256;
const MAX_NATIVE_DELTA_PER_EVENT: i32 = 128;
const ENTER_RETRY_MS: u64 = 120;
const SESSION_TIMEOUT_MS: u64 = 5_000;
const KEEP_ALIVE_MS: u64 = 1_000;
const EXTREME_MOVE_SEND_INTERVAL_MS: u64 = 2;
const BALANCED_MOVE_SEND_INTERVAL_MS: u64 = 4;
const SESSION_MAINTENANCE_INTERVAL_MS: u64 = 50;
const EDGE_INSET_PIXELS: i32 = 8;
#[cfg(target_os = "windows")]
const SOURCE_CURSOR_RECENTER_MARGIN: i32 = 64;
const RETURN_ARM_DISTANCE_PIXELS: i32 = 32;
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
    last_total_x_milli: i64,
    last_total_y_milli: i64,
    scroll_x_milli: i64,
    scroll_y_milli: i64,
    last_scroll_sequence: u64,
    last_total_scroll_x_milli: i64,
    last_total_scroll_y_milli: i64,
    last_keep_alive_at: u64,
    return_armed: bool,
    held_buttons: [bool; 3],
    held_keys: HashSet<HookKey>,
    last_event_at: u64,
    takeover_window_started: u64,
    takeover_distance: u32,
}

#[derive(Clone)]
struct MouseTarget {
    peer_id: String,
    position: ScreenPosition,
    screen_number: u8,
}

struct InjectionQueue {
    pending: Mutex<VecDeque<HookMouseEvent>>,
    ready: Condvar,
}

impl InjectionQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
        })
    }

    fn push(&self, event: HookMouseEvent) {
        let mut pending = self.pending.lock().expect("input injection queue lock");
        if matches!(event, HookMouseEvent::Move { .. })
            && pending
                .back()
                .is_some_and(|queued| matches!(queued, HookMouseEvent::Move { .. }))
        {
            if let Some(latest) = pending.back_mut() {
                *latest = event;
            }
        } else {
            pending.push_back(event);
        }
        self.ready.notify_one();
    }

    fn pop(&self) -> HookMouseEvent {
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
    outgoing: Option<OutgoingSession>,
    incoming: Option<IncomingSession>,
    local_held_keys: HashSet<HookKey>,
    suppressed_shortcut_keys: HashSet<HookKey>,
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
    outbound: mpsc::Sender<OutboundMouseSignal>,
    injector: Arc<InjectionQueue>,
    logger: Arc<Logger>,
    bounds: Mutex<DesktopBounds>,
}

pub struct MouseShare {
    inner: Arc<Inner>,
}

impl MouseShare {
    pub fn new(logger: Arc<Logger>, outbound: mpsc::Sender<OutboundMouseSignal>) -> Arc<Self> {
        let injector = InjectionQueue::new();
        let injection_receiver = Arc::clone(&injector);
        let extreme_performance = Arc::new(AtomicBool::new(false));
        let injector_extreme_performance = Arc::clone(&extreme_performance);
        let source_control_active = Arc::new(AtomicBool::new(false));
        let injector_source_control_active = Arc::clone(&source_control_active);
        let mut enigo = Enigo::new(&mouse_input_settings()).ok();
        let bounds = screen_bounds();
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
                let mut high_priority = false;
                loop {
                    let event = injection_receiver.pop();
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
        let was_enabled = self.inner.enabled.swap(enabled, Ordering::AcqRel);
        if enabled && !was_enabled {
            if !self.inner.listener_started.load(Ordering::Acquire) {
                self.inner
                    .listener_attempted
                    .store(false, Ordering::Release);
            }
            if !self.inner.keyboard_listener_started.load(Ordering::Acquire) {
                self.inner
                    .keyboard_listener_attempted
                    .store(false, Ordering::Release);
            }
        }
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
            if let Some(session) = runtime.incoming.take() {
                cancelled.push((session.peer_id.clone(), session.session_id.clone()));
                release_events.extend(release_held_input(&session));
            }
        }
        if outgoing_invalid || incoming_invalid {
            self.inner.latency_ms.store(NO_LATENCY, Ordering::Relaxed);
        }
        drop(runtime);
        for (peer_id, session_id) in cancelled {
            let _ = self
                .inner
                .outbound
                .try_send(outbound(&peer_id, MouseSignal::Cancel { session_id }));
        }
        for event in release_events {
            self.inject(event);
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
        let releases = runtime
            .incoming
            .take()
            .map(|incoming| release_held_input(&incoming))
            .unwrap_or_default();
        drop(runtime);
        self.inner.reconcile_source_cursor_capture();
        for event in releases {
            self.inject(event);
        }
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
                if let Some(previous) = runtime.incoming.take() {
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
                    last_total_x_milli: 0,
                    last_total_y_milli: 0,
                    scroll_x_milli: 0,
                    scroll_y_milli: 0,
                    last_scroll_sequence: 0,
                    last_total_scroll_x_milli: 0,
                    last_total_scroll_y_milli: 0,
                    last_keep_alive_at: now_ms(),
                    return_armed: false,
                    held_buttons: [false; 3],
                    held_keys: HashSet::new(),
                    last_event_at: now_ms(),
                    takeover_window_started: 0,
                    takeover_distance: 0,
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
                    return responses;
                };
                if incoming.peer_id != peer_id || incoming.session_id != session_id {
                    return responses;
                }
                if sequence <= incoming.last_move_sequence {
                    return responses;
                }
                let is_first_move = incoming.last_move_sequence == 0;
                incoming.last_event_at = now_ms();
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
                if incoming.return_armed
                    && crossed_return_edge(
                        incoming.return_edge,
                        next_x,
                        next_y,
                        delta_x_milli,
                        delta_y_milli,
                        width,
                        height,
                    )
                {
                    let ratio = edge_ratio(incoming.return_edge, next_x, next_y, width, height);
                    let session_id = incoming.session_id.clone();
                    simulated_events.extend(release_held_input(incoming));
                    runtime.incoming = None;
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
                    runtime.crossing_blocked_until = now_ms() + EDGE_TRANSITION_COOLDOWN_MS;
                    simulated_events.push(absolute_move(point.0, point.1));
                }
                if runtime.incoming.as_ref().is_some_and(|session| {
                    session.peer_id == peer_id && session.session_id == session_id
                }) {
                    if let Some(incoming) = runtime.incoming.take() {
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
        if source_ownership_changed {
            self.inner.reconcile_source_cursor_capture();
        }
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
                })
            {
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
                })
            {
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
                    // Movement is sent directly by the native hook.  This
                    // thread is only a recovery path and must not contend
                    // with every input frame for the runtime lock.
                    let delay = SESSION_MAINTENANCE_INTERVAL_MS;
                    std::thread::sleep(std::time::Duration::from_millis(delay));
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
                        if now.saturating_sub(incoming.last_event_at)
                            >= HELD_INPUT_SAFETY_TIMEOUT_MS
                            && (incoming.held_buttons.iter().any(|pressed| *pressed)
                                || !incoming.held_keys.is_empty())
                        {
                            releases = release_held_input(incoming);
                            incoming.held_buttons = [false; 3];
                            incoming.held_keys.clear();
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
            let _ = self.outbound.blocking_send(outbound(
                &previous.peer_id,
                MouseSignal::Cancel {
                    session_id: previous.session_id,
                },
            ));
        }
        let releases = if let Some(previous) = runtime.incoming.take() {
            let _ = self.outbound.blocking_send(outbound(
                &previous.peer_id,
                MouseSignal::Cancel {
                    session_id: previous.session_id.clone(),
                },
            ));
            release_held_input(&previous)
        } else {
            Vec::new()
        };
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
        drop(runtime);
        for event in releases {
            self.inject(event);
        }
        if let Err(error) = self.reconcile_source_cursor_capture_result() {
            self.focus_local();
            return Err(error);
        }
        if let Err(error) = recenter_cursor(anchor_x, anchor_y, bounds) {
            self.focus_local();
            return Err(error);
        }
        let _ = self.outbound.blocking_send(outbound(
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
        let incoming = runtime.incoming.take();
        if let Some(session) = &outgoing {
            let _ = self.outbound.blocking_send(outbound(
                &session.peer_id,
                MouseSignal::Cancel {
                    session_id: session.session_id.clone(),
                },
            ));
        }
        if let Some(session) = &incoming {
            let _ = self.outbound.blocking_send(outbound(
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
        runtime.crossing_blocked_until = now_ms() + EDGE_TRANSITION_COOLDOWN_MS;
        drop(runtime);
        self.reconcile_source_cursor_capture();
        if outgoing.is_some() {
            let bounds = self.desktop_bounds();
            self.inject(absolute_move(bounds.width / 2, bounds.height / 2));
        }
        for event in releases {
            self.inject(event);
        }
        self.logger.info("mouse_screen_switched", "target=local");
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
            let take_over = match event {
                HookMouseEvent::Move { x, y, native_delta } => {
                    let incoming = runtime.incoming.as_mut().expect("incoming session");
                    if now.saturating_sub(incoming.takeover_window_started)
                        > PHYSICAL_TAKEOVER_WINDOW_MS
                    {
                        incoming.takeover_window_started = now;
                        incoming.takeover_distance = 0;
                    }
                    let (delta_x, delta_y) = native_delta.unwrap_or((
                        x.saturating_sub(incoming.last_injected_x),
                        y.saturating_sub(incoming.last_injected_y),
                    ));
                    incoming.takeover_distance = incoming.takeover_distance.saturating_add(
                        delta_x
                            .unsigned_abs()
                            .saturating_add(delta_y.unsigned_abs()),
                    );
                    incoming.takeover_distance >= PHYSICAL_TAKEOVER_DISTANCE
                }
                HookMouseEvent::Button { .. } | HookMouseEvent::Scroll { .. } => true,
                HookMouseEvent::Key { .. } => false,
            };
            if !take_over {
                return true;
            }
            self.last_physical_at.store(now, Ordering::Relaxed);
            let takeover_detail = match event {
                HookMouseEvent::Move { x, y, native_delta } => format!(
                    "event=move distance={} x={x} y={y} native_delta={native_delta:?}",
                    runtime
                        .incoming
                        .as_ref()
                        .map_or(0, |incoming| incoming.takeover_distance)
                ),
                _ => format!("event={}", event_kind(event)),
            };
            let incoming = runtime.incoming.take().expect("incoming session");
            self.logger.warn(
                "mouse_incoming_cancelled",
                format!("reason=local_physical_intent {takeover_detail}"),
            );
            if let HookMouseEvent::Move { x, y, .. } = event {
                runtime.last_x = x;
                runtime.last_y = y;
            }
            let release_events = release_held_input(&incoming);
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

        self.last_physical_at.store(now, Ordering::Relaxed);

        if let Some(outgoing) = runtime.outgoing.as_mut() {
            let peer_id = outgoing.peer_id.clone();
            let session_id = outgoing.session_id.clone();
            #[cfg(target_os = "windows")]
            let mut should_recenter = false;
            #[cfg(not(target_os = "windows"))]
            let should_recenter = false;
            match event {
                HookMouseEvent::Move { x, y, native_delta } => {
                    let (raw_delta_x, raw_delta_y) =
                        native_delta.unwrap_or((x - outgoing.anchor_x, y - outgoing.anchor_y));
                    let native_outlier = native_delta.is_some()
                        && (raw_delta_x.unsigned_abs() > MAX_NATIVE_DELTA_PER_EVENT as u32
                            || raw_delta_y.unsigned_abs() > MAX_NATIVE_DELTA_PER_EVENT as u32);
                    let (delta_x, delta_y) = if native_outlier {
                        (0, 0)
                    } else {
                        (
                            clamp_physical_delta(raw_delta_x),
                            clamp_physical_delta(raw_delta_y),
                        )
                    };
                    #[cfg(target_os = "windows")]
                    {
                        outgoing.anchor_x = x;
                        outgoing.anchor_y = y;
                        if x <= SOURCE_CURSOR_RECENTER_MARGIN
                            || x >= width - 1 - SOURCE_CURSOR_RECENTER_MARGIN
                            || y <= SOURCE_CURSOR_RECENTER_MARGIN
                            || y >= height - 1 - SOURCE_CURSOR_RECENTER_MARGIN
                        {
                            outgoing.anchor_x = width / 2;
                            outgoing.anchor_y = height / 2;
                            should_recenter = true;
                        }
                    }
                    if delta_x != 0 || delta_y != 0 {
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
                            if self.outbound.try_send(signal).is_ok() {
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
            if let Err(error) = ensure_source_cursor_captured() {
                self.logger.warn("mouse_cursor_guard_failed", error);
            }
            if should_recenter {
                let _ = recenter_cursor(anchor.0, anchor.1, bounds);
            }
            return true;
        }

        if let HookMouseEvent::Move { x, y, .. } = event {
            let previous_x = runtime.last_x;
            let previous_y = runtime.last_y;
            runtime.last_x = x;
            runtime.last_y = y;
            let target = runtime
                .targets
                .iter()
                .find(|target| {
                    reached_exit_edge(target.position, x, y, previous_x, previous_y, width, height)
                })
                .cloned();
            if let Some(target) = target {
                if now_ms() >= runtime.crossing_blocked_until {
                    let peer_id = target.peer_id;
                    let position = target.position;
                    runtime.local_held_keys.clear();
                    runtime.suppressed_shortcut_keys.clear();
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
                        first_move_logged: false,
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
                    let _ = self.outbound.blocking_send(enter);
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
        let _ = self.outbound.blocking_send(outbound(
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

fn event_kind(event: HookMouseEvent) -> &'static str {
    match event {
        HookMouseEvent::Move { .. } => "move",
        HookMouseEvent::Button { .. } => "button",
        HookMouseEvent::Scroll { .. } => "scroll",
        HookMouseEvent::Key { .. } => "key",
    }
}

fn inject_mouse_event(enigo: &mut Enigo, event: HookMouseEvent) -> Result<(), String> {
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
