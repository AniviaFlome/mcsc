/// Player identity helpers: offline-mode (v3) UUIDs used by offline/cracked
/// servers vs online-mode (v4) UUIDs used by Mojang accounts. When they
/// differ, the game sees a fresh player: wrong spawn, empty inventory.
/// Compute the offline-mode UUID Mojang-style servers derive from a username:
/// MD5("OfflinePlayer:<name>") with UUID version/variant bits set.
pub fn offline_uuid(username: &str) -> String {
    let hex = md5_hex(&format!("OfflinePlayer:{username}"));
    let mut b = [0u8; 16];
    let (chunks, _) = hex.as_bytes().as_chunks::<2>();
    for (i, chunk) in chunks.iter().enumerate() {
        b[i] = u8::from_str_radix(std::str::from_utf8(chunk).unwrap_or("00"), 16).unwrap_or(0);
    }
    b[6] = (b[6] & 0x0f) | 0x30;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10],
        b[11], b[12], b[13], b[14], b[15]
    )
}

/// Minimal MD5 (RFC 1321) so mcsc-core stays dependency-free.
fn md5_hex(input: &str) -> String {
    let mut msg = input.as_bytes().to_vec();
    let bit_len = (msg.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let mut a: u32 = 0x67452301;
    let mut b: u32 = 0xefcdab89;
    let mut c: u32 = 0x98badcfe;
    let mut d: u32 = 0x10325476;

    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, //
        5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, //
        4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, //
        6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    let (blocks, _) = msg.as_chunks::<64>();
    for chunk in blocks {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        let (mut aa, mut bb, mut cc, mut dd) = (a, b, c, d);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((bb & cc) | ((!bb) & dd), i),
                16..=31 => ((dd & bb) | ((!dd) & cc), (5 * i + 1) % 16),
                32..=47 => (bb ^ cc ^ dd, (3 * i + 5) % 16),
                _ => (cc ^ (bb | (!dd)), (7 * i) % 16),
            };
            let tmp = dd;
            dd = cc;
            cc = bb;
            bb = bb.wrapping_add(
                (aa.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g])).rotate_left(S[i]),
            );
            aa = tmp;
        }
        a = a.wrapping_add(aa);
        b = b.wrapping_add(bb);
        c = c.wrapping_add(cc);
        d = d.wrapping_add(dd);
    }
    let mut fixed = String::with_capacity(32);
    for w in [a, b, c, d] {
        for byte in w.to_le_bytes() {
            fixed.push_str(&format!("{byte:02x}"));
        }
    }
    fixed
}

/// True for plausibly-shaped UUID strings (8-4-4-4-12 hex).
pub fn is_uuid(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    for (i, ch) in s.chars().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if ch != '-' {
                    return false;
                }
            }
            _ => {
                if !ch.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_uuid_matches_known_player() {
        // Verified against a real offline-mode world (Anivia31).
        assert_eq!(
            offline_uuid("Anivia31"),
            "5ad08d99-26a2-331c-8f77-d476dec25826"
        );
    }

    #[test]
    fn offline_uuid_differs_per_name() {
        assert_ne!(offline_uuid("Anivia31"), offline_uuid("SomeoneElse"));
    }

    #[test]
    fn uuid_shape_check() {
        assert!(is_uuid("10b8a89c-bb59-4113-a62f-5ac96b915dd9"));
        assert!(!is_uuid("not-a-uuid"));
        assert!(!is_uuid("10b8a89cbb594113a62f5ac96b915dd9"));
    }
}

/// Blocking online-UUID lookup used on worker threads. Returns the hyphenated
/// online UUID for a username, or None when unknown/unreachable.
type LookupFn = dyn Fn(&str) -> Option<String> + Send + Sync;

#[derive(Clone)]
pub struct UuidResolver {
    inner: std::sync::Arc<LookupFn>,
}

impl UuidResolver {
    pub fn new(f: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Self {
        Self {
            inner: std::sync::Arc::new(f),
        }
    }

    pub fn resolve(&self, username: &str) -> Option<String> {
        (self.inner)(username)
    }
}

impl std::fmt::Debug for UuidResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UuidResolver(..)")
    }
}

/// Read `(username, cached_uuid)` pairs from the server's usercache.json.
/// Missing or malformed files yield an empty list (never an error).
pub fn read_usercache_names(server_root: &std::path::Path) -> Vec<(String, String)> {
    read_name_uuid_file(&server_root.join("usercache.json"))
}

/// Read `(username, uuid)` pairs from every local identity source Bukkit
/// servers keep: usercache.json, ops.json, whitelist.json and
/// banned-players.json. Malformed files are skipped, never fatal.
pub fn read_identities(server_root: &std::path::Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for file in [
        "usercache.json",
        "ops.json",
        "whitelist.json",
        "banned-players.json",
    ] {
        out.extend(read_name_uuid_file(&server_root.join(file)));
    }
    out.sort();
    out.dedup();
    out
}

fn read_name_uuid_file(path: &std::path::Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(list) = json.as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|e| {
            let name = e.get("name")?.as_str()?.trim();
            let uuid = e.get("uuid")?.as_str()?.trim();
            if name.is_empty() || uuid.is_empty() {
                return None;
            }
            Some((name.to_string(), uuid.to_string()))
        })
        .collect()
}

/// UUID version nibble (the char at index 14): offline-mode IDs are `3`,
/// Mojang online IDs are `4`. Returns None for malformed input.
pub fn uuid_version(uuid: &str) -> Option<char> {
    if !is_uuid(uuid) {
        return None;
    }
    uuid.chars().nth(14).map(|c| c.to_ascii_lowercase())
}

#[cfg(test)]
mod usercache_tests {
    use super::*;

    #[test]
    fn reads_names_and_survives_garbage() {
        let dir = std::env::temp_dir().join(format!("mcsc-uc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read_usercache_names(&dir).is_empty());
        std::fs::write(dir.join("usercache.json"), b"not json").unwrap();
        assert!(read_usercache_names(&dir).is_empty());
        std::fs::write(
            dir.join("usercache.json"),
            br#"[{"name":"Anivia31","uuid":"5ad08d99-26a2-331c-8f77-d476dec25826","expiresOn":"x"},{"name":"","uuid":"y"}]"#,
        )
        .unwrap();
        assert_eq!(
            read_usercache_names(&dir),
            vec![(
                "Anivia31".to_string(),
                "5ad08d99-26a2-331c-8f77-d476dec25826".to_string()
            )]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
