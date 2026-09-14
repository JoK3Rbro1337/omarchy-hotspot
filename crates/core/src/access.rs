//! Списки доступа устройств: одобренные и чёрный список (этап 5).
//! Одно изменение меняет и брандмауэр (через помощника), и конфиг — чтобы после
//! перезапуска раздачи правила были те же.

use crate::CoreError;
use crate::config::Config;
use crate::devices::{Device, DeviceStatus};
use crate::helper::HelperRunner;
use crate::helper_proto::{HelperRequest, Iface, MAX_MAC_LIST, Mac};

/// Что делаем со списками доступа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessChange {
    /// Разрешить (и убрать из чёрного списка, если был).
    Allow(Mac),
    /// В чёрный список: доступ закрыт, устройство отключается от сети.
    Block(Mac),
    /// Убрать из чёрного списка. В одобренные при этом не попадает.
    Unblock(Mac),
}

impl AccessChange {
    pub fn mac(&self) -> &Mac {
        match self {
            AccessChange::Allow(m) | AccessChange::Block(m) | AccessChange::Unblock(m) => m,
        }
    }
}

/// Разобранные списки из конфига: плохие строки пропускаем (о них уже сказал разбор конфига).
#[derive(Debug, Clone, Default)]
pub struct AccessLists {
    pub allowed: Vec<Mac>,
    pub blocked: Vec<Mac>,
    /// Режим одобрения действительно работает (раздача включена с белым списком).
    pub approval: bool,
}

impl AccessLists {
    pub fn from_config(cfg: &Config, approval: bool) -> AccessLists {
        AccessLists {
            allowed: parse_list(&cfg.access.allowed_macs),
            blocked: parse_list(&cfg.access.blocked_macs),
            approval,
        }
    }

    pub fn status(&self, mac: &Mac) -> DeviceStatus {
        if self.blocked.contains(mac) {
            DeviceStatus::Blocked
        } else if self.approval && !self.allowed.contains(mac) {
            DeviceStatus::Pending
        } else {
            DeviceStatus::Allowed
        }
    }

    /// Проставить состояние каждому устройству в списке.
    pub fn annotate(&self, devices: &mut [Device]) {
        for d in devices.iter_mut() {
            d.status = self.status(&d.mac);
        }
    }
}

fn parse_list(list: &[String]) -> Vec<Mac> {
    list.iter()
        .filter_map(|s| match Mac::parse(s) {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::warn!("bad MAC {s:?} in config: {e}");
                None
            }
        })
        .collect()
}

/// Применить изменение: сначала брандмауэр, потом конфиг (при ошибке конфиг не трогаем).
/// `ap` задан, только когда раздача включена и правила применены — иначе помощника не зовём.
/// Возвращает `true`, если конфиг изменился и его нужно сохранить.
/// Вызывать под замком конфига (`config::update`): правило брандмауэра и запись в конфиг
/// должны меняться вместе.
pub fn apply(
    helper: &dyn HelperRunner,
    ap: Option<&Iface>,
    cfg: &mut Config,
    change: &AccessChange,
) -> Result<bool, CoreError> {
    // Место в списке проверяем до брандмауэра: иначе правило уже стояло бы, а записи в конфиге
    // не было — устройство с доступом, о котором мы «не знаем».
    room_for(cfg, change)?;
    let was_blocked = has(&cfg.access.blocked_macs, change.mac());
    let was_allowed = has(&cfg.access.allowed_macs, change.mac());
    match change {
        AccessChange::Allow(mac) => {
            if let Some(_ap) = ap {
                if was_blocked {
                    helper.call(&HelperRequest::FwUnblock { mac: mac.clone() })?;
                }
                helper.call(&HelperRequest::FwAllowAdd { mac: mac.clone() })?;
            }
            let mut changed = remove(&mut cfg.access.blocked_macs, mac);
            changed |= add(&mut cfg.access.allowed_macs, mac)?;
            Ok(changed)
        }
        AccessChange::Block(mac) => {
            if let Some(ap) = ap {
                helper.call(&HelperRequest::FwBlock { mac: mac.clone() })?;
                if was_allowed {
                    helper.call(&HelperRequest::FwAllowRemove { mac: mac.clone() })?;
                }
                // Отключение от Wi-Fi — не главное: доступ уже закрыт правилом. Если
                // устройства нет в эфире, `station-kick` ругается — это не ошибка.
                if let Err(e) = helper.call(&HelperRequest::StationKick {
                    ap: ap.clone(),
                    mac: mac.clone(),
                }) {
                    tracing::debug!("cannot kick {}: {e}", mac.as_str());
                }
            }
            let mut changed = remove(&mut cfg.access.allowed_macs, mac);
            changed |= add(&mut cfg.access.blocked_macs, mac)?;
            Ok(changed)
        }
        AccessChange::Unblock(mac) => {
            if ap.is_some() && was_blocked {
                helper.call(&HelperRequest::FwUnblock { mac: mac.clone() })?;
            }
            Ok(remove(&mut cfg.access.blocked_macs, mac))
        }
    }
}

/// Влезет ли ещё один MAC в нужный список.
fn room_for(cfg: &Config, change: &AccessChange) -> Result<(), CoreError> {
    let (list, mac) = match change {
        AccessChange::Allow(m) => (&cfg.access.allowed_macs, m),
        AccessChange::Block(m) => (&cfg.access.blocked_macs, m),
        AccessChange::Unblock(_) => return Ok(()),
    };
    if !has(list, mac) && list.len() >= MAX_MAC_LIST {
        return Err(list_full());
    }
    Ok(())
}

