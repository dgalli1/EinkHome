//! Live end-to-end: fetch a delta from the mock API, upsert into SQLite,
//! then read the shelf list back.  Proves client + store interoperate against
//! the real /api/v1 contract and a real on-disk DB (schema-compatible).
//!
//! Run against: cd api && python3 ./api/server.py --provider mock --host 127.0.0.1 --port 18765

use eh_app::client::ApiClient;
use eh_app::store::Store;
use std::path::PathBuf;

fn main() {
    let base = std::env::var("API").unwrap_or_else(|_| "http://127.0.0.1:18765".into());
    let db = std::env::var("DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("eh_app_live.db"));
    let _ = std::fs::remove_file(&db);

    let client = ApiClient::new(&base, "pbemu-dev-token");
    let store = Store::open(&db).expect("open store");

    // Sync one delta round fetching up to 50 books, persisted.
    let mut cursor = 0i64;
    let mut rounds = 0;
    loop {
        let d = client.delta(cursor, 50).expect("delta");
        for b in &d.added {
            store.upsert_book(b).expect("upsert");
        }
        for id in &d.removed {
            store.delete_book(id).expect("delete");
        }
        rounds += 1;
        if !d.more || d.added.is_empty() {
            if rounds == 1 && d.added.is_empty() {
                println!("no books in delta (server empty?)");
            }
            break;
        }
        cursor = d.next_cursor;
        if rounds > 20 {
            println!("stopping after {rounds} rounds (pagination safety)");
            break;
        }
    }

    let count = store.count().expect("count");
    let shelf = store.list_books(5, 0).expect("list");
    println!("store: {count} books persisted, {rounds} delta rounds");
    for b in &shelf {
        let dl = if b.downloaded { "   [downloaded]" } else { "" };
        println!("  - {} / {}{}", b.id, b.title, dl);
    }

    // Round-trip check: reopen and confirm the same count.
    let store2 = Store::open(&db).expect("reopen");
    assert_eq!(store2.count().expect("count"), count);
    println!("reopen count == {count} (schema round-trip ok)");
    let _ = std::fs::remove_file(&db);
}
