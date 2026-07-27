use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncSeekExt},
};

const MAX_EDITOR_FILE_SIZE: u64 = 16 * 1024 * 1024;
const MAX_RANGE_SIZE: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsEntry {
    pub name: String,
    pub path: String,
    pub directory: bool,
    pub symlink: bool,
    pub size: u64,
    pub modified_at: Option<u64>,
    pub readonly: bool,
    pub hidden: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FsRequest {
    Roots,
    List {
        path: String,
    },
    Metadata {
        path: String,
    },
    Read {
        path: String,
    },
    ReadRange {
        path: String,
        offset: u64,
        length: u64,
    },
    Write {
        path: String,
        data: String,
        expected_modified_at: Option<u64>,
    },
    CreateDirectory {
        path: String,
    },
    CreateFile {
        path: String,
    },
    Rename {
        path: String,
        destination: String,
    },
    Copy {
        path: String,
        destination: String,
    },
    Remove {
        path: String,
        recursive: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FsResponse {
    Entries {
        entries: Vec<FsEntry>,
    },
    File {
        path: String,
        data: String,
        modified_at: Option<u64>,
        size: u64,
    },
    FileRange {
        path: String,
        data: String,
        offset: u64,
        total_size: u64,
        eof: bool,
    },
    Done {
        entry: Option<FsEntry>,
    },
    Error {
        message: String,
    },
}

pub async fn handle(request: FsRequest) -> FsResponse {
    match handle_inner(request).await {
        Ok(response) => response,
        Err(message) => FsResponse::Error { message },
    }
}

pub async fn write_bytes(
    path: String,
    bytes: Vec<u8>,
    expected_modified_at: Option<u64>,
) -> FsResponse {
    match write_bytes_inner(path, bytes, expected_modified_at).await {
        Ok(response) => response,
        Err(message) => FsResponse::Error { message },
    }
}

pub fn checked_paths(values: Vec<String>) -> Result<Vec<PathBuf>, String> {
    if values.is_empty() || values.len() > 128 {
        return Err("请选择 1 到 128 个文件或文件夹".into());
    }
    values.iter().map(|value| checked_absolute(value)).collect()
}

async fn handle_inner(request: FsRequest) -> Result<FsResponse, String> {
    match request {
        FsRequest::Roots => Ok(FsResponse::Entries {
            entries: roots().await,
        }),
        FsRequest::List { path } => {
            let path = checked_absolute(&path)?;
            let mut reader = fs::read_dir(&path)
                .await
                .map_err(|error| friendly_io_error("无法打开文件夹", error))?;
            let mut entries = Vec::new();
            while let Some(item) = reader
                .next_entry()
                .await
                .map_err(|error| friendly_io_error("无法读取文件夹", error))?
            {
                if let Ok(entry) = entry_from_path(item.path()).await {
                    entries.push(entry);
                }
            }
            entries.sort_by(|left, right| {
                right
                    .directory
                    .cmp(&left.directory)
                    .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            });
            Ok(FsResponse::Entries { entries })
        }
        FsRequest::Metadata { path } => {
            let path = checked_absolute(&path)?;
            Ok(FsResponse::Done {
                entry: Some(entry_from_path(path).await?),
            })
        }
        FsRequest::Read { path } => {
            let path = checked_absolute(&path)?;
            let metadata = fs::metadata(&path)
                .await
                .map_err(|error| friendly_io_error("无法读取文件", error))?;
            if !metadata.is_file() {
                return Err("只能打开普通文件".into());
            }
            if metadata.len() > MAX_EDITOR_FILE_SIZE {
                return Err("文件超过 16 MB，请通过系统文件管理器或外部应用打开".into());
            }
            let mut file = fs::File::open(&path)
                .await
                .map_err(|error| friendly_io_error("无法打开文件", error))?;
            file.seek(std::io::SeekFrom::Start(0))
                .await
                .map_err(|error| friendly_io_error("无法定位文件", error))?;
            let mut bytes = Vec::with_capacity(metadata.len() as usize);
            file.read_to_end(&mut bytes)
                .await
                .map_err(|error| friendly_io_error("无法读取文件", error))?;
            Ok(FsResponse::File {
                path: path_to_wire(&path),
                data: STANDARD.encode(bytes),
                modified_at: modified_at(&metadata),
                size: metadata.len(),
            })
        }
        FsRequest::ReadRange {
            path,
            offset,
            length,
        } => {
            if length == 0 || length > MAX_RANGE_SIZE {
                return Err("单次文件分片必须在 1 字节到 4 MB 之间".into());
            }
            let path = checked_absolute(&path)?;
            let metadata = fs::metadata(&path)
                .await
                .map_err(|error| friendly_io_error("无法读取文件", error))?;
            if !metadata.is_file() {
                return Err("只能读取普通文件".into());
            }
            let mut file = fs::File::open(&path)
                .await
                .map_err(|error| friendly_io_error("无法打开文件", error))?;
            file.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|error| friendly_io_error("无法定位文件", error))?;
            let available = metadata.len().saturating_sub(offset);
            let read_size = available.min(length) as usize;
            let mut bytes = vec![0_u8; read_size];
            file.read_exact(&mut bytes)
                .await
                .map_err(|error| friendly_io_error("无法读取文件分片", error))?;
            Ok(FsResponse::FileRange {
                path: path_to_wire(&path),
                data: STANDARD.encode(bytes),
                offset,
                total_size: metadata.len(),
                eof: offset.saturating_add(read_size as u64) >= metadata.len(),
            })
        }
        FsRequest::Write {
            path,
            data,
            expected_modified_at,
        } => {
            let bytes = STANDARD
                .decode(data)
                .map_err(|_| "文件内容编码无效".to_string())?;
            write_bytes_inner(path, bytes, expected_modified_at).await
        }
        FsRequest::CreateDirectory { path } => {
            let path = checked_absolute(&path)?;
            fs::create_dir(&path)
                .await
                .map_err(|error| friendly_io_error("无法新建文件夹", error))?;
            Ok(FsResponse::Done {
                entry: Some(entry_from_path(path).await?),
            })
        }
        FsRequest::CreateFile { path } => {
            let path = checked_absolute(&path)?;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
                .map_err(|error| friendly_io_error("无法新建文件", error))?;
            Ok(FsResponse::Done {
                entry: Some(entry_from_path(path).await?),
            })
        }
        FsRequest::Rename { path, destination } => {
            let path = checked_absolute(&path)?;
            let destination = checked_absolute(&destination)?;
            fs::rename(&path, &destination)
                .await
                .map_err(|error| friendly_io_error("无法移动或重命名", error))?;
            Ok(FsResponse::Done {
                entry: Some(entry_from_path(destination).await?),
            })
        }
        FsRequest::Copy { path, destination } => {
            let path = checked_absolute(&path)?;
            let destination = checked_absolute(&destination)?;
            copy_path(&path, &destination).await?;
            Ok(FsResponse::Done {
                entry: Some(entry_from_path(destination).await?),
            })
        }
        FsRequest::Remove { path, recursive } => {
            let path = checked_absolute(&path)?;
            if is_filesystem_root(&path) {
                return Err("不能删除文件系统根目录".into());
            }
            let metadata = fs::symlink_metadata(&path)
                .await
                .map_err(|error| friendly_io_error("无法读取目标", error))?;
            if metadata.is_dir() {
                if recursive {
                    fs::remove_dir_all(path).await
                } else {
                    fs::remove_dir(path).await
                }
            } else {
                fs::remove_file(path).await
            }
            .map_err(|error| friendly_io_error("无法删除", error))?;
            Ok(FsResponse::Done { entry: None })
        }
    }
}

async fn copy_path(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source)
        .await
        .map_err(|error| friendly_io_error("无法读取复制源", error))?;
    if metadata.is_file() || metadata.file_type().is_symlink() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|error| friendly_io_error("无法创建目标文件夹", error))?;
        }
        fs::copy(source, destination)
            .await
            .map_err(|error| friendly_io_error("无法复制文件", error))?;
        return Ok(());
    }
    fs::create_dir(destination)
        .await
        .map_err(|error| friendly_io_error("无法创建目标文件夹", error))?;
    let mut pending = vec![(source.to_path_buf(), destination.to_path_buf())];
    while let Some((current_source, current_destination)) = pending.pop() {
        let mut reader = fs::read_dir(&current_source)
            .await
            .map_err(|error| friendly_io_error("无法读取源文件夹", error))?;
        while let Some(item) = reader
            .next_entry()
            .await
            .map_err(|error| friendly_io_error("无法读取源文件夹", error))?
        {
            let target = current_destination.join(item.file_name());
            let item_metadata = item
                .metadata()
                .await
                .map_err(|error| friendly_io_error("无法读取文件信息", error))?;
            if item_metadata.is_dir() {
                fs::create_dir(&target)
                    .await
                    .map_err(|error| friendly_io_error("无法创建目标文件夹", error))?;
                pending.push((item.path(), target));
            } else {
                fs::copy(item.path(), target)
                    .await
                    .map_err(|error| friendly_io_error("无法复制文件", error))?;
            }
        }
    }
    Ok(())
}

