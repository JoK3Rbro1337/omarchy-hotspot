use crate::CoreError;
use crate::net::cmd;

const PROBE: &str = "1.1.1.1";

/// Интерфейс, через который ПК ходит в интернет; `None` — маршрута нет.
pub fn detect_uplink() -> Result<Option<String>, CoreError> {
    match cmd::run("ip", &["-j", "route", "get", PROBE]) {
        Ok(json) => parse_route_get(&json),
        Err(CoreError::ToolFailed { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn parse_route_get(json: &str) -> Result<Option<String>, CoreError> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| CoreError::Parse {
        tool: "ip",
        reason: e.to_string(),
    })?;
    Ok(v.get(0)
        .and_then(|r| r.get("dev"))
        .and_then(|d| d.as_str())
        .map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dev() {
        let j = r#"[{"dst":"1.1.1.1","gateway":"192.168.1.1","dev":"enp14s0","prefsrc":"192.168.1.5"}]"#;
        assert_eq!(parse_route_get(j).unwrap(), Some("enp14s0".into()));
        assert_eq!(parse_route_get("[]").unwrap(), None);
        assert!(parse_route_get("not json").is_err());
    }
}
