//! Подкоманды раздачи: on, off, status, password, qr, set, reset.

use std::io::{BufRead, Write};
use std::time::SystemTime;

use omarchy_hotspot_core::backend::{Band, HotspotState, Security};
use omarchy_hotspot_core::config;
use omarchy_hotspot_core::hotspot::{ApplyOutcome, Hotspot, StartOutcome};
use omarchy_hotspot_core::ipc::{self, Request};
use omarchy_hotspot_core::net::iw;
use omarchy_hotspot_core::password::{self, Strength};
use omarchy_hotspot_core::secret::Secret;
use omarchy_hotspot_core::{CoreError, Lang, Msg, qr, t, tf};
use serde::Serialize;

use crate::daemon::control;
use crate::hotspot_ctx::{band_name, security_name, with_hotspot};

use super::Ctx;

pub fn on(ctx: &Ctx) -> anyhow::Result<()> {
    let lang = ctx.lang;
    let (cfg, path) = ctx.load_config()?;
    // Одна карта не может быть точкой доступа и клиентом одновременно: предупреждаем заранее.
    if let Ok(Some(network)) = with_hotspot(|h| h.client_connection(&cfg)) {
        eprintln!("! {}", tf(lang, Msg::WarnWifiClientFmt, &[&network]));
    }
    // Служба нужна для одобрения устройств, уведомлений и авто-выключения.
    let daemon_ready = control::ensure_running();
    let approval = cfg.access.approval_required && daemon_ready;
    if cfg.access.approval_required && !approval {
        eprintln!("! {}", t(lang, Msg::NoticeApprovalNoDaemon));
    }
    let outcome = with_hotspot(|h| h.start(&cfg, approval))?;
    match outcome {
        StartOutcome::AlreadyOn => println!("{}", t(lang, Msg::OnAlready)),
        StartOutcome::Started {
            settings,
            uplink,
            created,
        } => {
            // Первый запуск: фиксируем имя сети в файле, чтобы оно не зависело от имени ПК.
            if !path.exists() {
                config::save(&path, &cfg)?;
            }
            println!("󱜠 {}", tf(lang, Msg::OnStarted, &[&settings.ssid]));
            let channel = settings.channel.map(|c| c.to_string()).unwrap_or_default();
            let rows = [
                (
                    Msg::LabelSecurity,
                    security_name(lang, settings.security).to_string(),
                ),
                (Msg::LabelBand, band_name(lang, settings.band).to_string()),
                (Msg::LabelChannel, channel),
                (Msg::LabelAdapter, settings.ap_iface.clone()),
                (Msg::LabelUplink, uplink),
            ];
            print_rows(lang, &rows);
            if created {
                println!("{}", t(lang, Msg::OnNewPassword));
            }
            println!("{}", t(lang, Msg::OnHint));
        }
    }
    Ok(())
}

fn print_rows(lang: Lang, rows: &[(Msg, String)]) {
    let width = rows
        .iter()
        .map(|(m, _)| t(lang, *m).chars().count())
        .max()
        .unwrap_or(0);
    for (m, v) in rows {
        let label = t(lang, *m);
        let pad = width - label.chars().count();
        println!("  {label}:{} {v}", " ".repeat(pad));
    }
}

pub fn off(ctx: &Ctx) -> anyhow::Result<()> {
    let was_on = with_hotspot(|h| h.stop())?;
    // Служба нужна только во время раздачи.
    control::stop();
    let msg = if was_on {
        Msg::OffDone
    } else {
        Msg::OffAlready
    };
    println!("{}", t(ctx.lang, msg));
    Ok(())
}

/// Вкл/выкл одним действием (правый клик по значку на панели, пункт меню).
pub fn toggle(ctx: &Ctx) -> anyhow::Result<()> {
    let is_on = matches!(
        with_hotspot(|h| h.state())?,
        HotspotState::On { .. } | HotspotState::Starting
    );
    if is_on { off(ctx) } else { on(ctx) }
}

