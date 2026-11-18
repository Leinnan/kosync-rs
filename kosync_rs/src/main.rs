//! `KOReader` sync server binary entrypoint.

use web_mev_runtime::{LogConfig, init_logging, install_panic_hook, load_dotenv, report_fatal};

#[tokio::main]
async fn main() {
    install_panic_hook("kosync_rs");
    // Env files first, so a `RUST_LOG` from `.env` counts.
    load_dotenv();
    let _logs = match init_logging(&LogConfig::new("info,sqlx=warn")) {
        Ok(guard) => guard,
        Err(error) => {
            report_fatal("kosync_rs", &error);
            std::process::exit(1);
        }
    };
    if let Err(error) = kosync_rs::run().await {
        report_fatal("kosync_rs", &*error);
        std::process::exit(1);
    }
}
