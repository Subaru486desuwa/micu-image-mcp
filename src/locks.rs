use std::{path::PathBuf, sync::Arc, time::Duration};

use fs4::{AsyncFileExt, TryLockError};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Clone, Debug)]
pub struct BigRequestGate {
    semaphore: Arc<Semaphore>,
    lock_path: Arc<PathBuf>,
    poll_interval: Duration,
    wait_note_threshold: Duration,
}

pub struct BigRequestGuard {
    _permit: OwnedSemaphorePermit,
    file: tokio::fs::File,
}

impl Drop for BigRequestGuard {
    fn drop(&mut self) {
        let _ignored = AsyncFileExt::unlock(&self.file);
    }
}

#[derive(Debug, Error)]
pub enum LockError {
    #[error("进程内 ≥2K 锁已关闭")]
    SemaphoreClosed,
    #[error("无法创建跨进程锁目录 {path}: {detail}")]
    CreateDirectory { path: String, detail: String },
    #[error("无法打开跨进程锁文件 {path}: {detail}")]
    OpenFile { path: String, detail: String },
    #[error("跨进程锁失败: {0}")]
    FileLock(String),
}

impl BigRequestGate {
    pub fn new(lock_path: PathBuf) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(1)),
            lock_path: Arc::new(lock_path),
            poll_interval: Duration::from_millis(100),
            wait_note_threshold: Duration::from_secs(2),
        }
    }

    pub async fn acquire(&self, notes: &mut Vec<String>) -> Result<BigRequestGuard, LockError> {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| LockError::SemaphoreClosed)?;
        let parent = self
            .lock_path
            .parent()
            .ok_or_else(|| LockError::CreateDirectory {
                path: self.lock_path.display().to_string(),
                detail: "锁路径没有 parent".into(),
            })?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| LockError::CreateDirectory {
                path: parent.display().to_string(),
                detail: error.to_string(),
            })?;
        let file = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.lock_path.as_ref())
            .await
            .map_err(|error| LockError::OpenFile {
                path: self.lock_path.display().to_string(),
                detail: error.to_string(),
            })?;
        let started = tokio::time::Instant::now();
        loop {
            match AsyncFileExt::try_lock(&file) {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => tokio::time::sleep(self.poll_interval).await,
                Err(TryLockError::Error(error)) => {
                    return Err(LockError::FileLock(error.to_string()));
                }
            }
        }
        let waited = started.elapsed();
        if waited > self.wait_note_threshold {
            notes.push(format!(
                "等待跨进程 ≥2K 锁 {:.1}s（其他 Claude Code / Codex 窗口同时在跑 ≥2K，已串行）",
                waited.as_secs_f64()
            ));
        }
        Ok(BigRequestGuard {
            _permit: permit,
            file,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelling_a_waiter_does_not_orphan_the_file_lock() {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let gate = BigRequestGate::new(temp.path().join("big.lock"));
        let mut notes = Vec::new();
        let first = gate
            .acquire(&mut notes)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let waiting_gate = gate.clone();
        let waiter = tokio::spawn(async move { waiting_gate.acquire(&mut Vec::new()).await });
        tokio::time::sleep(Duration::from_millis(150)).await;
        waiter.abort();
        let _ = waiter.await;
        drop(first);
        let acquired =
            tokio::time::timeout(Duration::from_secs(2), gate.acquire(&mut Vec::new())).await;
        assert!(acquired.is_ok_and(|result| result.is_ok()));
    }

    #[tokio::test]
    async fn independent_gates_serialize_on_the_same_cross_process_file() {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let path = temp.path().join("big.lock");
        let first_gate = BigRequestGate::new(path.clone());
        let second_gate = BigRequestGate::new(path);
        let first = first_gate
            .acquire(&mut Vec::new())
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let waiter = tokio::spawn(async move { second_gate.acquire(&mut Vec::new()).await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!waiter.is_finished());
        drop(first);
        let second = tokio::time::timeout(Duration::from_secs(2), waiter).await;
        assert!(second.is_ok_and(|join| join.is_ok_and(|result| result.is_ok())));
    }
}
