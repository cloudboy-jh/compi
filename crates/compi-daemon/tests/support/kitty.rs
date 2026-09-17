use base64::{Engine, engine::general_purpose::STANDARD};
use flate2::{Compression, write::ZlibEncoder};
use std::io::Write;
use std::path::Path;

pub fn write_transfer(path: &Path, width: u32, height: u32) -> String {
    // Compress the repetitive fixture on the PTY path so tests exercise the
    // retained image frame rather than spend their timeout parsing base64.
    let pixels = vec![0x5a; width as usize * height as usize * 4];
    let expected = STANDARD.encode(&pixels);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&pixels).unwrap();
    let encoded = STANDARD.encode(encoder.finish().unwrap());
    let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    file.write_all(b"\x1b[H").unwrap();
    let chunks = encoded.as_bytes().chunks(4096);
    let count = chunks.len();
    for (index, chunk) in chunks.enumerate() {
        let more = u8::from(index + 1 < count);
        if index == 0 {
            write!(
                file,
                "\x1b_Ga=T,f=32,s={width},v={height},i=42,p=7,c=8,r=4,z=-1,o=z,m={more};"
            )
            .unwrap();
        } else {
            write!(file, "\x1b_Gm={more};").unwrap();
        }
        file.write_all(chunk).unwrap();
        file.write_all(b"\x1b\\").unwrap();
    }
    file.write_all(b"\r\nKITTY_READY_42\r\n").unwrap();
    file.flush().unwrap();
    expected
}
