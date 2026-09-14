//! QR-код для подключения к Wi-Fi: строка формата `WIFI:` и отрисовка полублоками.

use qrcode::{EcLevel, QrCode};

use crate::backend::Security;
use crate::secret::Secret;

/// Экранирование спецсимволов формата `WIFI:` (`\ ; , : "`).
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | ';' | ',' | ':' | '"') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `WIFI:T:WPA;S:<ssid>;P:<пароль>;[H:true;];`
///
/// Для WPA3 тоже `T:WPA`: Android и iOS так понимают WPA3-Personal (см. DECISIONS.md §10).
/// Результат содержит пароль — не логировать.
pub fn wifi_qr_string(ssid: &str, psk: &Secret, security: Security, hidden: bool) -> Secret {
    let t = match security {
        Security::Wpa3 | Security::Wpa2 => "WPA",
    };
    let mut s = format!("WIFI:T:{t};S:{};P:{};", escape(ssid), escape(psk.expose()));
    if hidden {
        s.push_str("H:true;");
    }
    s.push(';');
    Secret::new(s)
}

/// Матрица QR: `true` — тёмный модуль. С «тихой зоной» `quiet` модулей со всех сторон.
/// Коррекция ошибок L: код на экране не пачкается и не мнётся, а модулей меньше — строка
/// с именем сети и паролем умещается в версию 3 (29×29) вместо 4 (33×33). Проверено
/// декодерами zbar и ZXing на картинках «как в терминале» вплоть до ячейки 2×4 px
/// с тихой зоной в 1 модуль (docs/DECISIONS.md §8.2).
pub fn matrix(data: &str, quiet: usize) -> Option<Vec<Vec<bool>>> {
    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::L).ok()?;
    let w = code.width();
    let colors = code.to_colors();
    let size = w + quiet * 2;
    let mut m = vec![vec![false; size]; size];
    for y in 0..w {
        for x in 0..w {
            m[y + quiet][x + quiet] = colors[y * w + x] == qrcode::Color::Dark;
        }
    }
    Some(m)
}

/// Строки из полублоков: один символ = два модуля по вертикали.
/// Рассчитано на тёмный текст на светлом фоне (цвета задаёт вызывающий).
pub fn half_blocks(m: &[Vec<bool>]) -> Vec<String> {
    m.chunks(2)
        .map(|pair| {
            let top = &pair[0];
            let bottom = pair.get(1);
            top.iter()
                .enumerate()
                .map(|(x, &t)| {
                    let b = bottom.is_some_and(|row| row[x]);
                    match (t, b) {
                        (true, true) => '█',
                        (true, false) => '▀',
                        (false, true) => '▄',
                        (false, false) => ' ',
                    }
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_special_chars() {
        assert_eq!(escape(r#"a\b;c,d:e"f"#), r#"a\\b\;c\,d\:e\"f"#);
        assert_eq!(escape("Сеть 1"), "Сеть 1");
    }

    #[test]
    fn builds_wifi_string() {
        let psk = Secret::new("p;ss:w\"rd");
        let s = wifi_qr_string("My,Net", &psk, Security::Wpa3, false);
        assert_eq!(s.expose(), r#"WIFI:T:WPA;S:My\,Net;P:p\;ss\:w\"rd;;"#);
        let h = wifi_qr_string("N", &Secret::new("12345678"), Security::Wpa2, true);
        assert_eq!(h.expose(), "WIFI:T:WPA;S:N;P:12345678;H:true;;");
    }

    #[test]
    fn renders_matrix() {
        let m = matrix("WIFI:T:WPA;S:x;P:12345678;;", 2).unwrap();
        // Версия 2 (25 модулей) + по 2 модуля тихой зоны.
        assert_eq!(m.len(), 29);
        assert!(m[0].iter().all(|&d| !d)); // тихая зона светлая
        assert!(m[2][2]); // угол поискового узора тёмный
        let lines = half_blocks(&m);
        assert_eq!(lines.len(), 15);
        assert!(lines.iter().all(|l| l.chars().count() == 29));
    }
}