fn list_full() -> CoreError {
    CoreError::Config(format!("MAC list is full ({MAX_MAC_LIST})"))
}

fn has(list: &[String], mac: &Mac) -> bool {
    list.iter().any(|s| s.eq_ignore_ascii_case(mac.as_str()))
}

/// Добавить, если ещё нет. Список ограничен — иначе конфиг рос бы без предела.
fn add(list: &mut Vec<String>, mac: &Mac) -> Result<bool, CoreError> {
    if has(list, mac) {
        return Ok(false);
    }
    if list.len() >= MAX_MAC_LIST {
        return Err(list_full());
    }
    list.push(mac.as_str().to_string());
    Ok(true)
}

fn remove(list: &mut Vec<String>, mac: &Mac) -> bool {
    let before = list.len();
    list.retain(|s| !s.eq_ignore_ascii_case(mac.as_str()));
    list.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::mock::MockHelper;

    fn mac(last: &str) -> Mac {
        Mac::parse(&format!("aa:bb:cc:dd:ee:{last}")).unwrap()
    }

    fn ap() -> Iface {
        Iface::parse("wlp15s0").unwrap()
    }

    #[test]
    fn allow_moves_mac_from_blocked_to_allowed() {
        let h = MockHelper::installed();
        let mut cfg = Config::default();
        cfg.access.blocked_macs = vec!["AA:BB:CC:DD:EE:01".into()];
        let changed = apply(&h, Some(&ap()), &mut cfg, &AccessChange::Allow(mac("01"))).unwrap();
        assert!(changed);
        assert!(cfg.access.blocked_macs.is_empty());
        assert_eq!(cfg.access.allowed_macs, ["aa:bb:cc:dd:ee:01"]);
        assert_eq!(h.names(), ["fw-unblock", "fw-allow-add"]);
        // Повторное разрешение конфиг не меняет.
        assert!(!apply(&h, Some(&ap()), &mut cfg, &AccessChange::Allow(mac("01"))).unwrap());
    }

    #[test]
    fn block_kicks_and_removes_from_allowed() {
        let h = MockHelper::installed();
        let mut cfg = Config::default();
        cfg.access.allowed_macs = vec!["aa:bb:cc:dd:ee:02".into()];
        assert!(apply(&h, Some(&ap()), &mut cfg, &AccessChange::Block(mac("02"))).unwrap());
        assert!(cfg.access.allowed_macs.is_empty());
        assert_eq!(cfg.access.blocked_macs, ["aa:bb:cc:dd:ee:02"]);
        assert_eq!(h.names(), ["fw-block", "fw-allow-remove", "station-kick"]);
    }

    #[test]
    fn without_hotspot_only_config_changes() {
        let h = MockHelper::installed();
        let mut cfg = Config::default();
        assert!(apply(&h, None, &mut cfg, &AccessChange::Block(mac("03"))).unwrap());
        assert!(apply(&h, None, &mut cfg, &AccessChange::Unblock(mac("03"))).unwrap());
        assert!(h.names().is_empty());
        assert!(cfg.access.blocked_macs.is_empty());
    }

    #[test]
    fn helper_failure_keeps_config_untouched() {
        let mut h = MockHelper::installed();
        h.fail = true;
        let mut cfg = Config::default();
        assert!(apply(&h, Some(&ap()), &mut cfg, &AccessChange::Allow(mac("04"))).is_err());
        assert!(cfg.access.allowed_macs.is_empty());
    }

    #[test]
    fn statuses_follow_lists() {
        let mut cfg = Config::default();
        cfg.access.allowed_macs = vec!["aa:bb:cc:dd:ee:01".into()];
        cfg.access.blocked_macs = vec!["aa:bb:cc:dd:ee:02".into()];
        let lists = AccessLists::from_config(&cfg, true);
        assert_eq!(lists.status(&mac("01")), DeviceStatus::Allowed);
        assert_eq!(lists.status(&mac("02")), DeviceStatus::Blocked);
        assert_eq!(lists.status(&mac("03")), DeviceStatus::Pending);
        // Без режима одобрения незнакомое устройство — обычное.
        let lists = AccessLists::from_config(&cfg, false);
        assert_eq!(lists.status(&mac("03")), DeviceStatus::Allowed);
        assert_eq!(lists.status(&mac("02")), DeviceStatus::Blocked);
    }

    #[test]
    fn list_is_capped() {
        let mut cfg = Config::default();
        cfg.access.blocked_macs = (0..MAX_MAC_LIST)
            .map(|i| format!("02:00:00:00:{:02x}:{:02x}", i / 256, i % 256))
            .collect();
        let h = MockHelper::installed();
        assert!(matches!(
            apply(&h, Some(&ap()), &mut cfg, &AccessChange::Block(mac("ff"))),
            Err(CoreError::Config(_))
        ));
        // Главное: до брандмауэра дело не дошло, иначе правило стояло бы без записи в конфиге.
        assert!(h.names().is_empty());
        // Уже записанный MAC переполнения не вызывает.
        let last = Mac::parse(&cfg.access.blocked_macs[0]).unwrap();
        assert!(!apply(&h, None, &mut cfg, &AccessChange::Block(last)).unwrap());
    }
}
