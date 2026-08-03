pub(crate) const SYNTHETIC_INPUT_MARKER: usize = 0x4352_4f53_5343_4f50;

use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

static SOURCE_CURSOR_CAPTURED: AtomicBool = AtomicBool::new(false);
static SOURCE_CURSOR_LOCK: Mutex<()> = Mutex::new(());

#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::sync::atomic::{AtomicI32, AtomicU64};

#[cfg(any(target_os = "macos", target_os = "windows"))]
static LAST_SYNTHETIC_X: AtomicI32 = AtomicI32::new(i32::MIN);
#[cfg(any(target_os = "macos", target_os = "windows"))]
static LAST_SYNTHETIC_Y: AtomicI32 = AtomicI32::new(i32::MIN);
#[cfg(any(target_os = "macos", target_os = "windows"))]
static LAST_SYNTHETIC_AT: AtomicU64 = AtomicU64::new(0);
#[cfg(any(target_os = "macos", target_os = "windows"))]
const SYNTHETIC_ECHO_WINDOW_MS: u64 = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DesktopBounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HookMouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HookMouseEvent {
    Move {
        x: i32,
        y: i32,
        native_delta: Option<(i32, i32)>,
    },
    Button {
        button: HookMouseButton,
        pressed: bool,
    },
    Scroll {
        delta_x_milli: i64,
        delta_y_milli: i64,
    },
    Key {
        key: HookKey,
        pressed: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum HookKey {
    Character(char),
    Return,
    Escape,
    Backspace,
    Tab,
    Space,
    CapsLock,
    Function(u8),
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Left,
    Right,
    Up,
    Down,
    LeftShift,
    RightShift,
    LeftControl,
    RightControl,
    LeftAlt,
    RightAlt,
    LeftMeta,
    RightMeta,
    NumLock,
    Numpad(u8),
    NumpadDecimal,
    NumpadAdd,
    NumpadSubtract,
    NumpadMultiply,
    NumpadDivide,
    NumpadEnter,
    PrintScreen,
    Pause,
    VolumeUp,
    VolumeDown,
    VolumeMute,
    MediaPlayPause,
    MediaNext,
    MediaPrevious,
}

impl HookKey {
    pub fn to_enigo(self) -> Option<enigo::Key> {
        use enigo::Key;
        Some(match self {
            Self::Character(value) => Key::Unicode(value),
            Self::Return | Self::NumpadEnter => Key::Return,
            Self::Escape => Key::Escape,
            Self::Backspace => Key::Backspace,
            Self::Tab => Key::Tab,
            Self::Space => Key::Space,
            Self::CapsLock => Key::CapsLock,
            Self::Function(1) => Key::F1,
            Self::Function(2) => Key::F2,
            Self::Function(3) => Key::F3,
            Self::Function(4) => Key::F4,
            Self::Function(5) => Key::F5,
            Self::Function(6) => Key::F6,
            Self::Function(7) => Key::F7,
            Self::Function(8) => Key::F8,
            Self::Function(9) => Key::F9,
            Self::Function(10) => Key::F10,
            Self::Function(11) => Key::F11,
            Self::Function(12) => Key::F12,
            Self::Function(13) => Key::F13,
            Self::Function(14) => Key::F14,
            Self::Function(15) => Key::F15,
            Self::Function(16) => Key::F16,
            Self::Function(17) => Key::F17,
            Self::Function(18) => Key::F18,
            Self::Function(19) => Key::F19,
            Self::Function(20) => Key::F20,
            Self::Function(_) => return None,
            #[cfg(any(target_os = "windows", all(unix, not(target_os = "macos"))))]
            Self::Insert => Key::Insert,
            #[cfg(target_os = "macos")]
            Self::Insert => Key::Help,
            Self::Delete => Key::Delete,
            Self::Home => Key::Home,
            Self::End => Key::End,
            Self::PageUp => Key::PageUp,
            Self::PageDown => Key::PageDown,
            Self::Left => Key::LeftArrow,
            Self::Right => Key::RightArrow,
            Self::Up => Key::UpArrow,
            Self::Down => Key::DownArrow,
            Self::LeftShift => Key::LShift,
            Self::RightShift => Key::RShift,
            Self::LeftControl => Key::LControl,
            Self::RightControl => Key::RControl,
            Self::LeftAlt => Key::Alt,
            #[cfg(target_os = "windows")]
            Self::RightAlt => Key::RMenu,
            #[cfg(target_os = "macos")]
            Self::RightAlt => Key::ROption,
            #[cfg(target_os = "windows")]
            Self::LeftMeta => Key::LWin,
            #[cfg(target_os = "macos")]
            Self::LeftMeta => Key::Meta,
            #[cfg(target_os = "windows")]
            Self::RightMeta => Key::RWin,
            #[cfg(target_os = "macos")]
            Self::RightMeta => Key::RCommand,
            #[cfg(target_os = "windows")]
            Self::NumLock => Key::Numlock,
            #[cfg(target_os = "macos")]
            Self::NumLock => return None,
            Self::Numpad(0) => Key::Numpad0,
            Self::Numpad(1) => Key::Numpad1,
            Self::Numpad(2) => Key::Numpad2,
            Self::Numpad(3) => Key::Numpad3,
            Self::Numpad(4) => Key::Numpad4,
            Self::Numpad(5) => Key::Numpad5,
            Self::Numpad(6) => Key::Numpad6,
            Self::Numpad(7) => Key::Numpad7,
            Self::Numpad(8) => Key::Numpad8,
            Self::Numpad(9) => Key::Numpad9,
            Self::Numpad(_) => return None,
            Self::NumpadDecimal => Key::Decimal,
            Self::NumpadAdd => Key::Add,
            Self::NumpadSubtract => Key::Subtract,
            Self::NumpadMultiply => Key::Multiply,
            Self::NumpadDivide => Key::Divide,
            #[cfg(any(target_os = "windows", all(unix, not(target_os = "macos"))))]
            Self::PrintScreen => Key::PrintScr,
            #[cfg(target_os = "macos")]
            Self::PrintScreen => return None,
            #[cfg(any(target_os = "windows", all(unix, not(target_os = "macos"))))]
            Self::Pause => Key::Pause,
            #[cfg(target_os = "macos")]
            Self::Pause => return None,
            Self::VolumeUp => Key::VolumeUp,
            Self::VolumeDown => Key::VolumeDown,
            Self::VolumeMute => Key::VolumeMute,
            Self::MediaPlayPause => Key::MediaPlayPause,
            Self::MediaNext => Key::MediaNextTrack,
            Self::MediaPrevious => Key::MediaPrevTrack,
        })
    }
}

#[cfg(target_os = "macos")]
pub fn set_realtime_priority(enabled: bool) {
    type QosClass = u32;
    const QOS_CLASS_USER_INTERACTIVE: QosClass = 0x21;
    const QOS_CLASS_DEFAULT: QosClass = 0x15;
    unsafe extern "C" {
        fn pthread_set_qos_class_self_np(qos_class: QosClass, relative_priority: i32) -> i32;
    }
    unsafe {
        let _ = pthread_set_qos_class_self_np(
            if enabled {
                QOS_CLASS_USER_INTERACTIVE
            } else {
                QOS_CLASS_DEFAULT
            },
            0,
        );
    }
}

#[cfg(target_os = "windows")]
pub fn set_realtime_priority(enabled: bool) {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST, THREAD_PRIORITY_NORMAL,
    };
    unsafe {
        let _ = SetThreadPriority(
            GetCurrentThread(),
            if enabled {
                THREAD_PRIORITY_HIGHEST
            } else {
                THREAD_PRIORITY_NORMAL
            },
        );
    }
}

#[cfg(target_os = "macos")]
pub fn screen_bounds() -> DesktopBounds {
    use core_graphics::display::CGDisplay;
    let displays = CGDisplay::active_displays().unwrap_or_default();
    let bounds: Option<(i32, i32, i32, i32)> = displays
        .into_iter()
        .map(|id| CGDisplay::new(id).bounds())
        .fold(None, |combined, bounds| {
            let left = bounds.origin.x.floor() as i32;
            let top = bounds.origin.y.floor() as i32;
            let right = (bounds.origin.x + bounds.size.width).ceil() as i32;
            let bottom = (bounds.origin.y + bounds.size.height).ceil() as i32;
            Some(match combined {
                None => (left, top, right, bottom),
                Some((min_x, min_y, max_x, max_y)) => (
                    min_x.min(left),
                    min_y.min(top),
                    max_x.max(right),
                    max_y.max(bottom),
                ),
            })
        });
    let (x, y, right, bottom) = bounds.unwrap_or_else(|| {
        let main = CGDisplay::main().bounds();
        (
            main.origin.x.floor() as i32,
            main.origin.y.floor() as i32,
            (main.origin.x + main.size.width).ceil() as i32,
            (main.origin.y + main.size.height).ceil() as i32,
        )
    });
    DesktopBounds {
        x,
        y,
        width: (right - x).max(2),
        height: (bottom - y).max(2),
    }
}

#[cfg(target_os = "windows")]
pub fn screen_bounds() -> DesktopBounds {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    unsafe {
        DesktopBounds {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
        }
    }
}

#[cfg(target_os = "macos")]
pub fn recenter_cursor(x: i32, y: i32, bounds: DesktopBounds) -> Result<(), String> {
    use core_graphics::{display::CGDisplay, geometry::CGPoint};

    // Warping does not generate a mouse event, so the event tap only observes
    // real hardware deltas while the hidden source cursor remains away from
    // the screen edge.
    CGDisplay::warp_mouse_cursor_position(CGPoint::new(
        f64::from(bounds.x + x.clamp(0, bounds.width - 1)),
        f64::from(bounds.y + y.clamp(0, bounds.height - 1)),
    ))
    .map_err(|error| format!("macOS 鼠标回中失败：{error:?}"))
}

#[cfg(target_os = "macos")]
pub fn move_cursor_absolute(x: i32, y: i32) -> Result<(), String> {
    use core_graphics::{
        event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton, EventField},
        event_source::{CGEventSource, CGEventSourceStateID},
        geometry::CGPoint,
    };
    use std::cell::RefCell;

    thread_local! {
        static MOUSE_EVENT_SOURCE: RefCell<Option<CGEventSource>> = const { RefCell::new(None) };
    }

    MOUSE_EVENT_SOURCE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(
                CGEventSource::new(CGEventSourceStateID::HIDSystemState)
                    .map_err(|_| "无法创建 macOS 鼠标事件源".to_string())?,
            );
        }
        let event = CGEvent::new_mouse_event(
            slot.as_ref().expect("mouse event source").clone(),
            CGEventType::MouseMoved,
            CGPoint::new(f64::from(x), f64::from(y)),
            CGMouseButton::Left,
        )
        .map_err(|_| "无法创建 macOS 鼠标移动事件".to_string())?;
        event.set_integer_value_field(
            EventField::EVENT_SOURCE_USER_DATA,
            SYNTHETIC_INPUT_MARKER as i64,
        );
        record_synthetic_move(x, y);
        event.post(CGEventTapLocation::HID);
        Ok(())
    })
}

