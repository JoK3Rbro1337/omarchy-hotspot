use std::io;

use crate::i18n::{Lang, Msg, t, tf};
use crate::password::PasswordError;
use crate::ssid::SsidError;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// Нужная программа не найдена (например, `iw`).
    #[error("program not found: {0}")]
    MissingTool(&'static str),
    #[error("{tool} failed (exit {code}): {stderr}")]
    ToolFailed {
        tool: &'static str,
        code: i32,
        stderr: String,
    },
    #[error("cannot parse output of {tool}: {reason}")]
    Parse { tool: &'static str, reason: String },
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("config: {0}")]
    Config(String),
    #[error("NetworkManager is not running")]
    NmNotRunning,
    #[error("no Wi-Fi adapter")]
    NoWifi,
    #[error("Wi-Fi interface {0} not found")]
    IfaceNotFound(String),
    #[error("no Wi-Fi adapter supports AP mode")]
    NoApSupport,
    #[error("adapter does not support WPA3 (SAE)")]
    Wpa3Unsupported,
    #[error("5 GHz is not available for AP on this adapter")]
    Band5Unavailable,
    #[error("channel {0} is not available")]
    ChannelUnavailable(u8),
    #[error("no internet uplink")]
    NoUplink,
    #[error("internet uplink is the same Wi-Fi interface {0}")]
    UplinkIsAp(String),
    #[error("subnet is not a private network: {0}")]
    SubnetNotPrivate(String),
    /// Подсеть раздачи пересекается с сетью другого интерфейса (LAN, VPN): подсеть, чужой маршрут.
    #[error("subnet {0} overlaps with network {1}")]
    SubnetConflict(String, String),
    /// Другое действие с раздачей (вкл/выкл/применить) не закончилось за время ожидания.
    #[error("another hotspot action is still running")]
    Busy,
    /// Раздача работает на одном адаптере, а настройки указывают другой: правила остались бы на старом.
    #[error("hotspot runs on {running}, settings want {wanted}")]
    AdapterChanged { running: String, wanted: String },
    #[error("hotspot profile does not exist")]
    NoProfile,
    #[error("activation failed: {0}")]
    ActivationFailed(String),
    #[error("root helper is not installed")]
    HelperNotInstalled,
    #[error("root helper was not authorized")]
    HelperDenied,
    #[error("root helper failed: {0}")]
    HelperFailed(String),
    #[error("NetworkManager D-Bus: {0}")]
    NmDbus(String),
    #[error("invalid SSID: {0}")]
    Ssid(#[from] SsidError),
    #[error("invalid password: {0}")]
    Password(#[from] PasswordError),
}

impl CoreError {
    /// Понятное сообщение для пользователя (с советом, если он есть). Без технических деталей.
    pub fn user_message(&self, lang: Lang) -> String {
        use CoreError as E;
        let two = |a: Msg, b: Msg| format!("{}\n{}", t(lang, a), t(lang, b));
        match self {
            E::NmNotRunning => two(Msg::ErrNmNotRunning, Msg::AdviceNmInactive),
            E::NoWifi => two(Msg::DetailNoWifi, Msg::AdviceNoWifi),
            E::IfaceNotFound(i) => tf(lang, Msg::ErrIfaceNotFound, &[i]),
            E::NoApSupport => t(lang, Msg::AdviceNoAp).into(),
            E::Wpa3Unsupported => t(lang, Msg::ErrWpa3Unsupported).into(),
            E::Band5Unavailable => t(lang, Msg::ErrBand5Unavailable).into(),
            E::ChannelUnavailable(c) => tf(lang, Msg::ErrChannelUnavailable, &[&c.to_string()]),
            E::NoUplink => two(Msg::ErrNoUplink, Msg::AdviceNoUplink),
            E::UplinkIsAp(_) => t(lang, Msg::AdviceUplinkIsWifi).into(),
            E::NoProfile => t(lang, Msg::ErrNoProfile).into(),
            E::Busy => t(lang, Msg::ErrBusy).into(),
            E::AdapterChanged { running, wanted } => {
                tf(lang, Msg::ErrAdapterChangedFmt, &[running, wanted])
            }
            E::SubnetNotPrivate(s) => tf(lang, Msg::ErrSubnetNotPrivateFmt, &[s]),
            E::SubnetConflict(s, other) => tf(lang, Msg::ErrSubnetConflictFmt, &[s, other]),
            E::ActivationFailed(_) => t(lang, Msg::ErrActivation).into(),
            E::Ssid(SsidError::Empty) => t(lang, Msg::ErrSsidEmpty).into(),
            E::Ssid(SsidError::TooLong) => t(lang, Msg::ErrSsidTooLong).into(),
            E::Ssid(SsidError::ControlChars) => t(lang, Msg::ErrSsidControl).into(),
            E::Password(PasswordError::TooShort) => t(lang, Msg::ErrPwTooShort).into(),
            E::Password(PasswordError::TooLong) => t(lang, Msg::ErrPwTooLong).into(),
            E::Password(PasswordError::NotAscii) => t(lang, Msg::ErrPwNotAscii).into(),
            E::Config(detail) => tf(lang, Msg::ErrConfig, &[detail]),
            E::MissingTool(tool) => tf(lang, Msg::ErrMissingTool, &[tool]),
            E::HelperNotInstalled => t(lang, Msg::ErrHelperNotInstalled).into(),
            E::HelperDenied => t(lang, Msg::ErrHelperDenied).into(),
            E::HelperFailed(_) => t(lang, Msg::ErrHelperFailed).into(),
            E::NmDbus(_) => t(lang, Msg::ErrPasswordSave).into(),
            E::ToolFailed { .. } | E::Parse { .. } | E::Io(_) => t(lang, Msg::ErrGeneric).into(),
        }
    }
}
