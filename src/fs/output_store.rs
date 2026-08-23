use std::path::PathBuf;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use tokio::io::AsyncWriteExt;

use crate::{config::AppPaths, fs::image::inspect_image_file};

pub use super::sandbox::OutputDirectory;
use super::sandbox::{OutputSandbox, TempLease};

pub const MAX_RESPONSE_BYTES: u64 = 25 * 1024 * 1024;

#[derive(Clone)]
pub struct OutputStore {
    sandbox: OutputSandbox,
    max_response_bytes: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SavedImage {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub actual_size: (u32, u32),
    pub actual_megapixels: f64,
}

impl OutputStore {
    pub fn new(paths: &AppPaths) -> Result<Self, String> {
        Ok(Self {
            sandbox: OutputSandbox::new(paths)?,
            max_response_bytes: MAX_RESPONSE_BYTES,
        })
    }

    pub fn resolve_save_dir(&self, save_dir: Option<&str>) -> Result<OutputDirectory, String> {
        self.sandbox.resolve(save_dir)
    }

    pub async fn save_base64(
        &self,
        encoded: &str,
        location: &OutputDirectory,
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
        let (lease, std_file) = self.sandbox.create_temp(location)?;
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
        location: &OutputDirectory,
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
        let (lease, std_file) = self.sandbox.create_temp(location)?;
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

    async fn finalize(
        &self,
        mut lease: TempLease,
        file: std::fs::File,
        size_bytes: u64,
        location: &OutputDirectory,
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
        let path = self
            .sandbox
            .commit(&mut lease, location, basename, info.extension)?;
        let megapixels = f64::from(info.dimensions.0) * f64::from(info.dimensions.1) / 1_000_000.0;
        Ok(SavedImage {
            path,
            size_bytes,
            actual_size: info.dimensions,
            actual_megapixels: (megapixels * 100.0).round() / 100.0,
        })
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

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use base64::engine::general_purpose::STANDARD;
    use futures_util::stream;
    use image::{ImageFormat, Rgb, RgbImage};

    use crate::config::{AppPaths, test_paths};

    use super::*;

    fn fixture() -> (tempfile::TempDir, AppPaths, OutputStore, Vec<u8>) {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let root = temp.path().join("out");
        let environment = BTreeMap::from([
            (
                "MICU_SAVE_DIR_ROOT".into(),
                root.to_string_lossy().into_owned(),
            ),
            (
                "MICU_SAVE_DIR".into(),
                root.join("nested").to_string_lossy().into_owned(),
            ),
        ]);
        let paths = test_paths(temp.path(), environment);
        let storage = OutputStore::new(&paths).unwrap_or_else(|error| panic!("{error}"));
        let image_path = temp.path().join("fixture.png");
        RgbImage::from_pixel(32, 24, Rgb([10, 20, 30]))
            .save_with_format(&image_path, ImageFormat::Png)
            .unwrap_or_else(|error| panic!("{error}"));
        let image = fs::read(image_path).unwrap_or_else(|error| panic!("{error}"));
        (temp, paths, storage, image)
    }

    #[test]
    fn save_dir_is_capability_scoped_and_rejects_symlink_escape() {
        let (temp, paths, storage, _) = fixture();
        let default = storage
            .resolve_save_dir(None)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(default.absolute, paths.default_save_dir);
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
            let link = paths.save_root.join("escape");
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
        let encoded = base64::Engine::encode(&STANDARD, &image);
        let saved = storage
            .save_base64(&encoded, &location, "b64")
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(saved.actual_size, (32, 24));
        let truncated = base64::Engine::encode(&STANDARD, &image[..40]);
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
