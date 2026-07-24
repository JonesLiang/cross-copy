use super::{
    simulate_native_shortcut_on_current_thread, LocalClipboard, PendingClipboard,
    CLIPBOARD_RETRY_ATTEMPTS, CLIPBOARD_RETRY_DELAY_MS,
};
use crate::logger::Logger;
use clipboard_rs::{Clipboard, ClipboardContext, ContentFormat};
use std::{path::PathBuf, sync::Arc, thread, time::Duration};
use windows::Win32::System::{
    Com::IDataObject,
    DataExchange::{CloseClipboard, CountClipboardFormats, EmptyClipboard, OpenClipboard},
    Ole::{OleFlushClipboard, OleGetClipboard, OleInitialize, OleSetClipboard, OleUninitialize},
};

struct OleApartment;

impl OleApartment {
    fn initialize() -> Result<Self, String> {
        unsafe { OleInitialize(None) }
            .map(|_| Self)
            .map_err(|error| format!("初始化 Windows OLE 剪贴板失败：{error}"))
    }
}

impl Drop for OleApartment {
    fn drop(&mut self) {
        unsafe { OleUninitialize() };
    }
}

enum OleSnapshot {
    Data(IDataObject),
    Empty,
}

struct ClipboardRestoreGuard<'a> {
    snapshot: Option<OleSnapshot>,
    logger: &'a Logger,
}

impl<'a> ClipboardRestoreGuard<'a> {
    fn capture(logger: &'a Logger) -> Result<Self, String> {
        let mut last_error = String::new();
        for attempt in 1..=CLIPBOARD_RETRY_ATTEMPTS {
            match unsafe { OleGetClipboard() } {
                Ok(data) => {
                    logger.info(
                        "clipboard_snapshot_captured",
                        format!("provider=windows_ole attempt={attempt}"),
                    );
                    return Ok(Self {
                        snapshot: Some(OleSnapshot::Data(data)),
                        logger,
                    });
                }
                Err(error) => {
                    last_error = error.to_string();
                    if clipboard_is_empty().unwrap_or(false) {
                        logger.info(
                            "clipboard_snapshot_captured",
                            format!("provider=windows_ole empty=true attempt={attempt}"),
                        );
                        return Ok(Self {
                            snapshot: Some(OleSnapshot::Empty),
                            logger,
                        });
                    }
                }
            }
            retry_sleep(attempt);
        }
        Err(format!("保护 Windows 剪贴板失败：{last_error}"))
    }

    fn restore(&mut self) -> Result<(), String> {
        if self.snapshot.is_none() {
            return Ok(());
        }
        let mut last_error = String::new();
        for attempt in 1..=CLIPBOARD_RETRY_ATTEMPTS {
            let result = match self.snapshot.as_ref().expect("snapshot checked") {
                OleSnapshot::Data(data) => unsafe {
                    OleSetClipboard(data)?;
                    // The STA thread is intentionally short-lived. Materialize
                    // the restored IDataObject before it exits so Windows does
                    // not depend on this thread remaining a clipboard owner.
                    OleFlushClipboard()
                }
                .map_err(|error| error.to_string()),
                OleSnapshot::Empty => clear_clipboard(),
            };
            match result {
                Ok(()) => {
                    self.snapshot = None;
                    self.logger.info(
                        "clipboard_snapshot_restored",
                        format!("provider=windows_ole attempt={attempt}"),
                    );
                    return Ok(());
                }
                Err(error) => last_error = error,
            }
            retry_sleep(attempt);
        }
        Err(format!("恢复 Windows 剪贴板失败：{last_error}"))
    }
}

impl Drop for ClipboardRestoreGuard<'_> {
    fn drop(&mut self) {
        if self.snapshot.is_some() {
            if let Err(error) = self.restore() {
                self.logger
                    .error("clipboard_emergency_restore_failed", error);
            }
        }
    }
}

