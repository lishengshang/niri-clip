//! niri 管理 xdg 窗口的位置。只在上次所在的输出恢复本进程的浮窗。

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use niri_clip_core::config::Config;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const IPC_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct SavedPosition {
    output: String,
    xy: [f64; 2],
}

struct FloatingWindow {
    id: u64,
    workspace: u64,
    xy: [f64; 2],
}

impl FloatingWindow {
    fn ours(window: &Value) -> Option<Self> {
        if window["app_id"] != "niri-clip-gui"
            || window["pid"] != std::process::id()
            || window["is_floating"] != true
        {
            return None;
        }
        let pos = &window["layout"]["tile_pos_in_workspace_view"];
        Some(Self {
            id: window["id"].as_u64()?,
            workspace: window["workspace_id"].as_u64()?,
            xy: [pos[0].as_f64()?, pos[1].as_f64()?],
        })
    }

    fn position(&self, socket: &Path) -> Result<SavedPosition> {
        let (_, reply) = request(socket, json!("Workspaces"))?;
        let output = reply["Workspaces"].as_array().and_then(|ws| {
            ws.iter()
                .find(|w| w["id"] == self.workspace)?
                .get("output")?
                .as_str()
        });
        Ok(SavedPosition {
            output: output
                .ok_or_else(|| anyhow!("workspace has no output"))?
                .into(),
            xy: self.xy,
        })
    }
}

fn read_message(stream: &mut BufReader<UnixStream>) -> Result<Value> {
    let mut line = Vec::new();
    stream.read_until(b'\n', &mut line)?;
    Ok(serde_json::from_slice(&line)?)
}

fn request(socket: &Path, message: Value) -> Result<(BufReader<UnixStream>, Value)> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(IPC_TIMEOUT))?;
    stream.set_write_timeout(Some(IPC_TIMEOUT))?;
    writeln!(stream, "{message}")?;
    let mut stream = BufReader::new(stream);
    let mut reply = read_message(&mut stream)?;
    let ok = reply
        .get_mut("Ok")
        .ok_or_else(|| anyhow!("niri rejected IPC request"))?;
    Ok((stream, ok.take()))
}

fn restore(socket: &Path, saved: &SavedPosition) -> Result<()> {
    // Opened 可以先于合成器登记窗口。事件流自带初始快照，两种顺序都能收到。
    let (mut stream, _) = request(socket, json!("EventStream"))?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        stream.get_ref().set_read_timeout(Some(remaining))?;
        let event = read_message(&mut stream)?;
        let window =
            FloatingWindow::ours(&event["WindowOpenedOrChanged"]["window"]).or_else(|| {
                event["WindowsChanged"]["windows"]
                    .as_array()?
                    .iter()
                    .find_map(FloatingWindow::ours)
            });
        if let Some(window) = window {
            let current = window.position(socket)?;
            if current.output == saved.output && current.xy != saved.xy {
                // IPC 返回工作区视图坐标，而绝对移动以工作区可用区域为原点。
                // 两点作差后相对移动，面板、留白和小数缩放不会叠到坐标上。
                request(
                    socket,
                    json!({"Action":{"MoveFloatingWindow":{
                        "id":window.id, "x":{"AdjustFixed":saved.xy[0]-current.xy[0]},
                        "y":{"AdjustFixed":saved.xy[1]-current.xy[1]}
                    }}}),
                )?;
            }
            break;
        }
    }
    Ok(())
}

pub(super) struct PositionMemory {
    socket: PathBuf,
    path: PathBuf,
    saved: Option<SavedPosition>,
    restore: Option<JoinHandle<()>>,
}

impl PositionMemory {
    pub fn open(enabled: bool) -> Option<Self> {
        if !enabled {
            return None;
        }
        let socket = PathBuf::from(std::env::var_os("NIRI_SOCKET")?);
        let path = Config::state_dir().join("window-position.json");
        let saved: Option<SavedPosition> = fs::read(&path)
            .ok()
            .and_then(|s| serde_json::from_slice(&s).ok());
        let restore = saved.clone().and_then(|pos| {
            let socket = socket.clone();
            std::thread::Builder::new()
                .name("window-position".into())
                .spawn(move || {
                    let _ = restore(&socket, &pos);
                })
                .ok()
        });
        Some(Self {
            socket,
            path,
            saved,
            restore,
        })
    }

    pub fn save(&self) -> Result<()> {
        // 立即关闭时保留原记录，下次仍可恢复。
        if self.restore.as_ref().is_some_and(|t| !t.is_finished()) {
            return Ok(());
        }
        let (_, reply) = request(&self.socket, json!("Windows"))?;
        let Some(window) = reply["Windows"]
            .as_array()
            .and_then(|w| w.iter().find_map(FloatingWindow::ours))
        else {
            return Ok(());
        };
        let pos = window.position(&self.socket)?;
        if self.saved.as_ref() != Some(&pos) {
            let tmp = self
                .path
                .with_extension(format!("{}.tmp", std::process::id()));
            let file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            serde_json::to_writer(file, &pos)?;
            fs::rename(tmp, &self.path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_belongs_to_our_floating_window() {
        let mut w = json!({"id":5,"workspace_id":2,"app_id":"niri-clip-gui","pid":std::process::id(),
                         "is_floating":true,"layout":{"tile_pos_in_workspace_view":[9.6,-4]}});
        assert_eq!(FloatingWindow::ours(&w).unwrap().xy, [9.6, -4.]);
        w["is_floating"] = json!(false);
        assert!(FloatingWindow::ours(&w).is_none());
        w["is_floating"] = json!(true);
        w["pid"] = Value::Null;
        assert!(FloatingWindow::ours(&w).is_none());
    }
}
