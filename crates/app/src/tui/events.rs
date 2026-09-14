//! Ввод и таймер тика — оба идут в один канал, чтобы главный цикл не зависал.

use std::time::Duration;

use crossterm::event::{self, Event as CEvent, KeyEvent, KeyEventKind, MouseEvent, MouseEventKind};
use tokio::sync::mpsc;

use super::state::BgResponse;

/// Событие для главного цикла TUI.
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// Окно получило фокус (клик по неактивному окну не должен ничего нажимать).
    FocusGained,
    /// Окно потеряло фокус (окно Omarchy закрывается само, как панели).
    FocusLost,
    Resize,
    Tick,
    Bg(Box<BgResponse>),
}

/// Читает клавиатуру (отдельный поток, `crossterm::event::read` блокирующий) и раз в `tick_rate`
/// шлёт `Tick` — для анимации спиннера и авто-скрытия пароля. Фоновые задачи шлют `Bg(...)` в тот
/// же канал через клон отправителя (`sender()`), поэтому интерфейс никогда не ждёт их синхронно.
pub struct EventHandler {
    rx: mpsc::UnboundedReceiver<Event>,
    tx: mpsc::UnboundedSender<Event>,
}

impl EventHandler {
    pub fn new(tick_rate: Duration) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        let input_tx = tx.clone();
        std::thread::Builder::new()
            .name("omarchy-hotspot-tui-input".into())
            .spawn(move || {
                loop {
                    match event::read() {
                        Ok(CEvent::Key(k)) if k.kind == KeyEventKind::Press => {
                            if input_tx.send(Event::Key(k)).is_err() {
                                break;
                            }
                        }
                        // Движение без кнопок не нужно: его очень много.
                        Ok(CEvent::Mouse(m))
                            if !matches!(
                                m.kind,
                                MouseEventKind::Moved | MouseEventKind::Drag(_)
                            ) =>
                        {
                            if input_tx.send(Event::Mouse(m)).is_err() {
                                break;
                            }
                        }
                        Ok(CEvent::FocusGained) => {
                            if input_tx.send(Event::FocusGained).is_err() {
                                break;
                            }
                        }
                        Ok(CEvent::FocusLost) => {
                            if input_tx.send(Event::FocusLost).is_err() {
                                break;
                            }
                        }
                        Ok(CEvent::Resize(_, _)) => {
                            if input_tx.send(Event::Resize).is_err() {
                                break;
                            }
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            })
            .expect("cannot start input thread");

        let tick_tx = tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tick_rate);
            loop {
                interval.tick().await;
                if tick_tx.send(Event::Tick).is_err() {
                    break;
                }
            }
        });

        EventHandler { rx, tx }
    }

    pub async fn next(&mut self) -> Option<Event> {
        self.rx.recv().await
    }

    /// Отправитель для фоновых задач (см. `state::spawn_bg`).
    pub fn sender(&self) -> mpsc::UnboundedSender<Event> {
        self.tx.clone()
    }
}
