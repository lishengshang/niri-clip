//! 单实例互斥（flock）——全项目**唯一**的实例锁机制（任务 2.6 / C4）。
//!
//! 收敛前有两套实现：core `daemon.rs` 用 flock（`state/daemon.lock`），原生 UI
//! `instance.rs` 自己用 PID 文件（`state/gui.lock`）+ `/proc/{pid}/cmdline` 复核。
//! 两者要解决的问题相同（"已有实例在跑"），差异只在**被占用时做什么**：
//! daemon 报错退出，GUI 聚焦已开窗口后退出。故 core 只回答"能否取得锁"，
//! 后续动作由调用方决定。
//!
//! flock 优于 PID 文件：进程崩溃时由内核自动释放锁，**不存在陈锁残留**，
//! 也不需要解析 `/proc` 或防御 PID 回收造成的假阳性。

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::config::Config;

/// 锁守卫。单实例守卫贯穿进程生命周期，短时任务持有到操作结束。
pub struct InstanceGuard {
    _file: std::fs::File,
    path: PathBuf,
}

impl InstanceGuard {
    /// 尝试取得名为 `name` 的单实例锁（如 `"daemon"` / `"gui"`）。
    ///
    /// 返回 `Ok(None)` 表示已被其它实例占用——**这不是错误**，调用方按各自
    /// 语义处理（daemon：提示并退出；GUI：聚焦已开窗口后退出）。
    pub fn try_acquire(name: &str) -> Result<Option<Self>> {
        Self::lock(name, true)
    }

    /// 等待短时操作释放锁，例如 schema 迁移或更新当前项。
    pub(crate) fn acquire(name: &str) -> Result<Self> {
        Self::lock(name, false)?.context("blocking flock did not acquire the lock")
    }

    fn lock(name: &str, nonblocking: bool) -> Result<Option<Self>> {
        let dir = Config::state_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{name}.lock"));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("open lock {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let operation = if nonblocking {
                libc::LOCK_EX | libc::LOCK_NB
            } else {
                libc::LOCK_EX
            };
            // SAFETY: flock is called with an owned, live file descriptor.
            while unsafe { libc::flock(file.as_raw_fd(), operation) } != 0 {
                let error = std::io::Error::last_os_error();
                match error.kind() {
                    std::io::ErrorKind::Interrupted => continue,
                    std::io::ErrorKind::WouldBlock if nonblocking => return Ok(None),
                    _ => return Err(error).with_context(|| format!("lock {}", path.display())),
                }
            }
        }
        Ok(Some(Self { _file: file, path }))
    }

    /// 锁文件路径（供日志与诊断输出）
    pub fn path(&self) -> &Path {
        &self.path
    }
}
