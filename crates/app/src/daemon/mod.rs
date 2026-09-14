//! Фоновая служба (этап 5): следит за подключениями, спрашивает про новые устройства,
//! шлёт уведомления и выключает раздачу по простою. С окном общается по сокету
//! (docs/DECISIONS.md §6), с правами root — через помощника в режиме `serve`.

pub mod control;
pub mod notify;
mod server;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use omarchy_hotspot_core::access::{self, AccessChange, AccessLists};
use omarchy_hotspot_core::backend::HotspotState;
use omarchy_hotspot_core::config::{self, Config};
use omarchy_hotspot_core::devices::{Device, DeviceStatus};
use omarchy_hotspot_core::helper::{HelperRunner, ServeHelper};
use omarchy_hotspot_core::helper_proto::{HelperRequest, Iface, Mac};
use omarchy_hotspot_core::hotspot;
use omarchy_hotspot_core::ipc::{self, Event, Request, Response, WireState, err_code};
use omarchy_hotspot_core::net::leases::Lease;
use omarchy_hotspot_core::net::uplink;
use omarchy_hotspot_core::{CoreError, Lang, Msg, t, tf};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{broadcast, mpsc};

use crate::cli::Ctx;
use crate::hotspot_ctx::{with_hotspot, with_hotspot_helper};

use notify::{Notifier, Urgency};
use server::ClientCmd;

/// Шаг главного цикла: опрос устройств (root не нужен, `iw station dump`).
const TICK: Duration = Duration::from_secs(1);
/// Состояние раздачи спрашиваем реже: каждый опрос — запуск `nmcli`.
const STATE_EVERY_TICKS: u32 = 2;
/// Аренды dnsmasq (только ради имён) читает помощник — спрашиваем редко.
const LEASES_EVERY: Duration = Duration::from_secs(15);
const LEASES_SLOW: Duration = Duration::from_secs(120);
/// Устройство, которое за минуту не назвалось, имя уже вряд ли пришлёт.
const NAME_WAIT_SECS: u64 = 60;
/// Заблокированное устройство отключаем не чаще, чем раз в столько секунд.
const KICK_COOLDOWN: Duration = Duration::from_secs(10);
/// Сколько тиков ждём после включения раздачи, прежде чем применять свои правила:
/// сначала должен закончить тот, кто её включил (окно или CLI).
const RULES_AFTER_TICKS: u32 = 2;
/// Перечитывание конфига с диска (его меняют окно и CLI).
const RELOAD_EVERY_TICKS: u32 = 10;
/// За сколько минут до авто-выключения предупреждаем.
const IDLE_WARN_MINUTES: u32 = 1;
/// За сколько минут до выключения по таймеру предупреждаем.
const TIMER_WARN_MINUTES: u32 = 1;
/// Задержки между попытками применить правила (секунды): без них при отказе помощника
/// служба звала бы pkexec каждую секунду до конца раздачи.
const RULES_RETRY_SECS: [u64; 5] = [1, 2, 5, 15, 30];
/// Сколько новых устройств помним и о скольких шлём уведомления за сеанс: гость, знающий
/// пароль, может менять MAC по кругу — заваливать рабочий стол вопросами мы не дадим.
const MAX_ASKED: usize = 32;
const MAX_NEW_DEVICE_NOTIFICATIONS: u32 = 10;

pub fn run(ctx: &Ctx) -> anyhow::Result<()> {
    let (cfg, cfg_path) = ctx.load_config()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(run_async(ctx.lang, cfg, cfg_path))
}

async fn run_async(lang: Lang, cfg: Config, cfg_path: PathBuf) -> anyhow::Result<()> {
    let bound = server::bind()?;
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<ClientCmd>(32);
    let (events, _) = broadcast::channel(64);
    let accept = tokio::spawn(server::accept_loop(bound.listener, cmd_tx, events.clone()));

    let mut daemon = Daemon::new(lang, cfg, cfg_path, events);
    let result = daemon.main_loop(&mut cmd_rx).await;
    accept.abort();
    daemon.helper.shutdown();
    // Файл сокета наш: его удаление защищено замком службы (`bound.lock`).
    let _ = std::fs::remove_file(&bound.path);
    drop(bound.lock);
    tracing::info!("daemon stopped");
    result
}

