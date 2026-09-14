//! Уведомления на рабочий стол через `notify-send` (в Omarchy их показывает mako).

use std::process::{Command, Stdio};

const NOTIFY_SEND: &str = "notify-send";
const APP_NAME: &str = "omarchy-hotspot";
const ICON: &str = "network-wireless";

#[derive(Debug, Clone, Copy)]
pub enum Urgency {
    Low,
    Normal,
}

impl Urgency {
    fn as_str(self) -> &'static str {
        match self {
            Urgency::Low => "low",
            Urgency::Normal => "normal",
        }
    }
}

pub struct Notifier {
    /// Выключено в настройках (`access.notifications = false`).
    enabled: bool,
    /// `notify-send` в системе есть.
    available: bool,
}

/// Разовое уведомление вне службы (ошибка «вкл/выкл» с панели, где нет терминала).
/// Ждём завершения `notify-send`, чтобы не оставить «зомби».
pub fn send_now(summary: &str, body: &str) {
    let result = Command::new(NOTIFY_SEND)
        .arg(format!("--app-name={APP_NAME}"))
        .arg(format!("--icon={ICON}"))
        .arg("--urgency=normal")
        .arg("--")
        .arg(escape_markup(summary))
        .arg(escape_markup(body))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if let Err(e) = result {
        tracing::debug!("notify-send: {e}");
    }
}

impl Notifier {
    pub fn new(enabled: bool) -> Notifier {
        let available = which_notify_send();
        if enabled && !available {
            tracing::warn!("notify-send is not installed: desktop notifications are off");
        }
        Notifier { enabled, available }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Показать уведомление. Текст от гостя (имя устройства) сюда приходит уже очищенным,
    /// но разметку экранируем ещё раз: многие серверы уведомлений понимают HTML-теги.
    pub fn send(&self, urgency: Urgency, summary: &str, body: &str) {
        if !self.enabled || !self.available {
            return;
        }
        let args = vec![
            format!("--app-name={APP_NAME}"),
            format!("--icon={ICON}"),
            format!("--urgency={}", urgency.as_str()),
            "--expire-time=8000".to_string(),
            // Дальше идут не опции, а текст: имя устройства придумывает гость, и оно
            // не должно быть разобрано как ключ вроде `--icon=…`.
            "--".to_string(),
            escape_markup(summary),
            escape_markup(body),
        ];
        // Запускаем в отдельном потоке и обязательно дожидаемся конца процесса,
        // иначе в службе копились бы «зомби».
        tokio::task::spawn_blocking(move || {
            let result = Command::new(NOTIFY_SEND)
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if let Err(e) = result {
                tracing::debug!("notify-send failed: {e}");
            }
        });
    }
}

fn which_notify_send() -> bool {
    Command::new(NOTIFY_SEND)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// `&`, `<`, `>` → безопасные последовательности: имя устройства придумывает гость.
fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_from_guest_is_escaped() {
        assert_eq!(
            escape_markup("<b>hi</b> & co"),
            "&lt;b&gt;hi&lt;/b&gt; &amp; co"
        );
        assert_eq!(escape_markup("Pixel-8"), "Pixel-8");
    }

    #[test]
    fn disabled_notifier_sends_nothing() {
        // Без рантайма tokio вызов `send` не должен ничего запускать и паниковать.
        let n = Notifier {
            enabled: false,
            available: true,
        };
        n.send(Urgency::Normal, "a", "b");
    }
}
