//! Состояние окна: что показываем и что делаем по нажатиям клавиш.
//! Долгие операции (nmcli, pkexec) уходят в `tokio::task::spawn_blocking`, результат
//! возвращается через канал `Event::Bg`, чтобы отрисовка не зависала ни на миг.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use omarchy_hotspot_core::backend::HotspotState;
use omarchy_hotspot_core::config::{self, Config, UiMode};
use omarchy_hotspot_core::devices::{self, Device, DeviceStatus};
use omarchy_hotspot_core::doctor::Check;
use omarchy_hotspot_core::helper::{HelperRunner, PkexecHelper};
use omarchy_hotspot_core::helper_proto::Mac;
use omarchy_hotspot_core::hotspot::{ApplyOutcome, StartOutcome};
use omarchy_hotspot_core::ipc::{self, Event as DaemonEvent, Request, Response, ServerMsg};
use omarchy_hotspot_core::net::iw;
use omarchy_hotspot_core::net::leases::Lease;
use omarchy_hotspot_core::password::{self, Strength};
use omarchy_hotspot_core::secret::Secret;
use omarchy_hotspot_core::ssid::validate_ssid;
use omarchy_hotspot_core::traffic::TrafficMeter;
use omarchy_hotspot_core::{CoreError, Lang, Msg, t, tf};
use tokio::sync::mpsc::UnboundedSender;
use zeroize::Zeroize;

use crate::daemon::control;
use crate::hotspot_ctx::with_hotspot;

use super::advanced_actions::Saved;
use super::events::Event;
use super::settings::{self, Advanced, Env, ExpertOutput};

/// Раз в сколько тиков (см. `EventHandler::new`) молча обновлять статус — на случай, если
/// раздачу включили/выключили из другого места (CLI, панель).
const REFRESH_EVERY_TICKS: u32 = 12;
/// Опрос устройств и трафика — раз в секунду (PLAN, этап 4: 1–2 с). Root не нужен.
const DEVICES_EVERY_TICKS: u32 = 5;
/// Аренды dnsmasq читает помощник через pkexec (запись в журнал на каждый вызов),
/// поэтому спрашиваем редко: часто — только пока ждём имя только что подключившегося.
const LEASES_EVERY: Duration = Duration::from_secs(15);
/// Редкий повтор: адреса меняются при продлении аренды.
const LEASES_SLOW: Duration = Duration::from_secs(120);
/// Устройство, которое за минуту так и не назвалось, скорее всего имя и не пришлёт.
const NAME_WAIT_SECS: u64 = 60;
const PASSWORD_VISIBLE_FOR: Duration = Duration::from_secs(10);
/// Через сколько пробовать подключиться к службе заново.
const DAEMON_RETRY: Duration = Duration::from_secs(3);
/// Запись в сокет службы не должна подвешивать окно.
const DAEMON_WRITE_TIMEOUT: Duration = Duration::from_millis(500);
/// Сколько вопросов о новых устройствах держим в очереди и сколько ответов помним:
/// устройство со сменой MAC не должно заполнить память окна.
const MAX_PENDING: usize = 32;
/// Запас строк под устройства в окне: не меньше и не больше.
pub const MIN_DEVICE_ROWS: usize = 2;
pub const MAX_DEVICE_ROWS: usize = 8;
/// Сколько список устройств должен быть короче запаса, прежде чем окно станет ниже.
const DEVICE_RESERVE_SHRINK: Duration = Duration::from_secs(30);
const MAX_ANSWERED: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Simple,
    Advanced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    TurningOn,
    TurningOff,
    Applying,
    LoadingPassword,
}

