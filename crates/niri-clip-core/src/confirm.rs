//! 删除确认状态机（任务 2.6 / ADR-005）：★ 条目删除的二次确认，**全项目唯一实现**。
//!
//! 收敛前存在三套实现且语义分叉（fzf 内嵌 15s TTL / GUI 内存 bool 无 TTL /
//! CLI fuzzel 弹窗），详见 ADR-005。本模块接管该语义，前端只负责"如何呈现挂起态"：
//!
//! - fzf TUI：`list-raw` 在挂起行的预览列尾部追加提示标记
//! - 原生 GUI：消息路径刷新的横幅（渲染路径不做 fs IO，见 ADR-005 代价节）
//! - CLI 非 `--fzf`：打印"15 秒内再执行一次"提示，退出码仍为成功
//!
//! 状态落盘 `state/pending_delete`（`<id> <毫秒时间戳>`），因为 CLI 的
//! "Ctrl-X → list-raw 重载 → 再 Ctrl-X"是**三个不同进程**，挂起态必须跨进程存活。

use anyhow::Result;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::store;

/// 挂起有效期：超时自动作废——防止用户分心后回来误触真删（ADR-005 决策 15s）
pub const PENDING_TTL_MS: u128 = 15_000;

/// 状态机判定结果
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// 已真删（非星标直删，或星标确认通过）
    Deleted,
    /// 仅挂起，等待同一 id 的再次请求（或超时作废）
    Pending,
}