#[cfg(target_os = "windows")]
pub fn recenter_cursor(x: i32, y: i32, bounds: DesktopBounds) -> Result<(), String> {
    move_cursor_absolute(bounds.x.saturating_add(x), bounds.y.saturating_add(y))
}

#[cfg(target_os = "windows")]
pub fn move_cursor_absolute(x: i32, y: i32) -> Result<(), String> {
    use std::mem::size_of;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE,
        MOUSEEVENTF_MOVE_NOCOALESCE, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT,
    };

    let bounds = screen_bounds();
    let local_x = x.saturating_sub(bounds.x);
    let local_y = y.saturating_sub(bounds.y);
    let normalized_x =
        i64::from(local_x.clamp(0, bounds.width - 1)) * 65_535 / i64::from(bounds.width - 1);
    let normalized_y =
        i64::from(local_y.clamp(0, bounds.height - 1)) * 65_535 / i64::from(bounds.height - 1);
    LAST_SYNTHETIC_X.store(
        bounds.x.saturating_add(local_x.clamp(0, bounds.width - 1)),
        Ordering::Relaxed,
    );
    LAST_SYNTHETIC_Y.store(
        bounds.y.saturating_add(local_y.clamp(0, bounds.height - 1)),
        Ordering::Relaxed,
    );
    LAST_SYNTHETIC_AT.store(monotonic_ms(), Ordering::Release);
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: normalized_x as i32,
                dy: normalized_y as i32,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE
                    | MOUSEEVENTF_ABSOLUTE
                    | MOUSEEVENTF_MOVE_NOCOALESCE
                    | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                dwExtraInfo: SYNTHETIC_INPUT_MARKER,
            },
        },
    };
    let sent = unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
    if sent == 1 {
        Ok(())
    } else {
        Err(format!(
            "Windows 鼠标移动失败：{}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(target_os = "macos")]
pub fn set_cursor_visible(visible: bool) -> Result<(), String> {
    use core_graphics::display::CGDisplay;
    let result = if visible {
        CGDisplay::main().show_cursor()
    } else {
        CGDisplay::main().hide_cursor()
    };
    result.map_err(|error| format!("无法切换 macOS 鼠标指针显示状态：{error:?}"))
}

pub fn set_source_cursor_captured(captured: bool) -> Result<(), String> {
    let _guard = SOURCE_CURSOR_LOCK
        .lock()
        .map_err(|_| "鼠标控制权状态锁已损坏".to_string())?;
    let previous = SOURCE_CURSOR_CAPTURED.load(Ordering::Acquire);
    if previous == captured {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        use core_graphics::display::CGDisplay;
        let result = if captured {
            CGDisplay::associate_mouse_and_mouse_cursor_position(false).and_then(|_| {
                CGDisplay::main().hide_cursor().inspect_err(|_| {
                    let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(true);
                })
            })
        } else {
            CGDisplay::associate_mouse_and_mouse_cursor_position(true).and_then(|_| {
                CGDisplay::main().show_cursor().inspect_err(|_| {
                    let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(false);
                })
            })
        };
        if let Err(error) = result {
            return Err(format!("无法切换 macOS 鼠标控制权：{error:?}"));
        }
    }
    #[cfg(target_os = "windows")]
    if let Err(error) = set_cursor_visible(!captured) {
        return Err(error);
    }
    SOURCE_CURSOR_CAPTURED.store(captured, Ordering::Release);
    Ok(())
}

#[cfg(target_os = "macos")]
fn record_synthetic_move(x: i32, y: i32) {
    LAST_SYNTHETIC_X.store(x, Ordering::Relaxed);
    LAST_SYNTHETIC_Y.store(y, Ordering::Relaxed);
    LAST_SYNTHETIC_AT.store(monotonic_ms(), Ordering::Release);
}

#[cfg(target_os = "windows")]
pub fn set_cursor_visible(visible: bool) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::ShowCursor;
    unsafe {
        if visible {
            while ShowCursor(true) < 0 {}
        } else {
            while ShowCursor(false) >= 0 {}
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn run_mouse_hook(
    callback: impl Fn(HookMouseEvent) -> bool + Send + 'static,
) -> Result<(), String> {
    use core_foundation::runloop::CFRunLoop;
    use core_graphics::event::{
        CGEvent, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
        CGEventType, CallbackResult, EventField,
    };

    let event_types = vec![
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDragged,
        CGEventType::OtherMouseDragged,
        CGEventType::ScrollWheel,
    ];
    CGEventTap::with_enabled(
        CGEventTapLocation::HID,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        event_types,
        move |_proxy, event_type, event: &CGEvent| {
            let location = event.location();
            let synthetic_echo = matches!(
                event_type,
                CGEventType::MouseMoved
                    | CGEventType::LeftMouseDragged
                    | CGEventType::RightMouseDragged
                    | CGEventType::OtherMouseDragged
            ) && monotonic_ms()
                .saturating_sub(LAST_SYNTHETIC_AT.load(Ordering::Acquire))
                <= SYNTHETIC_ECHO_WINDOW_MS
                && (location.x.round() as i32).abs_diff(LAST_SYNTHETIC_X.load(Ordering::Relaxed))
                    <= 1
                && (location.y.round() as i32).abs_diff(LAST_SYNTHETIC_Y.load(Ordering::Relaxed))
                    <= 1;
            if synthetic_echo
                || event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA)
                    == SYNTHETIC_INPUT_MARKER as i64
            {
                return CallbackResult::Keep;
            }
            let hook_event = match event_type {
                CGEventType::MouseMoved
                | CGEventType::LeftMouseDragged
                | CGEventType::RightMouseDragged
                | CGEventType::OtherMouseDragged => Some(HookMouseEvent::Move {
                    x: location.x.round() as i32,
                    y: location.y.round() as i32,
                    native_delta: Some((
                        event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X) as i32,
                        event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y) as i32,
                    )),
                }),
                CGEventType::LeftMouseDown => Some(HookMouseEvent::Button {
                    button: HookMouseButton::Left,
                    pressed: true,
                }),
                CGEventType::LeftMouseUp => Some(HookMouseEvent::Button {
                    button: HookMouseButton::Left,
                    pressed: false,
                }),
                CGEventType::RightMouseDown => Some(HookMouseEvent::Button {
                    button: HookMouseButton::Right,
                    pressed: true,
                }),
                CGEventType::RightMouseUp => Some(HookMouseEvent::Button {
                    button: HookMouseButton::Right,
                    pressed: false,
                }),
                CGEventType::OtherMouseDown | CGEventType::OtherMouseUp
                    if event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER)
                        == 2 =>
                {
                    Some(HookMouseEvent::Button {
                        button: HookMouseButton::Middle,
                        pressed: matches!(event_type, CGEventType::OtherMouseDown),
                    })
                }
                CGEventType::ScrollWheel => {
                    let scroll_milli = |line_field, fixed_field| {
                        let lines = event.get_integer_value_field(line_field);
                        if lines != 0 {
                            lines.saturating_mul(1_000)
                        } else {
                            (event.get_double_value_field(fixed_field) * 1_000.0).round() as i64
                        }
                    };
                    Some(HookMouseEvent::Scroll {
                        delta_x_milli: scroll_milli(
                            EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2,
                            EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_2,
                        ),
                        delta_y_milli: scroll_milli(
                            EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1,
                            EventField::SCROLL_WHEEL_EVENT_FIXED_POINT_DELTA_AXIS_1,
                        ),
                    })
                }
                _ => None,
            };
            if hook_event.is_some_and(&callback) {
                CallbackResult::Drop
            } else {
                CallbackResult::Keep
            }
        },
        CFRunLoop::run_current,
    )
    .map_err(|_| "无法创建 macOS 鼠标事件监听，请检查辅助功能权限".to_string())
}

#[cfg(target_os = "windows")]
pub fn run_mouse_hook(
    callback: impl Fn(HookMouseEvent) -> bool + Send + Sync + 'static,
) -> Result<(), String> {
    use std::sync::{Arc, OnceLock};
    use windows::Win32::{
        Foundation::{LPARAM, LRESULT, WPARAM},
        UI::WindowsAndMessaging::{
            CallNextHookEx, GetMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION,
            LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP,
            WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL,
            WM_RBUTTONDOWN, WM_RBUTTONUP,
        },
    };

    type MouseCallback = dyn Fn(HookMouseEvent) -> bool + Send + Sync;
    static CALLBACK: OnceLock<Arc<MouseCallback>> = OnceLock::new();

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let data = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            let message = wparam.0 as u32;
            let explicitly_injected =
                data.dwExtraInfo == SYNTHETIC_INPUT_MARKER || data.flags & LLMHF_INJECTED != 0;
            let synthetic_echo = message == WM_MOUSEMOVE
                && monotonic_ms().saturating_sub(LAST_SYNTHETIC_AT.load(Ordering::Acquire))
                    <= SYNTHETIC_ECHO_WINDOW_MS
                && data.pt.x.abs_diff(LAST_SYNTHETIC_X.load(Ordering::Relaxed)) <= 1
                && data.pt.y.abs_diff(LAST_SYNTHETIC_Y.load(Ordering::Relaxed)) <= 1;
            if !explicitly_injected && !synthetic_echo {
                let wheel_delta = || (data.mouseData >> 16) as u16 as i16 as i64;
                let event = match message {
                    WM_MOUSEMOVE => Some(HookMouseEvent::Move {
                        x: data.pt.x,
                        y: data.pt.y,
                        native_delta: None,
                    }),
                    WM_LBUTTONDOWN => Some(HookMouseEvent::Button {
                        button: HookMouseButton::Left,
                        pressed: true,
                    }),
                    WM_LBUTTONUP => Some(HookMouseEvent::Button {
                        button: HookMouseButton::Left,
                        pressed: false,
                    }),
                    WM_RBUTTONDOWN => Some(HookMouseEvent::Button {
                        button: HookMouseButton::Right,
                        pressed: true,
                    }),
                    WM_RBUTTONUP => Some(HookMouseEvent::Button {
                        button: HookMouseButton::Right,
                        pressed: false,
                    }),
                    WM_MBUTTONDOWN => Some(HookMouseEvent::Button {
                        button: HookMouseButton::Middle,
                        pressed: true,
                    }),
                    WM_MBUTTONUP => Some(HookMouseEvent::Button {
                        button: HookMouseButton::Middle,
                        pressed: false,
                    }),
                    WM_MOUSEWHEEL => Some(HookMouseEvent::Scroll {
                        delta_x_milli: 0,
                        delta_y_milli: wheel_delta().saturating_mul(25),
                    }),
                    WM_MOUSEHWHEEL => Some(HookMouseEvent::Scroll {
                        delta_x_milli: wheel_delta().saturating_mul(25),
                        delta_y_milli: 0,
                    }),
                    _ => None,
                };
                if event.is_some_and(|event| CALLBACK.get().is_some_and(|callback| callback(event)))
                {
                    return LRESULT(1);
                }
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    let _ = CALLBACK.set(Arc::new(callback));
    let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), None, 0) }
        .map_err(|error| format!("无法创建 Windows 鼠标事件监听：{error}"))?;
    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 <= 0 {
            break;
        }
    }
    unsafe { UnhookWindowsHookEx(hook) }
        .map_err(|error| format!("无法移除 Windows 鼠标事件监听：{error}"))
}