/// Значок для панели Omarchy: JSON в стиле Waybar (`text`, `tooltip`, `class`).
/// Код выхода всегда 0 — панель читает только вывод.
pub fn status_waybar(ctx: &Ctx) -> anyhow::Result<()> {
    let lang = ctx.lang;
    let ssid = ctx.load_config().map(|(cfg, _)| cfg.hotspot.ssid).ok();
    let (class, state_msg, devices) = match with_hotspot(|h| h.state()) {
        Ok(HotspotState::On { ap_iface, .. }) => {
            let count = iw::station_dump(&ap_iface).map_or(0, |s| s.len());
            ("on", Msg::StateOn, Some(count))
        }
        Ok(HotspotState::Starting) => ("starting", Msg::StateStarting, None),
        _ => ("off", Msg::StateOff, None),
    };
    let mut tooltip = format!("{}: {}", t(lang, Msg::LabelHotspot), t(lang, state_msg));
    if let Some(ssid) = ssid {
        tooltip.push_str(&format!("\n{}: {ssid}", t(lang, Msg::LabelNetwork)));
    }
    if let Some(count) = devices {
        tooltip.push('\n');
        tooltip.push_str(&tf(lang, Msg::BarDevicesFmt, &[&count.to_string()]));
    }
    tooltip.push('\n');
    tooltip.push_str(t(lang, Msg::BarClickHint));
    let text = if class == "off" {
        BAR_ICON_OFF
    } else {
        BAR_ICON_ON
    };
    let out = serde_json::json!({
        "text": text,
        "tooltip": tooltip,
        "class": class,
        "alt": class,
        "devices": devices.unwrap_or(0),
    });
    println!("{out}");
    Ok(())
}

/// Значки Nerd Font: «точка доступа» и «точка доступа выключена».
const BAR_ICON_ON: &str = "\u{f1720}";
const BAR_ICON_OFF: &str = "\u{f1721}";

#[derive(Serialize)]
struct StatusJson {
    state: &'static str,
    ssid: String,
    security: Security,
    band: Band,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ap_iface: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uplink: Option<String>,
    /// Время включения, секунды Unix.
    #[serde(skip_serializing_if = "Option::is_none")]
    since: Option<u64>,
}

