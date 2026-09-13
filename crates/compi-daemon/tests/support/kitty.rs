use base64::{Engine, engine::general_purpose::STANDARD};
use std::io::Write;
use std::path::Path;

pub fn write_4k_transfer(path: &Path) -> String {
    // 3840 × 2160 × RGBA = 33,177,600 bytes; base64 = 44,236,800 bytes.
    // This exceeds the old graphics and frame caps without relying on PNG compression.
    let pixels = vec![0x5a; 3840 * 2160 * 4];
    let encoded = STANDARD.encode(&pixels);
    drop(pixels);
    let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    file.write_all(b"\x1b[H").unwrap();
    let chunks = encoded.as_bytes().chunks(4096);
    let count = chunks.len();
    for (index, chunk) in chunks.enumerate() {
        if index == 0 {
            write!(
                file,
                "\x1b_Ga=T,f=32,s=3840,v=2160,i=42,p=7,c=8,r=4,z=-1,m=1;"
            )
            .unwrap();
        } else {
            write!(file, "\x1b_Gm={};", u8::from(index + 1 < count)).unwrap();
        }
        file.write_all(chunk).unwrap();
        file.write_all(b"\x1b\\").unwrap();
    }
    file.write_all(b"\r\nKITTY_READY_42\r\n").unwrap();
    file.flush().unwrap();
    encoded
}
