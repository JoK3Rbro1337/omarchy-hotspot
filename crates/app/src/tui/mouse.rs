//! Мышь — в дополнение к клавишам. При отрисовке кликабельные места записывают свои
//! прямоугольники в `Hits`; клик или колесо ищут, во что попали, и вызывают те же
//! действия, что и клавиши (поэтому проверки «занято», «открыт вопрос» — те же).

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use tokio::sync::mpsc::UnboundedSender;

use super::events::Event;
use super::settings::{ExpertOutput, Focus, Section};
use super::state::{AppState, Mode};

/// Сколько строк журнала прокручивает один щелчок колеса.
const LOG_WHEEL_STEP: usize = 3;
/// Клик так скоро после получения фокуса — это клик, которым окно активировали: его не нажимаем.
/// Терминал сообщает о фокусе прямо перед таким кликом; обычный клик по уже активному окну
/// (или после наведения при «фокус следует за мышью») приходит заметно позже.
const FOCUS_CLICK: std::time::Duration = std::time::Duration::from_millis(150);

/// Во что можно кликнуть.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// `[ ВКЛ / ВЫКЛ ]` в рамке «Статус».
    Toggle,
    /// Вкладка режима в верхней рамке.
    Tab(Mode),
    /// Кнопка `[ QR ]` или сам QR: показать или спрятать.
    Qr,
    /// Раздел продвинутого режима (индекс в `Section::ALL`).
    Section(usize),
    /// Параметр раздела (индекс в `Section::items()`).
    Item(usize),
    /// Строка в разделе «Устройства» (индекс в `device_rows()`).
    Device(usize),
}

/// Что прокручивает колесо.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// Список устройств простого режима.
    SimpleDevices,
    Sections,
    Items,
    Devices,
    /// Журнал или проверка системы в «Эксперте».
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spot {
    Click(Target),
    Wheel(Scroll),
}

/// Кликабельные места последнего кадра. Позже записанное лежит «сверху».
#[derive(Debug, Default)]
pub struct Hits(Vec<(Rect, Spot)>);

impl Hits {
    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn click(&mut self, area: Rect, target: Target) {
        if !area.is_empty() {
            self.0.push((area, Spot::Click(target)));
        }
    }

    pub fn wheel(&mut self, area: Rect, scroll: Scroll) {
        if !area.is_empty() {
            self.0.push((area, Spot::Wheel(scroll)));
        }
    }

    /// Отметить надпись `label` в строке `y` (ищется в уже нарисованном буфере):
    /// так клик попадает ровно в текст, как бы ни раскладывались заголовки рамок.
    pub fn click_label(&mut self, buf: &Buffer, y: u16, span: Rect, label: &str, target: Target) {
        if let Some(area) = find_label(buf, y, span, label) {
            self.click(area, target);
        }
    }

    pub fn target_at(&self, x: u16, y: u16) -> Option<Target> {
        self.0.iter().rev().find_map(|(r, s)| match s {
            Spot::Click(t) if r.contains(Position { x, y }) => Some(*t),
            _ => None,
        })
    }

    pub fn scroll_at(&self, x: u16, y: u16) -> Option<Scroll> {
        self.0.iter().rev().find_map(|(r, s)| match s {
            Spot::Wheel(w) if r.contains(Position { x, y }) => Some(*w),
            _ => None,
        })
    }
}

/// Прямоугольник надписи в строке `y` в пределах `span` (по символам ячеек буфера).
pub fn find_label(buf: &Buffer, y: u16, span: Rect, label: &str) -> Option<Rect> {
    let want: Vec<char> = label.chars().collect();
    let len = u16::try_from(want.len()).ok()?;
    if len == 0 || y < span.top() || y >= span.bottom() {
        return None;
    }
    let right = span.right().min(buf.area.right());
    (span.left()..right.saturating_sub(len - 1)).find_map(|x| {
        let hit = want.iter().enumerate().all(|(i, c)| {
            buf.cell((x + i as u16, y))
                .is_some_and(|cell| cell.symbol().chars().eq(std::iter::once(*c)))
        });
        hit.then_some(Rect::new(x, y, len, 1))
    })
}