#[cfg(target_os = "macos")]
pub fn run_keyboard_hook(
    callback: impl Fn(HookKey, bool) -> bool + Send + 'static,
) -> Result<(), String> {
    use core_foundation::runloop::CFRunLoop;
    use core_graphics::event::{
        CGEvent, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
        CGEventType, CallbackResult, EventField,
    };

    let event_types = vec![
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::FlagsChanged,
    ];
    CGEventTap::with_enabled(
        CGEventTapLocation::HID,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        event_types,
        move |_proxy, event_type, event: &CGEvent| {
            if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA)
                == SYNTHETIC_INPUT_MARKER as i64
            {
                return CallbackResult::Keep;
            }
            let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
            let Some(key) = mac_key(code) else {
                return CallbackResult::Keep;
            };
            let captured = match event_type {
                CGEventType::FlagsChanged if key == HookKey::CapsLock => {
                    callback(key, true) | callback(key, false)
                }
                CGEventType::FlagsChanged => {
                    callback(key, mac_modifier_pressed(key, event.get_flags()))
                }
                CGEventType::KeyDown => callback(key, true),
                CGEventType::KeyUp => callback(key, false),
                _ => false,
            };
            if captured {
                CallbackResult::Drop
            } else {
                CallbackResult::Keep
            }
        },
        CFRunLoop::run_current,
    )
    .map_err(|_| "无法创建 macOS 键盘监听，请检查辅助功能权限".to_string())
}

