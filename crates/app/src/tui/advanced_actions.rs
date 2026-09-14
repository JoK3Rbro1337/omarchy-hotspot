//! Продвинутый режим: клавиши и действия (сохранение, устройства, журнал, проверка, сброс).
//! Всё долгое — в фоне через `spawn_bg`, как и в простом режиме.

use std::process::{Command, Stdio};

use crossterm::event::KeyCode;
use omarchy_hotspot_core::access::{self, AccessChange};
use omarchy_hotspot_core::backend::HotspotState;
use omarchy_hotspot_core::config::{self, Config};
use omarchy_hotspot_core::devices::{Device, DeviceStatus};
use omarchy_hotspot_core::doctor;
use omarchy_hotspot_core::helper::{HelperRunner, PkexecHelper};
use omarchy_hotspot_core::helper_proto::{HelperRequest, Iface, Mac};
use omarchy_hotspot_core::hotspot::{self, StartOutcome};
use omarchy_hotspot_core::ipc::{self, Request};
use omarchy_hotspot_core::{CoreError, Lang, Msg, t, tf};
use tokio::sync::mpsc::UnboundedSender;

use crate::daemon::control;
use crate::hotspot_ctx::with_hotspot;

use super::events::Event;
use super::settings::{self, Env, ExpertOutput, Focus, Item, Kind, Section, TextInput};
use super::state::{AppState, BgResponse, Busy, Confirm, EditField, request_refresh, spawn_bg};

/// Сколько строк журнала берём у каждого источника.
const LOG_LINES: &str = "150";
/// Предел длины поля ввода (подсеть, DNS, страна — всё короткое).
const INPUT_MAX_CHARS: usize = 80;

/// Строка списка в разделе «Устройства»: подключённое устройство или запись чёрного списка.
pub struct DeviceRow<'a> {
    pub mac: Mac,
    pub device: Option<&'a Device>,
    pub status: DeviceStatus,
}

/// Итог сохранения настроек.
pub struct Saved {
    pub cfg: Config,
    pub restarted: bool,
    /// Предупреждение вместо «сохранено» (например, одобрение без службы).
    pub warning: Option<Msg>,
}