/// Событие мыши. Клавиатурные окна (вопрос, ввод текста, изменение сети) мышь не трогает.
pub fn handle(app: &mut AppState, ev: MouseEvent, hits: &Hits, tx: &UnboundedSender<Event>) {
    let down = match ev.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let activating = app
                .focus_gained_at
                .take()
                .is_some_and(|t| t.elapsed() < FOCUS_CLICK);
            if !activating {
                click(app, hits.target_at(ev.column, ev.row), tx);
            }
            return;
        }
        MouseEventKind::ScrollDown => true,
        MouseEventKind::ScrollUp => false,
        _ => return,
    };
    if modal_open(app) {
        return;
    }
    if let Some(scroll) = hits.scroll_at(ev.column, ev.row) {
        wheel(app, scroll, down);
    }
}

fn modal_open(app: &AppState) -> bool {
    app.confirm.is_some() || app.editing.is_some() || app.adv.input.is_some()
}

fn click(app: &mut AppState, target: Option<Target>, tx: &UnboundedSender<Event>) {
    if modal_open(app) {
        return;
    }
    let Some(target) = target else { return };
    match target {
        Target::Toggle => app.request_toggle(tx),
        Target::Tab(mode) if mode != app.mode => app.toggle_mode(tx),
        Target::Tab(_) => {}
        Target::Qr => app.toggle_qr(tx),
        Target::Section(i) => {
            if let Some(section) = Section::ALL.get(i).copied() {
                if section != app.adv.section {
                    app.adv.section = section;
                    app.adv.item_idx = 0;
                    app.adv.output = ExpertOutput::None;
                }
                app.adv.focus = Focus::Sections;
            }
        }
        Target::Item(i) => {
            // Только выделение: значение меняют клавиши, чтобы случайный клик ничего не переключил.
            if i < app.adv.section.items().len() {
                app.adv.focus = Focus::Items;
                app.adv.item_idx = i;
            }
        }
        Target::Device(i) => {
            if app.adv.section == Section::Devices && i < app.device_rows().len() {
                app.adv.focus = Focus::Items;
                app.adv.device_idx = i;
            }
        }
    }
}