impl Busy {
    pub fn label(self, lang: Lang) -> &'static str {
        t(
            lang,
            match self {
                Busy::TurningOn => Msg::TuiBusyOn,
                Busy::TurningOff => Msg::TuiBusyOff,
                Busy::Applying => Msg::TuiBusyApply,
                Busy::LoadingPassword => Msg::TuiBusyPassword,
            },
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditField {
    Ssid,
    Password,
}

/// Оверлей `e`: имя сети и новый пароль (пусто — не менять).
pub struct Editor {
    pub field: EditField,
    pub ssid: String,
    pub password: String,
    pub error: Option<String>,
}

impl Drop for Editor {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

/// Вопрос пользователю поверх окна. Ответ — Enter (да) или Esc (нет):
/// буквы `y`/`n` в кириллице попадают на клавиши «н»/«т» и путают.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    /// Wi-Fi-адаптер занят клиентским подключением, раздача его оборвёт.
    StartOverClient(String),
    /// Новое устройство подключилось и ждёт разрешения выйти в интернет.
    NewDevice { mac: Mac, name: String },
    /// Первый запуск: включать ли режим одобрения устройств.
    EnableApproval,
    /// Сохранить продвинутые настройки с перезапуском раздачи.
    SaveRestart,
    /// Выход с несохранёнными продвинутыми настройками.
    UnsavedQuit,
    /// Полный сброс (как `omarchy-hotspot reset`).
    Reset,
    /// Создать новый пароль (все устройства отключатся).
    GeneratePassword,
}

/// То, что показывает блок статуса помимо сырого `HotspotState`.
pub struct StatusView {
    pub state: HotspotState,
    pub channel: Option<u8>,
}

impl Default for StatusView {
    fn default() -> Self {
        StatusView {
            state: HotspotState::Off,
            channel: None,
        }
    }
}

/// Что приносит тихое обновление статуса.
pub struct Refreshed {
    pub state: HotspotState,
    pub channel: Option<u8>,
    /// Сеть, к которой Wi-Fi-адаптер подключён как клиент (проверяем только при выключенной раздаче).
    pub client: Option<String>,
}

/// Что приходит от связи со службой (отдельный поток, см. `spawn_daemon_link`).
pub enum DaemonMsg {
    /// Связь установлена; через этот конец окно шлёт запросы.
    Connected(Box<UnixStream>),
    Message(Box<ServerMsg>),
    Lost,
}

/// Ответы фоновых задач.
pub enum BgResponse {
    Refreshed(Result<Refreshed, CoreError>),
    Started {
        result: Result<StartOutcome, CoreError>,
        /// Одобрение просили, но службы нет.
        approval_missing: bool,
    },
    Stopped(Result<bool, CoreError>),
    PasswordLoaded(Result<Option<Secret>, CoreError>),
    Applied(Result<(ApplyOutcome, Config), CoreError>),
    Scanned(Result<Vec<Device>, CoreError>),
    LeasesRead(Result<Vec<Lease>, CoreError>),
    Daemon(DaemonMsg),
    /// Каналы карты и подключения для продвинутого режима.
    AdvEnv(Env),
    AdvSaved(Result<Saved, String>),
    /// Доступ устройства изменён без службы: новый конфиг и сообщение «готово».
    AccessChanged(Result<(Config, String), String>),
    /// Сообщение «отключено».
    Kicked(Result<String, String>),
    LogLoaded(Vec<String>),
    DoctorDone(Vec<Check>),
    ResetDone(Result<(), String>),
}

pub struct AppState {
    pub lang: Lang,
    pub cfg: Config,
    pub cfg_path: PathBuf,
    pub mode: Mode,
    pub status: StatusView,
    pub password: Option<Secret>,
    /// `true`, когда точно известно, что профиля/пароля ещё нет (раздачу ни разу не включали).
    pub password_absent: bool,
    pub show_password: bool,
    password_hide_at: Option<Instant>,
    /// Строки QR полублоками (без цвета — цвет накладывает виджет).
    pub qr: Option<Vec<String>>,
    pub busy: Option<Busy>,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub editing: Option<Editor>,
    pub confirm: Option<Confirm>,
    pub devices: Vec<Device>,
    pub traffic: TrafficMeter,
    /// Продвинутый режим: разделы и черновик настроек.
    pub adv: Advanced,
    /// Аренды dnsmasq: из них берутся имена устройств. Пусто, если помощник не установлен.
    leases: Vec<Lease>,
    leases_at: Option<Instant>,
    /// Сеть, к которой Wi-Fi-адаптер подключён как клиент (о ней предупреждаем перед включением).
    pub wifi_client: Option<String>,
    /// Помощник с правами root установлен: без него не видно имён устройств.
    pub helper_installed: bool,
    /// Связь со службой: через неё шлём одобрения. `None` — службы нет, окно только смотрит.
    daemon_tx: Option<UnixStream>,
    /// Ответы службы, которых ждём (по порядку запросов): что написать при успехе.
    /// Служба отвечает на каждый запрос ровно один раз и в том же порядке.
    daemon_waiting: VecDeque<Option<String>>,
    /// Служба подтвердила, что режим одобрения работает.
    pub daemon_approval: bool,
    /// Состояния устройств по данным службы.
    statuses: HashMap<Mac, DeviceStatus>,
    /// Очередь вопросов про новые устройства (по одному за раз).
    pending: VecDeque<(Mac, String)>,
    /// О ком уже спросили — второй раз не спрашиваем.
    answered: Vec<Mac>,
    /// Первый запуск: спросим про режим одобрения и запишем ответ в конфиг.
    ask_approval: bool,
    ticks_since_devices: u32,
    pub spinner_tick: usize,
    /// QR показан (`c` или клик по `[ QR ]`); по умолчанию скрыт, чтобы окно не меняло размер.
    pub show_qr: bool,
    /// Пароль уже загружается в фоне — второй раз не просим.
    password_loading: bool,
    /// Сколько строк окно держит под устройства: растёт сразу (шагом 2), уменьшается не раньше
    /// чем через `DEVICE_RESERVE_SHRINK` — чтобы окно не дёргалось от каждого подключения.
    pub device_rows_reserved: usize,
    device_reserve_low_since: Option<Instant>,
    /// Когда окно получило фокус: клик сразу после этого — клик «по неактивному окну», он не нажимает.
    pub focus_gained_at: Option<Instant>,
    /// Программа в своём плавающем окне Omarchy: только тогда окно закрывается при потере фокуса.
    pub own_window: bool,
    /// Прокрутка списка устройств в простом режиме (первая показанная строка).
    pub simple_dev_scroll: usize,
    pub should_quit: bool,
    ticks_since_refresh: u32,
    weak_password_pending: bool,
}

impl AppState {
    pub fn new(lang: Lang, cfg: Config, cfg_path: PathBuf) -> Self {
        // Конфига ещё нет — значит программу открыли впервые.
        let ask_approval = !cfg_path.exists();
        let mode = match cfg.ui.mode {
            UiMode::Simple => Mode::Simple,
            UiMode::Advanced => Mode::Advanced,
        };
        AppState {
            lang,
            adv: Advanced::new(&cfg),
            cfg,
            cfg_path,
            mode,
            status: StatusView::default(),
            password: None,
            password_absent: false,
            show_password: false,
            password_hide_at: None,
            qr: None,
            busy: None,
            error: None,
            notice: None,
            editing: None,
            confirm: None,
            devices: Vec::new(),
            traffic: TrafficMeter::new(),
            leases: Vec::new(),
            leases_at: None,
            wifi_client: None,
            helper_installed: PkexecHelper.installed(),
            daemon_tx: None,
            daemon_waiting: VecDeque::new(),
            daemon_approval: false,
            statuses: HashMap::new(),
            pending: VecDeque::new(),
            answered: Vec::new(),
            ask_approval,
            ticks_since_devices: 0,
            spinner_tick: 0,
            show_qr: false,
            password_loading: false,
            device_rows_reserved: MIN_DEVICE_ROWS,
            device_reserve_low_since: None,
            focus_gained_at: None,
            own_window: false,
            simple_dev_scroll: 0,
            should_quit: false,
            ticks_since_refresh: 0,
            weak_password_pending: false,
        }
    }

    /// Фокус ушёл на другое окно: окно Omarchy закрывается сразу — только по самому событию.
    /// Не молча: при открытом вопросе, вводе, идущей операции или несохранённых правках окно
    /// остаётся. Операция закончилась, пока фокус был снаружи, — окно тоже остаётся: результат
    /// (ошибку, предупреждение) пользователь увидит, вернувшись, а закроет следующий уход фокуса.
    pub fn on_focus_lost(&mut self) {
        if !self.cfg.ui.close_on_focus_loss || !self.own_window {
            return;
        }
        let keep = self.editing.is_some()
            || self.confirm.is_some()
            || self.adv.input.is_some()
            || self.busy.is_some()
            || settings::any_dirty(&self.adv.draft, &self.cfg);
        if !keep {
            self.should_quit = true;
        }
    }

    /// Запас строк под устройства: подключились — сразу больше (шагом 2, до 8), ушли — меньше
    /// только когда их стало меньше и так держится `DEVICE_RESERVE_SHRINK`.
    pub fn update_device_reserve(&mut self, now: Instant) {
        let n = self.device_rows().len().max(self.devices.len());
        let target = (n.div_ceil(2) * 2).clamp(MIN_DEVICE_ROWS, MAX_DEVICE_ROWS);
        if target >= self.device_rows_reserved {
            self.device_rows_reserved = target;
            self.device_reserve_low_since = None;
            return;
        }
        match self.device_reserve_low_since {
            None => self.device_reserve_low_since = Some(now),
            Some(since) if now.duration_since(since) >= DEVICE_RESERVE_SHRINK => {
                self.device_rows_reserved = target;
                self.device_reserve_low_since = None;
            }
            Some(_) => {}
        }
    }

    /// ↑↓ и колесо над списком устройств простого режима. Предел — при отрисовке.
    pub fn scroll_simple_devices(&mut self, down: bool) {
        let max = self.devices.len().saturating_sub(1);
        self.simple_dev_scroll = if down {
            (self.simple_dev_scroll + 1).min(max)
        } else {
            self.simple_dev_scroll.saturating_sub(1)
        };
    }

    pub fn is_on(&self) -> bool {
        matches!(self.status.state, HotspotState::On { .. })
    }

    fn is_starting(&self) -> bool {
        matches!(self.status.state, HotspotState::Starting)
    }

    /// Первый опрос при запуске окна и связь со службой.
    pub fn request_initial_refresh(&mut self, tx: &UnboundedSender<Event>) {
        request_refresh(tx, self.cfg.clone());
        spawn_daemon_link(tx.clone());
        if self.mode == Mode::Advanced {
            self.on_enter_advanced(tx);
        }
    }

    /// Заменить конфиг, не потеряв несохранённые правки продвинутого режима.
    pub fn replace_cfg(&mut self, cfg: Config) {
        self.adv.sync_from(&self.cfg, &cfg);
        self.cfg = cfg;
    }

    /// Esc / q: выйти; если в продвинутых настройках есть несохранённое — сначала спросить.
    pub fn request_quit(&mut self) {
        if settings::any_dirty(&self.adv.draft, &self.cfg) && self.confirm.is_none() {
            self.confirm = Some(Confirm::UnsavedQuit);
        } else {
            self.should_quit = true;
        }
    }

    /// Связь со службой есть: запросы доступа идут через неё.
    pub fn daemon_connected(&self) -> bool {
        self.daemon_tx.is_some()
    }

    /// Раздачу перезапустили с новыми настройками: прошлый сеанс и пароль в памяти не действуют.
    pub fn after_restart(&mut self, tx: &UnboundedSender<Event>) {
        self.clear_session();
        self.password = None;
        self.show_password = false;
        self.password_hide_at = None;
        self.qr = None;
        self.load_password(tx);
    }

    /// Режим одобрения включён в настройках, но службы нет — об этом надо сказать.
    pub fn approval_unavailable(&self) -> bool {
        self.is_on() && self.cfg.access.approval_required && !self.daemon_approval
    }

    /// Интерфейс точки доступа, пока раздача включена.
    pub fn ap_iface(&self) -> Option<&str> {
        match &self.status.state {
            HotspotState::On { ap_iface, .. } => Some(ap_iface),
            _ => None,
        }
    }

    pub fn on_tick(&mut self, tx: &UnboundedSender<Event>) {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
        // Раздача выключилась — QR больше не нужен; при следующем включении он сам не всплывёт.
        if !self.is_on() && !self.is_starting() {
            self.show_qr = false;
        }
        self.update_device_reserve(Instant::now());
        if let Some(hide_at) = self.password_hide_at
            && Instant::now() >= hide_at
        {
            self.show_password = false;
            self.password_hide_at = None;
        }
        self.ticks_since_refresh += 1;
        if self.busy.is_none()
            && self.editing.is_none()
            && self.ticks_since_refresh >= REFRESH_EVERY_TICKS
        {
            self.ticks_since_refresh = 0;
            request_refresh(tx, self.cfg.clone());
        }
        self.poll_devices(tx);
        self.show_next_pending();
    }

    /// Первый запуск: спрашиваем про одобрение устройств до всего остального.
    fn ask_approval_once(&mut self) {
        if !self.ask_approval || self.confirm.is_some() || self.busy.is_some() {
            return;
        }
        self.ask_approval = false;
        self.confirm = Some(Confirm::EnableApproval);
    }

    /// Записать ответ про режим одобрения в конфиг (меняем только это поле).
    fn set_approval(&mut self, on: bool) {
        let result = config::update(&self.cfg_path, |cfg| {
            cfg.access.approval_required = on;
            Ok(())
        });
        match result {
            Ok((_, cfg)) => self.replace_cfg(cfg),
            Err(e) => self.error = Some(e.user_message(self.lang)),
        }
    }

    /// Раз в секунду обновляем список устройств, изредка — имена из аренд.
    fn poll_devices(&mut self, tx: &UnboundedSender<Event>) {
        let Some(ap) = self.ap_iface().map(str::to_string) else {
            return;
        };
        self.ticks_since_devices += 1;
        if self.ticks_since_devices < DEVICES_EVERY_TICKS {
            return;
        }
        self.ticks_since_devices = 0;
        request_devices(tx, ap.clone(), self.leases.clone());
        // У службы берём состояния устройств (кто ждёт одобрения, кто заблокирован).
        self.daemon_send(&Request::GetDevices);
        self.daemon_send(&Request::GetState);
        if self.devices.is_empty() {
            return;
        }
        // Часто спрашиваем, только пока ждём имя недавно подключившегося устройства.
        let waiting_for_name = self
            .devices
            .iter()
            .any(|d| d.hostname.is_none() && d.connected_secs < NAME_WAIT_SECS);
        let interval = if waiting_for_name {
            LEASES_EVERY
        } else {
            LEASES_SLOW
        };
        if self.leases_at.is_none_or(|at| at.elapsed() >= interval) {
            self.leases_at = Some(Instant::now());
            request_leases(tx, ap);
        }
    }

    /// Забыть всё, что относится к прошлому сеансу раздачи.
    pub fn clear_session(&mut self) {
        self.devices.clear();
        self.leases.clear();
        self.leases_at = None;
        self.traffic.reset();
        self.statuses.clear();
        self.pending.clear();
        self.answered.clear();
        if matches!(self.confirm, Some(Confirm::NewDevice { .. })) {
            self.confirm = None;
        }
    }

    /// Space: включить или выключить раздачу. Если адаптер занят обычным подключением,
    /// сначала спрашиваем: включение его оборвёт.
    pub fn request_toggle(&mut self, tx: &UnboundedSender<Event>) {
        if self.busy.is_some() || self.editing.is_some() || self.confirm.is_some() {
            return;
        }
        self.error = None;
        self.notice = None;
        if self.is_on() || self.is_starting() {
            self.busy = Some(Busy::TurningOff);
            spawn_bg(tx, || {
                let result = with_hotspot(|h| h.stop());
                // Служба нужна только во время раздачи.
                control::stop();
                BgResponse::Stopped(result)
            });
        } else if let Some(network) = self.wifi_client.clone() {
            self.confirm = Some(Confirm::StartOverClient(network));
        } else {
            self.start_now(tx);
        }
    }

    fn start_now(&mut self, tx: &UnboundedSender<Event>) {
        self.busy = Some(Busy::TurningOn);
        let cfg = self.cfg.clone();
        spawn_bg(tx, move || {
            // Служба нужна для одобрения устройств, уведомлений и авто-выключения.
            let daemon_ready = control::ensure_running();
            let approval = cfg.access.approval_required && daemon_ready;
            BgResponse::Started {
                result: with_hotspot(|h| h.start(&cfg, approval)),
                approval_missing: cfg.access.approval_required && !approval,
            }
        });
    }

    /// Enter в вопросе: согласие.
    pub fn confirm_accept(&mut self, tx: &UnboundedSender<Event>) {
        match self.confirm.take() {
            Some(Confirm::StartOverClient(_)) => self.start_now(tx),
            Some(Confirm::NewDevice { mac, .. }) => {
                self.statuses.insert(mac.clone(), DeviceStatus::Allowed);
                self.apply_statuses();
                self.daemon_send(&Request::Approve { mac });
            }
            Some(Confirm::EnableApproval) => self.set_approval(true),
            Some(Confirm::SaveRestart) => self.adv_save_now(tx),
            Some(Confirm::UnsavedQuit) => {
                self.adv.quit_after_save = true;
                self.adv_save(tx);
            }
            Some(Confirm::Reset) => self.reset_now(tx),
            Some(Confirm::GeneratePassword) => self.generate_password(tx),
            None => {}
        }
    }

    /// Esc в вопросе: отказ. Для нового устройства это чёрный список и отключение.
    pub fn confirm_cancel(&mut self) {
        match self.confirm.take() {
            Some(Confirm::NewDevice { mac, .. }) => {
                self.statuses.insert(mac.clone(), DeviceStatus::Blocked);
                self.apply_statuses();
                self.daemon_send(&Request::Deny { mac });
            }
            // «Решить позже»: ничего не записываем, остаётся значение по умолчанию
            // (спрашивать). Иначе привычным Esc можно было бы молча снять защиту.
            // Выход без сохранения: черновик просто пропадает вместе с окном.
            Some(Confirm::UnsavedQuit) => self.should_quit = true,
            Some(Confirm::EnableApproval)
            | Some(Confirm::StartOverClient(_))
            | Some(Confirm::SaveRestart)
            | Some(Confirm::Reset)
            | Some(Confirm::GeneratePassword)
            | None => {}
        }
    }

    /// Показать следующий вопрос про новое устройство, если окно сейчас свободно.
    fn show_next_pending(&mut self) {
        self.ask_approval_once();
        if self.confirm.is_some() || self.editing.is_some() || self.busy.is_some() {
            return;
        }
        while let Some((mac, name)) = self.pending.pop_front() {
            if self.answered.contains(&mac) {
                continue;
            }
            if self.answered.len() >= MAX_ANSWERED {
                self.answered.remove(0);
            }
            self.answered.push(mac.clone());
            self.confirm = Some(Confirm::NewDevice { mac, name });
            return;
        }
    }

    fn queue_pending(&mut self, mac: &Mac, name: String) {
        if self.pending.len() >= MAX_PENDING {
            return;
        }
        if self.answered.contains(mac) || self.pending.iter().any(|(m, _)| m == mac) {
            return;
        }
        if let Some(Confirm::NewDevice { mac: asked, .. }) = &self.confirm
            && asked == mac
        {
            return;
        }
        self.pending.push_back((mac.clone(), name));
    }

    /// Состояния от службы — на текущий список устройств.
    fn apply_statuses(&mut self) {
        for d in &mut self.devices {
            d.status = self.statuses.get(&d.mac).copied().unwrap_or_default();
        }
    }

    /// Запрос службе. Службы нет — молча ничего не делаем: окно работает и без неё.
    pub fn daemon_send(&mut self, req: &Request) {
        self.daemon_send_then(req, None);
    }

    /// Отправить запрос службе; `done` — сообщение, которое показать, когда служба ответит «готово»
    /// (а не сразу: запрос может закончиться ошибкой).
    pub fn daemon_send_then(&mut self, req: &Request, done: Option<String>) {
        let Some(sock) = &mut self.daemon_tx else {
            return;
        };
        let Ok(mut line) = serde_json::to_string(req) else {
            return;
        };
        line.push('\n');
        if sock.write_all(line.as_bytes()).is_err() {
            // Связь порвалась (или служба не читает): обрываем её совсем, чтобы поток
            // связи заметил конец и подключился заново.
            let _ = sock.shutdown(std::net::Shutdown::Both);
            self.daemon_tx = None;
            self.daemon_waiting.clear();
            return;
        }
        self.daemon_waiting.push_back(done);
    }

    /// Связь со службой пропала: её данные о состояниях устройств больше не действуют.
    fn on_bg_daemon_lost(&mut self) {
        self.daemon_tx = None;
        self.daemon_waiting.clear();
        self.daemon_approval = false;
        self.statuses.clear();
        self.pending.clear();
        self.apply_statuses();
    }

    fn on_daemon_msg(&mut self, msg: ServerMsg) {
        let done = match &msg {
            ServerMsg::Response(_) => self.daemon_waiting.pop_front().flatten(),
            ServerMsg::Event(_) => None,
        };
        match msg {
            ServerMsg::Response(Response::Devices { list }) => {
                self.statuses = list.iter().map(|d| (d.mac.clone(), d.status)).collect();
                self.apply_statuses();
                for d in &list {
                    if d.status == DeviceStatus::Pending {
                        let (mac, name) = (d.mac.clone(), d.display_name());
                        self.queue_pending(&mac, name);
                    }
                }
            }
            ServerMsg::Response(Response::State { approval, .. }) => {
                self.daemon_approval = approval;
            }
            ServerMsg::Response(Response::Ok) if done.is_some() => {
                self.error = None;
                self.notice = done;
            }
            ServerMsg::Response(Response::Err { msg, .. }) => self.error = Some(msg),
            ServerMsg::Response(_) => {}
            ServerMsg::Event(ev) => self.on_daemon_event(ev),
        }
    }

    fn on_daemon_event(&mut self, ev: DaemonEvent) {
        match ev {
            DaemonEvent::DevicePending { device } => {
                self.statuses
                    .insert(device.mac.clone(), DeviceStatus::Pending);
                let (mac, name) = (device.mac.clone(), device.display_name());
                self.queue_pending(&mac, name);
                self.apply_statuses();
            }
            DaemonEvent::DeviceApproved { mac } => {
                self.statuses.insert(mac, DeviceStatus::Allowed);
                self.apply_statuses();
                self.reload_access_lists();
            }
            DaemonEvent::DeviceBlocked { mac } => {
                self.statuses.insert(mac, DeviceStatus::Blocked);
                self.apply_statuses();
                self.reload_access_lists();
            }
            DaemonEvent::DeviceUnblocked { mac } => {
                self.statuses.remove(&mac);
                self.apply_statuses();
                self.reload_access_lists();
            }
            DaemonEvent::DeviceLeft { mac } => {
                self.statuses.remove(&mac);
                self.pending.retain(|(m, _)| m != &mac);
            }
            DaemonEvent::IdleWarning { minutes_left } => {
                self.notice = Some(tf(
                    self.lang,
                    Msg::TuiIdleWarningFmt,
                    &[&minutes_left.to_string()],
                ));
            }
            DaemonEvent::TimerWarning { minutes_left } => {
                self.notice = Some(tf(
                    self.lang,
                    Msg::TuiTimerWarningFmt,
                    &[&minutes_left.to_string()],
                ));
            }
            DaemonEvent::Stopped { reason } => {
                let msg = if reason == "timer" {
                    Msg::NoticeStoppedTimer
                } else {
                    Msg::NoticeStoppedIdle
                };
                self.notice = Some(t(self.lang, msg).to_string());
            }
            DaemonEvent::StateChanged { .. } | DaemonEvent::DeviceJoined { .. } => {}
        }
    }

    /// Списки доступа пишет служба — после её изменения перечитываем файл (для чёрного списка).
    fn reload_access_lists(&mut self) {
        match config::load(&self.cfg_path) {
            Ok(loaded) => self.replace_cfg(loaded.config),
            Err(e) => tracing::debug!("cannot reload config: {e}"),
        }
    }

    /// Новый случайный пароль (после вопроса): применяется так же, как пароль из окна `e`.
    fn generate_password(&mut self, tx: &UnboundedSender<Event>) {
        if self.busy.is_some() {
            return;
        }
        let secret = match password::generate() {
            Ok(s) => s,
            Err(e) => {
                self.error = Some(e.user_message(self.lang));
                return;
            }
        };
        let cfg = self.cfg.clone();
        let cfg_path = self.cfg_path.clone();
        self.error = None;
        self.notice = None;
        self.busy = Some(Busy::Applying);
        spawn_bg(tx, move || {
            let result = with_hotspot(|h| h.apply(&cfg, Some(secret))).and_then(|outcome| {
                // Конфиг не меняется, но берём свежий: его могла изменить служба.
                Ok((outcome, config::load(&cfg_path)?.config))
            });
            BgResponse::Applied(result)
        });
    }

    /// p: показать/скрыть пароль на 10 секунд.
    pub fn toggle_password(&mut self, tx: &UnboundedSender<Event>) {
        if self.editing.is_some() {
            return;
        }
        if self.show_password {
            self.show_password = false;
            self.password_hide_at = None;
            return;
        }
        self.show_password = true;
        self.password_hide_at = Some(Instant::now() + PASSWORD_VISIBLE_FOR);
        if self.password.is_none() && !self.password_absent && self.busy.is_none() {
            self.busy = Some(Busy::LoadingPassword);
            self.password_loading = true;
            spawn_bg(tx, || {
                BgResponse::PasswordLoaded(with_hotspot(|h| h.password()))
            });
        }
    }

    /// Tab: переключить простой/продвинутый режим. Последний открытый режим запоминается.
    pub fn toggle_mode(&mut self, tx: &UnboundedSender<Event>) {
        if self.editing.is_some() || self.adv.input.is_some() {
            return;
        }
        self.mode = match self.mode {
            Mode::Simple => Mode::Advanced,
            Mode::Advanced => Mode::Simple,
        };
        if self.mode == Mode::Advanced {
            self.on_enter_advanced(tx);
        }
        // Файла конфига ещё нет — не создаём его ради режима: иначе пропал бы вопрос первого запуска.
        if self.cfg_path.exists() {
            let mode = match self.mode {
                Mode::Simple => UiMode::Simple,
                Mode::Advanced => UiMode::Advanced,
            };
            match config::update(&self.cfg_path, |c| {
                c.ui.mode = mode;
                Ok(())
            }) {
                Ok((_, cfg)) => self.replace_cfg(cfg),
                Err(e) => tracing::debug!("cannot save window mode: {e}"),
            }
        }
    }

    /// e: открыть оверлей изменения имени сети и пароля.
    pub fn start_edit(&mut self) {
        if self.busy.is_some() || self.editing.is_some() {
            return;
        }
        self.editing = Some(Editor {
            field: EditField::Ssid,
            ssid: self.cfg.hotspot.ssid.clone(),
            password: String::new(),
            error: None,
        });
    }

    pub fn cancel_edit(&mut self) {
        self.editing = None;
    }

    pub fn edit_toggle_field(&mut self) {
        if let Some(e) = &mut self.editing {
            e.field = match e.field {
                EditField::Ssid => EditField::Password,
                EditField::Password => EditField::Ssid,
            };
        }
    }

    pub fn edit_push_char(&mut self, c: char) {
        let Some(e) = &mut self.editing else { return };
        e.error = None;
        match e.field {
            EditField::Ssid => e.ssid.push(c),
            EditField::Password => e.password.push(c),
        }
    }

    pub fn edit_backspace(&mut self) {
        let Some(e) = &mut self.editing else { return };
        e.error = None;
        match e.field {
            EditField::Ssid => {
                e.ssid.pop();
            }
            EditField::Password => {
                e.password.pop();
            }
        }
    }

    /// g на пустом поле пароля — сгенерировать надёжный пароль.
    pub fn edit_generate(&mut self) {
        let Some(e) = &mut self.editing else { return };
        if e.field != EditField::Password || !e.password.is_empty() {
            return;
        }
        match password::generate() {
            Ok(secret) => e.password = secret.expose().to_string(),
            Err(err) => e.error = Some(err.user_message(self.lang)),
        }
    }

    /// Enter в оверлее: проверить и применить.
    pub fn edit_submit(&mut self, tx: &UnboundedSender<Event>) {
        let Some(editor) = &mut self.editing else {
            return;
        };
        let ssid = editor.ssid.trim().to_string();
        if let Err(e) = validate_ssid(&ssid) {
            editor.error = Some(CoreError::from(e).user_message(self.lang));
            return;
        }
        let password_secret = if editor.password.is_empty() {
            None
        } else {
            match password::validate_user_password(&editor.password) {
                Ok(strength) => {
                    self.weak_password_pending = matches!(strength, Strength::Weak);
                    Some(Secret::new(std::mem::take(&mut editor.password)))
                }
                Err(e) => {
                    editor.error = Some(CoreError::from(e).user_message(self.lang));
                    return;
                }
            }
        };

        let mut cfg = self.cfg.clone();
        cfg.hotspot.ssid = ssid;
        let cfg_path = self.cfg_path.clone();
        self.editing = None;
        self.error = None;
        self.notice = None;
        self.busy = Some(Busy::Applying);
        spawn_bg(tx, move || {
            let result = with_hotspot(|h| h.apply(&cfg, password_secret)).and_then(|outcome| {
                // Конфиг мог измениться: служба записывает в него одобренные устройства.
                // Поэтому под замком берём свежий файл и правим в нём только имя сети.
                let (_, latest) = config::update(&cfg_path, |c| {
                    c.hotspot.ssid = cfg.hotspot.ssid.clone();
                    Ok(())
                })?;
                Ok((outcome, latest))
            });
            BgResponse::Applied(result)
        });
    }

    pub fn on_bg(&mut self, resp: BgResponse, tx: &UnboundedSender<Event>) {
        match resp {
            BgResponse::Refreshed(Ok(r)) => {
                let was_on = self.is_on();
                self.status = StatusView {
                    state: r.state,
                    channel: r.channel,
                };
                self.wifi_client = r.client;
                // Раздачу выключили не из окна (CLI, панель) — список и счётчики уже не наши.
                if was_on && !self.is_on() {
                    self.clear_session();
                }
                // Пароль мог прийти раньше, чем статус «включено»: тогда QR не собрался — собираем сейчас.
                if self.is_on() && self.qr.is_none() && self.password.is_some() {
                    self.rebuild_qr();
                }
            }
            BgResponse::Refreshed(Err(e)) => {
                self.error = Some(e.user_message(self.lang));
            }
            BgResponse::Started {
                result,
                approval_missing,
            } => {
                self.busy = None;
                match result {
                    Ok(StartOutcome::AlreadyOn) => {}
                    Ok(StartOutcome::Started { .. }) => {
                        self.notice = None;
                        if approval_missing {
                            self.notice =
                                Some(t(self.lang, Msg::NoticeApprovalNoDaemon).to_string());
                        }
                        self.clear_session();
                        self.wifi_client = None;
                        self.password = None;
                        // Включение всегда создаёт профиль (с новым или прежним паролем).
                        self.password_absent = false;
                        self.qr = None;
                        self.load_password(tx);
                    }
                    Err(e) => self.error = Some(e.user_message(self.lang)),
                }
                request_refresh(tx, self.cfg.clone());
            }
            BgResponse::Stopped(result) => {
                self.busy = None;
                match result {
                    Ok(_) => {
                        self.clear_session();
                        // Пароль в памяти дольше не нужен — забываем, пока не спросят снова.
                        self.password = None;
                        self.show_password = false;
                        self.password_hide_at = None;
                        self.qr = None;
                    }
                    Err(e) => self.error = Some(e.user_message(self.lang)),
                }
                request_refresh(tx, self.cfg.clone());
            }
            BgResponse::PasswordLoaded(result) => {
                self.password_loading = false;
                if self.busy == Some(Busy::LoadingPassword) {
                    self.busy = None;
                }
                match result {
                    Ok(psk) => {
                        self.password_absent = psk.is_none();
                        self.password = psk;
                        self.rebuild_qr();
                    }
                    Err(e) => self.error = Some(e.user_message(self.lang)),
                }
            }
            BgResponse::Scanned(Ok(devices)) => {
                self.traffic.update(&devices, Instant::now());
                self.devices = devices;
                self.apply_statuses();
            }
            BgResponse::Scanned(Err(e)) => {
                // Опрос идёт раз в секунду: ошибку не показываем, чтобы не мигать сообщением.
                tracing::debug!("cannot read device list: {e}");
            }
            BgResponse::Daemon(DaemonMsg::Connected(sock)) => {
                self.daemon_tx = Some(*sock);
                // Первый ответ — на подписку, её отправил поток связи.
                self.daemon_waiting = VecDeque::from([None]);
                self.daemon_send(&Request::GetState);
                self.daemon_send(&Request::GetDevices);
            }
            BgResponse::Daemon(DaemonMsg::Lost) => self.on_bg_daemon_lost(),
            BgResponse::Daemon(DaemonMsg::Message(msg)) => self.on_daemon_msg(*msg),
            BgResponse::LeasesRead(Ok(leases)) => self.leases = leases,
            BgResponse::LeasesRead(Err(e)) => {
                tracing::debug!("cannot read dnsmasq leases: {e}");
            }
            BgResponse::AdvEnv(env) => self.adv.env = env,
            BgResponse::AdvSaved(result) => self.on_adv_saved(result, tx),
            BgResponse::AccessChanged(result) => self.on_access_changed(result),
            BgResponse::Kicked(Ok(done)) => self.notice = Some(done),
            BgResponse::Kicked(Err(e)) => {
                self.notice = None;
                self.error = Some(e);
            }
            BgResponse::LogLoaded(lines) => self.adv.output = ExpertOutput::Log(lines),
            BgResponse::DoctorDone(checks) => self.adv.output = ExpertOutput::Doctor(checks),
            BgResponse::ResetDone(result) => self.on_reset_done(result, tx),
            BgResponse::Applied(result) => {
                self.busy = None;
                match result {
                    Ok((outcome, cfg)) => {
                        self.replace_cfg(cfg);
                        let mut msg = t(
                            self.lang,
                            match outcome {
                                ApplyOutcome::Saved => Msg::SetSaved,
                                ApplyOutcome::Updated => Msg::SetUpdated,
                                ApplyOutcome::Restarted => Msg::SetRestarted,
                            },
                        )
                        .to_string();
                        if self.weak_password_pending {
                            msg.push_str(" (");
                            msg.push_str(t(self.lang, Msg::TuiPwWeakShort));
                            msg.push(')');
                        }
                        self.notice = Some(msg);
                        self.password = None;
                        self.show_password = false;
                        self.qr = None;
                        self.load_password(tx);
                    }
                    Err(e) => self.error = Some(e.user_message(self.lang)),
                }
                self.weak_password_pending = false;
                request_refresh(tx, self.cfg.clone());
            }
        }
    }

    /// Загрузить пароль в фоне (для QR и индикатора силы), если он ещё не загружается.
    pub fn load_password(&mut self, tx: &UnboundedSender<Event>) {
        if !self.password_loading {
            self.password_loading = true;
            request_password_load(tx);
        }
    }

    /// `c` или клик по `[ QR ]`: показать или спрятать QR. Показываем — значит, картинка нужна
    /// сейчас: пароль в памяти — собираем сразу, нет — загружаем (окно могли открыть, когда
    /// раздача уже работала, и пароль до этого никто не просил).
    pub fn toggle_qr(&mut self, tx: &UnboundedSender<Event>) {
        self.toggle_qr_with(|| request_password_load(tx));
    }

    fn toggle_qr_with(&mut self, load: impl FnOnce()) {
        if !self.is_on() {
            return;
        }
        self.show_qr = !self.show_qr;
        if !self.show_qr || self.qr.is_some() {
            return;
        }
        if self.password.is_some() {
            self.rebuild_qr();
        } else if !self.password_loading {
            self.password_loading = true;
            load();
        }
    }

    fn rebuild_qr(&mut self) {
        self.qr = None;
        if !self.is_on() {
            return;
        }
        let Some(psk) = &self.password else { return };
        let data = omarchy_hotspot_core::qr::wifi_qr_string(
            &self.cfg.hotspot.ssid,
            psk,
            self.cfg.hotspot.security,
            self.cfg.hotspot.hidden,
        );
        // Тихая зона в 1 модуль: вокруг блока белая заливка кода, камера его находит (§8.2).
        if let Some(matrix) = omarchy_hotspot_core::qr::matrix(data.expose(), 1) {
            self.qr = Some(omarchy_hotspot_core::qr::half_blocks(&matrix));
        }
    }
}

pub fn request_refresh(tx: &UnboundedSender<Event>, cfg: Config) {
    spawn_bg(tx, move || {
        let result = with_hotspot(|h| {
            let state = h.state()?;
            let (channel, client) = match &state {
                HotspotState::On { ap_iface, .. } => (iw::current_channel(ap_iface), None),
                // Пока раздача выключена, адаптер может быть занят обычным подключением.
                _ => (None, h.client_connection(&cfg).unwrap_or(None)),
            };
            Ok(Refreshed {
                state,
                channel,
                client,
            })
        });
        BgResponse::Refreshed(result)
    });
}

/// Опрос подключённых устройств (`iw station dump` + таблица соседей). Root не нужен.
fn request_devices(tx: &UnboundedSender<Event>, ap_iface: String, leases: Vec<Lease>) {
    spawn_bg(tx, move || {
        BgResponse::Scanned(devices::scan(&ap_iface, &leases))
    });
}

/// Аренды dnsmasq через помощника — только ради имён устройств.
fn request_leases(tx: &UnboundedSender<Event>, ap_iface: String) {
    spawn_bg(tx, move || {
        BgResponse::LeasesRead(with_hotspot(|h| h.leases(&ap_iface)))
    });
}

pub fn request_password_load(tx: &UnboundedSender<Event>) {
    spawn_bg(tx, || {
        BgResponse::PasswordLoaded(with_hotspot(|h| h.password()))
    });
}

/// Связь со службой: отдельный поток подключается к сокету, подписывается на события
/// и шлёт всё в тот же канал, что и остальные фоновые ответы. Если службы нет —
/// поток просто пробует снова: окно работает и без неё, только без одобрения устройств.
fn spawn_daemon_link(tx: UnboundedSender<Event>) {
    let send = move |msg: DaemonMsg| {
        tx.send(Event::Bg(Box::new(BgResponse::Daemon(msg))))
            .is_ok()
    };
    std::thread::Builder::new()
        .name("omarchy-hotspot-daemon-link".into())
        .spawn(move || {
            loop {
                if let Ok(mut client) = ipc::Client::connect() {
                    match connect(&mut client) {
                        Ok(writer) => {
                            if !send(DaemonMsg::Connected(Box::new(writer))) {
                                return;
                            }
                            loop {
                                match client.recv() {
                                    Ok(Some(msg)) => {
                                        if !send(DaemonMsg::Message(Box::new(msg))) {
                                            return;
                                        }
                                    }
                                    Ok(None) => break,
                                    Err(e) => {
                                        tracing::debug!("daemon link: {e}");
                                        break;
                                    }
                                }
                            }
                            if !send(DaemonMsg::Lost) {
                                return;
                            }
                        }
                        Err(e) => tracing::debug!("cannot subscribe to the service: {e}"),
                    }
                }
                std::thread::sleep(DAEMON_RETRY);
            }
        })
        .expect("cannot start daemon link thread");
}

/// Подписаться на события и получить конец для отправки запросов.
fn connect(client: &mut ipc::Client) -> Result<UnixStream, CoreError> {
    client.set_timeout(None)?;
    client.send(&Request::Subscribe)?;
    let writer = client.try_clone_writer()?;
    writer.set_write_timeout(Some(DAEMON_WRITE_TIMEOUT))?;
    Ok(writer)
}

pub fn spawn_bg(tx: &UnboundedSender<Event>, job: impl FnOnce() -> BgResponse + Send + 'static) {
    let tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        let resp = job();
        let _ = tx.send(Event::Bg(Box::new(resp)));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> AppState {
        let mut a = AppState::new(
            Lang::Ru,
            Config::default(),
            PathBuf::from("/tmp/oh-state-test.toml"),
        );
        // Вопрос первого запуска проверяется отдельно, остальным тестам он мешает.
        a.ask_approval = false;
        a
    }

    #[test]
    fn first_run_asks_about_approval() {
        let mut a = app();
        a.ask_approval = true;
        a.show_next_pending();
        assert_eq!(a.confirm, Some(Confirm::EnableApproval));
        // «Решить позже» ничего не записывает и защиту не снимает.
        a.confirm_cancel();
        assert!(a.cfg.access.approval_required);
        assert!(!a.cfg_path.exists());
        a.show_next_pending();
        assert!(a.confirm.is_none(), "вопрос повторился в том же сеансе");

        // Согласие записывается в файл, и второй раз вопроса не будет.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut a = app();
        let dir = std::env::temp_dir().join(format!("oh-ask-{}", std::process::id()));
        a.cfg_path = dir.join("config.toml");
        a.ask_approval = true;
        a.show_next_pending();
        a.confirm_accept(&tx);
        assert_eq!(a.error, None);
        assert!(a.cfg.access.approval_required);
        assert!(a.cfg_path.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn mac(last: &str) -> Mac {
        Mac::parse(&format!("3c:2e:f5:11:22:{last}")).unwrap()
    }

    fn device(last: &str) -> Device {
        Device {
            mac: mac(last),
            ip: None,
            hostname: Some(format!("Phone-{last}")),
            signal_dbm: Some(-50),
            rx_bytes: 0,
            tx_bytes: 0,
            connected_secs: 3,
            status: DeviceStatus::Pending,
        }
    }

    #[test]
    fn new_device_turns_into_a_question_once() {
        let mut a = app();
        a.devices = vec![device("01")];
        a.on_daemon_event(DaemonEvent::DevicePending {
            device: device("01"),
        });
        a.show_next_pending();
        assert!(
            matches!(&a.confirm, Some(Confirm::NewDevice { name, .. }) if name == "Phone-01"),
            "не спросили про новое устройство"
        );
        // Пока вопрос на экране, второе такое же событие его не дублирует.
        a.on_daemon_event(DaemonEvent::DevicePending {
            device: device("01"),
        });
        assert!(a.pending.is_empty());
    }

    #[test]
    fn answer_changes_status_and_is_not_asked_again() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut a = app();
        a.devices = vec![device("01"), device("02")];
        for d in ["01", "02"] {
            a.on_daemon_event(DaemonEvent::DevicePending { device: device(d) });
        }
        // Первому — разрешаем, второму — отказываем. Службы нет, запросы просто не уходят.
        a.show_next_pending();
        a.confirm_accept(&tx);
        assert_eq!(a.devices[0].status, DeviceStatus::Allowed);
        a.show_next_pending();
        assert!(matches!(&a.confirm, Some(Confirm::NewDevice { mac: m, .. }) if m == &mac("02")));
        a.confirm_cancel();
        assert_eq!(a.devices[1].status, DeviceStatus::Blocked);
        // Повторное событие о тех же устройствах вопросов больше не вызывает.
        for d in ["01", "02"] {
            a.on_daemon_event(DaemonEvent::DevicePending { device: device(d) });
        }
        a.show_next_pending();
        assert!(a.confirm.is_none());
    }

    #[test]
    fn service_messages_become_notices() {
        let mut a = app();
        a.on_daemon_event(DaemonEvent::IdleWarning { minutes_left: 1 });
        assert!(a.notice.as_deref().is_some_and(|n| n.contains('1')));
        a.on_daemon_event(DaemonEvent::Stopped {
            reason: "idle".into(),
        });
        assert_eq!(
            a.notice.as_deref(),
            Some(t(Lang::Ru, Msg::NoticeStoppedIdle))
        );
    }

    #[test]
    fn statuses_from_service_land_on_the_list() {
        let mut a = app();
        a.devices = vec![device("01")];
        a.devices[0].status = DeviceStatus::Allowed;
        a.on_daemon_msg(ServerMsg::Response(Response::Devices {
            list: vec![device("01")],
        }));
        assert_eq!(a.devices[0].status, DeviceStatus::Pending);
        // Связь со службой пропала — состояния больше не наши.
        a.on_bg_daemon_lost();
        assert_eq!(a.devices[0].status, DeviceStatus::Allowed);
    }

    #[test]
    fn window_without_service_warns_only_when_approval_is_on() {
        let mut a = app();
        a.status.state = HotspotState::On {
            since: None,
            ap_iface: "wlp15s0".into(),
            uplink: None,
        };
        a.cfg.access.approval_required = true;
        assert!(a.approval_unavailable());
        a.daemon_approval = true;
        assert!(!a.approval_unavailable());
        a.daemon_approval = false;
        a.cfg.access.approval_required = false;
        assert!(!a.approval_unavailable());
    }

    #[test]
    fn service_action_notice_waits_for_its_own_reply() {
        let mut a = app();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (ours, _service) = UnixStream::pair().unwrap();
        // Окно подключилось: ждёт ответы на подписку, GetState и GetDevices.
        a.on_bg(
            BgResponse::Daemon(DaemonMsg::Connected(Box::new(ours))),
            &tx,
        );
        let mac = Mac::parse("aa:bb:cc:dd:ee:ff").unwrap();
        a.daemon_send_then(&Request::Block { mac }, Some("blocked".into()));
        assert_eq!(a.notice, None, "сообщение до ответа службы");

        let reply = |a: &mut AppState, r: Response| a.on_daemon_msg(ServerMsg::Response(r));
        reply(&mut a, Response::Ok);
        reply(&mut a, Response::Devices { list: vec![] });
        a.on_daemon_msg(ServerMsg::Event(DaemonEvent::DeviceLeft {
            mac: Mac::parse("12:22:33:44:55:66").unwrap(),
        }));
        reply(&mut a, Response::Devices { list: vec![] });
        assert_eq!(a.notice, None, "сообщение по чужому ответу");
        reply(&mut a, Response::err("failed", "boom"));
        assert_eq!(
            (a.notice.as_deref(), a.error.as_deref()),
            (None, Some("boom"))
        );

        let mac = Mac::parse("aa:bb:cc:dd:ee:ff").unwrap();
        a.daemon_send_then(&Request::Unblock { mac }, Some("unblocked".into()));
        reply(&mut a, Response::Ok);
        assert_eq!(
            (a.notice.as_deref(), a.error.as_deref()),
            (Some("unblocked"), None)
        );
    }

    #[test]
    fn window_closes_on_focus_loss_but_never_drops_open_work() {
        let ours = || {
            let mut a = app();
            a.own_window = true;
            a
        };
        // Фокус ушёл — закрываемся сразу.
        let mut a = ours();
        a.on_focus_lost();
        assert!(a.should_quit, "окно не закрылось сразу");
        // Обычный терминал пользователя или выключенная настройка — не закрываемся.
        let mut a = app();
        a.on_focus_lost();
        assert!(!a.should_quit, "закрылись в чужом терминале");
        let mut a = ours();
        a.cfg.ui.close_on_focus_loss = false;
        a.on_focus_lost();
        assert!(!a.should_quit, "закрылись при выключенной настройке");
        // Открытый ввод, вопрос, несохранённые правки — окно остаётся.
        let mut a = ours();
        a.start_edit();
        a.on_focus_lost();
        assert!(!a.should_quit, "бросили окно изменения сети");
        let mut a = ours();
        a.confirm = Some(Confirm::NewDevice {
            mac: Mac::parse("aa:bb:cc:dd:ee:01").unwrap(),
            name: "Phone".into(),
        });
        a.on_focus_lost();
        assert!(!a.should_quit, "бросили вопрос об устройстве");
        let mut a = ours();
        a.adv.draft.automation.timer_minutes = 30;
        a.on_focus_lost();
        assert!(
            !a.should_quit && a.confirm.is_none(),
            "бросили несохранённые правки"
        );
        // Фокус ушёл посреди включения — окно ждёт; включение закончилось с ошибкой, пока фокус
        // снаружи, — окно всё ещё открыто, ошибку видно. Закроет только следующий уход фокуса.
        let mut a = ours();
        a.busy = Some(Busy::TurningOn);
        a.on_focus_lost();
        assert!(!a.should_quit, "закрылись посреди включения");
        // Так заканчивается включение с ошибкой (`on_bg` → `Started`): окно закрывается только по
        // событию потери фокуса, а нового не будет, пока пользователь не вернётся и не уйдёт снова.
        a.busy = None;
        a.error = Some("Не удалось включить защиту раздачи".into());
        assert!(!a.should_quit, "результат включения не показан");
        a.on_focus_lost();
        assert!(a.should_quit);
    }

    fn on_state() -> HotspotState {
        HotspotState::On {
            since: None,
            ap_iface: "wlp15s0".into(),
            uplink: None,
        }
    }

    #[test]
    fn qr_is_built_even_if_the_password_arrives_before_the_on_state() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut a = app();
        // Включили из окна: пароль и статус грузятся параллельно, пароль пришёл первым.
        a.on_bg(
            BgResponse::PasswordLoaded(Ok(Some(Secret::new("Ab3dE5gH7jK9mN2p")))),
            &tx,
        );
        a.on_bg(
            BgResponse::Refreshed(Ok(Refreshed {
                state: on_state(),
                channel: Some(36),
                client: None,
            })),
            &tx,
        );
        assert!(
            a.qr.is_some(),
            "пароль есть, раздача включена, а QR не собран"
        );
    }

    #[test]
    fn opening_qr_loads_the_password_when_nobody_did() {
        // Окно открыли, когда раздача уже работала: пароль ещё никто не загружал.
        let mut a = app();
        a.status.state = on_state();
        let mut loads = 0;
        a.toggle_qr_with(|| loads += 1);
        assert!(a.show_qr);
        assert_eq!(loads, 1, "QR открыли, а пароль загружать не стали");
        // Повторное открытие, пока пароль ещё грузится, второй загрузки не просит.
        a.toggle_qr_with(|| loads += 1);
        a.toggle_qr_with(|| loads += 1);
        assert_eq!(loads, 1);
        // Пароль пришёл — QR собран сразу, без Tab.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        a.on_bg(
            BgResponse::PasswordLoaded(Ok(Some(Secret::new("Ab3dE5gH7jK9mN2p")))),
            &tx,
        );
        assert!(a.show_qr && a.qr.is_some());
        // Пароль уже в памяти, QR сбросили (например, после смены настроек) — собираем без загрузки.
        a.qr = None;
        a.show_qr = false;
        a.toggle_qr_with(|| loads += 1);
        assert!(a.qr.is_some());
        assert_eq!(loads, 1);
        // Раздача выключена — QR не открывается и ничего не грузится.
        let mut off = app();
        off.toggle_qr_with(|| loads += 1);
        assert!(!off.show_qr);
        assert_eq!(loads, 1);
    }
}
