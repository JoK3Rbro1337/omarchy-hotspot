//! Правила ufw с комментарием `omarchy-hotspot` (SECURITY.md §3.2). Чужие правила не трогаем.

use omarchy_hotspot_proto::Iface;

use crate::run::{self, UFW};

pub const COMMENT: &str = "omarchy-hotspot";

pub fn is_active() -> anyhow::Result<bool> {
    let done = run::exec(UFW, &["status"], None)?;
    Ok(done.ok() && parse_active(&done.stdout))
}

pub fn parse_active(status: &str) -> bool {
    status
        .lines()
        .next()
        .is_some_and(|l| l.trim() == "Status: active")
}

/// Номера наших правил из `ufw status numbered`, по убыванию (удалять с конца).
pub fn parse_our_numbers(numbered: &str) -> Vec<u32> {
    let mut nums: Vec<u32> = numbered
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if !l.ends_with(&format!("# {COMMENT}")) {
                return None;
            }
            let inner = l.strip_prefix('[')?;
            let end = inner.find(']')?;
            inner[..end].trim().parse().ok()
        })
        .collect();
    nums.sort_unstable_by(|a, b| b.cmp(a));
    nums.dedup();
    nums
}

/// Удалить все наши правила. ufw неактивен или правил нет — ничего не делаем.
/// Каждое удаление — после свежего чтения номеров: сдвиг нумерации не может задеть чужое правило.
pub fn clear() -> anyhow::Result<()> {
    // Предел на случай, если ufw почему-то не удаляет: не крутиться вечно.
    for _ in 0..(2 * MAX_OUR_RULES) {
        let done = run::exec(UFW, &["status", "numbered"], None)?;
        if !done.ok() || !parse_active(&done.stdout) {
            return Ok(());
        }
        let Some(n) = parse_our_numbers(&done.stdout).first().copied() else {
            return Ok(());
        };
        run::run(UFW, &["--force", "delete", &n.to_string()], None)?;
    }
    anyhow::bail!("ufw: could not remove all omarchy-hotspot rules")
}

/// 4 правила × (v4 + v6).
const MAX_OUR_RULES: usize = 8;

/// Добавить четыре разрешения (§3.2). Перед этим убираем старые наши — так нет дубликатов.
pub fn apply(ap: &Iface, uplink: &Iface) -> anyhow::Result<()> {
    clear()?;
    let ap = ap.as_str();
    let uplink = uplink.as_str();
    let rules: [Vec<&str>; 4] = [
        vec![
            "allow", "in", "on", ap, "proto", "udp", "from", "any", "port", "68", "to", "any",
            "port", "67", "comment", COMMENT,
        ],
        vec![
            "allow", "in", "on", ap, "proto", "udp", "to", "any", "port", "53", "comment", COMMENT,
        ],
        vec![
            "allow", "in", "on", ap, "proto", "tcp", "to", "any", "port", "53", "comment", COMMENT,
        ],
        vec![
            "route", "allow", "in", "on", ap, "out", "on", uplink, "comment", COMMENT,
        ],
    ];
    for r in &rules {
        run::run(UFW, r, None)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_status() {
        assert!(parse_active("Status: active\n\nTo  Action  From\n"));
        assert!(!parse_active("Status: inactive\n"));
        assert!(!parse_active(""));
    }

    #[test]
    fn numbers_only_ours_descending() {
        let s = "Status: active\n\n     To                         Action      From\n\
                 [ 1] 53317/tcp                  ALLOW IN    Anywhere\n\
                 [ 2] 67/udp on wlp15s0          ALLOW IN    Anywhere                   # omarchy-hotspot\n\
                 [ 3] Anywhere on enp14s0        ALLOW FWD   Anywhere on wlp15s0        # omarchy-hotspot\n\
                 [ 4] 53317/tcp (v6)             ALLOW IN    Anywhere (v6)\n\
                 [12] 67/udp (v6) on wlp15s0     ALLOW IN    Anywhere (v6)              # omarchy-hotspot\n\
                 [13] 22/tcp                     ALLOW IN    Anywhere  # omarchy-hotspot-not-really\n";
        assert_eq!(parse_our_numbers(s), vec![12, 3, 2]);
        assert!(parse_our_numbers("Status: inactive\n").is_empty());
    }
}
