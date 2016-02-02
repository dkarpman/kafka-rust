extern crate rsnappy;

use std::io::{self, Cursor, Read};
use byteorder::{BigEndian, ByteOrder};

use error::{Result, Error};

// ~ Uncompresses 'src' into 'dst'.
// ~ 'dst' will receive the newly uncompressed data appended.
// ~ No guarantees are provided about the contents of 'dst' if this function
// results in an error.
fn uncompress_into(src: &[u8], dst: &mut Vec<u8>) -> Result<()> {
    rsnappy::decompress(&mut Cursor::new(src), dst).map_err(|_| Error::InvalidInputSnappy)
}

pub fn compress(src: &[u8]) -> Result<Vec<u8>> {
    let mut dst = Vec::new();
    if let Err(_) = rsnappy::compress(&mut Cursor::new(src), &mut dst) {
        return Err(Error::InvalidInputSnappy);
    }
    Ok(dst)
}

// --------------------------------------------------------------------

const MAGIC: &'static [u8] = &[0x82, b'S', b'N', b'A', b'P', b'P', b'Y', 0];

// ~ reads a i32 valud and "advances" the given slice by four bytes;
// assumes "slice" is a mutable reference to a &[u8].
macro_rules! next_i32 {
    ($slice:expr) => {{
        if $slice.len() < 4 {
            return Err(Error::UnexpectedEOF);
        }
        { let n = BigEndian::read_i32($slice);
          $slice = &$slice[4..];
          n
        }
    }}
}

/// Validates the expected header at the beginning of the
/// stream. Further, checks the version and compatibility of the
/// stream indicating we can parse the stream. Returns the rest of the
/// stream following the validated header.
fn validate_stream(mut stream: &[u8]) -> Result<&[u8]> {
    // ~ check the "header magic"
    if stream.len() < MAGIC.len() { return Err(Error::UnexpectedEOF); }
    if &stream[..MAGIC.len()] != MAGIC { return Err(Error::InvalidInputSnappy); }
    stream = &stream[MAGIC.len()..];
    // ~ let's be assertive and (for the moment) restrict ourselves to
    // version == 1 and compatibility == 1.
    let version = next_i32!(stream);
    if version != 1 { return Err(Error::InvalidInputSnappy); }
    let compat = next_i32!(stream);
    if compat != 1 { return Err(Error::InvalidInputSnappy); }
    Ok(stream)
}

#[test]
fn test_validate_stream() {
    let header = [0x82, 0x53, 0x4e, 0x41, 0x50, 0x50, 0x59, 0x00, 0, 0, 0, 1, 0, 0, 0, 1, 0x56];
    // ~ this must not result in a panic
    let rest = validate_stream(&header).unwrap();
    // ~ the rest of the input after the parsed header must be
    // correctly delivered
    assert_eq!(rest, &[0x56]);
}

// ~ An implementation of a reader over a stream of snappy compressed
// chunks as produced by org.xerial.snappy.SnappyOutputStream
// (https://github.com/xerial/snappy-java/ version: 1.1.1.*)
pub struct SnappyReader<'a> {
    // the compressed data itself
    compressed_data: &'a [u8],

    // a pointer into `uncompressed_chunk` indicating the next data
    // byte to serve
    uncompressed_pos: usize,
    // the uncompressed chunk of data available for consumption
    uncompressed_chunk: Vec<u8>,
}

impl<'a> SnappyReader<'a> {
    pub fn new(mut stream: &[u8]) -> Result<SnappyReader> {
        stream = try!(validate_stream(stream));
        Ok(SnappyReader {
            compressed_data: stream,
            uncompressed_pos: 0,
            uncompressed_chunk: Vec::new(),
        })
    }

    fn _read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if self.uncompressed_pos < self.uncompressed_chunk.len() {
            self.read_uncompressed(buf)
        } else if try!(self.next_chunk()) {
            self.read_uncompressed(buf)
        } else {
            Ok(0)
        }
    }

    fn read_uncompressed(&mut self, buf: &mut [u8]) -> Result<usize> {
        let n = try!((&self.uncompressed_chunk[self.uncompressed_pos..]).read(buf));
        self.uncompressed_pos += n;
        Ok(n)
    }

    fn next_chunk(&mut self) -> Result<bool> {
        if self.compressed_data.is_empty() {
            return Ok(false);
        }
        self.uncompressed_pos = 0;
        let chunk_size = next_i32!(self.compressed_data);
        if chunk_size <= 0 {
            return Err(Error::InvalidInputSnappy);
        }
        let chunk_size = chunk_size as usize;
        self.uncompressed_chunk.clear();
        try!(uncompress_into(&self.compressed_data[..chunk_size], &mut self.uncompressed_chunk));
        self.compressed_data = &self.compressed_data[chunk_size..];
        Ok(true)
    }

    fn _read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        let init_len = buf.len();
        // ~ first consume already uncompressed and unconsumed data - if any
        if self.uncompressed_pos < self.uncompressed_chunk.len() {
            let rest = &self.uncompressed_chunk[self.uncompressed_pos..];
            buf.extend_from_slice(rest);
            self.uncompressed_pos += rest.len();
        }
        // ~ now decompress data directly to the output target
        while !self.compressed_data.is_empty() {
            let chunk_size = next_i32!(self.compressed_data);
            if chunk_size <= 0 {
                return Err(Error::InvalidInputSnappy);
            }
            let (c1, c2) = self.compressed_data.split_at(chunk_size as usize);
            try!(uncompress_into(c1, buf));
            self.compressed_data = c2;
        }
        Ok(buf.len() - init_len)
    }
}

