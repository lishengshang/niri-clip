//! 一个打开的选择器，或一段没有窗口的等待时间。每次打开创建独立的 App。
use std::io::{self, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

use iced::futures::{stream, SinkExt};
use iced::{window, Element, Subscription, Task};
use niri_clip_core::config::SelectorConfig;

use super::{executor::GuiExecutor, instance, window_settings, App, Message};

#[derive(Debug)]
enum Event {
    Ui(window::Id, Message),
    Show(UnixStream),
    Closed(window::Id),
    Expired(window::Id),
    ListenFailed(io::Error),
}

struct Standby {
    window: Option<(window::Id, App)>,
    last_closed: Option<window::Id>,
    timeout: Duration,
    reopen: bool,
    listening: bool,
    exiting: bool,
}

pub(super) fn run(cfg: SelectorConfig, listener: UnixListener) -> iced::Result {
    iced::daemon(
        move || {
            let mut state = Standby {
                window: None,
                last_closed: None,
                timeout: Duration::ZERO,
                reopen: false,
                listening: true,
                exiting: false,
            };
            let open = state.open(cfg.clone());
            (state, Task::batch([open, listen(listener.try_clone())]))
        },
        Standby::update,
        Standby::view,
    )
    .executor::<GuiExecutor>()
    .title("niri-clip")
    .theme(iced::Theme::Dark)
    .default_font(super::UI_FONT)
    .subscription(Standby::subscription)
    .run()
}

fn listen(listener: io::Result<UnixListener>) -> Task<Event> {
    let incoming = iced::stream::channel(1, async move |mut sender| {
        let result: io::Result<()> = async {
            let listener = tokio::net::UnixListener::from_std(listener?)?;
            loop {
                let (peer, _) = listener.accept().await?;
                if sender.send(Event::Show(peer.into_std()?)).await.is_err() {
                    return Ok(());
                }
            }
        }
        .await;
        if let Err(e) = result {
            let _ = sender.send(Event::ListenFailed(e)).await;
        }
    });
    Task::run(incoming, std::convert::identity)
}

impl Standby {
    fn open(&mut self, cfg: SelectorConfig) -> Task<Event> {
        self.last_closed = None;
        self.reopen = false;
        self.timeout = Duration::from_secs(cfg.gui_idle_timeout_secs);
        let (id, open) = window::open(window_settings());
        let (mut app, initialize) = App::boot(cfg);
        app.warm_window = Some(id);
        self.window = Some((id, app));
        Task::batch([open.discard(), initialize.map(move |m| Event::Ui(id, m))])
    }

    fn exit(&mut self) -> Task<Event> {
        self.exiting = true;
        iced::exit()
    }

    fn update(&mut self, event: Event) -> Task<Event> {
        match event {
            Event::Ui(id, message) => {
                // 旧窗口的搜索、图片或复制结果可能在新窗口打开后才到达。
                if let Some((current, app)) = self.window.as_mut() {
                    if *current == id && !app.closing {
                        return app.update(message).map(move |m| Event::Ui(id, m));
                    }
                }
            }
            Event::Show(mut peer) if !self.exiting => {
                let task = match &self.window {
                    Some((_, app)) if app.closing => {
                        self.reopen = true;
                        Task::none()
                    }
                    Some((id, _)) => {
                        if instance::focus_existing_window(Some(std::process::id())) {
                            Task::none()
                        } else {
                            window::gain_focus(*id)
                        }
                    }
                    None => self.open(SelectorConfig::load()),
                };
                // 主循环接受后确认。若超时退出已开始，丢弃连接让启动方重试。
                let _ = peer.write_all(&[1]);
                return task;
            }
            Event::Closed(id) if self.window.as_ref().is_some_and(|(w, _)| *w == id) => {
                self.window = None;
                if self.reopen {
                    return self.open(SelectorConfig::load());
                }
                if self.timeout.is_zero() || !self.listening {
                    return self.exit();
                }
                self.last_closed = Some(id);
            }
            Event::Expired(id) if self.last_closed == Some(id) => return self.exit(),
            Event::ListenFailed(e) => {
                eprintln!("[niri-clip gui] standby listener: {e}");
                self.listening = false;
                if self.window.is_none() {
                    return self.exit();
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn view(&self, id: window::Id) -> Element<'_, Event> {
        if let Some((current, app)) = &self.window {
            if *current == id {
                return app.view().map(move |m| Event::Ui(id, m));
            }
        }
        iced::widget::Space::new().into()
    }

    fn subscription(&self) -> Subscription<Event> {
        let events = if self.window.is_some() {
            iced::event::listen_with(|event, status, id| {
                App::input_event(event, status).map(|m| Event::Ui(id, m))
            })
        } else if let Some(id) = self.last_closed {
            // 重开窗口自动取消旧定时器。标记窗口 ID 也保护已入队的到期消息。
            Subscription::run_with((id, self.timeout), |&(id, timeout)| {
                stream::once(async move {
                    tokio::time::sleep(timeout).await;
                    Event::Expired(id)
                })
            })
        } else {
            Subscription::none()
        };
        Subscription::batch([events, window::close_events().map(Event::Closed)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_window_results_and_expiry_cannot_change_a_reopened_window() {
        let old = window::Id::unique();
        let new = window::Id::unique();
        let app = App::with_history(niri_clip_core::config::Config::default().into(), Vec::new());
        let mut state = Standby {
            window: Some((new, app)),
            last_closed: None,
            timeout: Duration::from_secs(30),
            reopen: false,
            listening: true,
            exiting: false,
        };
        let _ = state.update(Event::Ui(old, Message::Query("old".into())));
        let _ = state.update(Event::Closed(old));
        let _ = state.update(Event::Expired(old));
        assert!(state.window.as_ref().unwrap().1.query.is_empty());
        assert!(!state.exiting);
        let _ = state.update(Event::Closed(new));
        assert!(state.window.is_none());
        let _ = state.update(Event::Expired(old));
        assert!(!state.exiting);
        let _ = state.update(Event::Expired(new));
        assert!(state.exiting);

        // 超时退出已开始。启动者应看到 EOF 并启动新进程，否则本次热键会丢失。
        let (mut client, server) = UnixStream::pair().unwrap();
        let _ = state.update(Event::Show(server));
        let mut ack = [0];
        assert_eq!(std::io::Read::read(&mut client, &mut ack).unwrap(), 0);
    }
}