pub(super) async fn windows_capture_selection(
    logger: Arc<Logger>,
) -> Result<LocalClipboard, String> {
    run_ole_task("crosscopy-copy", move || {
        let _apartment = OleApartment::initialize()?;
        let mut original = ClipboardRestoreGuard::capture(&logger)?;
        let operation = (|| {
            logger.info(
                "shortcut_copy_simulation_started",
                "dispatch=windows_blocking_thread provider=windows_ole",
            );
            simulate_native_shortcut_on_current_thread('c')?;
            logger.info("shortcut_copy_simulation_completed", "success=true");
            thread::sleep(Duration::from_millis(120));
            read_current_clipboard(&logger)
        })();
        let restore = original.restore();
        restore?;
        operation
    })
    .await
}

pub(super) async fn windows_paste_pending(
    pending: PendingClipboard,
    logger: Arc<Logger>,
) -> Result<(), String> {
    run_ole_task("crosscopy-paste", move || {
        let _apartment = OleApartment::initialize()?;
        let mut original = ClipboardRestoreGuard::capture(&logger)?;
        let operation = (|| {
            write_pending_clipboard(&pending, &logger)?;
            logger.info(
                "shortcut_paste_simulation_started",
                "dispatch=windows_blocking_thread provider=windows_ole",
            );
            simulate_native_shortcut_on_current_thread('v')?;
            logger.info("shortcut_paste_simulation_completed", "success=true");
            thread::sleep(Duration::from_millis(650));
            Ok(())
        })();
        let restore = original.restore();
        restore?;
        operation
    })
    .await
}

async fn run_ole_task<T, F>(name: &str, task: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    let (sender, receiver) = tokio::sync::oneshot::channel();
    thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            let _ = sender.send(task());
        })
        .map_err(|error| format!("无法启动 Windows 剪贴板线程：{error}"))?;
    receiver
        .await
        .map_err(|_| "Windows 剪贴板线程未返回结果".to_string())?
}

fn read_current_clipboard(logger: &Logger) -> Result<LocalClipboard, String> {
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
                        "provider=windows_ole kind={} attempt={attempt}",
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
        retry_sleep(attempt);
    }
    Err(last_error)
}

fn write_pending_clipboard(pending: &PendingClipboard, logger: &Logger) -> Result<(), String> {
    let mut last_error = String::new();
    for attempt in 1..=CLIPBOARD_RETRY_ATTEMPTS {
        let result = (|| {
            let context = ClipboardContext::new().map_err(|error| error.to_string())?;
            match pending {
                PendingClipboard::Text(text) => {
                    context
                        .set_text(text.clone())
                        .map_err(|error| error.to_string())?;
                    let actual = context.get_text().map_err(|error| error.to_string())?;
                    if &actual != text {
                        return Err("clipboard verification mismatch".into());
                    }
                }
                PendingClipboard::Files(files) => {
                    context
                        .set_files(files.clone())
                        .map_err(|error| error.to_string())?;
                    let actual = context.get_files().map_err(|error| error.to_string())?;
                    if actual.len() != files.len()
                        || !files
                            .iter()
                            .all(|expected| actual.iter().any(|value| value == expected))
                    {
                        return Err("clipboard verification mismatch".into());
                    }
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                logger.info(
                    "clipboard_pending_written",
                    format!("provider=windows_ole attempt={attempt}"),
                );
                return Ok(());
            }
            Err(error) => last_error = error,
        }
        retry_sleep(attempt);
    }
    Err(format!("写入 Windows 剪贴板失败：{last_error}"))
}

fn clipboard_is_empty() -> Result<bool, String> {
    unsafe { OpenClipboard(None) }.map_err(|error| error.to_string())?;
    let count = unsafe { CountClipboardFormats() };
    let close = unsafe { CloseClipboard() }.map_err(|error| error.to_string());
    close?;
    Ok(count == 0)
}

fn clear_clipboard() -> Result<(), String> {
    unsafe { OpenClipboard(None) }.map_err(|error| error.to_string())?;
    let empty = unsafe { EmptyClipboard() }.map_err(|error| error.to_string());
    let close = unsafe { CloseClipboard() }.map_err(|error| error.to_string());
    empty?;
    close
}

fn retry_sleep(attempt: usize) {
    if attempt < CLIPBOARD_RETRY_ATTEMPTS {
        thread::sleep(Duration::from_millis(CLIPBOARD_RETRY_DELAY_MS));
    }
}
