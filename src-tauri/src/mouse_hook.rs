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
    CursorVisible(bool),
}

#[cfg(target_os = "macos")]
pub fn screen_size() -> (i32, i32) {
    use core_graphics::display::CGDisplay;
    let bounds = CGDisplay::main().bounds();
    (
        (bounds.size.width.round() as i32).max(2),
        (bounds.size.height.round() as i32).max(2),
    )
}

#[cfg(target_os = "windows")]
pub fn screen_size() -> (i32, i32) {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    unsafe {
        (
            GetSystemMetrics(SM_CXSCREEN).max(2),
            GetSystemMetrics(SM_CYSCREEN).max(2),
        )
    }
}

#[cfg(target_os = "macos")]
pub fn recenter_cursor(_x: i32, _y: i32, _width: i32, _height: i32) -> Result<(), String> {
    // macOS supplies reliable post-acceleration relative deltas. Keeping the
    // hidden source pointer parked at the edge avoids synthetic warp events.
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn recenter_cursor(x: i32, y: i32, width: i32, height: i32) -> Result<(), String> {
    use std::mem::size_of;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE,
        MOUSEEVENTF_MOVE_NOCOALESCE, MOUSEINPUT,
    };

    let normalized_x = i64::from(x.clamp(0, width - 1)) * 65_535 / i64::from(width - 1);
    let normalized_y = i64::from(y.clamp(0, height - 1)) * 65_535 / i64::from(height - 1);
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: normalized_x as i32,
                dy: normalized_y as i32,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_MOVE_NOCOALESCE,
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
            "Windows 鼠标回中失败：{}",
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
                        native_delta: Some((
                            event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X) as i32,
                            event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y) as i32,
                        )),
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
            CallNextHookEx, GetMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, MSG,
            MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN,
            WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN,
            WM_RBUTTONUP,
        },
    };

    type MouseCallback = dyn Fn(HookMouseEvent) -> bool + Send + Sync;
    static CALLBACK: OnceLock<Arc<MouseCallback>> = OnceLock::new();

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let data = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            if data.dwExtraInfo != SYNTHETIC_INPUT_MARKER {
                let wheel_delta = || (data.mouseData >> 16) as u16 as i16 as i64;
                let event = match wparam.0 as u32 {
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
