use std::io::{self, Read, Write};

pub const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

#[derive(Debug, Eq, PartialEq)]
pub struct Frame {
    pub kind: u8,
    pub payload: Vec<u8>,
}

pub fn read<R: Read>(reader: &mut R) -> io::Result<Option<Frame>> {
    let mut header = [0_u8; 5];
    match reader.read_exact(&mut header[..1]) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    reader.read_exact(&mut header[1..])?;

    let length = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
    if length > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame payload is too large: {length} bytes"),
        ));
    }

    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;
    Ok(Some(Frame {
        kind: header[4],
        payload,
    }))
}

pub fn write<W: Write>(writer: &mut W, kind: u8, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "payload exceeds u32"))?;
    if payload.len() > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame payload exceeds the protocol limit",
        ));
    }

    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(&[kind])?;
    writer.write_all(payload)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Chunked<R> {
        inner: R,
        chunk_size: usize,
    }

    impl<R: Read> Read for Chunked<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let length = buffer.len().min(self.chunk_size);
            self.inner.read(&mut buffer[..length])
        }
    }

    #[test]
    fn round_trips_frames() {
        let mut bytes = Vec::new();
        write(&mut bytes, 7, b"echo hello\n").unwrap();

        let frame = read(&mut Cursor::new(bytes)).unwrap().unwrap();
        assert_eq!(frame.kind, 7);
        assert_eq!(frame.payload, b"echo hello\n");
    }

    #[test]
    fn reads_consecutive_frames() {
        let mut bytes = Vec::new();
        write(&mut bytes, 1, b"").unwrap();
        write(&mut bytes, 2, b"payload").unwrap();
        let mut cursor = Cursor::new(bytes);

        assert_eq!(read(&mut cursor).unwrap().unwrap().kind, 1);
        assert_eq!(read(&mut cursor).unwrap().unwrap().payload, b"payload");
        assert!(read(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn reads_frames_split_across_small_chunks() {
        let payload = vec![0x5a; 96 * 1024];
        let mut bytes = Vec::new();
        write(&mut bytes, 2, &payload).unwrap();
        let mut reader = Chunked {
            inner: Cursor::new(bytes),
            chunk_size: 3,
        };

        assert_eq!(read(&mut reader).unwrap().unwrap().payload, payload);
    }

    #[test]
    fn rejects_oversized_payloads() {
        let mut header = ((MAX_PAYLOAD + 1) as u32).to_le_bytes().to_vec();
        header.push(1);
        let error = read(&mut Cursor::new(header)).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