#[cfg(target_os = "macos")]
fn mac_modifier_pressed(key: HookKey, flags: core_graphics::event::CGEventFlags) -> bool {
    const LEFT_CONTROL: u64 = 0x0000_0001;
    const LEFT_SHIFT: u64 = 0x0000_0002;
    const RIGHT_SHIFT: u64 = 0x0000_0004;
    const LEFT_COMMAND: u64 = 0x0000_0008;
    const RIGHT_COMMAND: u64 = 0x0000_0010;
    const LEFT_OPTION: u64 = 0x0000_0020;
    const RIGHT_OPTION: u64 = 0x0000_0040;
    const RIGHT_CONTROL: u64 = 0x0000_2000;
    let mask = match key {
        HookKey::LeftControl => LEFT_CONTROL,
        HookKey::RightControl => RIGHT_CONTROL,
        HookKey::LeftShift => LEFT_SHIFT,
        HookKey::RightShift => RIGHT_SHIFT,
        HookKey::LeftAlt => LEFT_OPTION,
        HookKey::RightAlt => RIGHT_OPTION,
        HookKey::LeftMeta => LEFT_COMMAND,
        HookKey::RightMeta => RIGHT_COMMAND,
        _ => return false,
    };
    flags.bits() & mask != 0
}

#[cfg(target_os = "macos")]
fn mac_key(code: u16) -> Option<HookKey> {
    Some(match code {
        0 => HookKey::Character('a'),
        1 => HookKey::Character('s'),
        2 => HookKey::Character('d'),
        3 => HookKey::Character('f'),
        4 => HookKey::Character('h'),
        5 => HookKey::Character('g'),
        6 => HookKey::Character('z'),
        7 => HookKey::Character('x'),
        8 => HookKey::Character('c'),
        9 => HookKey::Character('v'),
        11 => HookKey::Character('b'),
        12 => HookKey::Character('q'),
        13 => HookKey::Character('w'),
        14 => HookKey::Character('e'),
        15 => HookKey::Character('r'),
        16 => HookKey::Character('y'),
        17 => HookKey::Character('t'),
        18..=29 => HookKey::Character(match code {
            18 => '1',
            19 => '2',
            20 => '3',
            21 => '4',
            22 => '6',
            23 => '5',
            24 => '=',
            25 => '9',
            26 => '7',
            27 => '-',
            28 => '8',
            _ => '0',
        }),
        30 => HookKey::Character(']'),
        31 => HookKey::Character('o'),
        32 => HookKey::Character('u'),
        33 => HookKey::Character('['),
        34 => HookKey::Character('i'),
        35 => HookKey::Character('p'),
        36 => HookKey::Return,
        37 => HookKey::Character('l'),
        38 => HookKey::Character('j'),
        39 => HookKey::Character('\''),
        40 => HookKey::Character('k'),
        41 => HookKey::Character(';'),
        42 => HookKey::Character('\\'),
        43 => HookKey::Character(','),
        44 => HookKey::Character('/'),
        45 => HookKey::Character('n'),
        46 => HookKey::Character('m'),
        47 => HookKey::Character('.'),
        48 => HookKey::Tab,
        49 => HookKey::Space,
        50 => HookKey::Character('`'),
        51 => HookKey::Backspace,
        53 => HookKey::Escape,
        54 => HookKey::RightMeta,
        55 => HookKey::LeftMeta,
        56 => HookKey::LeftShift,
        57 => HookKey::CapsLock,
        58 => HookKey::LeftAlt,
        59 => HookKey::LeftControl,
        60 => HookKey::RightShift,
        61 => HookKey::RightAlt,
        62 => HookKey::RightControl,
        65 => HookKey::NumpadDecimal,
        67 => HookKey::NumpadMultiply,
        69 => HookKey::NumpadAdd,
        71 => HookKey::NumLock,
        75 => HookKey::NumpadDivide,
        76 => HookKey::NumpadEnter,
        78 => HookKey::NumpadSubtract,
        81 => HookKey::Character('='),
        82..=92 => HookKey::Numpad(match code {
            82 => 0,
            83 => 1,
            84 => 2,
            85 => 3,
            86 => 4,
            87 => 5,
            88 => 6,
            89 => 7,
            91 => 8,
            92 => 9,
            _ => return None,
        }),
        96 => HookKey::Function(5),
        97 => HookKey::Function(6),
        98 => HookKey::Function(7),
        99 => HookKey::Function(3),
        100 => HookKey::Function(8),
        101 => HookKey::Function(9),
        103 => HookKey::Function(11),
        105 => HookKey::Function(13),
        106 => HookKey::Function(16),
        107 => HookKey::Function(14),
        109 => HookKey::Function(10),
        111 => HookKey::Function(12),
        113 => HookKey::Function(15),
        114 => HookKey::Insert,
        115 => HookKey::Home,
        116 => HookKey::PageUp,
        117 => HookKey::Delete,
        118 => HookKey::Function(4),
        119 => HookKey::End,
        120 => HookKey::Function(2),
        121 => HookKey::PageDown,
        122 => HookKey::Function(1),
        123 => HookKey::Left,
        124 => HookKey::Right,
        125 => HookKey::Down,
        126 => HookKey::Up,
        _ => return None,
    })
}