struct Daemon {
    lang: Lang,
    cfg: Config,
    cfg_path: PathBuf,
    helper: Arc<ServeHelper>,
    notifier: Notifier,
    events: broadcast::Sender<Event>,
    state: HotspotState,
    devices: Vec<Device>,
    leases: Vec<Lease>,
    leases_at: Option<Instant>,
    /// Кто уже подключён (MAC → имя для уведомления об отключении).
    known: HashMap<Mac, String>,
    kicked_at: HashMap<Mac, Instant>,
    /// О ком уже спросили в этом сеансе (чтобы не спрашивать каждую секунду).
    asked: Vec<Mac>,
    /// С какими настройками применены правила брандмауэра этой раздачи.
    applied_rules: Option<RulesKey>,
    /// Следующая попытка применить правила и сколько их уже было.
    rules_retry_at: Option<Instant>,
    rules_attempts: usize,
    /// Помощника нет или он не разрешён: пробовать снова до конца раздачи бессмысленно.
    rules_giveup: bool,
    /// Список устройств хотя бы раз прочитан, и сколько подряд прочитать не удалось.
    scan_ok: bool,
    scan_errors: u32,
    /// Сколько уведомлений о новых устройствах уже послали.
    new_device_notifications: u32,
    on_ticks: u32,
    ticks: u32,
    idle_since: Option<Instant>,
    idle_warned: bool,
    /// Таймер раздачи: сколько минут задано и от какого момента считаем.
    timer: Option<(u32, SystemTime)>,
    timer_warned: bool,
    /// Автозапуск при входе уже проверен (один раз за жизнь службы).
    autostart_checked: bool,
}

/// Настройки, от которых зависят правила брандмауэра (SECURITY.md §3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
struct RulesKey {
    approval: bool,
    guests_reach_pc: bool,
    /// Источник интернета из настроек: он стоит в правиле `oifname != uplink drop`.
    uplink: String,
}

impl Daemon {
    fn new(lang: Lang, cfg: Config, cfg_path: PathBuf, events: broadcast::Sender<Event>) -> Daemon {
        let notifier = Notifier::new(cfg.access.notifications);
        Daemon {
            lang,
            cfg,
            cfg_path,
            helper: Arc::new(ServeHelper::new()),
            notifier,
            events,
            state: HotspotState::Off,
            devices: Vec::new(),
            leases: Vec::new(),
            leases_at: None,
            known: HashMap::new(),
            kicked_at: HashMap::new(),
            asked: Vec::new(),
            applied_rules: None,
            rules_retry_at: None,
            rules_attempts: 0,
            rules_giveup: false,
            scan_ok: false,
            scan_errors: 0,
            new_device_notifications: 0,
            on_ticks: 0,
            ticks: 0,
            idle_since: None,
            idle_warned: false,
            timer: None,
            timer_warned: false,
            autostart_checked: false,
        }
    }

    async fn main_loop(&mut self, cmd_rx: &mut mpsc::Receiver<ClientCmd>) -> anyhow::Result<()> {
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut term = signal(SignalKind::terminate())?;
        let mut hup = signal(SignalKind::hangup())?;
        tracing::info!("omarchy-hotspot daemon started");
        loop {
            // В `select!` ждут только короткие безопасные ожидания; тела веток
            // выполняются целиком и прерваться не могут.
            tokio::select! {
                _ = ticker.tick() => self.tick().await,
                Some(cmd) = cmd_rx.recv() => {
                    let resp = self.handle(cmd.req).await;
                    let _ = cmd.reply.send(resp);
                }
                _ = term.recv() => break,
                _ = hup.recv() => self.reload_config(),
                _ = tokio::signal::ctrl_c() => break,
            }
        }
        Ok(())
    }

    async fn tick(&mut self) {
        if self.ticks.is_multiple_of(RELOAD_EVERY_TICKS) {
            self.reload_config();
        }
        if self.ticks.is_multiple_of(STATE_EVERY_TICKS) {
            let _ = self.poll_state().await;
            self.autostart_once().await;
        }
        self.poll_devices().await;
        if self.is_on() {
            self.on_ticks = self.on_ticks.saturating_add(1);
        }
        self.ensure_rules().await;
        self.reconcile().await;
        self.poll_leases().await;
        self.check_idle().await;
        self.check_timer().await;
        // Счётчик двигаем в конце: на самом первом тике опрашиваем всё.
        self.ticks = self.ticks.wrapping_add(1);
    }

    fn is_on(&self) -> bool {
        matches!(self.state, HotspotState::On { .. })
    }

    fn ap_iface(&self) -> Option<&str> {
        match &self.state {
            HotspotState::On { ap_iface, .. } => Some(ap_iface),
            _ => None,
        }
    }

