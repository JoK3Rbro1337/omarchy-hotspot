//! Системные проверки аргументов (SECURITY.md §2.2): интерфейс существует, AP — беспроводной в режиме AP.

use std::path::PathBuf;

use anyhow::bail;
use omarchy_hotspot_proto::Iface;

use crate::run::{self, IW};

fn sysfs(iface: &Iface) -> PathBuf {
    // Имя проверено при разборе: только [A-Za-z0-9_.-], не "." и "..".
    PathBuf::from("/sys/class/net").join(iface.as_str())
}

pub fn check_exists(iface: &Iface) -> anyhow::Result<()> {
    if !sysfs(iface).is_dir() {
        bail!("interface {iface} does not exist");
    }
    Ok(())
}

/// Беспроводной интерфейс в режиме AP (по `iw dev <iface> info`).
pub fn check_ap(iface: &Iface) -> anyhow::Result<()> {
    check_exists(iface)?;
    if !sysfs(iface).join("phy80211").exists() {
        bail!("interface {iface} is not wireless");
    }
    let info = run::run(IW, &["dev", iface.as_str(), "info"], None)?;
    if !is_ap_mode(&info) {
        bail!("interface {iface} is not in AP mode");
    }
    Ok(())
}

pub fn is_ap_mode(iw_info: &str) -> bool {
    iw_info.lines().any(|l| l.trim() == "type AP")
}

/// Беспроводной интерфейс (не обязательно в режиме AP).
pub fn check_wireless(iface: &Iface) -> anyhow::Result<()> {
    check_exists(iface)?;
    if !sysfs(iface).join("phy80211").exists() {
        bail!("interface {iface} is not wireless");
    }
    Ok(())
}

/// Источник интернета: существует, не точка доступа и не мост (гости попали бы в контейнеры/ВМ).
pub fn check_uplink(uplink: &Iface, ap: &Iface) -> anyhow::Result<()> {
    check_exists(uplink)?;
    if uplink == ap {
        bail!("uplink must differ from the AP interface");
    }
    if sysfs(uplink).join("bridge").exists() {
        bail!("uplink {uplink} is a bridge");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ap_mode() {
        let ap = "Interface wlp15s0\n\tifindex 3\n\ttype AP\n\tchannel 36\n";
        let managed = "Interface wlp15s0\n\ttype managed\n";
        assert!(is_ap_mode(ap));
        assert!(!is_ap_mode(managed));
        assert!(!is_ap_mode(""));
    }

    #[test]
    fn missing_iface_is_error() {
        let i = Iface::parse("nope-xyz-999").unwrap();
        assert!(check_exists(&i).is_err());
        assert!(check_ap(&i).is_err());
    }
}
