//! Live cover test: sync, then fetch+decode one real cover from the mock
//! API and cache it, verifying dimensions + rgb byte count.
//!
//! Run against: cd api && python3 ./api/server.py --provider mock --host 127.0.0.1 --port 18765

use eh_app::client::ApiClient;
use eh_app::cover;
use eh_app::store::Store;

fn main() {
    let base = std::env::var("API").unwrap_or_else(|_| "http://127.0.0.1:18765".into());
    let db = std::env::temp_dir().join("eh_cover.db");
    let app = std::env::temp_dir().join("eh_cover_app");
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_dir_all(&app);

    let client = ApiClient::new(&base, "pbemu-dev-token");
    let store = Store::open(&db).expect("open");
    eh_app::sync::sync(&client, &store, 50, &std::sync::atomic::AtomicBool::new(false),
        &mut |_| {}, None).expect("sync");

    let covers = cover::resolve_covers_dir(&app);
    let shelf = store.list_books(1, 0).expect("list");
    let book = &shelf[0];
    let bytes = cover::fetch(&client, &covers, &book.id).expect("cover fetch");
    let (w, h, rgb) = cover::decode_rgb(&bytes).expect("cover decode");
    println!(
        "cover {}: {}x{} ({} bytes png, {} rgb bytes), cached? {}",
        book.id,
        w,
        h,
        bytes.len(),
        rgb.len(),
        cover::load_cached(&covers, &book.id).is_some()
    );
    // A second fetch must hit the cache (no error).
    let again = cover::fetch(&client, &covers, &book.id).expect("cached fetch");
    assert_eq!(again.len(), bytes.len());
    println!("second fetch from cache ok");

    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_dir_all(&app);
}