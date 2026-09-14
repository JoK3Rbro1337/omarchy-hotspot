//! Таблица `inet omarchy_hotspot` (SECURITY.md §3.1). Только она создаётся, меняется и удаляется.

use anyhow::bail;
use omarchy_hotspot_proto::{Iface, Mac};

use crate::run::{self, NFT};

pub const TABLE: &str = "omarchy_hotspot";
pub const SET_BLOCKED: &str = "blocked_macs";
pub const SET_ALLOWED: &str = "allowed_macs";

pub struct Rules<'a> {
    pub ap: &'a Iface,
    pub uplink: &'a Iface,
    pub approval: bool,
    pub guests_reach_pc: bool,
    pub allow: &'a [Mac],
    pub block: &'a [Mac],
}

fn set_body(macs: &[Mac]) -> String {
    if macs.is_empty() {
        return String::new();
    }
    let list: Vec<&str> = macs.iter().map(Mac::as_str).collect();
    format!(" elements = {{ {} }};", list.join(", "))
}

/// Полный текст для `nft -f -`: старая таблица заменяется атомарно.
pub fn ruleset(r: &Rules<'_>) -> String {
    let ap = r.ap.as_str();
    let uplink = r.uplink.as_str();
    let pc_drop = if r.guests_reach_pc {
        ""
    } else {
        "        drop\n"
    };
    let approval = if r.approval {
        format!("        ether saddr != @{SET_ALLOWED} drop\n")
    } else {
        String::new()
    };
    format!(
        "table inet {TABLE} {{}}\n\
         delete table inet {TABLE}\n\
         table inet {TABLE} {{\n\
         \x20   set {SET_BLOCKED} {{ type ether_addr;{} }}\n\
         \x20   set {SET_ALLOWED} {{ type ether_addr;{} }}\n\
         \x20   chain input {{\n\
         \x20       type filter hook input priority filter; policy accept;\n\
         \x20       iifname \"{ap}\" jump from_ap_input\n\
         \x20   }}\n\
         \x20   chain from_ap_input {{\n\
         \x20       ether saddr @{SET_BLOCKED} drop\n\
         \x20       ct state established,related accept\n\
         \x20       ct state invalid drop\n\
         \x20       udp dport 67 accept\n\
         \x20       udp dport 53 accept\n\
         \x20       tcp dport 53 accept\n\
         {pc_drop}\
         \x20   }}\n\
         \x20   chain forward {{\n\
         \x20       type filter hook forward priority filter; policy accept;\n\
         \x20       iifname \"{ap}\" jump from_ap_forward\n\
         \x20   }}\n\
         \x20   chain from_ap_forward {{\n\
         \x20       ether saddr @{SET_BLOCKED} drop\n\
         \x20       oifname != \"{uplink}\" drop\n\
         {approval}\
         \x20   }}\n\
         }}\n",
        set_body(r.block),
        set_body(r.allow),
    )
}

pub fn apply(r: &Rules<'_>) -> anyhow::Result<()> {
    run::run(NFT, &["-f", "-"], Some(&ruleset(r)))?;
    Ok(())
}

pub fn table_exists() -> anyhow::Result<bool> {
    Ok(run::exec(NFT, &["list", "table", "inet", TABLE], None)?.ok())
}

/// Удалить таблицу; её отсутствие — не ошибка.
pub fn clear() -> anyhow::Result<()> {
    if table_exists()? {
        run::run(NFT, &["delete", "table", "inet", TABLE], None)?;
    }
    Ok(())
}

fn require_table() -> anyhow::Result<()> {
    if !table_exists()? {
        bail!("firewall table is not applied (hotspot is off?)");
    }
    Ok(())
}

pub fn set_add(set: &str, mac: &Mac) -> anyhow::Result<()> {
    require_table()?;
    let elem = format!("{{ {} }}", mac.as_str());
    run::run(NFT, &["add", "element", "inet", TABLE, set, &elem], None)?;
    Ok(())
}

/// Убрать элемент; если его нет — не ошибка.
pub fn set_remove(set: &str, mac: &Mac) -> anyhow::Result<()> {
    require_table()?;
    let elem = format!("{{ {} }}", mac.as_str());
    let present = run::exec(NFT, &["get", "element", "inet", TABLE, set, &elem], None)?.ok();
    if present {
        run::run(NFT, &["delete", "element", "inet", TABLE, set, &elem], None)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules<'a>(approval: bool, pc: bool, allow: &'a [Mac], block: &'a [Mac]) -> String {
        let ap = Iface::parse("wlp15s0").unwrap();
        let up = Iface::parse("enp14s0").unwrap();
        ruleset(&Rules {
            ap: &ap,
            uplink: &up,
            approval,
            guests_reach_pc: pc,
            allow,
            block,
        })
    }

    #[test]
    fn default_ruleset_hides_pc_and_isolates() {
        let s = rules(false, false, &[], &[]);
        assert!(
            s.starts_with("table inet omarchy_hotspot {}\ndelete table inet omarchy_hotspot\n")
        );
        assert!(s.contains("iifname \"wlp15s0\" jump from_ap_input"));
        assert!(s.contains("udp dport 67 accept"));
        assert!(s.contains("        drop\n    }\n    chain forward"));
        assert!(s.contains("oifname != \"enp14s0\" drop"));
        assert!(!s.contains("allowed_macs drop"));
        assert!(s.contains("set blocked_macs { type ether_addr; }"));
    }

    #[test]
    fn options_change_rules() {
        let m = Mac::parse("aa:bb:cc:dd:ee:01").unwrap();
        let s = rules(
            true,
            true,
            std::slice::from_ref(&m),
            std::slice::from_ref(&m),
        );
        assert!(s.contains("ether saddr != @allowed_macs drop"));
        assert!(!s.contains("        drop\n    }\n    chain forward"));
        assert!(
            s.contains("set allowed_macs { type ether_addr; elements = { aa:bb:cc:dd:ee:01 }; }")
        );
        assert!(
            s.contains("set blocked_macs { type ether_addr; elements = { aa:bb:cc:dd:ee:01 }; }")
        );
    }
}