#[cfg(target_os = "windows")]
pub fn run_keyboard_hook(
    callback: impl Fn(HookKey, bool) -> bool + Send + Sync + 'static,
) -> Result<(), String> {
    use std::sync::{Arc, OnceLock};
    use windows::Win32::{
        Foundation::{LPARAM, LRESULT, WPARAM},
        UI::WindowsAndMessaging::{
            CallNextHookEx, GetMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION,
            KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
            WM_SYSKEYDOWN, WM_SYSKEYUP,
        },
    };

    type KeyboardCallback = dyn Fn(HookKey, bool) -> bool + Send + Sync;
    static CALLBACK: OnceLock<Arc<KeyboardCallback>> = OnceLock::new();

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let data = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            if data.dwExtraInfo != SYNTHETIC_INPUT_MARKER && !data.flags.contains(LLKHF_INJECTED) {
                let message = wparam.0 as u32;
                let pressed = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
                let released = message == WM_KEYUP || message == WM_SYSKEYUP;
                if (pressed || released)
                    && windows_key(data.vkCode as u16)
                        .is_some_and(|key| CALLBACK.get().is_some_and(|cb| cb(key, pressed)))
                {
                    return LRESULT(1);
                }
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    let _ = CALLBACK.set(Arc::new(callback));
    let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) }
        .map_err(|error| format!("无法创建 Windows 键盘监听：{error}"))?;
    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 <= 0 {
            break;
        }
    }
    unsafe { UnhookWindowsHookEx(hook) }
        .map_err(|error| format!("无法移除 Windows 键盘监听：{error}"))
}

