//! Repository tasks: version display now; manifest generation, packaging and icon work later.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("version") | None => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("manifest") => {
            eprintln!("xtask manifest: lands with the updater milestone (M8)");
            ExitCode::from(2)
        }
        Some(other) => {
            eprintln!("unknown task `{other}`; available: version, manifest");
            ExitCode::from(2)
        }
    }
}
