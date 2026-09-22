use super::{CLIPBOARD_RETRY_ATTEMPTS, CLIPBOARD_RETRY_DELAY_MS};
use crate::logger::Logger;
use objc2::{rc::autoreleasepool, runtime::ProtocolObject};
use objc2_app_kit::{NSPasteboard, NSPasteboardItem};
use objc2_foundation::{NSArray, NSData, NSString};
use std::time::Duration;

// Owned bytes can cross await/thread boundaries. Preserve item boundaries and
// every native representation, including application-specific pasteboard types.
pub(super) struct ClipboardSnapshot(Vec<Vec<(String, Vec<u8>)>>);

fn capture_from(board: &NSPasteboard) -> Result<ClipboardSnapshot, String> {
    autoreleasepool(|_| {
        let revision = board.changeCount();
        let mut snapshot = Vec::new();
        if let Some(items) = board.pasteboardItems() {
            for item in items {
                let mut representations = Vec::new();
                for kind in item.types() {
                    let bytes = item
                        .dataForType(&kind)
                        .ok_or_else(|| format!("无法读取剪贴板格式：{kind}"))?;
                    representations.push((kind.to_string(), bytes.to_vec()));
                }
                snapshot.push(representations);
            }
        }
        if snapshot.is_empty() && board.types().is_some_and(|types| !types.is_empty()) {
            return Err("剪贴板包含无法读取的原生数据，已保留原内容".into());
        }
        if board.changeCount() != revision {
            return Err("读取期间剪贴板发生变化".into());
        }
        Ok(ClipboardSnapshot(snapshot))
    })
}

fn restore_to(board: &NSPasteboard, snapshot: &ClipboardSnapshot) -> Result<(), String> {
    autoreleasepool(|_| {
        let mut items = Vec::new();
        for representations in &snapshot.0 {
            let item = NSPasteboardItem::new();
            for (kind, bytes) in representations {
                let data = NSData::with_bytes(bytes);
                if !item.setData_forType(&data, &NSString::from_str(kind)) {
                    return Err(format!("无法恢复剪贴板格式：{kind}"));
                }
            }
            items.push(ProtocolObject::from_retained(item));
        }
        board.clearContents();
        if !items.is_empty() && !board.writeObjects(&NSArray::from_retained_slice(&items)) {
            return Err("写回本机剪贴板失败".into());
        }
        Ok(())
    })
}

pub(super) fn revision() -> isize {
    NSPasteboard::generalPasteboard().changeCount()
}

pub(super) async fn wait_for_change(previous: isize) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while revision() == previous {
        if tokio::time::Instant::now() >= deadline {
            return Err("目标程序未更新剪贴板，请确认已选中文本或文件".into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

pub(super) async fn capture_clipboard(logger: &Logger) -> Result<ClipboardSnapshot, String> {
    let mut last_error = String::new();
    for attempt in 1..=CLIPBOARD_RETRY_ATTEMPTS {
        match capture_from(&NSPasteboard::generalPasteboard()) {
            Ok(snapshot) => {
                logger.info(
                    "clipboard_snapshot_captured",
                    format!("provider=macos_native attempt={attempt}"),
                );
                return Ok(snapshot);
            }
            Err(error) => last_error = error,
        }
        if attempt < CLIPBOARD_RETRY_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(CLIPBOARD_RETRY_DELAY_MS)).await;
        }
    }
    Err(format!("保护本机剪贴板失败：{last_error}"))
}

pub(super) async fn restore_clipboard(
    snapshot: ClipboardSnapshot,
    logger: &Logger,
) -> Result<(), String> {
    let mut last_error = String::new();
    for attempt in 1..=CLIPBOARD_RETRY_ATTEMPTS {
        match restore_to(&NSPasteboard::generalPasteboard(), &snapshot) {
            Ok(()) => {
                logger.info(
                    "clipboard_snapshot_restored",
                    format!("provider=macos_native attempt={attempt}"),
                );
                return Ok(());
            }
            Err(error) => last_error = error,
        }
        if attempt < CLIPBOARD_RETRY_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(CLIPBOARD_RETRY_DELAY_MS)).await;
        }
    }
    Err(format!("恢复本机剪贴板失败：{last_error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_multiple_items_and_custom_binary_formats() {
        let board = NSPasteboard::pasteboardWithUniqueName();
        let snapshot = ClipboardSnapshot(vec![
            vec![
                ("com.crosscopy.test.custom".into(), vec![0, 128, 255]),
                ("public.utf8-plain-text".into(), b"first".to_vec()),
            ],
            vec![("public.utf8-plain-text".into(), b"second".to_vec())],
        ]);
        restore_to(&board, &snapshot).unwrap();
        let actual = capture_from(&board).unwrap();
        assert_eq!(actual.0, snapshot.0);
        restore_to(&board, &ClipboardSnapshot(Vec::new())).unwrap();
        assert!(capture_from(&board).unwrap().0.is_empty());
    }
}
