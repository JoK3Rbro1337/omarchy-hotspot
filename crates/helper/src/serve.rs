//! Режим службы (SECURITY.md §5): строка JSON на вход — строка JSON на выход.

use std::io::{BufRead, Read, Write};

use omarchy_hotspot_proto::{HelperRequest, HelperResponse, SERVE_MAX_LINE};

use crate::{handle, journal};

pub fn run() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    serve_loop(stdin.lock(), stdout.lock(), handle, journal::error)
}

/// На каждую прочитанную строку — ровно один ответ, иначе служба и помощник ждали бы друг друга.
/// `reject` пишет отказ в журнал (без текста строки: её прислал вызывающий).
fn serve_loop(
    mut input: impl BufRead,
    mut out: impl Write,
    handle: impl Fn(&HelperRequest) -> anyhow::Result<String>,
    reject: impl Fn(&str),
) -> anyhow::Result<()> {
    let mut buf = Vec::with_capacity(1024);
    loop {
        buf.clear();
        // Строка до предела плюс перевод строки.
        let n = (&mut input)
            .take((SERVE_MAX_LINE + 1) as u64)
            .read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Ok(()); // EOF — служба закрыла канал
        }
        let complete = buf.ends_with(b"\n");
        let resp = if !complete && buf.len() > SERVE_MAX_LINE {
            // Перевода строки ещё нет: выбрасываем хвост порциями, не держа его в памяти.
            loop {
                buf.clear();
                let n = (&mut input)
                    .take(SERVE_MAX_LINE as u64)
                    .read_until(b'\n', &mut buf)?;
                if n == 0 || buf.ends_with(b"\n") {
                    break;
                }
            }
            reject("rejected: request too long");
            HelperResponse::err("request too long")
        } else if let Ok(req) = serde_json::from_slice::<HelperRequest>(&buf) {
            match handle(&req) {
                Ok(output) => HelperResponse::ok(output),
                Err(e) => HelperResponse::err(e.to_string()),
            }
        } else {
            reject("rejected: bad request");
            HelperResponse::err("bad request")
        };
        serde_json::to_writer(&mut out, &resp)?;
        out.write_all(b"\n")?;
        out.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ответы помощника; каждый отказ должен оставить ровно одну запись в журнале.
    fn answers(input: &[u8]) -> Vec<HelperResponse> {
        let mut out = Vec::new();
        let rejected = std::cell::Cell::new(0);
        serve_loop(
            input,
            &mut out,
            |req| Ok(req.name().to_string()),
            |_| rejected.set(rejected.get() + 1),
        )
        .unwrap();
        let got: Vec<HelperResponse> = out
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_slice(l).unwrap())
            .collect();
        assert_eq!(rejected.get(), got.iter().filter(|r| !r.ok).count());
        got
    }

    #[test]
    fn one_answer_per_line_even_at_the_limit() {
        let ok = br#"{"cmd":"fw-clear"}"#;
        for len in [
            SERVE_MAX_LINE - 1,
            SERVE_MAX_LINE,
            SERVE_MAX_LINE + 1,
            3 * SERVE_MAX_LINE,
        ] {
            let mut input = vec![b' '; len - ok.len()];
            input.extend_from_slice(ok); // JSON с пробелами впереди — ровно `len` байт
            input.push(b'\n');
            input.extend_from_slice(ok);
            input.push(b'\n');
            let got = answers(&input);
            assert_eq!(got.len(), 2, "длина {len}: {got:?}");
            let first_ok = len <= SERVE_MAX_LINE;
            assert_eq!(got[0].ok, first_ok, "длина {len}: {got:?}");
            if !first_ok {
                assert_eq!(got[0].error, "request too long");
            }
            assert_eq!(got[1], HelperResponse::ok("fw-clear".into()), "длина {len}");
        }
    }

    #[test]
    fn bad_lines_get_errors_and_last_line_without_newline_works() {
        let got = answers(b"not json\n{\"cmd\":\"serve\"}\n{\"cmd\":\"dns-clear\"}");
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].error, "bad request");
        assert_eq!(got[1].error, "bad request");
        assert_eq!(got[2], HelperResponse::ok("dns-clear".into()));
    }
}
