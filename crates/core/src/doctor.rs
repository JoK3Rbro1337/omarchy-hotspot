//! Проверка готовности системы к раздаче. Без root.

use std::path::Path;

use crate::backend::nmcli_parse::devices_of_type;
use crate::i18n::{Lang, Msg, t};
use crate::net::{cmd, iw, uplink};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckId {
    NetworkManager,
    Dnsmasq,
    WifiAp,
    Uplink,
    Firewall,
    Country,
    Helper,
}

impl CheckId {
    pub fn title(self) -> Msg {
        match self {
            CheckId::NetworkManager => Msg::CheckNetworkManager,
            CheckId::Dnsmasq => Msg::CheckDnsmasq,
            CheckId::WifiAp => Msg::CheckWifiAp,
            CheckId::Uplink => Msg::CheckUplink,
            CheckId::Firewall => Msg::CheckFirewall,
            CheckId::Country => Msg::CheckCountry,
            CheckId::Helper => Msg::CheckHelper,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub id: CheckId,
    pub status: Status,
    pub detail: String,
    pub advice: Option<Msg>,
}

impl Check {
    fn new(id: CheckId, status: Status, detail: impl Into<String>, advice: Option<Msg>) -> Self {
        Check {
            id,
            status,
            detail: detail.into(),
            advice,
        }
    }
}

pub fn run_all(lang: Lang) -> Vec<Check> {
    let wifi_ifaces = wifi_ifaces();
    vec![
        check_network_manager(lang),
        check_dnsmasq(lang),
        check_wifi_ap(lang, &wifi_ifaces),
        check_uplink(lang, &wifi_ifaces),
        check_firewall(lang),
        check_country(lang),
        check_helper(lang),
    ]
}

fn wifi_ifaces() -> Vec<String> {
    cmd::run("nmcli", &["-t", "-f", "DEVICE,TYPE", "device"])
        .map(|out| devices_of_type(&out, "wifi"))
        .unwrap_or_default()
}

fn check_network_manager(lang: Lang) -> Check {
    let running = cmd::run("nmcli", &["-t", "-f", "RUNNING", "general"])
        .map(|o| o.trim() == "running")
        .unwrap_or(false);
    if running {
        Check::new(
            CheckId::NetworkManager,
            Status::Ok,
            t(lang, Msg::DetailRunning),
            None,
        )
    } else {
        Check::new(
            CheckId::NetworkManager,
            Status::Fail,
            t(lang, Msg::DetailNotRunning),
            Some(Msg::AdviceNmInactive),
        )
    }
}

fn check_dnsmasq(lang: Lang) -> Check {
    if Path::new("/usr/bin/dnsmasq").exists() {
        Check::new(
            CheckId::Dnsmasq,
            Status::Ok,
            t(lang, Msg::DetailInstalled),
            None,
        )
    } else {
        Check::new(
            CheckId::Dnsmasq,
            Status::Fail,
            t(lang, Msg::DetailNotInstalled),
            Some(Msg::AdviceInstallDnsmasq),
        )
    }
}

fn check_wifi_ap(lang: Lang, wifi_ifaces: &[String]) -> Check {
    let Some(iface) = wifi_ifaces.first() else {
        return Check::new(
            CheckId::WifiAp,
            Status::Fail,
            t(lang, Msg::DetailNoWifi),
            Some(Msg::AdviceNoWifi),
        );
    };
    let caps = iw::wifi_caps().unwrap_or_default();
    if !caps.ap {
        return Check::new(
            CheckId::WifiAp,
            Status::Fail,
            t(lang, Msg::DetailNoAp),
            Some(Msg::AdviceNoAp),
        );
    }
    let yes_no = |b: bool| t(lang, if b { Msg::Yes } else { Msg::No });
    let detail = format!(
        "{iface}; WPA3: {}; 5 GHz: {}",
        yes_no(caps.sae),
        yes_no(!caps.channels_5ghz.is_empty())
    );
    Check::new(CheckId::WifiAp, Status::Ok, detail, None)
}

fn check_uplink(lang: Lang, wifi_ifaces: &[String]) -> Check {
    match uplink::detect_uplink() {
        Ok(Some(dev)) if wifi_ifaces.contains(&dev) => Check::new(
            CheckId::Uplink,
            Status::Warn,
            dev,
            Some(Msg::AdviceUplinkIsWifi),
        ),
        Ok(Some(dev)) => Check::new(CheckId::Uplink, Status::Ok, dev, None),
        _ => Check::new(
            CheckId::Uplink,
            Status::Fail,
            t(lang, Msg::DetailNoRoute),
            Some(Msg::AdviceNoUplink),
        ),
    }
}

fn check_firewall(lang: Lang) -> Check {
    match cmd::succeeds("systemctl", &["is-active", "--quiet", "ufw"]) {
        Ok(true) => Check::new(
            CheckId::Firewall,
            Status::Ok,
            t(lang, Msg::DetailUfwActive),
            None,
        ),
        Ok(false) => Check::new(
            CheckId::Firewall,
            Status::Ok,
            t(lang, Msg::DetailUfwInactive),
            None,
        ),
        Err(_) => Check::new(
            CheckId::Firewall,
            Status::Warn,
            t(lang, Msg::DetailUfwUnknown),
            None,
        ),
    }
}

/// Помощник и polkit-политика на месте (SECURITY.md §8). Без них раздача идёт без брандмауэра.
fn check_helper(lang: Lang) -> Check {
    let helper = Path::new(crate::helper::HELPER_PATH).is_file();
    let policy = Path::new("/usr/share/polkit-1/actions/org.omarchy.hotspot.policy").is_file();
    if helper && policy {
        Check::new(
            CheckId::Helper,
            Status::Ok,
            t(lang, Msg::DetailInstalled),
            None,
        )
    } else {
        Check::new(
            CheckId::Helper,
            Status::Warn,
            t(lang, Msg::DetailHelperMissing),
            Some(Msg::AdviceHelperMissing),
        )
    }
}

fn check_country(lang: Lang) -> Check {
    match iw::reg_country() {
        Ok(Some(cc)) => Check::new(CheckId::Country, Status::Ok, cc, None),
        _ => Check::new(
            CheckId::Country,
            Status::Warn,
            t(lang, Msg::DetailCountryUnset),
            Some(Msg::AdviceCountryUnset),
        ),
    }
}
