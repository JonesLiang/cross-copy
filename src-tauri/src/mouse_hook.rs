pub(crate) const SYNTHETIC_INPUT_MARKER: usize = 0x4352_4f53_5343_4f50;

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
    },
    Button {
        button: HookMouseButton,
        pressed: bool,
    },
    Scroll {
        delta_x: i64,
        delta_y: i64,
    },
    CursorVisible(bool),
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
            if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA)
                == SYNTHETIC_INPUT_MARKER as i64
            {
                return CallbackResult::Keep;
            }
            let hook_event = match event_type {
                CGEventType::MouseMoved
                | CGEventType::LeftMouseDragged
                | CGEventType::RightMouseDragged
                | CGEventType::OtherMouseDragged => {
                    let point = event.location();
                    Some(HookMouseEvent::Move {
                        x: point.x.round() as i32,
                        y: point.y.round() as i32,
                    })
                }
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
                CGEventType::ScrollWheel => Some(HookMouseEvent::Scroll {
                    delta_x: event
                        .get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2),
                    delta_y: event
                        .get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1),
                }),
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
            if data.dwExtraInfo != SYNTHETIC_INPUT_MARKER && data.flags & LLMHF_INJECTED == 0 {
                let wheel_delta = || (data.mouseData >> 16) as u16 as i16 as i64;
                let event = match wparam.0 as u32 {
                    WM_MOUSEMOVE => Some(HookMouseEvent::Move {
                        x: data.pt.x,
                        y: data.pt.y,
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
                        delta_x: 0,
                        delta_y: wheel_delta(),
                    }),
                    WM_MOUSEHWHEEL => Some(HookMouseEvent::Scroll {
                        delta_x: wheel_delta(),
                        delta_y: 0,
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
