//! Minimal Anvil region (.mca) reader.
//! Parses the 8 KiB header, then yields decompressed chunk NBT payloads.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub struct ChunkPayload {
    pub index: usize,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub enum RegionError {
    Io(std::io::Error),
    BadHeader(&'static str),
    UnsupportedCompression(u8),
}

impl From<std::io::Error> for RegionError {
    fn from(e: std::io::Error) -> Self {
        RegionError::Io(e)
    }
}

impl std::fmt::Display for RegionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegionError::Io(e) => write!(f, "io: {e}"),
            RegionError::BadHeader(s) => write!(f, "bad header: {s}"),
            RegionError::UnsupportedCompression(c) => write!(f, "unsupported compression: {c}"),
        }
    }
}

/// Read all non-empty chunks from a .mca file, decompressed to raw NBT.
pub fn read_chunks(path: &Path) -> Result<Vec<ChunkPayload>, RegionError> {
    let mut file = File::open(path)?;
    let mut header = [0u8; 8192];
    file.read_exact(&mut header)
        .map_err(|_| RegionError::BadHeader("short header"))?;

    let mut out = Vec::new();
    for i in 0..1024 {
        let off = ((header[i * 4] as u32) << 16)
            | ((header[i * 4 + 1] as u32) << 8)
            | header[i * 4 + 2] as u32;
        let sectors = header[i * 4 + 3] as u32;
        if off == 0 || sectors == 0 {
            continue;
        }
        let pos = (off as u64) * 4096;
        file.seek(SeekFrom::Start(pos))?;
        let mut len_buf = [0u8; 4];
        file.read_exact(&mut len_buf)?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if !(2..=64 * 1024 * 1024).contains(&len) {
            continue; // corrupt entry, skip
        }
        let mut kind_buf = [0u8; 1];
        file.read_exact(&mut kind_buf)?;
        let kind = kind_buf[0];
        let mut data = vec![0u8; len - 1];
        file.read_exact(&mut data)?;

        let bytes = match kind {
            1 => {
                // gzip
                let mut d = flate2::read::GzDecoder::new(&data[..]);
                let mut o = Vec::new();
                d.read_to_end(&mut o)?;
                o
            }
            2 => {
                // zlib
                let mut d = flate2::read::ZlibDecoder::new(&data[..]);
                let mut o = Vec::new();
                d.read_to_end(&mut o)?;
                o
            }
            3 => data, // uncompressed
            _ => return Err(RegionError::UnsupportedCompression(kind)),
        };
        out.push(ChunkPayload { index: i, bytes });
    }
    Ok(out)
}
