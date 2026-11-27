//! 原生窗口短时待机的唤醒通道。同一测试库或用户库由 GUI 锁文件的 inode 标识。

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{config::Config, single_instance::InstanceGuard};

fn socket_path(lock: &Path) -> io::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or(io::ErrorKind::NotFound)?;
    let dir = fs::metadata(&runtime)?;
    let lock = fs::metadata(lock)?;
    // XDG_RUNTIME_DIR 是用户私有目录。拒绝在共享目录中建立可唤醒窗口的端点。
    if !dir.is_dir() || dir.uid() != lock.uid() || dir.mode() & 0o077 != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(PathBuf::from(runtime).join(format!("niri-clip-{:x}-{:x}.sock", lock.dev(), lock.ino())))
}

/// 只有窗口事件循环已接受唤醒才返回 true，超时或旧进程正在退出时走正常启动。
pub fn show_existing() -> bool {
    fn activate() -> io::Result<bool> {
        let path = socket_path(&Config::state_dir().join("gui.lock"))?;
        let mut stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_millis(400)))?;
        let mut ack = [0];
        stream.read_exact(&mut ack)?;
        Ok(ack == [1])
    }
    activate().unwrap_or(false)
}

/// 调用方在持有 GUI 实例锁期间持有本守卫，退出时先清理地址再释放锁。
pub struct Endpoint(PathBuf);

impl Endpoint {
    pub fn bind(guard: &InstanceGuard) -> io::Result<(Self, UnixListener)> {
        let path = socket_path(guard.path())?;
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_socket() => fs::remove_file(&path)?,
            Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(&path)?;
        let endpoint = Self(path);
        fs::set_permissions(&endpoint.0, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok((endpoint, listener))
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
