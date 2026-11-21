use std::future::Future;
use std::io;

use tokio::runtime::{Builder, Handle, Runtime};

const BLOCKING_THREADS: usize = 4;

/// 窗口只有一条异步消息流。SQLite 和图片任务另外使用阻塞池。
pub(super) struct GuiExecutor {
    handle: Handle,
    runtime: Option<Runtime>,
}

impl iced::Executor for GuiExecutor {
    fn new() -> io::Result<Self> {
        let runtime = Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(BLOCKING_THREADS)
            .enable_all()
            .build()?;
        Ok(Self {
            handle: runtime.handle().clone(),
            runtime: Some(runtime),
        })
    }

    fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        self.handle.spawn(future);
    }

    fn block_on<T>(&self, future: impl Future<Output = T>) -> T {
        self.handle.block_on(future)
    }

    fn enter<R>(&self, f: impl FnOnce() -> R) -> R {
        let _entered = self.handle.enter();
        f()
    }
}

impl Drop for GuiExecutor {
    fn drop(&mut self) {
        // 窗口关闭时仍可能有 SQLite 忙等或缩略图解码，及时释放 GUI 实例锁。
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::Executor;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn subscriptions_run_while_all_blocking_workers_are_busy() {
        let executor = GuiExecutor::new().unwrap();
        let (ready, started) = mpsc::channel();
        let mut releases = Vec::new();
        for _ in 0..BLOCKING_THREADS {
            let ready = ready.clone();
            let (release, waiting) = mpsc::channel::<()>();
            releases.push(release);
            executor.handle.spawn_blocking(move || {
                ready.send(()).unwrap();
                let _ = waiting.recv();
            });
        }
        let timeout = Duration::from_secs(3);
        for _ in 0..BLOCKING_THREADS {
            started.recv_timeout(timeout).unwrap();
        }
        let (dispatched, received) = mpsc::channel();
        executor.spawn(async move { dispatched.send(()).unwrap() });
        received.recv_timeout(timeout).unwrap();
        drop(releases);
    }
}
