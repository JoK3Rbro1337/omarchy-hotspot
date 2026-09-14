//! Подкоманды доступа устройств: `devices`, `access list|allow|block|unblock|approval`.
//! Если фоновая служба работает, изменения идут через неё (у неё свой конфиг в памяти);
//! иначе то же самое делаем сами.

use anyhow::Context;
use omarchy_hotspot_core::access::{self, AccessChange, AccessLists};
use omarchy_hotspot_core::backend::HotspotState;
use omarchy_hotspot_core::config;
use omarchy_hotspot_core::devices::{self, Device, DeviceStatus, bars_str, signal_bars};
use omarchy_hotspot_core::helper::PkexecHelper;
use omarchy_hotspot_core::helper_proto::{Iface, Mac};
use omarchy_hotspot_core::ipc::{self, Request, Response};
use omarchy_hotspot_core::{CoreError, Lang, Msg, t, tf};

use crate::hotspot_ctx::with_hotspot;

use super::Ctx;

/// Что делаем со списками доступа.
pub enum AccessWhat {
    List,
    Allow(String),
    Block(String),
    Unblock(String),
    Approval(bool),
}

pub fn access(ctx: &Ctx, what: AccessWhat) -> anyhow::Result<()> {
    match what {
        AccessWhat::List => list(ctx),
        AccessWhat::Allow(mac) => change(ctx, &mac, Kind::Allow),
        AccessWhat::Block(mac) => change(ctx, &mac, Kind::Block),
        AccessWhat::Unblock(mac) => change(ctx, &mac, Kind::Unblock),
        AccessWhat::Approval(on) => approval(ctx, on),
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Allow,
    Block,
    Unblock,
}

impl Kind {
    fn request(self, mac: Mac) -> Request {
        match self {
            Kind::Allow => Request::Approve { mac },
            Kind::Block => Request::Block { mac },
            Kind::Unblock => Request::Unblock { mac },
        }
    }

    fn change(self, mac: Mac) -> AccessChange {
        match self {
            Kind::Allow => AccessChange::Allow(mac),
            Kind::Block => AccessChange::Block(mac),
            Kind::Unblock => AccessChange::Unblock(mac),
        }
    }

    fn done(self) -> Msg {
        match self {
            Kind::Allow => Msg::AccessDoneAllowFmt,
            Kind::Block => Msg::AccessDoneBlockFmt,
            Kind::Unblock => Msg::AccessDoneUnblockFmt,
        }
    }
}

fn list(ctx: &Ctx) -> anyhow::Result<()> {
    let lang = ctx.lang;
    let (cfg, _) = ctx.load_config()?;
    let approval = if cfg.access.approval_required {
        Msg::AccessApprovalOn
    } else {
        Msg::AccessApprovalOff
    };
    println!("{}", t(lang, approval));
    print_list(lang, Msg::AccessAllowedTitleFmt, &cfg.access.allowed_macs);
    print_list(lang, Msg::AccessBlockedTitleFmt, &cfg.access.blocked_macs);
    Ok(())
}

fn print_list(lang: Lang, title: Msg, macs: &[String]) {
    println!("{}", tf(lang, title, &[&macs.len().to_string()]));
    if macs.is_empty() {
        println!("  {}", t(lang, Msg::AccessEmptyList));
    }
    for m in macs {
        println!("  {m}");
    }
}

fn change(ctx: &Ctx, mac: &str, kind: Kind) -> anyhow::Result<()> {
    let lang = ctx.lang;
    let mac = Mac::parse(mac).map_err(|e| CoreError::Config(e.to_string()))?;
    match ipc::try_request(&kind.request(mac.clone()))? {
        Some(Response::Ok) => {}
        Some(Response::Err { msg, .. }) => anyhow::bail!("{msg}"),
        Some(_) => anyhow::bail!("unexpected answer from the service"),
        // Службы нет — делаем сами.
        None => {
            let (mut cfg, path) = ctx.load_config()?;
            let ap = ap_iface();
            let changed = access::apply(
                &PkexecHelper,
                ap.as_ref(),
                &mut cfg,
                &kind.change(mac.clone()),
            )?;
            if changed {
                config::save(&path, &cfg)?;
            }
        }
    }
    println!("{}", tf(lang, kind.done(), &[mac.as_str()]));
    Ok(())
}

fn approval(ctx: &Ctx, on: bool) -> anyhow::Result<()> {
    let path = config::config_path().context("cannot find home directory ($HOME)")?;
    config::update(&path, |cfg| {
        cfg.access.approval_required = on;
        Ok(())
    })?;
    // Служба перечитает настройку и сама поправит правила.
    let _ = ipc::try_request(&Request::ReloadConfig);
    let state = t(ctx.lang, if on { Msg::Yes } else { Msg::No });
    println!("{}", tf(ctx.lang, Msg::AccessApprovalSetFmt, &[state]));
    Ok(())
}

/// Интерфейс точки доступа, пока раздача включена (для правил брандмауэра).
fn ap_iface() -> Option<Iface> {
    match with_hotspot(|h| h.state()) {
        Ok(HotspotState::On { ap_iface, .. }) => Iface::parse(&ap_iface).ok(),
        _ => None,
    }
}

pub fn devices(ctx: &Ctx) -> anyhow::Result<()> {
    let lang = ctx.lang;
    let (cfg, _) = ctx.load_config()?;
    let HotspotState::On { ap_iface, .. } = with_hotspot(|h| h.state())? else {
        println!("{}", t(lang, Msg::DevicesNeedOn));
        return Ok(());
    };
    // У службы список уже с состояниями; без неё смотрим сами.
    let list = match ipc::try_request(&Request::GetDevices)? {
        Some(Response::Devices { list }) => list,
        _ => {
            let leases = with_hotspot(|h| h.leases(&ap_iface)).unwrap_or_default();
            let mut list = devices::scan(&ap_iface, &leases)?;
            AccessLists::from_config(&cfg, false).annotate(&mut list);
            list
        }
    };
    println!(
        "{}",
        tf(lang, Msg::TuiSectionDevicesFmt, &[&list.len().to_string()])
    );
    if list.is_empty() {
        println!("  {}", t(lang, Msg::TuiDevicesEmpty));
    }
    for d in &list {
        println!("  {}", device_row(lang, d));
    }
    Ok(())
}

fn device_row(lang: Lang, d: &Device) -> String {
    let bars = bars_str(d.signal_dbm.map_or(0, signal_bars));
    let ip = d.ip.map(|ip| ip.to_string()).unwrap_or_default();
    let status = t(
        lang,
        match d.status {
            DeviceStatus::Allowed => Msg::StatusAllowed,
            DeviceStatus::Pending => Msg::StatusPending,
            DeviceStatus::Blocked => Msg::StatusBlocked,
        },
    );
    format!(
        "󰄜 {:<20} {bars}  {:<15} {}  {status}",
        crate::cli::truncate_chars(&d.display_name(), 20),
        ip,
        d.mac.as_str()
    )
}