/// Код выхода: 0 — раздача включена, 1 — выключена или ошибка.
pub fn status(ctx: &Ctx, json: bool, quiet: bool) -> anyhow::Result<i32> {
    let lang = ctx.lang;
    let (cfg, _) = ctx.load_config()?;
    let state = match with_hotspot(|h| h.state()) {
        Ok(s) => s,
        Err(e) if json => {
            let code = match e {
                CoreError::NmNotRunning => "nm_not_running",
                _ => "error",
            };
            println!("{}", serde_json::json!({ "state": "error", "error": code }));
            return Ok(1);
        }
        Err(e) if quiet => {
            tracing::debug!("status: {e}");
            return Ok(1);
        }
        Err(e) => return Err(e.into()),
    };
    let mut out = StatusJson {
        state: "off",
        ssid: cfg.hotspot.ssid.clone(),
        security: cfg.hotspot.security,
        band: cfg.hotspot.band,
        channel: None,
        ap_iface: None,
        uplink: None,
        since: None,
    };
    match &state {
        HotspotState::Off | HotspotState::Error(_) => {}
        HotspotState::Starting => out.state = "starting",
        HotspotState::On {
            since,
            ap_iface,
            uplink,
        } => {
            out.state = "on";
            out.channel = iw::current_channel(ap_iface);
            if let Some(ch) = out.channel {
                out.band = if ch <= 14 { Band::Ghz2_4 } else { Band::Ghz5 };
            }
            out.ap_iface = Some(ap_iface.clone());
            out.uplink = uplink.clone();
            out.since = since
                .and_then(|s| s.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
        }
    }
    let code = if out.state == "on" { 0 } else { 1 };
    if quiet {
        return Ok(code);
    }
    if json {
        println!("{}", serde_json::to_string(&out)?);
        return Ok(code);
    }
    let state_msg = match out.state {
        "on" => Msg::StateOn,
        "starting" => Msg::StateStarting,
        _ => Msg::StateOff,
    };
    let mut rows = vec![
        (Msg::LabelHotspot, t(lang, state_msg).to_string()),
        (Msg::LabelNetwork, out.ssid.clone()),
        (Msg::LabelSecurity, security_name(lang, out.security).into()),
        (Msg::LabelBand, band_name(lang, out.band).into()),
    ];
    if let Some(ch) = out.channel {
        rows.push((Msg::LabelChannel, ch.to_string()));
    }
    if let Some(i) = &out.ap_iface {
        rows.push((Msg::LabelAdapter, i.clone()));
    }
    if let Some(u) = &out.uplink {
        rows.push((Msg::LabelUplink, u.clone()));
    }
    if let HotspotState::On {
        since: Some(since), ..
    } = state
    {
        let mins = since.elapsed().map_or(0, |d| d.as_secs() / 60);
        let up = tf(
            lang,
            Msg::UptimeFmt,
            &[&(mins / 60).to_string(), &(mins % 60).to_string()],
        );
        rows.push((Msg::LabelUptime, up));
    }
    print_rows(lang, &rows);
    Ok(code)
}

fn require_password(h: &Hotspot) -> Result<Secret, CoreError> {
    h.password()?.ok_or(CoreError::NoProfile)
}

pub fn password(_ctx: &Ctx) -> anyhow::Result<()> {
    let psk = with_hotspot(require_password)?;
    println!("{}", psk.expose());
    Ok(())
}

pub fn qr(ctx: &Ctx) -> anyhow::Result<()> {
    let lang = ctx.lang;
    let (cfg, _) = ctx.load_config()?;
    let psk = with_hotspot(require_password)?;
    let data = qr::wifi_qr_string(
        &cfg.hotspot.ssid,
        &psk,
        cfg.hotspot.security,
        cfg.hotspot.hidden,
    );
    let matrix =
        qr::matrix(data.expose(), 2).ok_or_else(|| anyhow::anyhow!("QR encoding failed"))?;
    // Чёрные модули на белом фоне (ANSI 30/47): так код читается и на тёмной теме.
    let mut out = std::io::stdout().lock();
    for line in qr::half_blocks(&matrix) {
        writeln!(out, "\x1b[30;47m{line}\x1b[0m")?;
    }
    writeln!(out)?;
    writeln!(out, "{}: {}", t(lang, Msg::LabelNetwork), cfg.hotspot.ssid)?;
    writeln!(out, "{}", t(lang, Msg::QrHint))?;
    Ok(())
}

/// Что меняет `set`.
pub enum SetWhat {
    Ssid(String),
    /// Выключать раздачу, если никто не подключён столько минут (0 — не выключать).
    IdleOff(u32),
    Band(Band),
    Security(Security),
    Password {
        generate: bool,
        stdin: bool,
    },
}

/// Изменение одного поля конфига: применяется к свежему файлу под замком.
type FieldChange = Box<dyn FnOnce(&mut config::Config)>;

pub fn set(ctx: &Ctx, what: SetWhat) -> anyhow::Result<()> {
    let lang = ctx.lang;
    let (mut cfg, path) = ctx.load_config()?;
    let mut new_password = None;
    // Что записать в файл после применения настроек. Пишем только изменённое поле:
    // остальное в файле могли поменять окно или служба, пока шёл `nmcli`.
    let mut persist: Option<FieldChange> = None;
    match what {
        SetWhat::Ssid(s) => {
            cfg.hotspot.ssid = s.clone();
            persist = Some(Box::new(move |c| c.hotspot.ssid = s));
        }
        // Настройка службы: NetworkManager тут ни при чём, профиль не трогаем.
        SetWhat::IdleOff(minutes) => {
            config::update(&path, |c| {
                c.automation.idle_off_minutes = minutes;
                Ok(())
            })?;
            let _ = ipc::try_request(&Request::ReloadConfig);
            if minutes == 0 {
                println!("{}", t(lang, Msg::SetIdleOffNever));
            } else {
                println!("{}", tf(lang, Msg::SetIdleOffFmt, &[&minutes.to_string()]));
            }
            return Ok(());
        }
        SetWhat::Band(b) => {
            cfg.hotspot.band = b;
            cfg.hotspot.channel = 0;
            persist = Some(Box::new(move |c| {
                c.hotspot.band = b;
                c.hotspot.channel = 0;
            }));
        }
        SetWhat::Security(sec) => {
            cfg.hotspot.security = sec;
            persist = Some(Box::new(move |c| c.hotspot.security = sec));
        }
        SetWhat::Password { generate, stdin } => {
            new_password = Some(if generate {
                password::generate()?
            } else {
                read_new_password(ctx, stdin)?
            });
        }
    }
    let is_password = new_password.is_some();
    let outcome = with_hotspot(|h| h.apply(&cfg, new_password))?;
    if let Some(change) = persist {
        config::update(&path, |c| {
            change(c);
            Ok(())
        })?;
    }
    let msg = match outcome {
        ApplyOutcome::Saved => Msg::SetSaved,
        ApplyOutcome::Updated => Msg::SetUpdated,
        ApplyOutcome::Restarted => Msg::SetRestarted,
    };
    println!("{}", t(lang, msg));
    if is_password {
        println!("{}", t(lang, Msg::OnHint));
        if outcome == ApplyOutcome::Restarted {
            println!("{}", t(lang, Msg::PwChangedHint));
        }
    }
    Ok(())
}

fn read_new_password(ctx: &Ctx, from_stdin: bool) -> anyhow::Result<Secret> {
    let lang = ctx.lang;
    let psk = if from_stdin {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        let trimmed = line.strip_suffix('\n').unwrap_or(&line);
        let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
        let s = Secret::new(trimmed);
        // Исходный буфер тоже затираем (Secret делает это при удалении).
        drop(Secret::new(line));
        s
    } else {
        if !ctx.stdin_is_tty() {
            anyhow::bail!(t(lang, Msg::PwNeedTty));
        }
        let first = Secret::new(rpassword::prompt_password(t(lang, Msg::PromptPassword))?);
        // Сразу проверяем, чтобы не просить повтор для заведомо неподходящего пароля.
        password::validate_user_password(first.expose()).map_err(CoreError::from)?;
        let second = Secret::new(rpassword::prompt_password(t(lang, Msg::PromptRepeat))?);
        if first != second {
            anyhow::bail!(t(lang, Msg::PwMismatch));
        }
        first
    };
    let strength = password::validate_user_password(psk.expose()).map_err(CoreError::from)?;
    if matches!(strength, Strength::Weak | Strength::TooShort) {
        eprintln!("! {}", t(lang, Msg::PwWeak));
    }
    Ok(psk)
}

pub fn reset(ctx: &Ctx, yes: bool) -> anyhow::Result<()> {
    let lang = ctx.lang;
    let dir = config::config_dir();
    let dir_s = dir
        .as_ref()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    println!("{}", tf(lang, Msg::ResetWhat, &[&dir_s]));
    if !yes {
        if !ctx.stdin_is_tty() {
            anyhow::bail!(t(lang, Msg::ResetNeedYes));
        }
        print!("{}", t(lang, Msg::ResetAsk));
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        let a = answer.trim().to_lowercase();
        if !matches!(a.as_str(), "y" | "yes" | "д" | "да" | "т" | "так") {
            println!("{}", t(lang, Msg::ResetCancelled));
            return Ok(());
        }
    }
    // Сначала останавливаем службу: иначе она пересоздаст удалённый конфиг из памяти.
    control::stop();
    // Автозапуск при входе — тоже наше системное изменение, его откатываем.
    control::set_autostart(false);
    with_hotspot(|h| h.reset())?;
    // Настройки и запомненный размер окна (и его правило в памяти Hyprland).
    crate::tui::forget_window_size();
    for d in [dir, crate::tui::state_dir()].into_iter().flatten() {
        match std::fs::remove_dir_all(&d) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    println!("{}", t(lang, Msg::ResetDone));
    Ok(())
}