/// Колесо двигает выделение на шаг, без перескока с конца списка на начало.
fn wheel(app: &mut AppState, scroll: Scroll, down: bool) {
    let step = |cur: usize, n: usize| {
        if down {
            (cur + 1).min(n.saturating_sub(1))
        } else {
            cur.saturating_sub(1)
        }
    };
    match scroll {
        Scroll::SimpleDevices => app.scroll_simple_devices(down),
        Scroll::Sections => {
            let n = Section::ALL.len();
            let cur = Section::ALL
                .iter()
                .position(|s| *s == app.adv.section)
                .unwrap_or(0);
            let next = Section::ALL[step(cur, n)];
            if next != app.adv.section {
                app.adv.section = next;
                app.adv.item_idx = 0;
                app.adv.output = ExpertOutput::None;
            }
            app.adv.focus = Focus::Sections;
        }
        Scroll::Items => {
            let n = app.adv.section.items().len();
            if n > 0 {
                if app.adv.focus == Focus::Items {
                    app.adv.item_idx = step(app.adv.item_idx, n);
                }
                app.adv.focus = Focus::Items;
            }
        }
        Scroll::Devices => {
            let n = app.device_rows().len();
            if n > 0 {
                let cur = app.adv.device_idx.min(n - 1);
                app.adv.device_idx = if app.adv.focus == Focus::Items {
                    step(cur, n)
                } else {
                    cur
                };
                app.adv.focus = Focus::Items;
            }
        }
        Scroll::Output => {
            // Журнал показан с конца: колесо вверх уводит в прошлое.
            let max = match &app.adv.output {
                ExpertOutput::Log(lines) => lines.len(),
                _ => 0,
            };
            app.adv.log_scroll = if down {
                app.adv.log_scroll.saturating_sub(LOG_WHEEL_STEP)
            } else {
                (app.adv.log_scroll + LOG_WHEEL_STEP).min(max)
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::SystemTime;

    use crossterm::event::KeyModifiers;
    use omarchy_hotspot_core::Lang;
    use omarchy_hotspot_core::backend::HotspotState;
    use omarchy_hotspot_core::config::Config;
    use omarchy_hotspot_core::devices::{Device, DeviceStatus};
    use omarchy_hotspot_core::helper_proto::Mac;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::super::state::{Confirm, StatusView};
    use super::super::ui;
    use super::*;

    fn app_on() -> AppState {
        let mut a = AppState::new(
            Lang::Ru,
            Config::default(),
            PathBuf::from("/tmp/oh-mouse-test.toml"),
        );
        a.status = StatusView {
            state: HotspotState::On {
                since: Some(SystemTime::now()),
                ap_iface: "wlp15s0".into(),
                uplink: Some("enp14s0".into()),
            },
            channel: Some(36),
        };
        a
    }

    fn device(i: u8) -> Device {
        Device {
            mac: Mac::parse(&format!("3c:2e:f5:11:22:{i:02x}")).unwrap(),
            ip: Some(std::net::Ipv4Addr::new(10, 42, 0, 10 + i)),
            hostname: Some(format!("Dev{i:02}")),
            signal_dbm: Some(-50),
            rx_bytes: 0,
            tx_bytes: 0,
            connected_secs: 1,
            status: DeviceStatus::Allowed,
        }
    }

    /// Нарисовать кадр (по умолчанию — в нужном окну размере) и вернуть буфер с картой кликов.
    fn frame(app: &AppState, size: Option<(u16, u16)>) -> (Buffer, Hits) {
        let (w, h) = size.unwrap_or_else(|| ui::desired_size(app));
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut hits = Hits::default();
        terminal.draw(|f| ui::draw(f, app, &mut hits)).unwrap();
        (terminal.backend().buffer().clone(), hits)
    }

    /// Координаты первой надписи `label` на экране.
    fn spot(buf: &Buffer, label: &str) -> (u16, u16) {
        (0..buf.area.height)
            .find_map(|y| find_label(buf, y, buf.area, label).map(|r| (r.x, r.y)))
            .unwrap_or_else(|| panic!("нет «{label}» на экране"))
    }

    fn ev(kind: MouseEventKind, (column, row): (u16, u16)) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn click_on(app: &mut AppState, label: &str) {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (buf, hits) = frame(app, None);
        let at = spot(&buf, label);
        handle(
            app,
            ev(MouseEventKind::Down(MouseButton::Left), at),
            &hits,
            &tx,
        );
    }

    fn wheel_on(app: &mut AppState, label: &str, down: bool) {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (buf, hits) = frame(app, None);
        let kind = if down {
            MouseEventKind::ScrollDown
        } else {
            MouseEventKind::ScrollUp
        };
        handle(app, ev(kind, spot(&buf, label)), &hits, &tx);
    }

    #[test]
    fn click_that_activates_the_window_presses_nothing() {
        let mut a = AppState::new(
            Lang::Ru,
            Config::default(),
            PathBuf::from("/tmp/oh-mouse-test.toml"),
        );
        a.wifi_client = Some("Home".into());
        a.focus_gained_at = Some(std::time::Instant::now());
        click_on(&mut a, "[ ВЫКЛ ]");
        assert!(a.confirm.is_none(), "клик по неактивному окну нажал кнопку");
        // Следующий клик — уже обычный.
        click_on(&mut a, "[ ВЫКЛ ]");
        assert!(a.confirm.is_some());
        // Фокус получен давно (навели мышь, потом кликнули) — клик работает сразу.
        let mut b = AppState::new(
            Lang::Ru,
            Config::default(),
            PathBuf::from("/tmp/oh-mouse-test.toml"),
        );
        b.wifi_client = Some("Home".into());
        b.focus_gained_at = Some(std::time::Instant::now() - FOCUS_CLICK * 3);
        click_on(&mut b, "[ ВЫКЛ ]");
        assert!(b.confirm.is_some());
    }

    #[test]
    fn toggle_button_does_what_space_does() {
        let mut a = AppState::new(
            Lang::Ru,
            Config::default(),
            PathBuf::from("/tmp/oh-mouse-test.toml"),
        );
        a.wifi_client = Some("Home".into());
        click_on(&mut a, "[ ВЫКЛ ]");
        assert_eq!(a.confirm, Some(Confirm::StartOverClient("Home".into())));
        // Пока открыт вопрос, клики мимо него ничего не делают.
        click_on(&mut a, "Простой");
        assert!(a.confirm.is_some());
    }

    #[test]
    fn tabs_sections_and_items_are_clickable() {
        let mut a = app_on();
        a.mode = Mode::Advanced;
        click_on(&mut a, "Сеть");
        assert_eq!(
            (a.adv.section, a.adv.focus),
            (Section::Network, Focus::Sections)
        );
        click_on(&mut a, "Подсеть");
        assert_eq!(a.adv.focus, Focus::Items);
        let subnet = Section::Network
            .items()
            .iter()
            .position(|i| *i == super::super::settings::Item::Subnet)
            .unwrap();
        assert_eq!(a.adv.item_idx, subnet);
        // Клик только выделяет: черновик не изменился.
        assert_eq!(a.adv.draft.network.subnet, a.cfg.network.subnet);
        click_on(&mut a, "Простой");
        assert_eq!(a.mode, Mode::Simple);
    }

    #[test]
    fn wheel_moves_selection_without_wrapping() {
        let mut a = app_on();
        a.mode = Mode::Advanced;
        a.adv.focus = Focus::Items;
        let n = a.adv.section.items().len();
        for _ in 0..n + 3 {
            wheel_on(&mut a, "Диапазон", true);
        }
        assert_eq!(a.adv.item_idx, n - 1, "колесо перескочило в начало");
        wheel_on(&mut a, "Безопасность", true);
        assert_eq!(a.adv.section, Section::Security);
    }

    #[test]
    fn device_rows_are_clickable() {
        let mut a = app_on();
        a.mode = Mode::Advanced;
        a.adv.section = Section::Devices;
        a.devices = vec![device(1), device(2)];
        click_on(&mut a, "Dev02");
        assert_eq!((a.adv.focus, a.adv.device_idx), (Focus::Items, 1));
    }

    #[test]
    fn simple_device_list_scrolls_with_the_wheel() {
        let mut a = app_on();
        a.devices = (0..20).map(device).collect();
        wheel_on(&mut a, "Dev00", true);
        assert_eq!(a.simple_dev_scroll, 1);
        let (buf, _) = frame(&a, None);
        assert!(find_label(&buf, 0, buf.area, "Dev00").is_none());
        let text: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect();
        assert!(!text.contains("Dev00") && text.contains("Dev01"), "{text}");
    }

    #[test]
    fn qr_button_toggles_and_a_click_on_the_code_hides_it() {
        let mut a = app_on();
        a.qr = Some((0..16).map(|_| "#".repeat(31)).collect());
        click_on(&mut a, "[ QR ]");
        assert!(a.show_qr);
        click_on(&mut a, "[ QR ]");
        assert!(!a.show_qr, "второе нажатие не спрятало QR");
        a.show_qr = true;
        click_on(&mut a, "наведи камеру");
        assert!(!a.show_qr, "клик по самому QR не спрятал его");
        // Окно слишком мало для QR рядом с блоками — QR поверх окна, клик в любом месте прячет.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        a.show_qr = true;
        let (_, hits) = frame(&a, Some((50, 20)));
        handle(
            &mut a,
            ev(MouseEventKind::Down(MouseButton::Left), (1, 1)),
            &hits,
            &tx,
        );
        assert!(!a.show_qr);
    }
}