macro_rules! to_io_error {
    ($expr:expr) => {
        match $expr {
            Ok(n) => Ok(n),
            // ~ pass io errors through directly
            Err(Error::Io(io_error)) => Err(io_error),
            // ~ wrapp our other errors
            Err(e) => Err(io::Error::new(io::ErrorKind::Other, e)),
        }
    }
}

impl<'a> Read for SnappyReader<'a> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        to_io_error!(self._read(buf))
    }

    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        to_io_error!(self._read_to_end(buf))
    }
}

// --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    extern crate rsnappy;

    use std::io::Read;

    use error::{Error, Result};
    use super::{compress, uncompress_into, SnappyReader};

    fn uncompress(src: &[u8]) -> Result<Vec<u8>> {
        let mut v = Vec::new();
        match uncompress_into(src, &mut v) {
            Ok(_) => Ok(v),
            Err(e) => Err(e),
        }
    }

    #[test]
    fn test_compress() {
        let msg = "This is test".as_bytes();
        let compressed = compress(msg).unwrap();
        let expected = &[12, 44, 84, 104, 105, 115, 32, 105, 115, 32, 116, 101, 115, 116];
        assert_eq!(&compressed, expected);
    }

    #[test]
    fn test_uncompress() {
        // The vector should uncompress to "This is test"
        let compressed = &[12, 44, 84, 104, 105, 115, 32, 105, 115, 32, 116, 101, 115, 116];
        let uncompressed = String::from_utf8(uncompress(compressed).unwrap()).unwrap();
        assert_eq!(&uncompressed, "This is test");
    }

    #[test]
    fn test_uncompress_invalid_input() {
        // The vector is an invalid snappy message (second byte modified on purpose)
        let compressed = &[12, 42, 84, 104, 105, 115, 32, 105, 115, 32, 116, 101, 115, 116];
        let uncompressed = uncompress(compressed);
        assert!(uncompressed.is_err());
        assert!(if let Some(Error::InvalidInputSnappy) = uncompressed.err() { true } else { false });
    }

    static ORIGINAL: &'static str =
        include_str!("../../test-data/fetch1.txt");
    static COMPRESSED: &'static [u8] =
        include_bytes!("../../test-data/fetch1.snappy.chunked.4k");

    #[test]
    fn test_snappy_reader_read() {
        let mut buf = Vec::new();
        let mut r = SnappyReader::new(COMPRESSED).unwrap();

        let mut tmp_buf = [0u8; 1024];
        loop {
            match r.read(&mut tmp_buf).unwrap() {
                0 => break,
                n => buf.extend_from_slice(&tmp_buf[..n]),
            }
        }
        assert_eq!(ORIGINAL.as_bytes(), &buf[..]);
    }

    #[test]
    fn test_snappy_reader_read_to_end() {
        let mut buf = Vec::new();
        let mut r = SnappyReader::new(COMPRESSED).unwrap();
        r.read_to_end(&mut buf).unwrap();
        assert_eq!(ORIGINAL.as_bytes(), &buf[..]);
    }


    #[test]
    fn test_snappy_reader_read_to_end_multi() {
        for _ in 0 .. 3 {
            let mut buf = Vec::new();
            let mut r = SnappyReader::new(COMPRESSED).unwrap();
            r.read_to_end(&mut buf).unwrap();
            assert_eq!(ORIGINAL.as_bytes(), &buf[..]);
        }
    }

    #[cfg(feature = "nightly")]
    mod benches {
        extern crate rsnappy;

        use super::COMPRESSED;
        use super::super::SnappyReader;
        use std::io::Read;

        use test::{Bencher};

        #[bench]
        fn bench_snappy_reader_rsnappy(b: &mut Bencher) {
            b.bytes = COMPRESSED.len() as u64;
            b.iter(|| {
                let mut buf = Vec::new();
                let mut r = SnappyReader::new_rsnappy(COMPRESSED).unwrap();
                r.read_to_end(&mut buf).unwrap()
            });
        }

        #[bench]
        fn bench_snappy_reader(b: &mut Bencher) {
            b.bytes = COMPRESSED.len() as u64;
            b.iter(|| {
                let mut buf = Vec::new();
                let mut r = SnappyReader::new(COMPRESSED).unwrap();
                r.read_to_end(&mut buf).unwrap()
            });
        }
    }
}
