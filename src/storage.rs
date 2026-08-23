use std::{
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use futures_util::{Stream, StreamExt};
use tokio::io::AsyncWriteExt;

use crate::{config::Config, validation::image::inspect_image_file};

pub const MAX_RESPONSE_BYTES: u64 = 25 * 1024 * 1024;

#[derive(Clone)]
pub struct Storage {
    root: Arc<Dir>,
    root_path: Arc<PathBuf>,
    configured_root: Arc<PathBuf>,
    default_save_dir: Arc<PathBuf>,
    max_response_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaveLocation {
    pub relative: PathBuf,
    pub absolute: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SavedImage {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub actual_size: (u32, u32),
    pub actual_megapixels: f64,
}

impl Storage {
    pub fn new(config: &Config) -> Result<Self, String> {
        std::fs::create_dir_all(&config.save_root).map_err(|error| {
            format!("无法创建 save root {}: {error}", config.save_root.display())
        })?;
        let root_path = std::fs::canonicalize(&config.save_root).map_err(|error| {
            format!("无法解析 save root {}: {error}", config.save_root.display())
        })?;
        let root = Dir::open_ambient_dir(&root_path, ambient_authority())
            .map_err(|error| format!("无法打开 save root {}: {error}", root_path.display()))?;
        Ok(Self {
            root: Arc::new(root),
            root_path: Arc::new(root_path),
            configured_root: Arc::new(normalize_absolute(&config.save_root)?),
            default_save_dir: Arc::new(config.save_dir.clone()),
            max_response_bytes: MAX_RESPONSE_BYTES,
        })
    }

    pub fn resolve_save_dir(&self, save_dir: Option<&str>) -> Result<SaveLocation, String> {
        let raw_for_error = save_dir.map(str::to_owned);
        let requested = match save_dir {
            Some(raw) => expand_path(raw)?,
            None => self.default_save_dir.as_ref().clone(),
        };
        let normalized = normalize_absolute(&requested)?;
        let relative = match normalized
            .strip_prefix(self.root_path.as_ref())
            .or_else(|_| normalized.strip_prefix(self.configured_root.as_ref()))
        {
            Ok(relative) => relative.to_path_buf(),
            Err(_) if save_dir.is_none() => PathBuf::new(),
            Err(_) => return Err(self.save_dir_error(raw_for_error.as_deref().unwrap_or_default())),
        };
        if !relative.as_os_str().is_empty() {
            self.root.create_dir_all(&relative).map_err(|_| {
                self.save_dir_error(
                    raw_for_error
                        .as_deref()
                        .unwrap_or_else(|| requested.to_str().unwrap_or_default()),
                )
            })?;
        }
        let absolute = self.root_path.join(&relative);
        let canonical = std::fs::canonicalize(&absolute).map_err(|_| {
            self.save_dir_error(
                raw_for_error
                    .as_deref()
                    .unwrap_or_else(|| requested.to_str().unwrap_or_default()),
            )
        })?;
        let canonical_relative = canonical
            .strip_prefix(self.root_path.as_ref())
            .map_err(|_| {
                self.save_dir_error(
                    raw_for_error
                        .as_deref()
                        .unwrap_or_else(|| requested.to_str().unwrap_or_default()),
                )
            })?
            .to_path_buf();
        // Opening through the capability root also rejects a symlink traversal that raced the
        // canonicalization above.
        if !canonical_relative.as_os_str().is_empty() {
            self.root.open_dir(&canonical_relative).map_err(|_| {
                self.save_dir_error(
                    raw_for_error
                        .as_deref()
                        .unwrap_or_else(|| requested.to_str().unwrap_or_default()),
                )
            })?;
        }
        Ok(SaveLocation {
            relative: canonical_relative,
            absolute: canonical,
        })
    }

    pub async fn save_base64(
        &self,
        encoded: &str,
        location: &SaveLocation,
        basename: &str,
    ) -> Result<SavedImage, String> {
        let estimated = (encoded.len() as u64).saturating_mul(3) / 4;
        if estimated > self.max_response_bytes {
            return Err(format!(
                "b64 响应解码后约 {:.1}MB 超过单图上限 {}MB；可能是代理返回了错误内容",
                estimated as f64 / 1024.0 / 1024.0,
                self.max_response_bytes / 1024 / 1024
            ));
        }
        let (lease, std_file) = self.create_temp(location)?;
        let mut file = tokio::fs::File::from_std(std_file);
        let mut encoded_chunk = Vec::with_capacity(8_192);
        let mut total = 0_u64;
        let mut saw_padding = false;
        for byte in encoded.bytes() {
            if byte.is_ascii_whitespace() {
                continue;
            }
            if saw_padding && byte != b'=' {
                return Err("base64 解码失败: padding 后仍有数据".into());
            }
            if byte == b'=' {
                saw_padding = true;
            }
            encoded_chunk.push(byte);
            if encoded_chunk.len() == 8_192 && !saw_padding {
                total =
                    decode_chunk_to_file(&encoded_chunk, &mut file, total, self.max_response_bytes)
                        .await?;
                encoded_chunk.clear();
            }
        }
        if !encoded_chunk.is_empty() {
            total = decode_chunk_to_file(&encoded_chunk, &mut file, total, self.max_response_bytes)
                .await?;
        }
        file.flush()
            .await
            .map_err(|error| format!("b64 响应临时文件 flush 失败: {error}"))?;
        file.sync_data()
            .await
            .map_err(|error| format!("b64 响应临时文件 sync 失败: {error}"))?;
        let std_file = file.into_std().await;
        self.finalize(lease, std_file, total, location, basename, "b64 响应")
            .await
    }

    pub async fn save_stream<S, E>(
        &self,
        stream: S,
        content_length: Option<u64>,
        location: &SaveLocation,
        basename: &str,
        source_label: &str,
    ) -> Result<SavedImage, String>
    where
        S: Stream<Item = Result<Bytes, E>> + Send,
        E: std::fmt::Display + Send,
    {
        if content_length.is_some_and(|length| length > self.max_response_bytes) {
            let length = content_length.unwrap_or_default();
            return Err(format!(
                "{source_label} Content-Length={:.1}MB 超过 {}MB 上限",
                length as f64 / 1024.0 / 1024.0,
                self.max_response_bytes / 1024 / 1024
            ));
        }
        let (lease, std_file) = self.create_temp(location)?;
        let mut file = tokio::fs::File::from_std(std_file);
        let mut total = 0_u64;
        futures_util::pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| format!("{source_label} 下载失败: {error}"))?;
            total = total.saturating_add(chunk.len() as u64);
            if total > self.max_response_bytes {
                return Err(format!(
                    "{source_label} 实际下载 >{}MB，已中断",
                    self.max_response_bytes / 1024 / 1024
                ));
            }
            file.write_all(&chunk)
                .await
                .map_err(|error| format!("{source_label} 临时文件写入失败: {error}"))?;
        }
        file.flush()
            .await
            .map_err(|error| format!("{source_label} 临时文件 flush 失败: {error}"))?;
        file.sync_data()
            .await
            .map_err(|error| format!("{source_label} 临时文件 sync 失败: {error}"))?;
        let std_file = file.into_std().await;
        self.finalize(lease, std_file, total, location, basename, source_label)
            .await
    }

    #[cfg(test)]
    fn with_max_response_bytes(mut self, limit: u64) -> Self {
        self.max_response_bytes = limit;
        self
    }

    fn save_dir_error(&self, raw: &str) -> String {
        format!(
            "save_dir 必须在安全根目录 {} 之下；收到 {}。留空让 MCP 用默认目录，或先把 MICU_SAVE_DIR_ROOT 改到你想要的位置。",
            self.root_path.display(),
            python_string_repr(raw)
        )
    }

    fn create_temp(&self, location: &SaveLocation) -> Result<(TempLease, std::fs::File), String> {
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);
        let epoch_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for _ in 0..1_000 {
            let counter = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let filename = format!(".micu-{}-{epoch_nanos}-{counter}.tmp", std::process::id());
            let relative = location.relative.join(filename);
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            match self.root.open_with(&relative, &options) {
                Ok(file) => {
                    return Ok((
                        TempLease {
                            cleanup: Arc::new(TempCleanup {
                                root: self.root.clone(),
                                relative,
                                removed: AtomicBool::new(false),
                            }),
                        },
                        file.into_std(),
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!("无法创建输出临时文件: {error}"));
                }
            }
        }
        Err("无法创建唯一输出临时文件".into())
    }

    async fn finalize(
        &self,
        mut lease: TempLease,
        file: std::fs::File,
        size_bytes: u64,
        location: &SaveLocation,
        basename: &str,
        source_label: &str,
    ) -> Result<SavedImage, String> {
        let label = source_label.to_owned();
        let validation_lease = lease.clone();
        let info = tokio::task::spawn_blocking(move || {
            let _keep_temp_alive = validation_lease;
            inspect_image_file(&file, size_bytes, &label)
        })
        .await
        .map_err(|error| format!("{source_label} 图片校验任务失败: {error}"))??;
        let mut committed: Option<PathBuf> = None;
        for index in 1..=1_000 {
            let filename = if index == 1 {
                format!("{basename}.{}", info.extension)
            } else {
                format!("{basename}_{index}.{}", info.extension)
            };
            let candidate = location.relative.join(filename);
            match self
                .root
                .hard_link(lease.relative(), self.root.as_ref(), &candidate)
            {
                Ok(()) => {
                    committed = Some(candidate);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("输出文件原子提交失败: {error}")),
            }
        }
        let relative = committed.ok_or_else(|| format!("basename 冲突过多：{basename}"))?;
        lease.remove_now();
        let path = self.root_path.join(relative);
        let megapixels = f64::from(info.dimensions.0) * f64::from(info.dimensions.1) / 1_000_000.0;
        Ok(SavedImage {
            path,
            size_bytes,
            actual_size: info.dimensions,
            actual_megapixels: (megapixels * 100.0).round() / 100.0,
        })
    }
}

#[derive(Clone)]
struct TempLease {
    cleanup: Arc<TempCleanup>,
}

impl TempLease {
    fn relative(&self) -> &Path {
        &self.cleanup.relative
    }

    fn remove_now(&mut self) {
        if self
            .cleanup
            .root
            .remove_file(&self.cleanup.relative)
            .is_ok()
        {
            self.cleanup.removed.store(true, Ordering::Release);
        }
    }
}

struct TempCleanup {
    root: Arc<Dir>,
    relative: PathBuf,
    removed: AtomicBool,
}

impl Drop for TempCleanup {
    fn drop(&mut self) {
        if !self.removed.load(Ordering::Acquire) && self.root.remove_file(&self.relative).is_ok() {
            self.removed.store(true, Ordering::Release);
        }
    }
}

async fn decode_chunk_to_file(
    encoded: &[u8],
    file: &mut tokio::fs::File,
    previous_total: u64,
    limit: u64,
) -> Result<u64, String> {
    let estimated = encoded.len().saturating_mul(3) / 4 + 3;
    let mut decoded = vec![0_u8; estimated];
    let decoded_len = STANDARD
        .decode_slice(encoded, &mut decoded)
        .map_err(|error| format!("base64 解码失败: {error}"))?;
    let total = previous_total.saturating_add(decoded_len as u64);
    if total > limit {
        return Err(format!(
            "b64 响应解码后超过单图上限 {}MB；可能是代理返回了错误内容",
            limit / 1024 / 1024
        ));
    }
    file.write_all(&decoded[..decoded_len])
        .await
        .map_err(|error| format!("b64 响应临时文件写入失败: {error}"))?;
    Ok(total)
}

fn expand_path(raw: &str) -> Result<PathBuf, String> {
    let path = if raw == "~" {
        dirs::home_dir().ok_or_else(|| "无法确定用户 home 目录".to_owned())?
    } else if let Some(remainder) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        dirs::home_dir()
            .ok_or_else(|| "无法确定用户 home 目录".to_owned())?
            .join(remainder)
    } else {
        PathBuf::from(raw)
    };
    normalize_absolute(&path)
}

