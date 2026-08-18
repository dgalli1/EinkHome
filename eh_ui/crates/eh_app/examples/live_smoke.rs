//! Live smoke test: talk to a running pbemu-api (mock provider) and print
//! what the Rust client parses.  Run against a server started with:
//!   cd api && python3 ./api/server.py --provider mock --host 127.0.0.1 --port 18765

use eh_app::client::ApiClient;

fn main() {
    let base = std::env::var("API").unwrap_or_else(|_| "http://127.0.0.1:18765".into());
    let client = ApiClient::new(&base, "pbemu-dev-token");

    match client.list_books(5) {
        Ok(list) => {
            println!("list_books(5): {} items", list.len());
            for b in list.iter().take(3) {
                println!("  - {} / {}", b.id, b.title);
            }
        }
        Err(e) => println!("list_books error: {e}"),
    }

    match client.delta(0, 3) {
        Ok(d) => {
            println!("delta(0,3): added={} removed={} nextCursor={} more={}",
                d.added.len(), d.removed.len(), d.next_cursor, d.more);
            for b in d.added.iter().take(3) {
                println!("  + {} / {} / {}", b.id, b.title, b.author());
            }
        }
        Err(e) => println!("delta error: {e}"),
    }
}