    /// Интерфейс точки доступа для помощника; `None` — раздача выключена.
    fn ap_typed(&self) -> Option<Iface> {
        self.ap_iface().and_then(|s| Iface::parse(s).ok())
    }

    /// Режим одобрения действительно работает (правила с белым списком применены).
    fn approval_active(&self) -> bool {
        self.applied_rules.as_ref().is_some_and(|r| r.approval)
    }

    fn emit(&self, ev: Event) {
        // Ошибка означает только «никто не подписан».
        let _ = self.events.send(ev);
    }

    fn reload_config(&mut self) {
        match config::load(&self.cfg_path) {
            Ok(loaded) => self.set_cfg(loaded.config),
            Err(e) => tracing::warn!("cannot read config: {e}"),
        }
    }

    fn set_cfg(&mut self, cfg: Config) {
        self.notifier.set_enabled(cfg.access.notifications);
        self.cfg = cfg;
    }

    /// Состояние раздачи (`nmcli`, root не нужен).
    /// Перечитать состояние раздачи. `Err` — прочитать не удалось, старое состояние не менялось.
    async fn poll_state(&mut self) -> Result<(), CoreError> {
        let helper = self.helper.clone();
        let res =
            tokio::task::spawn_blocking(move || with_hotspot_helper(&*helper, |h| h.state())).await;
        match res {
            Ok(Ok(state)) => {
                self.set_state(state);
                Ok(())
            }
            Ok(Err(e)) => {
                tracing::debug!("cannot read hotspot state: {e}");
                Err(e)
            }
            Err(e) => {
                tracing::warn!("state task failed: {e}");
                Err(CoreError::Config(format!("state task failed: {e}")))
            }
        }
    }

    /// Подключённые устройства (`iw station dump` + таблица соседей, root не нужен).
    async fn poll_devices(&mut self) {
        let Some(ap) = self.ap_iface().map(str::to_string) else {
            return;
        };
        let leases = self.leases.clone();
        let res =
            tokio::task::spawn_blocking(move || omarchy_hotspot_core::devices::scan(&ap, &leases))
                .await;
        match res {
            Ok(Ok(devices)) => {
                self.devices = devices;
                self.scan_ok = true;
                self.scan_errors = 0;
            }
            // Прошлый список оставляем: иначе одна неудачная команда выглядела бы так,
            // будто все устройства разом отключились (и запустила бы авто-выключение).
            Ok(Err(e)) => {
                self.scan_errors = self.scan_errors.saturating_add(1);
                tracing::debug!("cannot read device list: {e}");
            }
            Err(e) => {
                self.scan_errors = self.scan_errors.saturating_add(1);
                tracing::warn!("device task failed: {e}");
            }
        }
    }

    fn set_state(&mut self, new: HotspotState) {
        let before = wire_state(&self.state);
        let after = wire_state(&new);
        let was_on = self.is_on();
        self.state = new;
        if before != after {
            self.emit(Event::StateChanged { state: after });
        }
        match (was_on, self.is_on()) {
            // Новая раздача: список устройств начинаем с чистого листа.
            (false, true) => {
                self.on_ticks = 0;
                self.clear_devices();
            }
            (true, false) => self.end_session(),
            _ => {}
        }
    }

    fn clear_devices(&mut self) {
        self.devices.clear();
        self.known.clear();
        self.kicked_at.clear();
        self.asked.clear();
        self.leases.clear();
        self.leases_at = None;
        self.idle_since = None;
        self.idle_warned = false;
        self.rules_retry_at = None;
        self.rules_attempts = 0;
        self.rules_giveup = false;
        self.scan_ok = false;
        self.scan_errors = 0;
        self.new_device_notifications = 0;
    }

    fn end_session(&mut self) {
        self.clear_devices();
        self.applied_rules = None;
        self.timer = None;
        self.timer_warned = false;
        self.on_ticks = 0;
        // Помощник с правами root нужен только во время раздачи. Закрываем его в отдельном
        // потоке: ожидание чужого процесса не должно останавливать главный цикл.
        let helper = self.helper.clone();
        tokio::task::spawn_blocking(move || helper.shutdown());
    }