impl AppState {
    /// Клавиша в продвинутом режиме. `false` — не наша, её обработают общие правила
    /// (Space, p, e, Tab, выход).
    pub fn advanced_key(
        &mut self,
        code: KeyCode,
        hot: Option<char>,
        tx: &UnboundedSender<Event>,
    ) -> bool {
        if self.adv.input.is_some() {
            self.input_key(code);
            return true;
        }
        let section = self.adv.section;
        let focus = self.adv.focus;
        match code {
            KeyCode::Up | KeyCode::Down => {
                let down = code == KeyCode::Down;
                match (focus, section) {
                    (Focus::Sections, _) => {
                        self.adv.move_section(down);
                        self.adv.output = ExpertOutput::None;
                    }
                    (Focus::Items, Section::Devices) => self.move_device(down),
                    (Focus::Items, _) => self.adv.move_item(down),
                }
                true
            }
            KeyCode::Right | KeyCode::Enter if focus == Focus::Sections => {
                self.adv.focus = Focus::Items;
                self.adv.item_idx = 0;
                true
            }
            KeyCode::Esc if focus == Focus::Items => {
                self.adv.focus = Focus::Sections;
                true
            }
            KeyCode::Left if focus == Focus::Items => {
                match self.adv.item() {
                    Some(item) if changes_with_arrows(item) => self.step(item, false),
                    _ => self.adv.focus = Focus::Sections,
                }
                true
            }
            KeyCode::Right if focus == Focus::Items => {
                if let Some(item) = self.adv.item().filter(|i| changes_with_arrows(*i)) {
                    self.step(item, true);
                }
                true
            }
            KeyCode::Enter if section == Section::Devices => {
                self.device_action(DeviceAction::Allow, tx);
                true
            }
            KeyCode::Enter => {
                if let Some(item) = self.adv.item() {
                    self.enter_item(item, tx);
                }
                true
            }
            KeyCode::PageUp => {
                self.adv.log_scroll = self.adv.log_scroll.saturating_add(10);
                true
            }
            KeyCode::PageDown => {
                self.adv.log_scroll = self.adv.log_scroll.saturating_sub(10);
                true
            }
            KeyCode::Char(_) => match hot {
                Some('s') => {
                    self.adv_save(tx);
                    true
                }
                Some('g') if self.adv.item() == Some(Item::Password) => {
                    if self.busy.is_none() {
                        self.confirm = Some(Confirm::GeneratePassword);
                    }
                    true
                }
                Some(c @ ('b' | 'k' | 'a' | 'o'))
                    if section == Section::Devices && focus == Focus::Items =>
                {
                    let action = match c {
                        'b' => DeviceAction::ToggleBlock,
                        'k' => DeviceAction::Kick,
                        'a' => DeviceAction::Allow,
                        _ => DeviceAction::ToggleApproval,
                    };
                    self.device_action(action, tx);
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn input_key(&mut self, code: KeyCode) {
        let Some(input) = &mut self.adv.input else {
            return;
        };
        match code {
            KeyCode::Esc => self.adv.input = None,
            KeyCode::Enter => {
                let item = input.item;
                let value = input.value.clone();
                match settings::set_text(&mut self.adv.draft, item, &value, self.lang) {
                    Ok(()) => self.adv.input = None,
                    Err(e) => input.error = Some(e),
                }
            }
            KeyCode::Backspace => {
                input.value.pop();
                input.error = None;
            }
            KeyCode::Char(c)
                if !c.is_control() && input.value.chars().count() < INPUT_MAX_CHARS =>
            {
                input.value.push(c);
                input.error = None;
            }
            _ => {}
        }
    }

    fn step(&mut self, item: Item, forward: bool) {
        settings::step(&mut self.adv.draft, item, forward, &self.adv.env);
    }

    fn enter_item(&mut self, item: Item, tx: &UnboundedSender<Event>) {
        match item.kind() {
            Kind::Toggle | Kind::Choice => self.step(item, true),
            Kind::ChoiceOrText => {
                self.adv.input = Some(TextInput {
                    item,
                    value: settings::text_value(&self.adv.draft, item),
                    error: None,
                });
            }
            Kind::ReadOnly => {}
            Kind::Action => match item {
                Item::Blacklist => {
                    self.adv.section = Section::Devices;
                    self.adv.focus = Focus::Items;
                    self.adv.device_idx = self.devices.len();
                }
                Item::Password => {
                    self.start_edit();
                    if let Some(e) = &mut self.editing {
                        e.field = EditField::Password;
                    }
                }
                Item::Log => self.load_log(tx),
                Item::Doctor => self.run_doctor(tx),
                Item::Reset if self.busy.is_none() => self.confirm = Some(Confirm::Reset),
                _ => {}
            },
        }
    }

    /// Вход в продвинутый режим: узнать каналы карты и доступные подключения.
    pub fn on_enter_advanced(&mut self, tx: &UnboundedSender<Event>) {
        let cfg = self.cfg.clone();
        spawn_bg(tx, move || {
            let env = with_hotspot(|h| Env {
                caps: h.wifi_caps(&cfg).ok().map(|(_, caps)| caps),
                uplinks: h.uplink_candidates().unwrap_or_default(),
            });
            BgResponse::AdvEnv(env)
        });
        // Индикатору силы пароля нужен сам пароль (он и так загружается для QR).
        if self.password.is_none() && !self.password_absent {
            self.load_password(tx);
        }
    }

    // ---- Сохранение ----

    /// `s`: проверить черновик и сохранить. Если нужен перезапуск включённой раздачи — спросить.
    pub fn adv_save(&mut self, tx: &UnboundedSender<Event>) {
        if self.busy.is_some() {
            return;
        }
        if !settings::any_dirty(&self.adv.draft, &self.cfg) {
            if std::mem::take(&mut self.adv.quit_after_save) {
                self.should_quit = true;
            } else {
                self.notice = Some(t(self.lang, Msg::AdvNothingToSave).to_string());
            }
            return;
        }
        if let Err(e) = settings::validate(&self.adv.draft, &self.adv.env) {
            self.error = Some(e.user_message(self.lang));
            self.adv.quit_after_save = false;
            return;
        }
        let restart = self.is_on() && settings::restart_needed(&self.adv.draft, &self.cfg);
        if restart && !self.adv.quit_after_save {
            self.confirm = Some(Confirm::SaveRestart);
            return;
        }
        self.adv_save_now(tx);
    }

    pub fn adv_save_now(&mut self, tx: &UnboundedSender<Event>) {
        self.busy = Some(Busy::Applying);
        self.error = None;
        self.notice = None;
        let job = SaveJob {
            lang: self.lang,
            draft: self.adv.draft.clone(),
            old: self.cfg.clone(),
            path: self.cfg_path.clone(),
            state: self.status.state.clone(),
        };
        spawn_bg(tx, move || BgResponse::AdvSaved(job.run()));
    }

    pub fn on_adv_saved(&mut self, result: Result<Saved, String>, tx: &UnboundedSender<Event>) {
        self.busy = None;
        match result {
            Ok(saved) => {
                self.adv.draft = saved.cfg.clone();
                self.replace_cfg(saved.cfg);
                let msg = if saved.restarted {
                    Msg::AdvSavedRestarted
                } else {
                    Msg::AdvSaved
                };
                self.notice = Some(t(self.lang, saved.warning.unwrap_or(msg)).to_string());
                if saved.restarted {
                    self.after_restart(tx);
                }
                if std::mem::take(&mut self.adv.quit_after_save) {
                    self.should_quit = true;
                }
            }
            Err(e) => {
                self.error = Some(e);
                self.adv.quit_after_save = false;
            }
        }
        request_refresh(tx, self.cfg.clone());
    }

    // ---- Устройства ----

    /// Строки раздела «Устройства»: сначала подключённые, потом чёрный список не в сети.
    pub fn device_rows(&self) -> Vec<DeviceRow<'_>> {
        let mut rows: Vec<DeviceRow> = self
            .devices
            .iter()
            .map(|d| DeviceRow {
                mac: d.mac.clone(),
                device: Some(d),
                status: if self
                    .cfg
                    .access
                    .blocked_macs
                    .iter()
                    .any(|m| m == d.mac.as_str())
                {
                    DeviceStatus::Blocked
                } else {
                    d.status
                },
            })
            .collect();
        for m in &self.cfg.access.blocked_macs {
            let Ok(mac) = Mac::parse(m) else { continue };
            if !rows.iter().any(|r| r.mac == mac) {
                rows.push(DeviceRow {
                    mac,
                    device: None,
                    status: DeviceStatus::Blocked,
                });
            }
        }
        rows
    }

    fn move_device(&mut self, down: bool) {
        let n = self.device_rows().len();
        if n == 0 {
            self.adv.device_idx = 0;
            return;
        }
        let cur = self.adv.device_idx.min(n - 1);
        self.adv.device_idx = if down {
            (cur + 1) % n
        } else {
            (cur + n - 1) % n
        };
    }

    /// Выбранная строка (индекс не выходит за список, даже если устройство ушло).
    pub fn selected_device_idx(&self) -> Option<usize> {
        let n = self.device_rows().len();
        (n > 0).then(|| self.adv.device_idx.min(n - 1))
    }

    fn device_action(&mut self, action: DeviceAction, tx: &UnboundedSender<Event>) {
        if action == DeviceAction::ToggleApproval {
            self.toggle_approval_now();
            return;
        }
        let Some(idx) = self.selected_device_idx() else {
            return;
        };
        let (mac, status, connected) = {
            let row = &self.device_rows()[idx];
            (row.mac.clone(), row.status, row.device.is_some())
        };
        let change = match action {
            DeviceAction::ToggleBlock if status == DeviceStatus::Blocked => {
                AccessChange::Unblock(mac.clone())
            }
            DeviceAction::ToggleBlock => AccessChange::Block(mac.clone()),
            DeviceAction::Allow if status == DeviceStatus::Allowed => return,
            DeviceAction::Allow => AccessChange::Allow(mac.clone()),
            DeviceAction::Kick => {
                if connected {
                    self.kick(mac, tx);
                }
                return;
            }
            DeviceAction::ToggleApproval => return,
        };
        self.error = None;
        self.notice = None;
        let done = tf(self.lang, done_msg(&change), &[mac.as_str()]);
        if self.daemon_connected() {
            let req = match &change {
                AccessChange::Allow(m) => Request::Approve { mac: m.clone() },
                AccessChange::Block(m) => Request::Block { mac: m.clone() },
                AccessChange::Unblock(m) => Request::Unblock { mac: m.clone() },
            };
            self.daemon_send_then(&req, Some(done));
            return;
        }
        // Службы нет — меняем сами: правило брандмауэра и конфиг вместе, под замком.
        let path = self.cfg_path.clone();
        let ap = self.ap_iface().and_then(|s| Iface::parse(s).ok());
        let lang = self.lang;
        spawn_bg(tx, move || {
            let result = config::update(&path, |cfg| {
                access::apply(&PkexecHelper, ap.as_ref(), cfg, &change)
            });
            let result = result.map(|(_, cfg)| (cfg, done));
            BgResponse::AccessChanged(result.map_err(|e| e.user_message(lang)))
        });
    }

    fn kick(&mut self, mac: Mac, tx: &UnboundedSender<Event>) {
        self.error = None;
        self.notice = None;
        let done = tf(self.lang, Msg::AdvKickedFmt, &[mac.as_str()]);
        if self.daemon_connected() {
            self.daemon_send_then(&Request::Kick { mac }, Some(done));
            return;
        }
        let Some(ap) = self.ap_iface().and_then(|s| Iface::parse(s).ok()) else {
            return;
        };
        let lang = self.lang;
        spawn_bg(tx, move || {
            let result = PkexecHelper
                .call(&HelperRequest::StationKick { ap, mac })
                .map(|_| done)
                .map_err(|e| e.user_message(lang));
            BgResponse::Kicked(result)
        });
    }

    /// `o` в «Устройствах»: одобрение новых устройств включается сразу, без `s`.
    fn toggle_approval_now(&mut self) {
        let on = !self.cfg.access.approval_required;
        let result = config::update(&self.cfg_path, |cfg| {
            cfg.access.approval_required = on;
            Ok(())
        });
        match result {
            Ok((_, cfg)) => {
                self.replace_cfg(cfg);
                // Служба перечитает настройку и сама поправит правила брандмауэра.
                self.daemon_send(&Request::ReloadConfig);
                let msg = if !on {
                    Msg::AccessApprovalOff
                } else if self.is_on() && !self.daemon_connected() {
                    // Без службы белый список не применится — не пишем «включено».
                    Msg::NoticeApprovalNoDaemon
                } else {
                    Msg::AccessApprovalOn
                };
                self.notice = Some(t(self.lang, msg).to_string());
            }
            Err(e) => self.error = Some(e.user_message(self.lang)),
        }
    }

    pub fn on_access_changed(&mut self, result: Result<(Config, String), String>) {
        match result {
            Ok((cfg, done)) => {
                self.replace_cfg(cfg);
                self.notice = Some(done);
            }
            Err(e) => {
                self.notice = None;
                self.error = Some(e);
            }
        }
    }

    // ---- Эксперт ----

    fn load_log(&mut self, tx: &UnboundedSender<Event>) {
        self.adv.output = ExpertOutput::Loading;
        self.adv.log_scroll = 0;
        let lang = self.lang;
        spawn_bg(tx, move || BgResponse::LogLoaded(read_log(lang)));
    }

    fn run_doctor(&mut self, tx: &UnboundedSender<Event>) {
        self.adv.output = ExpertOutput::Loading;
        let lang = self.lang;
        spawn_bg(tx, move || BgResponse::DoctorDone(doctor::run_all(lang)));
    }

    /// Сброс (после вопроса): то же, что `omarchy-hotspot reset --yes`.
    pub fn reset_now(&mut self, tx: &UnboundedSender<Event>) {
        self.busy = Some(Busy::TurningOff);
        self.error = None;
        self.notice = None;
        let lang = self.lang;
        spawn_bg(tx, move || {
            // Сначала служба: иначе она пересоздаст удалённый конфиг из памяти.
            control::stop();
            control::set_autostart(false);
            let mut result = with_hotspot(|h| h.reset());
            // Настройки и запомненный размер окна (и его правило в памяти Hyprland).
            super::forget_window_size();
            for dir in [config::config_dir(), super::state_dir()]
                .into_iter()
                .flatten()
            {
                match std::fs::remove_dir_all(&dir) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => result = result.and(Err(e.into())),
                }
            }
            BgResponse::ResetDone(result.map_err(|e| e.user_message(lang)))
        });
    }

    pub fn on_reset_done(&mut self, result: Result<(), String>, tx: &UnboundedSender<Event>) {
        self.busy = None;
        match result {
            Ok(()) => {
                self.replace_cfg(Config::default());
                self.adv.draft = self.cfg.clone();
                self.clear_session();
                self.password = None;
                self.password_absent = true;
                self.show_password = false;
                self.qr = None;
                self.notice = Some(t(self.lang, Msg::ResetDone).to_string());
            }
            Err(e) => self.error = Some(e),
        }
        request_refresh(tx, self.cfg.clone());
    }
}

/// ←/→ меняют значение (иначе ← возвращает к списку разделов).
fn changes_with_arrows(item: Item) -> bool {
    matches!(
        item.kind(),
        Kind::Choice | Kind::Toggle | Kind::ChoiceOrText
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceAction {
    ToggleBlock,
    Allow,
    Kick,
    ToggleApproval,
}

fn done_msg(change: &AccessChange) -> Msg {
    match change {
        AccessChange::Allow(_) => Msg::AccessDoneAllowFmt,
        AccessChange::Block(_) => Msg::AccessDoneBlockFmt,
        AccessChange::Unblock(_) => Msg::AccessDoneUnblockFmt,
    }
}

/// Всё, что нужно фоновой задаче сохранения.
struct SaveJob {
    lang: Lang,
    draft: Config,
    old: Config,
    path: std::path::PathBuf,
    state: HotspotState,
}

impl SaveJob {
    /// Порядок как у `set` в CLI: сначала проверить и применить, потом записать конфиг —
    /// неприменимая настройка не попадает в файл. Исключение — перезапуск: конфиг пишется
    /// между выключением и включением, чтобы служба применила правила новой раздачи уже по нему.
    fn run(self) -> Result<Saved, String> {
        let lang = self.lang;
        let user = |e: CoreError| e.user_message(lang);
        let on = matches!(self.state, HotspotState::On { .. });
        let restart = on && settings::restart_needed(&self.draft, &self.old);
        // Берём свежий файл: списки устройств и поля, которые могли поменять служба или CLI.
        let mut next = config::load(&self.path).map_err(user)?.config;
        settings::merge_changed(&mut next, &self.draft, &self.old);
        let write = || {
            config::update(&self.path, |cfg| {
                settings::merge_changed(cfg, &self.draft, &self.old);
                Ok(())
            })
            .map(|(_, cfg)| cfg)
        };

        // Автозапуск первым: его ошибка не должна оставить раздачу перезапущенной без записи в конфиг.
        let autostart = self.draft.automation.autostart_on_login;
        if autostart != self.old.automation.autostart_on_login && !control::set_autostart(autostart)
        {
            return Err(t(lang, Msg::AdvErrAutostart).to_string());
        }

        let latest = if restart {
            with_hotspot(|h| -> Result<Config, String> {
                // Проверяем настройки до выключения: иначе раздача осталась бы выключенной.
                h.resolve(&next, None).map_err(user)?;
                h.stop().map_err(user)?;
                let latest = write().map_err(user)?;
                let _ = ipc::try_request(&Request::ReloadConfig);
                let approval = latest.access.approval_required && control::ensure_running();
                match h.start(&latest, approval).map_err(user)? {
                    // Пока мы перезапускали, раздачу включил кто-то другой — со старым профилем.
                    StartOutcome::AlreadyOn => {
                        Err(t(lang, Msg::AdvErrStartedElsewhere).to_string())
                    }
                    StartOutcome::Started { .. } => Ok(latest),
                }
            })?
        } else {
            if settings::profile_changed(&self.draft, &self.old) {
                // Раздача выключена: обновляем профиль (или только проверяем, если его нет).
                with_hotspot(|h| h.apply(&next, None)).map_err(user)?;
            }
            let guests_changed =
                self.draft.hotspot.guests_can_reach_pc != self.old.hotspot.guests_can_reach_pc;
            if on && guests_changed && !ipc::daemon_alive() {
                // Службы нет — правила брандмауэра обновляем сами (с ней это делает она).
                refresh_firewall(&next, &self.state).map_err(user)?;
            }
            let latest = write().map_err(user)?;
            // Служба перечитает одобрение, уведомления, простой, таймер и доступ гостей к ПК.
            let _ = ipc::try_request(&Request::ReloadConfig);
            latest
        };

        // То, что работает только со службой, без неё молча не «сохраняем».
        let daemon = ipc::daemon_alive();
        let timers_changed = self.draft.automation.idle_off_minutes
            != self.old.automation.idle_off_minutes
            || self.draft.automation.timer_minutes != self.old.automation.timer_minutes;
        let warning = if daemon {
            None
        } else if on && latest.access.approval_required {
            Some(Msg::NoticeApprovalNoDaemon)
        } else if timers_changed
            && (latest.automation.idle_off_minutes > 0 || latest.automation.timer_minutes > 0)
        {
            Some(Msg::AdvWarnNoDaemonTimers)
        } else {
            None
        };
        Ok(Saved {
            cfg: latest,
            restarted: restart,
            warning,
        })
    }
}

fn refresh_firewall(cfg: &Config, state: &HotspotState) -> Result<(), CoreError> {
    if !PkexecHelper.installed() {
        return Ok(());
    }
    let HotspotState::On {
        ap_iface, uplink, ..
    } = state
    else {
        return Ok(());
    };
    let uplink = match cfg.network.uplink_interface.trim() {
        "" => uplink
            .clone()
            .or_else(|| {
                omarchy_hotspot_core::net::uplink::detect_uplink()
                    .ok()
                    .flatten()
            })
            .ok_or(CoreError::NoUplink)?,
        forced => forced.to_string(),
    };
    // Режим одобрения — как в настройках: иначе правила без белого списка молча пустили бы
    // в интернет неодобренные устройства (белый список мог поставить раньше служба).
    let req = hotspot::firewall_request(cfg, ap_iface, &uplink, cfg.access.approval_required)?;
    PkexecHelper.call(&req).map(|_| ())
}

/// Последние записи службы и помощника. Текст журнала — не наш вывод: управляющие символы
/// убираем, чтобы строка не могла «рисовать» в терминале.
fn read_log(lang: Lang) -> Vec<String> {
    let mut out = Vec::new();
    let sources: [(Msg, &[&str]); 2] = [
        (
            Msg::AdvLogService,
            &[
                "--user",
                "-u",
                control::UNIT,
                "-n",
                LOG_LINES,
                "--no-pager",
                "-o",
                "short",
            ],
        ),
        (
            Msg::AdvLogHelper,
            &[
                "-t",
                "omarchy-hotspot-helper",
                // `_UID=0` ставит сам journald: «от имени помощника» (`logger -t`) может
                // написать любой процесс, но не от root.
                "_UID=0",
                "-n",
                LOG_LINES,
                "--no-pager",
                "-o",
                "short",
            ],
        ),
    ];
    for (title, args) in sources {
        out.push(format!("── {} ──", t(lang, title)));
        let result = Command::new("journalctl")
            .args(args)
            .env("LANG", "C")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        match result {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                let lines: Vec<String> = text
                    .lines()
                    .filter(|l| !l.starts_with("-- "))
                    .map(clean_line)
                    .collect();
                if lines.is_empty() {
                    out.push(t(lang, Msg::AdvLogEmpty).to_string());
                }
                out.extend(lines);
            }
            _ => out.push(t(lang, Msg::AdvLogFailed).to_string()),
        }
    }
    out
}

/// Управляющие символы — в пробел; невидимые и переворачивающие текст — убрать.
fn clean_line(line: &str) -> String {
    line.chars()
        .filter(|c| {
            !matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
        })
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_lines_lose_control_characters() {
        assert_eq!(clean_line("a\u{1b}[31mb\tc"), "a [31mb c");
        assert_eq!(clean_line("x\u{202E}y\u{200B}z"), "xyz");
    }
}
