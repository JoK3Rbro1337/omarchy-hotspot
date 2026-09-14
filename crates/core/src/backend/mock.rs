//! Заглушка NetworkManager для тестов: всё хранится в памяти.

use std::sync::Mutex;
use std::time::SystemTime;

use super::{HotspotSettings, HotspotState, NetworkBackend};
use crate::CoreError;
use crate::secret::Secret;

#[derive(Debug, Default)]
pub struct MockState {
    pub nm_running: bool,
    pub wifi_ifaces: Vec<String>,
    pub profile: Option<HotspotSettings>,
    pub password: Option<Secret>,
    pub active: bool,
    /// Сделать следующий `up` неудачным.
    pub fail_up: bool,
    /// Сделать следующий `down` неудачным (раздача остаётся включённой).
    pub fail_down: bool,
    /// Сеть, к которой Wi-Fi-адаптер подключён как клиент.
    pub client_connection: Option<String>,
    pub up_calls: usize,
    pub down_calls: usize,
}

#[derive(Debug, Default)]
pub struct MockBackend {
    pub s: Mutex<MockState>,
}

impl MockBackend {
    pub fn new(ifaces: &[&str]) -> Self {
        MockBackend {
            s: Mutex::new(MockState {
                nm_running: true,
                wifi_ifaces: ifaces.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }),
        }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut MockState) -> R) -> R {
        f(&mut self.s.lock().unwrap())
    }
}

impl NetworkBackend for MockBackend {
    fn profile_exists(&self) -> Result<bool, CoreError> {
        Ok(self.with(|s| s.profile.is_some()))
    }

    fn ensure_profile(&self, new: &HotspotSettings) -> Result<(), CoreError> {
        self.with(|s| {
            if let Some(p) = &new.password {
                s.password = Some(p.clone());
            }
            let mut stored = new.clone();
            stored.password = None;
            s.profile = Some(stored);
        });
        Ok(())
    }

    fn set_password(&self, psk: &Secret) -> Result<(), CoreError> {
        self.with(|s| {
            if s.profile.is_none() {
                return Err(CoreError::NoProfile);
            }
            s.password = Some(psk.clone());
            Ok(())
        })
    }

    fn get_password(&self) -> Result<Secret, CoreError> {
        self.with(|s| s.password.clone().ok_or(CoreError::NoProfile))
    }

    fn up(&self) -> Result<(), CoreError> {
        self.with(|s| {
            s.up_calls += 1;
            if s.profile.is_none() {
                return Err(CoreError::NoProfile);
            }
            if std::mem::take(&mut s.fail_up) {
                return Err(CoreError::ActivationFailed("mock".into()));
            }
            s.active = true;
            Ok(())
        })
    }

    fn down(&self) -> Result<(), CoreError> {
        self.with(|s| {
            s.down_calls += 1;
            if std::mem::take(&mut s.fail_down) {
                return Err(CoreError::ActivationFailed("mock".into()));
            }
            s.active = false;
            Ok(())
        })
    }

    fn delete_profile(&self) -> Result<(), CoreError> {
        self.with(|s| {
            s.active = false;
            s.profile = None;
            s.password = None;
        });
        Ok(())
    }

    fn state(&self) -> Result<HotspotState, CoreError> {
        Ok(self.with(|s| match (&s.profile, s.active) {
            (Some(p), true) => HotspotState::On {
                since: Some(SystemTime::UNIX_EPOCH),
                ap_iface: p.ap_iface.clone(),
                uplink: Some("enp14s0".into()),
            },
            _ => HotspotState::Off,
        }))
    }

    fn wifi_ifaces(&self) -> Result<Vec<String>, CoreError> {
        Ok(self.with(|s| s.wifi_ifaces.clone()))
    }

    fn client_connection(&self, _iface: &str) -> Result<Option<String>, CoreError> {
        Ok(self.s.lock().unwrap().client_connection.clone())
    }

    fn connected_ifaces(&self) -> Result<Vec<String>, CoreError> {
        Ok(vec!["enp14s0".into()])
    }

    fn nm_running(&self) -> Result<bool, CoreError> {
        Ok(self.with(|s| s.nm_running))
    }
}