    /// Раздача включена, а режим одобрения не тот, что нужен — применяем правила сами.
    /// Так неважно, кто включил раздачу: окно, CLI или панель.
    async fn ensure_rules(&mut self) {
        if !self.is_on() || self.on_ticks < RULES_AFTER_TICKS || self.rules_giveup {
            return;
        }
        if self.rules_retry_at.is_some_and(|t| Instant::now() < t) {
            return;
        }
        let want = RulesKey {
            approval: self.cfg.access.approval_required,
            guests_reach_pc: self.cfg.hotspot.guests_can_reach_pc,
            uplink: self.cfg.network.uplink_interface.trim().to_string(),
        };
        // Служба сама один раз применяет правила каждой раздачи: тот, кто её включил, мог
        // прочитать конфиг раньше. Дальше — только когда в настройках что-то поменялось.
        if self.applied_rules.as_ref() == Some(&want) {
            return;
        }
        let Some(ap) = self.ap_iface().map(str::to_string) else {
            return;
        };
        let state_uplink = match &self.state {
            HotspotState::On { uplink, .. } => uplink.clone(),
            _ => None,
        };
        let cfg = self.cfg.clone();
        let helper = self.helper.clone();
        let res = tokio::task::spawn_blocking(move || {
            let uplink = match cfg.network.uplink_interface.trim() {
                "" => state_uplink.or_else(|| uplink::detect_uplink().ok().flatten()),
                forced => Some(forced.to_string()),
            }
            .ok_or(CoreError::NoUplink)?;
            let req = hotspot::firewall_request(&cfg, &ap, &uplink, cfg.access.approval_required)?;
            helper.call(&req).map(|_| ())
        })
        .await;
        match res {
            Ok(Ok(())) => {
                tracing::info!(
                    approval = want.approval,
                    guests_reach_pc = want.guests_reach_pc,
                    "firewall rules applied by the service"
                );
                self.applied_rules = Some(want);
                self.rules_attempts = 0;
                self.rules_retry_at = None;
            }
            Ok(Err(e)) => {
                tracing::error!("cannot apply firewall rules: {e}");
                self.rules_failed(matches!(
                    e,
                    CoreError::HelperDenied | CoreError::HelperNotInstalled
                ));
            }
            Err(e) => {
                tracing::error!("firewall task failed: {e}");
                self.rules_failed(false);
            }
        }
    }

    /// Неудачная попытка применить правила: ждём дольше, а без помощника — больше не пробуем.
    fn rules_failed(&mut self, hopeless: bool) {
        if hopeless {
            self.rules_giveup = true;
            tracing::error!("firewall rules cannot be applied: the root helper is not available");
            return;
        }
        let idx = self.rules_attempts.min(RULES_RETRY_SECS.len() - 1);
        self.rules_retry_at = Some(Instant::now() + Duration::from_secs(RULES_RETRY_SECS[idx]));
        self.rules_attempts += 1;
    }

    /// Кто пришёл, кто ушёл, кому нужен допуск. Здесь же уведомления.
    async fn reconcile(&mut self) {
        let lists = AccessLists::from_config(&self.cfg, self.approval_active());
        lists.annotate(&mut self.devices);
        let (gone, fresh) = diff_devices(&self.known, &self.devices);

        for (mac, name) in gone {
            self.known.remove(&mac);
            self.kicked_at.remove(&mac);
            self.asked.retain(|m| m != &mac);
            self.emit(Event::DeviceLeft { mac });
            self.notifier
                .send(Urgency::Low, t(self.lang, Msg::NotifyLeft), &name);
        }

        for d in fresh {
            self.known.insert(d.mac.clone(), d.display_name());
            // Про ожидающих спрашиваем ниже: устройство может стать «ожидающим» и позже,
            // когда служба успеет применить правила с белым списком.
            if d.status == DeviceStatus::Allowed {
                let name = d.display_name();
                self.emit(Event::DeviceJoined { device: d });
                self.notifier
                    .send(Urgency::Low, t(self.lang, Msg::NotifyJoined), &name);
            }
        }

        let waiting: Vec<Device> = self
            .devices
            .iter()
            .filter(|d| d.status == DeviceStatus::Pending && !self.asked.contains(&d.mac))
            .cloned()
            .collect();
        for d in waiting {
            if self.asked.len() >= MAX_ASKED {
                tracing::warn!("too many new devices: stopped asking about them");
                break;
            }
            self.asked.push(d.mac.clone());
            let name = d.display_name();
            self.emit(Event::DevicePending { device: d });
            // Уведомлений за сеанс — не больше предела: вопрос в окне всё равно останется.
            if self.new_device_notifications < MAX_NEW_DEVICE_NOTIFICATIONS {
                self.new_device_notifications += 1;
                self.notifier.send(
                    Urgency::Normal,
                    t(self.lang, Msg::NotifyNewDevice),
                    &tf(self.lang, Msg::NotifyNewDeviceBodyFmt, &[&name]),
                );
            }
        }
        self.kick_blocked().await;
    }

