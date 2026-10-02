//! `KOReader` sync server binary entrypoint.

#[tokio::main]
async fn main() {
    if let Err(err) = kosync_rs::run().await {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}
