//! Управление службой со стороны приложения: запустить, остановить, спросить.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use omarchy_hotspot_core::ipc;

pub const UNIT: &str = "omarchy-hotspot.service";
const SYSTEMCTL: &str = "systemctl";
/// Сколько ждём, пока запущенная служба откроет сокет.
const START_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_EVERY: Duration = Duration::from_millis(100);

/// Файл службы у пользователя (его ставит установщик, этап 7).
pub fn unit_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("systemd/user").join(UNIT))
}

pub fn unit_installed() -> bool {
    unit_path().is_some_and(|p| p.exists())
}

pub fn is_running() -> bool {
    ipc::daemon_alive()
}

/// Запустить службу, если она ещё не работает. Она нужна для одобрения устройств,
/// уведомлений и авто-выключения. `false` — службы нет (файл службы не установлен
/// или она не открыла сокет); тогда режим одобрения включать нельзя: одобрять некому.
pub fn ensure_running() -> bool {
    if is_running() {
        return true;
    }
    if !unit_installed() {
        return false;
    }
    // Если служба раньше падала, systemd помнит это и `start` ничего не сделает.
    systemctl(&["reset-failed", UNIT]);
    if !systemctl(&["start", UNIT]) {
        return false;
    }
    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        if is_running() {
            return true;
        }
        std::thread::sleep(POLL_EVERY);
    }
    tracing::warn!("{UNIT} did not open its socket in time");
    false
}

/// Остановить службу (когда раздачу выключили). Запущенную вручную не трогаем.
pub fn stop() {
    if unit_installed() {
        systemctl(&["stop", UNIT]);
    }
}

/// Автозапуск при входе: служба стартует вместе с графическим сеансом и сама включает
/// раздачу (см. `Daemon::autostart_once`). `false` — файл службы не установлен или
/// systemctl отказал.
pub fn set_autostart(on: bool) -> bool {
    if !unit_installed() {
        return !on;
    }
    systemctl(&[if on { "enable" } else { "disable" }, UNIT])
}

fn systemctl(args: &[&str]) -> bool {
    let mut cmd = Command::new(SYSTEMCTL);
    cmd.arg("--user")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match cmd.status() {
        Ok(s) if s.success() => true,
        Ok(s) => {
            // `reset-failed` для незапускавшейся службы возвращает ошибку — это нормально.
            tracing::debug!("systemctl --user {args:?} failed: {s}");
            false
        }
        Err(e) => {
            tracing::warn!("cannot run systemctl: {e}");
            false
        }
    }
}
