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

/// Cap on a single decompressed chunk payload (32 MiB). Vanilla chunks are
/// ~1 MiB at most (the sector table itself caps entries at 255 sectors);
/// anything larger is a corrupt entry or a decompression bomb, so the entry
/// is skipped instead of exhausting memory.
pub const MAX_CHUNK_BYTES: u64 = 32 * 1024 * 1024;

/// Read all non-empty chunks from a .mca file, decompressed to raw NBT.
///
/// Corrupt entries are skipped, never fatal: a single bad chunk (truncated
/// data, unknown compression, oversized lengths, decompression failure)
/// yields no payload for that slot while the rest of the file still parses.
/// When *no* chunk parses, the first encountered error is returned so
/// callers (e.g. the modded-content scan) can warn about the file instead
/// of silently reporting zero chunks.
pub fn read_chunks(path: &Path) -> Result<Vec<ChunkPayload>, RegionError> {
    let mut file = File::open(path)?;
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(u64::MAX);
    let mut header = [0u8; 8192];
    file.read_exact(&mut header)
        .map_err(|_| RegionError::BadHeader("short header"))?;

    let mut out = Vec::new();
    let mut first_err: Option<RegionError> = None;
    let mut record = |e: RegionError| {
        if first_err.is_none() {
            first_err = Some(e);
        }
    };
    for i in 0..1024 {
        let off = ((header[i * 4] as u32) << 16)
            | ((header[i * 4 + 1] as u32) << 8)
            | header[i * 4 + 2] as u32;
        let sectors = header[i * 4 + 3] as u32;
        // Sector 0/1 are the header itself; 0-length entries are empty.
        if off < 2 || sectors == 0 {
            continue;
        }
        let pos = (off as u64) * 4096;
        if pos.saturating_add(5) > file_len {
            record(RegionError::BadHeader("chunk offset past EOF"));
            continue;
        }
        if file.seek(SeekFrom::Start(pos)).is_err() {
            record(RegionError::BadHeader("chunk seek failed"));
            continue;
        }
        let mut len_buf = [0u8; 4];
        if file.read_exact(&mut len_buf).is_err() {
            record(RegionError::BadHeader("truncated chunk length"));
            continue;
        }
        let len = u32::from_be_bytes(len_buf) as usize;
        // The payload must fit inside its allocated sectors (max 255
        // sectors ≈ 1 MiB); anything larger would allocate unbounded memory
        // on a crafted length prefix.
        if !(2..=64 * 1024 * 1024).contains(&len)
            || (len as u64) + 4 > (sectors as u64) * 4096
            || (len as u64) - 1 > MAX_CHUNK_BYTES
        {
            record(RegionError::BadHeader("bad chunk length"));
            continue;
        }
        let mut kind_buf = [0u8; 1];
        if file.read_exact(&mut kind_buf).is_err() {
            record(RegionError::BadHeader("truncated chunk kind"));
            continue;
        }
        let kind = kind_buf[0];
        let mut data = vec![0u8; len - 1];
        if file.read_exact(&mut data).is_err() {
            record(RegionError::BadHeader("truncated chunk data"));
            continue;
        }

        let bytes = match kind {
            1 => {
                // gzip
                let d = flate2::read::GzDecoder::new(&data[..]);
                match read_capped(d) {
                    Ok(o) => o,
                    Err(e) => {
                        record(e);
                        continue;
                    }
                }
            }
            2 => {
                // zlib
                let d = flate2::read::ZlibDecoder::new(&data[..]);
                match read_capped(d) {
                    Ok(o) => o,
                    Err(e) => {
                        record(e);
                        continue;
                    }
                }
            }
            3 => data, // uncompressed
            _ => {
                record(RegionError::UnsupportedCompression(kind));
                continue;
            }
        };
        out.push(ChunkPayload { index: i, bytes });
    }
    if out.is_empty() {
        if let Some(e) = first_err {
            return Err(e);
        }
    }
    Ok(out)
}