async fn write_bytes_inner(
    path: String,
    bytes: Vec<u8>,
    expected_modified_at: Option<u64>,
) -> Result<FsResponse, String> {
    let path = checked_absolute(&path)?;
    if let Some(expected) = expected_modified_at {
        let metadata = fs::metadata(&path)
            .await
            .map_err(|error| friendly_io_error("无法检查文件", error))?;
        if modified_at(&metadata).is_some_and(|actual| actual != expected) {
            return Err("文件已在另一处被修改，请重新打开后再保存".into());
        }
    }
    if bytes.len() as u64 > MAX_EDITOR_FILE_SIZE {
        return Err("单次编辑保存不能超过 16 MB".into());
    }
    fs::write(&path, bytes)
        .await
        .map_err(|error| friendly_io_error("无法保存文件", error))?;
    Ok(FsResponse::Done {
        entry: Some(entry_from_path(path).await?),
    })
}

async fn roots() -> Vec<FsEntry> {
    let mut paths = Vec::new();
    #[cfg(target_os = "windows")]
    {
        for letter in b'A'..=b'Z' {
            let path = PathBuf::from(format!("{}:\\", letter as char));
            if path.exists() {
                paths.push(path);
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        paths.push(PathBuf::from("/"));
        if let Some(home) = dirs::home_dir() {
            paths.push(home.clone());
            for name in ["Desktop", "Documents", "Downloads"] {
                let path = home.join(name);
                if path.exists() {
                    paths.push(path);
                }
            }
        }
        let volumes = PathBuf::from("/Volumes");
        if let Ok(mut reader) = fs::read_dir(volumes).await {
            while let Ok(Some(item)) = reader.next_entry().await {
                paths.push(item.path());
            }
        }
    }
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for path in paths {
        let wire = path_to_wire(&path);
        if seen.insert(wire) {
            if let Ok(entry) = entry_from_path(path).await {
                entries.push(entry);
            }
        }
    }
    entries
}

async fn entry_from_path(path: PathBuf) -> Result<FsEntry, String> {
    let symlink_metadata = fs::symlink_metadata(&path)
        .await
        .map_err(|error| friendly_io_error("无法读取文件信息", error))?;
    let metadata = fs::metadata(&path)
        .await
        .unwrap_or(symlink_metadata.clone());
    let name = display_name(&path);
    Ok(FsEntry {
        hidden: name.starts_with('.'),
        name,
        path: path_to_wire(&path),
        directory: metadata.is_dir(),
        symlink: symlink_metadata.file_type().is_symlink(),
        size: if metadata.is_file() {
            metadata.len()
        } else {
            0
        },
        modified_at: modified_at(&metadata),
        readonly: metadata.permissions().readonly(),
    })
}

fn checked_absolute(value: &str) -> Result<PathBuf, String> {
    if value.contains('\0') {
        return Err("文件路径无效".into());
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err("只允许访问绝对路径".into());
    }
    Ok(path)
}

fn is_filesystem_root(path: &Path) -> bool {
    path.parent().is_none()
}

fn path_to_wire(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_to_wire(path))
}

fn modified_at(metadata: &std::fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as u64)
}

fn friendly_io_error(action: &str, error: std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => format!("{action}：没有系统权限"),
        std::io::ErrorKind::NotFound => format!("{action}：文件或文件夹不存在"),
        std::io::ErrorKind::AlreadyExists => format!("{action}：同名项目已经存在"),
        _ => format!("{action}：{error}"),
    }
}