fn pending_path() -> PathBuf {
    Config::state_dir().join("pending_delete")
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 读取挂起中的目标；过期 / 损坏一律视作"无挂起"并顺手清文件。
///
/// TTL 在**读取侧**判定而非靠定时器：CLI 是跨进程的一次性调用，没有定时器可用；
/// GUI 也不必为了一个 15s 截止多养一个 timer。代价是过期文件会留到下次读取
/// （或下次 `clear()`）才消失，不影响判定正确性。
pub fn pending_id() -> Option<i64> {
    let raw = std::fs::read_to_string(pending_path()).ok()?;
    let mut it = raw.split_whitespace();
    let id: i64 = it.next()?.parse().ok()?;
    let ts: u128 = it.next()?.parse().ok()?;
    if now_ms().saturating_sub(ts) > PENDING_TTL_MS {
        clear();
        return None;
    }
    Some(id)
}

/// 主动作废挂起（Esc / 选中移动 / 会话开始时的清理）
pub fn clear() {
    let _ = std::fs::remove_file(pending_path());
}

/// 挂起目标（写失败即报错——挂不上就不能让调用方以为"已进入确认流程"）
fn arm(id: i64) -> Result<()> {
    std::fs::create_dir_all(Config::state_dir())?;
    std::fs::write(pending_path(), format!("{id} {}", now_ms()))?;
    Ok(())
}

/// **纯判定**：只读/写挂起状态文件（无 sqlite 访问），返回"是否允许删除"。
///
/// 拆出这一层是为了让长驻 UI 能在**消息路径**安全调用（fs 小文件读写，
/// 不阻塞 UI，也不违反"sqlite 写锁最长 busy_timeout 5s 不能上 UI 线程"的纪律）：
/// 判定为 `Deleted` 后由调用方自行把 `store::delete` 丢到后台线程执行。
///
/// * 非星标 → 清残留挂起，返回 `Deleted`
/// * 星标 + 同一 id 的有效挂起（≤15s）→ 清挂起，返回 `Deleted`
/// * 星标 + 无挂起 / 挂起到期 / 指向别的 id → 挂起，返回 `Pending`
pub fn decide(id: i64, pinned: bool) -> Result<Decision> {
    if !pinned {
        // 非 ★ 行单击直删。顺手清掉残留挂起：否则它会在 TTL 内继续有效，
        // 让用户下一次对某 ★ 条目的单击意外构成"第二段确认"
        clear();
        return Ok(Decision::Deleted);
    }
    if pending_id() == Some(id) {
        clear();
        return Ok(Decision::Deleted);
    }
    arm(id)?;
    Ok(Decision::Pending)
}

/// CLI 便捷入口：判定 + 立即执行删除（CLI 是一次性进程，阻塞可接受）。
/// 长驻 UI 请用 `decide` 把删除丢到后台线程。
pub fn request(id: i64) -> Result<Decision> {
    let pinned = store::is_pinned(id)?;
    let decision = decide(id, pinned)?;
    if decision == Decision::Deleted {
        store::delete(id)?;
    }
    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::ENV_LOCK;

    /// XDG 隔离（共享全局锁，见 lib.rs test_util 注释），返回 (锁, 临时根, 原 XDG 值)
    fn isolated(tag: &str) -> (std::sync::MutexGuard<'static, ()>, PathBuf, Option<String>) {
        let guard = ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("niri-clip-confirm-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("state")).unwrap();
        let prev = std::env::var("XDG_STATE_HOME").ok();
        std::env::set_var("XDG_STATE_HOME", root.join("state"));
        (guard, root, prev)
    }

    fn restore(root: PathBuf, prev: Option<String>) {
        match prev {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    fn seed_pinned(text: &str) -> i64 {
        store::insert(text.to_string(), None).unwrap();
        let id = store::list(50)
            .unwrap()
            .into_iter()
            .find(|c| c.text == text)
            .expect("seeded")
            .id;
        store::toggle_pin(id).unwrap();
        id
    }

    /// 非星标：一次请求即真删，且清掉残留挂起
    #[test]
    fn non_pinned_deletes_immediately_and_clears_pending() {
        let (_g, root, prev) = isolated("nonpinned");
        store::insert("confirm-plain".into(), None).unwrap();
        let id = store::list(50)
            .unwrap()
            .into_iter()
            .find(|c| c.text == "confirm-plain")
            .unwrap()
            .id;
        // 预置一个残留挂起：直删后必须被清掉
        std::fs::write(pending_path(), format!("{} {}", id, now_ms())).unwrap();
        assert_eq!(request(id).unwrap(), Decision::Deleted);
        assert!(store::get(id).is_err());
        assert_eq!(pending_id(), None, "直删路径必须清掉残留挂起");
        restore(root, prev);
    }

    /// 星标：首请求挂起（不真删）→ 同 id 再请求真删
    #[test]
    fn pinned_needs_two_requests() {
        let (_g, root, prev) = isolated("pinned");
        let id = seed_pinned("confirm-star");
        assert_eq!(request(id).unwrap(), Decision::Pending);
        assert!(store::get(id).is_ok(), "挂起阶段不得真删");
        assert_eq!(pending_id(), Some(id));
        assert_eq!(request(id).unwrap(), Decision::Deleted);
        assert!(store::get(id).is_err());
        assert_eq!(pending_id(), None);
        restore(root, prev);
    }

    /// 过期挂起不作数：重新挂起而不是误删（ADR-005 的核心安全属性）
    #[test]
    fn expired_pending_rearms_instead_of_deleting() {
        let (_g, root, prev) = isolated("expired");
        let id = seed_pinned("confirm-expired");
        std::fs::write(pending_path(), format!("{id} 0")).unwrap(); // 时间戳 0 = 必然过期
        assert_eq!(request(id).unwrap(), Decision::Pending);
        assert!(store::get(id).is_ok(), "过期挂起不得触发真删");
        restore(root, prev);
    }

    /// 损坏的状态文件视作无挂起（不 panic、不误删）
    #[test]
    fn corrupted_pending_file_is_ignored() {
        let (_g, root, prev) = isolated("corrupt");
        let id = seed_pinned("confirm-corrupt");
        std::fs::write(pending_path(), "not-a-number\n").unwrap();
        assert_eq!(pending_id(), None);
        assert_eq!(request(id).unwrap(), Decision::Pending);
        assert!(store::get(id).is_ok());
        restore(root, prev);
    }

    /// 挂起目标可转移：对另一个 ★ 条目发起确认会覆盖挂起目标
    #[test]
    fn pending_target_moves_to_the_latest_pinned() {
        let (_g, root, prev) = isolated("move");
        let a = seed_pinned("confirm-a");
        let b = seed_pinned("confirm-b");
        assert_eq!(request(a).unwrap(), Decision::Pending);
        assert_eq!(pending_id(), Some(a));
        // 转到 b：a 未被删，挂起目标变成 b
        assert_eq!(request(b).unwrap(), Decision::Pending);
        assert_eq!(pending_id(), Some(b));
        assert!(store::get(a).is_ok(), "转移挂起不得删除原目标");
        assert!(store::get(b).is_ok());
        restore(root, prev);
    }

    /// `decide` 只判定、不执行删除：判定通过时行仍在，由调用方自行删除。
    /// GUI 依赖这一分层把 sqlite 写入留在后台线程（UI 线程只做 fs 小文件读写）
    #[test]
    fn decide_only_judges_and_never_deletes() {
        let (_g, root, prev) = isolated("decide");
        let id = seed_pinned("confirm-decide");
        assert_eq!(decide(id, true).unwrap(), Decision::Pending);
        assert!(store::get(id).is_ok());
        assert_eq!(decide(id, true).unwrap(), Decision::Deleted);
        assert!(
            store::get(id).is_ok(),
            "decide 不得自行删除——删除由调用方执行"
        );
        assert_eq!(pending_id(), None, "判定通过后挂起必须已清除");
        restore(root, prev);
    }
}
