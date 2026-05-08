//! 双层锁：进程内 `tokio::sync::Mutex` + 跨进程 `fs2::FileExt::lock_exclusive`。
//!
//! 与 Python `_BIG_SIZE_LOCK` (asyncio.Semaphore) + `_big_size_file_lock_async`
//! (fcntl.flock / msvcrt.locking) 等价。
//!
//! `fs2` 在 POSIX 用 `flock(2)`，Windows 用 `LockFileEx`，关 fd 自动释放，
//! 进程崩溃由内核回收，不留死锁。

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use fs2::FileExt;
use once_cell::sync::Lazy;
use tokio::sync::Mutex;

pub static BIG_SIZE_PROCESS_LOCK: Lazy<Arc<Mutex<()>>> = Lazy::new(|| Arc::new(Mutex::new(())));

pub static BIG_SIZE_LOCK_PATH: Lazy<PathBuf> = Lazy::new(|| {
    let cache = dirs::home_dir()
        .map(|h| h.join(".cache").join("micu-image"))
        .unwrap_or_else(|| std::env::temp_dir().join("micu-image"));
    cache.join("bigsize.lock")
});

/// RAII 跨进程锁 guard：drop 时自动释放 fd。
pub struct CrossProcessLock {
    file: std::fs::File,
}

impl Drop for CrossProcessLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// 阻塞获取跨进程文件锁。在 `tokio::task::spawn_blocking` 里调用以避免堵 event loop。
pub fn acquire_blocking() -> Result<CrossProcessLock> {
    let path = BIG_SIZE_LOCK_PATH.clone();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    file.lock_exclusive()?;
    Ok(CrossProcessLock { file })
}

/// 异步包装：spawn_blocking 拿锁，drop 时同样在 blocking 上释放（fs2::unlock 是 syscall，几乎零开销，drop 同步即可）。
pub async fn acquire_async() -> Result<CrossProcessLock> {
    tokio::task::spawn_blocking(acquire_blocking).await?
}
