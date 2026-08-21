//! Exercise the Rust sync engine against a live mock API.

use eh_app::client::ApiClient;
use eh_app::store::Store;
use std::sync::atomic::AtomicBool;
fn main() {
    let base = std::env::var("API").unwrap_or_else(|_| "http://127.0.0.1:18765".into());
    let db = std::env::temp_dir().join("eh_sync_engine.db");
    let _ = std::fs::remove_file(&db);

    let client = ApiClient::new(&base, "pbemu-dev-token");
    let store = Store::open(&db).expect("open");

    let n = eh_app::sync::sync(&client, &store, 50, &AtomicBool::new(false),
        &mut |_| {}, None).expect("sync");
    println!("sync done: {n} books, cursor={}", store.cursor().expect("cursor"));

    // A second sync should be a cheap no-op delta (cursor already advanced).
    let n2 = eh_app::sync::sync(&client, &store, 50, &AtomicBool::new(false),
        &mut |_| {}, None).expect("sync2");
    println!("second sync: {n2} books (should be same, cheap)");
    let _ = std::fs::remove_file(&db);
}
