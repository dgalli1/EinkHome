// Quick check: does eh_app's decode_rgb parse the PIL-written PNG?
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    println!("file {} bytes", bytes.len());
    // decode_rgb is pub in eh_app::cover
    match eh_app::cover::decode_rgb(&bytes) {
        Ok((w, h, rgb)) => println!("decoded {w}x{h}, {} bytes rgb", rgb.len()),
        Err(e) => println!("DECODE FAILED: {e}"),
    }
}
