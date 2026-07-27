use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
const ROTATED_LOGS: usize = 3;

struct LogState {
    file: File,
    bytes: u64,
}

struct LogTarget {
    name: &'static str,
    current: PathBuf,
    state: Mutex<LogState>,
}

pub struct Logger {
    directory: PathBuf,
    general: LogTarget,
    mouse: LogTarget,
    clipboard: LogTarget,
}

impl Logger {
    pub fn new(app_directory: &Path) -> io::Result<Self> {
        let directory = app_directory.join("logs");
        fs::create_dir_all(&directory)?;
        Ok(Self {
            general: open_target(&directory, "crosscopy.log")?,
            mouse: open_target(&directory, "mouse.log")?,
            clipboard: open_target(&directory, "clipboard.log")?,
            directory,
        })
    }

    pub fn info(&self, event: &str, detail: impl AsRef<str>) {
        self.write("INFO", event, detail.as_ref());
    }

    pub fn warn(&self, event: &str, detail: impl AsRef<str>) {
        self.write("WARN", event, detail.as_ref());
    }

    pub fn error(&self, event: &str, detail: impl AsRef<str>) {
        self.write("ERROR", event, detail.as_ref());
    }

    pub fn export(&self, destination_directory: &Path, summary: &str) -> io::Result<PathBuf> {
        fs::create_dir_all(destination_directory)?;
        let destination =
            destination_directory.join(format!("CrossCopy-diagnostics-{}.txt", now_ms()));
        let mut output = File::create(&destination)?;
        writeln!(output, "CrossCopy diagnostics")?;
        writeln!(output, "generated_at_ms={}", now_ms())?;
        writeln!(output, "app_version={}", env!("CARGO_PKG_VERSION"))?;
        writeln!(
            output,
            "platform={}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )?;
        writeln!(output, "{summary}")?;
        self.append_target("general logs", &self.general, &mut output)?;
        self.append_target("mouse logs", &self.mouse, &mut output)?;
        self.append_target("clipboard logs", &self.clipboard, &mut output)?;
        output.flush()?;
        Ok(destination)
    }

    fn write(&self, level: &str, event: &str, detail: &str) {
        let mut clean_detail = detail.replace(['\r', '\n'], " ");
        if let Some(home) = dirs::home_dir() {
            clean_detail = clean_detail.replace(&home.to_string_lossy().to_string(), "$HOME");
        }
        let line = format!(
            "{} level={} event={} detail={}\n",
            now_ms(),
            level,
            event,
            clean_detail
        );
        let target = self.target(event);
        let Ok(mut state) = target.state.lock() else {
            return;
        };
        if state.bytes + line.len() as u64 > MAX_LOG_BYTES
            && self.rotate(target, &mut state).is_err()
        {
            return;
        }
        if state.file.write_all(line.as_bytes()).is_ok() {
            state.bytes += line.len() as u64;
            let _ = state.file.flush();
        }
    }

    fn target(&self, event: &str) -> &LogTarget {
        if event.starts_with("mouse_") {
            &self.mouse
        } else if is_clipboard_event(event) {
            &self.clipboard
        } else {
            &self.general
        }
    }

    fn rotate(&self, target: &LogTarget, state: &mut LogState) -> io::Result<()> {
        state.file.flush()?;
        let placeholder_path = self.directory.join(format!("{}.rotate.tmp", target.name));
        let placeholder = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&placeholder_path)?;
        let old_file = std::mem::replace(&mut state.file, placeholder);
        drop(old_file);
        for index in (1..=ROTATED_LOGS).rev() {
            let source = if index == 1 {
                target.current.clone()
            } else {
                self.directory
                    .join(format!("{}.{}", target.name, index - 1))
            };
            let destination = self.directory.join(format!("{}.{index}", target.name));
            if source.exists() {
                if destination.exists() {
                    fs::remove_file(&destination)?;
                }
                fs::rename(source, destination)?;
            }
        }
        state.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target.current)?;
        let _ = fs::remove_file(placeholder_path);
        state.bytes = 0;
        Ok(())
    }

    fn append_target(&self, title: &str, target: &LogTarget, output: &mut File) -> io::Result<()> {
        writeln!(output, "\n--- {title} ---")?;
        for index in (1..=ROTATED_LOGS).rev() {
            self.append_file(
                &self.directory.join(format!("{}.{index}", target.name)),
                output,
            )?;
        }
        self.append_file(&target.current, output)
    }

    fn append_file(&self, path: &Path, output: &mut File) -> io::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let mut input = File::open(path)?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let size = input.read(&mut buffer)?;
            if size == 0 {
                break;
            }
            output.write_all(&buffer[..size])?;
        }
        Ok(())
    }
}

fn open_target(directory: &Path, name: &'static str) -> io::Result<LogTarget> {
    let current = directory.join(name);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&current)?;
    let bytes = file.metadata()?.len();
    Ok(LogTarget {
        name,
        current,
        state: Mutex::new(LogState { file, bytes }),
    })
}

fn is_clipboard_event(event: &str) -> bool {
    event.starts_with("clipboard_")
        || event.starts_with("shortcut_copy_")
        || event.starts_with("shortcut_paste_")
        || event.starts_with("file_transfer_")
}

pub fn masked_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(value) => {
            let octets = value.octets();
            format!("{}.{}.{}.x", octets[0], octets[1], octets[2])
        }
        IpAddr::V6(value) => {
            let segments = value.segments();
            format!(
                "{:x}:{:x}:{:x}:{:x}::",
                segments[0], segments[1], segments[2], segments[3]
            )
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