/// Decompress `r` with a hard cap so a tiny crafted payload cannot expand
/// into gigabytes (decompression bomb). Oversized or corrupt streams err.
fn read_capped(r: impl Read) -> Result<Vec<u8>, RegionError> {
    let mut limited = r.take(MAX_CHUNK_BYTES + 1);
    let mut o = Vec::new();
    limited.read_to_end(&mut o).map_err(RegionError::Io)?;
    if (o.len() as u64) > MAX_CHUNK_BYTES {
        return Err(RegionError::BadHeader("chunk decompresses too large"));
    }
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_mca(path: &Path, entries: &[(usize, u8, Vec<u8>)]) {
        let mut header = vec![0u8; 8192];
        let mut body: Vec<u8> = Vec::new();
        let mut sector: u32 = 2;
        for (idx, kind, payload) in entries {
            let comp: Vec<u8> = match kind {
                2 => {
                    let mut e =
                        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
                    e.write_all(payload).unwrap();
                    e.finish().unwrap()
                }
                1 => {
                    let mut e =
                        flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                    e.write_all(payload).unwrap();
                    e.finish().unwrap()
                }
                _ => payload.clone(),
            };
            let len = (comp.len() + 1) as u32;
            let sectors = ((len + 4 + 4095) / 4096).max(1);
            header[(*idx) * 4] = ((sector >> 16) & 0xFF) as u8;
            header[(*idx) * 4 + 1] = ((sector >> 8) & 0xFF) as u8;
            header[(*idx) * 4 + 2] = (sector & 0xFF) as u8;
            header[(*idx) * 4 + 3] = sectors as u8;
            while (body.len() as u32) < (sector - 2) * 4096 {
                body.push(0);
            }
            body.extend_from_slice(&len.to_be_bytes());
            body.push(*kind);
            body.extend_from_slice(&comp);
            while (body.len() % 4096) != 0 {
                body.push(0);
            }
            sector += sectors;
        }
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(&header).unwrap();
        f.write_all(&body).unwrap();
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("mcsc-region-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn bad_entry_does_not_lose_good_chunks() {
        let dir = tmp("skip");
        let p = dir.join("a.mca");
        write_mca(
            &p,
            &[(0, 2, b"hello-good".to_vec()), (1, 99, b"bad".to_vec())],
        );
        let chunks = read_chunks(&p).expect("good chunk must survive a bad neighbor");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].index, 0);
        assert_eq!(chunks[0].bytes, b"hello-good");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncated_entry_is_skipped() {
        let dir = tmp("trunc");
        let p = dir.join("c.mca");
        let mut header = vec![0u8; 8192];
        header[0] = 0;
        header[1] = 0;
        header[2] = 2;
        header[3] = 1;
        std::fs::write(&p, &header).unwrap(); // offset claims data, none present
        assert!(
            read_chunks(&p).is_err(),
            "fully-bad file still errors for scan warnings"
        );
        // Mixed: good chunk plus a dangling offset past EOF.
        let q = dir.join("mix.mca");
        write_mca(&q, &[(0, 2, b"kept".to_vec())]);
        let mut raw = std::fs::read(&q).unwrap();
        raw[4] = 0; // slot 1 -> sector 200 (past EOF)
        raw[5] = 0;
        raw[6] = 200;
        raw[7] = 1;
        std::fs::write(&q, &raw).unwrap();
        let chunks = read_chunks(&q).expect("good chunk must survive truncated neighbor");
        assert_eq!(chunks.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decompression_bomb_is_capped() {
        // 40 MiB of zeros compresses to kilobytes; the reader must refuse
        // to expand it instead of ballooning memory.
        let dir = tmp("bomb");
        let p = dir.join("bomb.mca");
        let big = vec![0u8; (MAX_CHUNK_BYTES + 8 * 1024 * 1024) as usize];
        write_mca(&p, &[(0, 2, big)]);
        assert!(
            read_chunks(&p).is_err(),
            "oversized payload must be refused"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_length_is_rejected_without_huge_alloc() {
        let dir = tmp("len");
        let p = dir.join("b.mca");
        let mut header = vec![0u8; 8192];
        header[0] = 0;
        header[1] = 0;
        header[2] = 2;
        header[3] = 1; // 1 sector can never hold a 64 MiB payload
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(&header).unwrap();
        f.write_all(&((64 * 1024 * 1024u32).to_be_bytes())).unwrap();
        f.write_all(&[2u8]).unwrap();
        f.write_all(b"x").unwrap();
        let t = std::time::Instant::now();
        let res = read_chunks(&p);
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
        assert!(res.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