#[cfg(target_os = "windows")]
fn windows_key(code: u16) -> Option<HookKey> {
    Some(match code {
        0x08 => HookKey::Backspace,
        0x09 => HookKey::Tab,
        0x0D => HookKey::Return,
        0x10 => HookKey::LeftShift,
        0x11 => HookKey::LeftControl,
        0x12 => HookKey::LeftAlt,
        0x13 => HookKey::Pause,
        0x14 => HookKey::CapsLock,
        0x1B => HookKey::Escape,
        0x20 => HookKey::Space,
        0x21 => HookKey::PageUp,
        0x22 => HookKey::PageDown,
        0x23 => HookKey::End,
        0x24 => HookKey::Home,
        0x25 => HookKey::Left,
        0x26 => HookKey::Up,
        0x27 => HookKey::Right,
        0x28 => HookKey::Down,
        0x2C => HookKey::PrintScreen,
        0x2D => HookKey::Insert,
        0x2E => HookKey::Delete,
        0x30..=0x39 => HookKey::Character(char::from_u32(code as u32).unwrap_or('0')),
        0x41..=0x5A => HookKey::Character(char::from_u32((code as u32) + 32).unwrap_or('a')),
        0x5B => HookKey::LeftMeta,
        0x5C => HookKey::RightMeta,
        0x60..=0x69 => HookKey::Numpad((code - 0x60) as u8),
        0x6A => HookKey::NumpadMultiply,
        0x6B => HookKey::NumpadAdd,
        0x6D => HookKey::NumpadSubtract,
        0x6E => HookKey::NumpadDecimal,
        0x6F => HookKey::NumpadDivide,
        0x70..=0x87 => HookKey::Function((code - 0x6F) as u8),
        0x90 => HookKey::NumLock,
        0xA0 => HookKey::LeftShift,
        0xA1 => HookKey::RightShift,
        0xA2 => HookKey::LeftControl,
        0xA3 => HookKey::RightControl,
        0xA4 => HookKey::LeftAlt,
        0xA5 => HookKey::RightAlt,
        0xAD => HookKey::VolumeMute,
        0xAE => HookKey::VolumeDown,
        0xAF => HookKey::VolumeUp,
        0xB0 => HookKey::MediaNext,
        0xB1 => HookKey::MediaPrevious,
        0xB3 => HookKey::MediaPlayPause,
        0xBA => HookKey::Character(';'),
        0xBB => HookKey::Character('='),
        0xBC => HookKey::Character(','),
        0xBD => HookKey::Character('-'),
        0xBE => HookKey::Character('.'),
        0xBF => HookKey::Character('/'),
        0xC0 => HookKey::Character('`'),
        0xDB => HookKey::Character('['),
        0xDC => HookKey::Character('\\'),
        0xDD => HookKey::Character(']'),
        0xDE => HookKey::Character('\''),
        _ => return None,
    })
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn monotonic_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;

    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}
