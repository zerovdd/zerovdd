//! Binary entrypoint for `zerovdd-svc.exe`.

use anyhow::{bail, Result};
use zerovdd_svc::service::{run_console_mode, run_service_dispatcher};

fn main() -> Result<()> {
    let mut console_mode = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--console" => console_mode = true,
            "--help" | "-h" => {
                println!("Usage: zerovdd-svc [--console]");
                return Ok(());
            }
            "--version" | "-V" => {
                println!("zerovdd-svc {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => bail!("unknown argument `{other}`; expected `--console`"),
        }
    }

    if console_mode {
        run_console_mode()
    } else {
        run_service_dispatcher()
    }
}
