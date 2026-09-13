use base64::{Engine, engine::general_purpose::STANDARD};
use flate2::{Compression, write::ZlibEncoder};
use std::io::Write;
use std::path::Path;

pub fn write_4k_transfer(path: &Path) -> String {
    // The decoded 3840 × 2160 RGBA image is 33,177,600 bytes and its retained
    // replica payload is 44,236,800 bytes. Compress the repetitive fixture on
    // the PTY path so hosted debug builds exercise the large retained frame
    // without spending their timeout parsing 42 MiB of redundant base64.
    let pixels = vec![0x5a; 3840 * 2160 * 4];
    let expected = STANDARD.encode(&pixels);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&pixels).unwrap();
    let encoded = STANDARD.encode(encoder.finish().unwrap());
    let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    file.write_all(b"\x1b[H").unwrap();
    let chunks = encoded.as_bytes().chunks(4096);
    let count = chunks.len();
    for (index, chunk) in chunks.enumerate() {
        if index == 0 {
            write!(
                file,
                "\x1b_Ga=T,f=32,s=3840,v=2160,i=42,p=7,c=8,r=4,z=-1,o=z,m=1;"
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
    expected
}