    /// Устройство из чёрного списка отключаем от сети (доступ ему и так закрыт правилом).
    async fn kick_blocked(&mut self) {
        let Some(ap) = self.ap_typed() else { return };
        let now = Instant::now();
        let macs: Vec<Mac> = self
            .devices
            .iter()
            .filter(|d| d.status == DeviceStatus::Blocked)
            .filter(|d| {
                self.kicked_at
                    .get(&d.mac)
                    .is_none_or(|t| now.duration_since(*t) >= KICK_COOLDOWN)
            })
            .map(|d| d.mac.clone())
            .collect();
        for mac in macs {
            self.kicked_at.insert(mac.clone(), now);
            let helper = self.helper.clone();
            let req = HelperRequest::StationKick {
                ap: ap.clone(),
                mac,
            };
            let res = tokio::task::spawn_blocking(move || helper.call(&req)).await;
            if let Ok(Err(e)) = res {
                tracing::debug!("cannot kick device: {e}");
            }
        }
    }

    /// Аренды dnsmasq — только ради имён устройств; читает их помощник.
    async fn poll_leases(&mut self) {
        let Some(ap) = self.ap_iface().map(str::to_string) else {
            return;
        };
        if self.devices.is_empty() || !self.helper.installed() {
            return;
        }
        let waiting_for_name = self
            .devices
            .iter()
            .any(|d| d.hostname.is_none() && d.connected_secs < NAME_WAIT_SECS);
        let interval = if waiting_for_name {
            LEASES_EVERY
        } else {
            LEASES_SLOW
        };
        if self.leases_at.is_some_and(|at| at.elapsed() < interval) {
            return;
        }
        self.leases_at = Some(Instant::now());
        let helper = self.helper.clone();
        let res =
            tokio::task::spawn_blocking(move || with_hotspot_helper(&*helper, |h| h.leases(&ap)))
                .await;
        match res {
            Ok(Ok(leases)) => self.leases = leases,
            Ok(Err(e)) => tracing::debug!("cannot read dnsmasq leases: {e}"),
            Err(e) => tracing::warn!("leases task failed: {e}"),
        }
    }

    /// Никого нет дольше, чем разрешено, — выключаем раздачу.
    async fn check_idle(&mut self) {
        let minutes = self.cfg.automation.idle_off_minutes;
        if minutes == 0 || !self.is_on() || !self.devices.is_empty() {
            self.idle_since = None;
            self.idle_warned = false;
            return;
        }
        // Пока список устройств не читается, отсчёт простоя не идёт: пустой список может
        // означать не «никого нет», а сломанный опрос — выключать раздачу под гостями нельзя.
        if !self.scan_ok || self.scan_errors > 0 {
            self.idle_since = None;
            self.idle_warned = false;
            return;
        }
        let since = *self.idle_since.get_or_insert_with(Instant::now);
        let limit = Duration::from_secs(u64::from(minutes) * 60);
        let elapsed = since.elapsed();
        if elapsed >= limit {
            self.stop_by_idle(minutes).await;
            return;
        }
        let left = limit - elapsed;
        if !self.idle_warned && left <= Duration::from_secs(u64::from(IDLE_WARN_MINUTES) * 60) {
            self.idle_warned = true;
            self.emit(Event::IdleWarning {
                minutes_left: IDLE_WARN_MINUTES,
            });
        }
    }

    /// Таймер раздачи: выключаем через заданное число минут после включения. Если таймер
    /// поменяли во время раздачи, отсчёт идёт от момента изменения — иначе новая настройка
    /// могла бы выключить давно работающую раздачу в ту же секунду.
    async fn check_timer(&mut self) {
        let minutes = self.cfg.automation.timer_minutes;
        let since = match &self.state {
            HotspotState::On { since, .. } => *since,
            _ => None,
        };
        if !self.is_on() {
            self.timer = None;
            self.timer_warned = false;
            return;
        }
        if minutes == 0 {
            // Запоминаем «таймера нет»: если его включат во время раздачи, отсчёт пойдёт
            // от момента включения таймера, а не от начала раздачи.
            self.timer = Some((0, SystemTime::now()));
            self.timer_warned = false;
            return;
        }
        let now = SystemTime::now();
        let base = timer_base(self.timer, minutes, since, now);
        if self.timer.is_some_and(|(m, _)| m != minutes) {
            self.timer_warned = false;
        }
        self.timer = Some((minutes, base));
        let limit = Duration::from_secs(u64::from(minutes) * 60);
        let elapsed = now.duration_since(base).unwrap_or_default();
        if elapsed >= limit {
            self.stop_by_timer(minutes).await;
            return;
        }
        let left = limit - elapsed;
        if !self.timer_warned && left <= Duration::from_secs(u64::from(TIMER_WARN_MINUTES) * 60) {
            self.timer_warned = true;
            self.emit(Event::TimerWarning {
                minutes_left: TIMER_WARN_MINUTES,
            });
        }
    }

