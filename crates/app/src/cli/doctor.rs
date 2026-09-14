use omarchy_hotspot_core::doctor::{self, Status};
use omarchy_hotspot_core::{Lang, Msg, t};

pub fn run(lang: Lang) -> anyhow::Result<()> {
    println!("{}", t(lang, Msg::DoctorTitle));
    let checks = doctor::run_all(lang);
    for c in &checks {
        let mark = match c.status {
            Status::Ok => "✓",
            Status::Warn => "!",
            Status::Fail => "✗",
        };
        println!("{mark} {}: {}", t(lang, c.id.title()), c.detail);
        if let Some(advice) = c.advice {
            println!("    → {}", t(lang, advice));
        }
    }
    let failed = checks.iter().any(|c| c.status == Status::Fail);
    println!();
    println!(
        "{}",
        t(
            lang,
            if failed {
                Msg::DoctorHasProblems
            } else {
                Msg::DoctorAllOk
            }
        )
    );
    if failed {
        std::process::exit(1);
    }
    Ok(())
}