fn normalize_absolute(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("无法解析 save_dir: {error}"))?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err("save_dir 包含无法解析的 ..".into());
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Ok(normalized)
}

fn python_string_repr(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use futures_util::stream;
    use image::{ImageFormat, Rgb, RgbImage};

    use super::*;

    fn fixture() -> (tempfile::TempDir, Config, Storage, Vec<u8>) {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let root = temp.path().join("out");
        let config = Config::from_map(&BTreeMap::from([
            (
                "HOME".into(),
                temp.path().join("home").to_string_lossy().into_owned(),
            ),
            (
                "MICU_SAVE_DIR_ROOT".into(),
                root.to_string_lossy().into_owned(),
            ),
            (
                "MICU_SAVE_DIR".into(),
                root.join("nested").to_string_lossy().into_owned(),
            ),
        ]))
        .unwrap_or_else(|error| panic!("{error}"));
        let storage = Storage::new(&config).unwrap_or_else(|error| panic!("{error}"));
        let image_path = temp.path().join("fixture.png");
        RgbImage::from_pixel(32, 24, Rgb([10, 20, 30]))
            .save_with_format(&image_path, ImageFormat::Png)
            .unwrap_or_else(|error| panic!("{error}"));
        let image = fs::read(image_path).unwrap_or_else(|error| panic!("{error}"));
        (temp, config, storage, image)
    }

    #[test]
    fn save_dir_is_capability_scoped_and_rejects_symlink_escape() {
        let (temp, config, storage, _) = fixture();
        let default = storage
            .resolve_save_dir(None)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            default.absolute,
            fs::canonicalize(&config.save_dir).unwrap_or_else(|error| panic!("{error}"))
        );
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap_or_else(|error| panic!("{error}"));
        assert!(
            storage
                .resolve_save_dir(Some(&outside.to_string_lossy()))
                .is_err_and(|error| error.contains("安全根目录"))
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let link = config.save_root.join("escape");
            symlink(&outside, &link).unwrap_or_else(|error| panic!("{error}"));
            assert!(
                storage
                    .resolve_save_dir(Some(&link.to_string_lossy()))
                    .is_err_and(|error| error.contains("安全根目录"))
            );
        }
    }

    #[tokio::test]
    async fn atomic_no_clobber_preserves_both_outputs() {
        let (_temp, _config, storage, image) = fixture();
        let location = storage
            .resolve_save_dir(None)
            .unwrap_or_else(|error| panic!("{error}"));
        let first = storage
            .save_stream(
                stream::iter([Ok::<_, String>(Bytes::from(image.clone()))]),
                None,
                &location,
                "same",
                "fixture",
            )
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let second = storage
            .save_stream(
                stream::iter([Ok::<_, String>(Bytes::from(image))]),
                None,
                &location,
                "same",
                "fixture",
            )
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(first.path.ends_with("same.png"));
        assert!(second.path.ends_with("same_2.png"));
        assert!(first.path.is_file() && second.path.is_file());
    }

    #[tokio::test]
    async fn base64_decodes_directly_to_disk_and_rejects_truncation() {
        let (_temp, _config, storage, image) = fixture();
        let location = storage
            .resolve_save_dir(None)
            .unwrap_or_else(|error| panic!("{error}"));
        let encoded = STANDARD.encode(&image);
        let saved = storage
            .save_base64(&encoded, &location, "b64")
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(saved.actual_size, (32, 24));
        let truncated = STANDARD.encode(&image[..40]);
        assert!(
            storage
                .save_base64(&truncated, &location, "bad")
                .await
                .is_err_and(|error| error.contains("完整解码"))
        );
        assert!(!location.absolute.join("bad.png").exists());
    }

    #[tokio::test]
    async fn content_length_and_streaming_caps_abort_without_final_file() {
        let (_temp, _config, storage, _image) = fixture();
        let storage = storage.with_max_response_bytes(100);
        let location = storage
            .resolve_save_dir(None)
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            storage
                .save_stream(
                    stream::empty::<Result<Bytes, String>>(),
                    Some(101),
                    &location,
                    "length",
                    "远端图"
                )
                .await
                .is_err_and(|error| error.contains("Content-Length"))
        );
        assert!(
            storage
                .save_stream(
                    stream::iter([
                        Ok::<_, String>(Bytes::from(vec![0_u8; 60])),
                        Ok(Bytes::from(vec![0_u8; 60]))
                    ]),
                    None,
                    &location,
                    "stream",
                    "远端图",
                )
                .await
                .is_err_and(|error| error.contains("实际下载"))
        );
        assert!(!location.absolute.join("length.png").exists());
        assert!(!location.absolute.join("stream.png").exists());
    }

    #[tokio::test]
    async fn cancelling_a_stream_removes_the_partial_temp_file() {
        let (_temp, _config, storage, _image) = fixture();
        let location = storage
            .resolve_save_dir(None)
            .unwrap_or_else(|error| panic!("{error}"));
        let task_storage = storage.clone();
        let task_location = location.clone();
        let task = tokio::spawn(async move {
            task_storage
                .save_stream(
                    stream::pending::<Result<Bytes, String>>(),
                    None,
                    &task_location,
                    "cancelled",
                    "远端图",
                )
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        task.abort();
        let _ = task.await;
        let leftovers = fs::read_dir(&location.absolute)
            .unwrap_or_else(|error| panic!("{error}"))
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".micu-"))
            .count();
        assert_eq!(leftovers, 0);
    }
}