    async fn stop_by_timer(&mut self, minutes: u32) {
        tracing::info!(minutes, "stopping the hotspot: timer");
        match self.stop_hotspot().await {
            Ok(_) => {
                self.emit(Event::Stopped {
                    reason: "timer".into(),
                });
                self.notifier.send(
                    Urgency::Normal,
                    t(self.lang, Msg::NotifyIdleOff),
                    &tf(
                        self.lang,
                        Msg::NotifyTimerOffBodyFmt,
                        &[&minutes.to_string()],
                    ),
                );
            }
            Err(e) => tracing::error!("cannot stop the hotspot: {e}"),
        }
        self.timer = None;
        self.timer_warned = false;
    }

    /// Автозапуск при входе: служба стартует вместе с сеансом (её включает настройка
    /// `autostart_on_login`) и один раз за вход включает раздачу. Метка в `$XDG_RUNTIME_DIR`
    /// живёт до выхода из сеанса, поэтому перезапуск службы (или `on` после `off`)
    /// раздачу сам не включает.
    async fn autostart_once(&mut self) {
        if self.autostart_checked {
            return;
        }
        self.autostart_checked = true;
        let Some(marker) = ipc::runtime_dir().map(|d| d.join("autostart-done")) else {
            return;
        };
        let first_in_session = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
            .is_ok();
        if !first_in_session || !self.cfg.automation.autostart_on_login {
            return;
        }
        if !matches!(self.state, HotspotState::Off) {
            return;
        }
        // Без помощника раздача включилась бы без брандмауэра — при входе молча так не делаем.
        if !self.helper.installed() {
            tracing::warn!("autostart skipped: the root helper is not installed");
            self.notifier.send(
                Urgency::Normal,
                t(self.lang, Msg::NotifyAutostartFailed),
                t(self.lang, Msg::ErrHelperNotInstalled),
            );
            return;
        }
        // Wi-Fi-адаптер занят обычным подключением — не обрываем его молча при входе.
        let cfg = self.cfg.clone();
        let busy = tokio::task::spawn_blocking(move || {
            with_hotspot(|h| h.client_connection(&cfg)).unwrap_or(None)
        })
        .await
        .unwrap_or(None);
        if let Some(network) = busy {
            tracing::info!(%network, "autostart skipped: Wi-Fi adapter is connected to a network");
            self.notifier.send(
                Urgency::Normal,
                t(self.lang, Msg::NotifyAutostartSkipped),
                &tf(self.lang, Msg::NotifyAutostartSkippedBodyFmt, &[&network]),
            );
            return;
        }
        tracing::info!("autostart on login");
        if let Response::Err { msg, .. } = self.do_start().await {
            tracing::error!("autostart failed: {msg}");
            self.notifier.send(
                Urgency::Normal,
                t(self.lang, Msg::NotifyAutostartFailed),
                &msg,
            );
        }
        let _ = self.poll_state().await;
    }

    async fn stop_by_idle(&mut self, minutes: u32) {
        tracing::info!(minutes, "stopping the hotspot: nobody connected");
        match self.stop_hotspot().await {
            Ok(_) => {
                self.emit(Event::Stopped {
                    reason: "idle".into(),
                });
                self.notifier.send(
                    Urgency::Normal,
                    t(self.lang, Msg::NotifyIdleOff),
                    &tf(
                        self.lang,
                        Msg::NotifyIdleOffBodyFmt,
                        &[&minutes.to_string()],
                    ),
                );
            }
            Err(e) => tracing::error!("cannot stop the hotspot: {e}"),
        }
        self.idle_since = None;
        self.idle_warned = false;
    }

