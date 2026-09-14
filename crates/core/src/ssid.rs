//! Проверка имени сети (SSID).

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SsidError {
    #[error("SSID is empty")]
    Empty,
    #[error("SSID is longer than 32 bytes")]
    TooLong,
    #[error("SSID contains control characters")]
    ControlChars,
}

/// Максимальная длина SSID в байтах (стандарт 802.11).
pub const SSID_MAX_BYTES: usize = 32;

/// SSID: 1–32 байта UTF-8, без управляющих символов.
pub fn validate_ssid(ssid: &str) -> Result<(), SsidError> {
    if ssid.is_empty() {
        return Err(SsidError::Empty);
    }
    if ssid.len() > SSID_MAX_BYTES {
        return Err(SsidError::TooLong);
    }
    if ssid.chars().any(char::is_control) {
        return Err(SsidError::ControlChars);
    }
    Ok(())
}

/// Обрезает строку до `max` байт по границе символа.
pub fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates() {
        assert_eq!(validate_ssid("Omarchy-pc"), Ok(()));
        assert_eq!(validate_ssid("Сеть Жени"), Ok(()));
        assert_eq!(validate_ssid(""), Err(SsidError::Empty));
        assert_eq!(validate_ssid(&"a".repeat(32)), Ok(()));
        assert_eq!(validate_ssid(&"a".repeat(33)), Err(SsidError::TooLong));
        // 17 кириллических букв = 34 байта
        assert_eq!(validate_ssid(&"ж".repeat(17)), Err(SsidError::TooLong));
        assert_eq!(validate_ssid("a\nb"), Err(SsidError::ControlChars));
        assert_eq!(validate_ssid("a\u{7f}"), Err(SsidError::ControlChars));
    }

    #[test]
    fn truncates_on_char_boundary() {
        assert_eq!(truncate_bytes("жжж", 5), "жж");
        assert_eq!(truncate_bytes("abc", 5), "abc");
    }
}
