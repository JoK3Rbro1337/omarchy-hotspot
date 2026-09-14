//! Общее для CLI и TUI: сборка `Hotspot` из настоящих реализаций и подписи для UI.

use omarchy_hotspot_core::backend::nmcli::NmcliBackend;
use omarchy_hotspot_core::backend::{Band, Security};
use omarchy_hotspot_core::helper::{HelperRunner, PkexecHelper};
use omarchy_hotspot_core::hotspot::{Hotspot, RealProbe};
use omarchy_hotspot_core::{Lang, Msg, t};

/// Собирает `Hotspot` на настоящих реализациях (nmcli, iw/uplink/ufw, pkexec) на время вызова.
pub fn with_hotspot<R>(f: impl FnOnce(&Hotspot) -> R) -> R {
    with_hotspot_helper(&PkexecHelper, f)
}

/// То же, но с готовым помощником: фоновая служба держит один `pkexec … serve` на всю раздачу.
pub fn with_hotspot_helper<R>(helper: &dyn HelperRunner, f: impl FnOnce(&Hotspot) -> R) -> R {
    let backend = NmcliBackend::new();
    let probe = RealProbe;
    f(&Hotspot::new(&backend, &probe, helper))
}

pub fn security_name(lang: Lang, s: Security) -> &'static str {
    t(
        lang,
        match s {
            Security::Wpa3 => Msg::SecurityWpa3,
            Security::Wpa2 => Msg::SecurityWpa2,
        },
    )
}

pub fn band_name(lang: Lang, b: Band) -> &'static str {
    t(
        lang,
        match b {
            Band::Auto => Msg::BandAuto,
            Band::Ghz2_4 => Msg::Band24,
            Band::Ghz5 => Msg::Band5,
        },
    )
}