    async fn stop_hotspot(&mut self) -> Result<bool, CoreError> {
        let helper = self.helper.clone();
        let res = tokio::task::spawn_blocking(move || with_hotspot_helper(&*helper, |h| h.stop()))
            .await
            .map_err(|e| CoreError::Config(format!("stop task failed: {e}")))?;
        if res.is_ok() {
            self.set_state(HotspotState::Off);
        }
        res
    }

    async fn handle(&mut self, req: Request) -> Response {
        match req {
            Request::Ping => Response::Pong,
            Request::GetState => Response::State {
                state: wire_state(&self.state),
                uplink: match &self.state {
                    HotspotState::On { uplink, .. } => uplink.clone(),
                    _ => None,
                },
                since: match &self.state {
                    HotspotState::On { since, .. } => since.and_then(unix_secs),
                    _ => None,
                },
                devices_count: self.devices.len(),
                approval: self.approval_active(),
            },
            Request::GetDevices => Response::Devices {
                list: self.devices.clone(),
            },
            Request::Start => self.do_start().await,
            Request::Stop => match self.stop_hotspot().await {
                Ok(_) => Response::Ok,
                Err(e) => Response::err(err_code::FAILED, e.user_message(self.lang)),
            },
            Request::Approve { mac } => self.change(AccessChange::Allow(mac)).await,
            Request::Deny { mac } | Request::Block { mac } => {
                self.change(AccessChange::Block(mac)).await
            }
            Request::Unblock { mac } => self.change(AccessChange::Unblock(mac)).await,
            Request::Kick { mac } => self.do_kick(mac).await,
            Request::ReloadConfig => {
                self.reload_config();
                Response::Ok
            }
            // Подписку обрабатывает сам обработчик клиента.
            Request::Subscribe => Response::Ok,
        }
    }

    async fn do_start(&mut self) -> Response {
        let cfg = self.cfg.clone();
        let approval = cfg.access.approval_required;
        let guests = cfg.hotspot.guests_can_reach_pc;
        let uplink = cfg.network.uplink_interface.trim().to_string();
        let helper = self.helper.clone();
        let res = tokio::task::spawn_blocking(move || {
            with_hotspot_helper(&*helper, |h| h.start(&cfg, approval))
        })
        .await;
        match res {
            Ok(Ok(_)) => {
                self.applied_rules = Some(RulesKey {
                    approval,
                    guests_reach_pc: guests,
                    uplink,
                });
                Response::Ok
            }
            Ok(Err(e)) => Response::err(err_code::FAILED, e.user_message(self.lang)),
            Err(e) => Response::err(err_code::FAILED, format!("start task failed: {e}")),
        }
    }

    async fn do_kick(&mut self, mac: Mac) -> Response {
        let Some(ap) = self.ap_typed() else {
            return Response::err(err_code::FAILED, "hotspot is off");
        };
        let helper = self.helper.clone();
        let req = HelperRequest::StationKick { ap, mac };
        match tokio::task::spawn_blocking(move || helper.call(&req)).await {
            Ok(Ok(_)) => Response::Ok,
            Ok(Err(e)) => Response::err(err_code::FAILED, e.user_message(self.lang)),
            Err(e) => Response::err(err_code::FAILED, format!("kick task failed: {e}")),
        }
    }

    /// Разрешить, заблокировать или разблокировать устройство: брандмауэр + конфиг.
    async fn change(&mut self, change: AccessChange) -> Response {
        // Состояние раздачи опрашивается раз в две секунды и могло устареть. Если по нашим
        // данным раздача выключена, проверяем ещё раз: иначе правило не применилось бы,
        // а пользователю ушло бы «готово».
        if self.ap_typed().is_none()
            && let Err(e) = self.poll_state().await
        {
            return Response::err(err_code::FAILED, e.user_message(self.lang));
        }
        // Раздача включается: правило ставить ещё некуда, а «готово» было бы неправдой.
        if matches!(self.state, HotspotState::Starting) {
            return Response::err(err_code::FAILED, t(self.lang, Msg::ErrStartingRetry));
        }
        let mac = change.mac().clone();
        let ap = self.ap_typed();
        let helper = self.helper.clone();
        let path = self.cfg_path.clone();
        let ch = change.clone();
        // Конфиг читается и пишется под замком, а правило брандмауэра ставится внутри него:
        // либо изменилось и то и другое, либо ничего.
        let res = tokio::task::spawn_blocking(move || {
            config::update(&path, |cfg| access::apply(&*helper, ap.as_ref(), cfg, &ch))
        })
        .await;
        match res {
            Ok(Ok((_changed, cfg))) => {
                self.set_cfg(cfg);
                self.emit(match change {
                    AccessChange::Allow(_) => Event::DeviceApproved { mac: mac.clone() },
                    AccessChange::Block(_) => Event::DeviceBlocked { mac: mac.clone() },
                    AccessChange::Unblock(_) => Event::DeviceUnblocked { mac: mac.clone() },
                });
                // Сразу пересчитываем состояния, чтобы окно увидело ответ без задержки.
                let lists = AccessLists::from_config(&self.cfg, self.approval_active());
                lists.annotate(&mut self.devices);
                if matches!(change, AccessChange::Block(_)) {
                    self.kicked_at.insert(mac, Instant::now());
                }
                Response::Ok
            }
            Ok(Err(e)) => Response::err(err_code::FAILED, e.user_message(self.lang)),
            Err(e) => Response::err(err_code::FAILED, format!("access task failed: {e}")),
        }
    }
}

