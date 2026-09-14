//! Генератор пароля и проверка пароля пользователя.

use rand::TryRng;
use rand::rngs::SysRng;

use crate::CoreError;
use crate::secret::Secret;

/// Без похожих символов: l, 1, I, O, 0.
const ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
pub const GENERATED_LEN: usize = 16;
/// Ограничения WPA на пароль (passphrase): 8–63 печатных ASCII-символа.
pub const MIN_LEN: usize = 8;
pub const MAX_LEN: usize = 63;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strength {
    TooShort,
    Weak,
    Ok,
    Strong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PasswordError {
    #[error("password is shorter than 8 characters")]
    TooShort,
    #[error("password is longer than 63 characters")]
    TooLong,
    #[error("password must contain only latin letters, digits and symbols")]
    NotAscii,
}

/// Случайный пароль из 16 символов (системный криптостойкий генератор).
pub fn generate() -> Result<Secret, CoreError> {
    let mut rng = SysRng;
    loop {
        let mut out = String::with_capacity(GENERATED_LEN);
        for _ in 0..GENERATED_LEN {
            out.push(pick(&mut rng)? as char);
        }
        // Гарантируем все три класса символов (отбраковка не портит равномерность).
        if classes(&out) == 3 {
            return Ok(Secret::new(out));
        }
    }
}

/// Равномерный выбор символа: отбрасываем значения из «хвоста», который дал бы перекос.
fn pick(rng: &mut SysRng) -> Result<u8, CoreError> {
    let n = ALPHABET.len() as u32;
    let zone = u32::MAX - (u32::MAX % n);
    loop {
        let v = rng
            .try_next_u32()
            .map_err(|e| CoreError::Io(std::io::Error::other(e.to_string())))?;
        if v < zone {
            return Ok(ALPHABET[(v % n) as usize]);
        }
    }
}

/// Сколько классов символов: строчные, заглавные, цифры, прочие.
fn classes(p: &str) -> usize {
    let checks: [fn(&char) -> bool; 4] = [
        char::is_ascii_lowercase,
        char::is_ascii_uppercase,
        char::is_ascii_digit,
        |c| !c.is_ascii_alphanumeric(),
    ];
    checks.iter().filter(|f| p.chars().any(|c| f(&c))).count()
}

const COMMON: &[&str] = &[
    "password",
    "passw0rd",
    "qwerty",
    "qwertyui",
    "12345678",
    "123456789",
    "1234567890",
    "11111111",
    "00000000",
    "iloveyou",
    "admin123",
    "abcdefgh",
    "abc12345",
    "omarchy",
    "letmein",
    "welcome",
];

/// Оценка пароля: <8 — слишком короткий; <12, один класс символов, повторы или
/// частый пароль — слабый; ≥16 и ≥3 классов — сильный.
pub fn check_strength(p: &str) -> Strength {
    let len = p.chars().count();
    if len < MIN_LEN {
        return Strength::TooShort;
    }
    let lower = p.to_ascii_lowercase();
    let one_char = p.chars().all(|c| p.starts_with(c));
    let common = COMMON.iter().any(|w| lower.contains(w));
    let sequence = is_sequence(p);
    if len < 12 || classes(p) < 2 || one_char || common || sequence {
        return Strength::Weak;
    }
    if len >= 16 && classes(p) >= 3 {
        Strength::Strong
    } else {
        Strength::Ok
    }
}

/// «abcdefghijkl», «987654321…» — возрастающая или убывающая последовательность.
fn is_sequence(p: &str) -> bool {
    let b = p.as_bytes();
    let step_all = |d: i16| b.windows(2).all(|w| w[1] as i16 - w[0] as i16 == d);
    step_all(1) || step_all(-1)
}

/// Проверка пароля, введённого пользователем. Ошибка — если WPA его не примет.
pub fn validate_user_password(p: &str) -> Result<Strength, PasswordError> {
    if !p.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return Err(PasswordError::NotAscii);
    }
    if p.len() < MIN_LEN {
        return Err(PasswordError::TooShort);
    }
    if p.len() > MAX_LEN {
        return Err(PasswordError::TooLong);
    }
    Ok(check_strength(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_password_is_valid() {
        for _ in 0..200 {
            let s = generate().unwrap();
            let p = s.expose();
            assert_eq!(p.len(), GENERATED_LEN);
            assert!(p.bytes().all(|b| ALPHABET.contains(&b)));
            assert!(!p.contains(['l', '1', 'I', 'O', '0']));
            assert_eq!(validate_user_password(p), Ok(Strength::Strong));
        }
    }

    #[test]
    fn generated_passwords_differ() {
        assert_ne!(generate().unwrap(), generate().unwrap());
    }

    #[test]
    fn alphabet_has_no_lookalikes() {
        assert_eq!(ALPHABET.len(), 57);
        for c in *b"l1IO0" {
            assert!(!ALPHABET.contains(&c));
        }
    }

    #[test]
    fn strength() {
        assert_eq!(check_strength("short"), Strength::TooShort);
        assert_eq!(check_strength("abcdefgh"), Strength::Weak);
        assert_eq!(check_strength("Xy7kQ2mZ"), Strength::Weak); // < 12
        assert_eq!(check_strength("aaaaaaaaaaaaaaaa"), Strength::Weak);
        assert_eq!(check_strength("myPassword2024!!"), Strength::Weak); // частое слово
        assert_eq!(check_strength("123456789012"), Strength::Weak); // один класс
        assert_eq!(check_strength("Hk4mZ9pQ2rTw"), Strength::Ok);
        assert_eq!(check_strength("Hk4mZ9pQ2rTwXv7n"), Strength::Strong);
    }

    #[test]
    fn user_password_limits() {
        assert_eq!(
            validate_user_password("1234567"),
            Err(PasswordError::TooShort)
        );
        assert_eq!(
            validate_user_password(&"a".repeat(64)),
            Err(PasswordError::TooLong)
        );
        assert_eq!(
            validate_user_password("пароль123456"),
            Err(PasswordError::NotAscii)
        );
        assert_eq!(
            validate_user_password("tab\there12"),
            Err(PasswordError::NotAscii)
        );
        assert!(validate_user_password("with space and ; : \\ \"").is_ok());
    }
}