/// Кто отключился (MAC и имя для уведомления) и кто появился с прошлого раза.
fn diff_devices(
    known: &HashMap<Mac, String>,
    devices: &[Device],
) -> (Vec<(Mac, String)>, Vec<Device>) {
    let gone = known
        .iter()
        .filter(|(mac, _)| !devices.iter().any(|d| &d.mac == *mac))
        .map(|(mac, name)| (mac.clone(), name.clone()))
        .collect();
    let fresh = devices
        .iter()
        .filter(|d| !known.contains_key(&d.mac))
        .cloned()
        .collect();
    (gone, fresh)
}

/// От какого момента считать таймер раздачи: первый раз — от времени включения,
/// после смены настройки — от «сейчас», иначе — как раньше.
fn timer_base(
    prev: Option<(u32, SystemTime)>,
    minutes: u32,
    since: Option<SystemTime>,
    now: SystemTime,
) -> SystemTime {
    match prev {
        Some((m, base)) if m == minutes => base,
        Some(_) => now,
        None => since.unwrap_or(now),
    }
}

fn wire_state(state: &HotspotState) -> WireState {
    match state {
        HotspotState::Off => WireState::Off,
        HotspotState::Starting => WireState::Starting,
        HotspotState::On { .. } => WireState::On,
        HotspotState::Error(_) => WireState::Error,
    }
}

fn unix_secs(t: SystemTime) -> Option<u64> {
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            connected_secs: 5,
            status: DeviceStatus::Allowed,
        }
    }

    #[test]
    fn diff_finds_new_and_gone() {
        let mut known = HashMap::new();
        known.insert(mac("01"), "Phone-01".to_string());
        let devices = [device("02")];
        let (gone, fresh) = diff_devices(&known, &devices);
        assert_eq!(gone, [(mac("01"), "Phone-01".to_string())]);
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].mac, mac("02"));
        // Тот же список второй раз — изменений нет.
        known.clear();
        known.insert(mac("02"), "Phone-02".to_string());
        let (gone, fresh) = diff_devices(&known, &devices);
        assert!(gone.is_empty() && fresh.is_empty());
    }

    #[test]
    fn timer_counts_from_start_or_from_the_change() {
        let start = UNIX_EPOCH + Duration::from_secs(1_000);
        let now = UNIX_EPOCH + Duration::from_secs(10_000);
        // Раздача включена давно, службу подняли сейчас — считаем от включения.
        assert_eq!(timer_base(None, 30, Some(start), now), start);
        assert_eq!(timer_base(None, 30, None, now), now);
        // Та же настройка — отсчёт не сдвигается.
        assert_eq!(timer_base(Some((30, start)), 30, Some(start), now), start);
        // Таймер поменяли во время раздачи — от момента изменения, а не выключить сразу.
        assert_eq!(timer_base(Some((30, start)), 60, Some(start), now), now);
        // Раздача шла без таймера, и его включили — тоже от «сейчас».
        assert_eq!(timer_base(Some((0, start)), 30, Some(start), now), now);
    }

    #[test]
    fn wire_states_match_hotspot_states() {
        assert_eq!(wire_state(&HotspotState::Off), WireState::Off);
        assert_eq!(wire_state(&HotspotState::Starting), WireState::Starting);
        assert_eq!(
            wire_state(&HotspotState::On {
                since: None,
                ap_iface: "wlp15s0".into(),
                uplink: None,
            }),
            WireState::On
        );
        assert_eq!(
            wire_state(&HotspotState::Error("x".into())),
            WireState::Error
        );
    }
}
