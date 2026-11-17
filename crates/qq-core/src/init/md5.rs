//! Standard MD5 digest implementation (RFC 1321) in pure Rust without external dependencies.
//! Used by `init` (template lock) and the rule-drift baseline: change detection, not security.

pub fn md5_hex(data: &[u8]) -> String {
    let mut a: u32 = 0x67452301;
    let mut b: u32 = 0xefcdab89;
    let mut c: u32 = 0x98badcfe;
    let mut d: u32 = 0x10325476;

    let orig_len_bits = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::from(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&orig_len_bits.to_le_bytes());

    for chunk in padded.as_chunks::<64>().0 {
        let mut m = [0u32; 16];
        for (i, part) in chunk.as_chunks::<4>().0.iter().enumerate() {
            m[i] = u32::from_le_bytes([part[0], part[1], part[2], part[3]]);
        }

        let (mut aa, mut bb, mut cc, mut dd) = (a, b, c, d);

        macro_rules! step {
            ($f:expr, $a:ident, $b:ident, $c:ident, $d:ident, $k:expr, $s:expr, $t:expr) => {
                $a = $a.wrapping_add($f).wrapping_add(m[$k]).wrapping_add($t);
                $a = $b.wrapping_add($a.rotate_left($s));
            };
        }

        // Round 1
        step!((bb & cc) | (!bb & dd), aa, bb, cc, dd, 0, 7, 0xd76aa478);
        step!((aa & bb) | (!aa & cc), dd, aa, bb, cc, 1, 12, 0xe8c7b756);
        step!((dd & aa) | (!dd & bb), cc, dd, aa, bb, 2, 17, 0x242070db);
        step!((cc & dd) | (!cc & aa), bb, cc, dd, aa, 3, 22, 0xc1bdceee);
        step!((bb & cc) | (!bb & dd), aa, bb, cc, dd, 4, 7, 0xf57c0faf);
        step!((aa & bb) | (!aa & cc), dd, aa, bb, cc, 5, 12, 0x4787c62a);
        step!((dd & aa) | (!dd & bb), cc, dd, aa, bb, 6, 17, 0xa8304613);
        step!((cc & dd) | (!cc & aa), bb, cc, dd, aa, 7, 22, 0xfd469501);
        step!((bb & cc) | (!bb & dd), aa, bb, cc, dd, 8, 7, 0x698098d8);
        step!((aa & bb) | (!aa & cc), dd, aa, bb, cc, 9, 12, 0x8b44f7af);
        step!((dd & aa) | (!dd & bb), cc, dd, aa, bb, 10, 17, 0xffff5bb1);
        step!((cc & dd) | (!cc & aa), bb, cc, dd, aa, 11, 22, 0x895cd7be);
        step!((bb & cc) | (!bb & dd), aa, bb, cc, dd, 12, 7, 0x6b901122);
        step!((aa & bb) | (!aa & cc), dd, aa, bb, cc, 13, 12, 0xfd987193);
        step!((dd & aa) | (!dd & bb), cc, dd, aa, bb, 14, 17, 0xa679438e);
        step!((cc & dd) | (!cc & aa), bb, cc, dd, aa, 15, 22, 0x49b40821);

        // Round 2
        step!((bb & dd) | (cc & !dd), aa, bb, cc, dd, 1, 5, 0xf61e2562);
        step!((aa & cc) | (bb & !cc), dd, aa, bb, cc, 6, 9, 0xc040b340);
        step!((dd & bb) | (aa & !bb), cc, dd, aa, bb, 11, 14, 0x265e5a51);
        step!((cc & aa) | (dd & !aa), bb, cc, dd, aa, 0, 20, 0xe9b6c7aa);
        step!((bb & dd) | (cc & !dd), aa, bb, cc, dd, 5, 5, 0xd62f105d);
        step!((aa & cc) | (bb & !cc), dd, aa, bb, cc, 10, 9, 0x02441453);
        step!((dd & bb) | (aa & !bb), cc, dd, aa, bb, 15, 14, 0xd8a1e681);
        step!((cc & aa) | (dd & !aa), bb, cc, dd, aa, 4, 20, 0xe7d3fbc8);
        step!((bb & dd) | (cc & !dd), aa, bb, cc, dd, 9, 5, 0x21e1cde6);
        step!((aa & cc) | (bb & !cc), dd, aa, bb, cc, 14, 9, 0xc33707d6);
        step!((dd & bb) | (aa & !bb), cc, dd, aa, bb, 3, 14, 0xf4d50d87);
        step!((cc & aa) | (dd & !aa), bb, cc, dd, aa, 8, 20, 0x455a14ed);
        step!((bb & dd) | (cc & !dd), aa, bb, cc, dd, 13, 5, 0xa9e3e905);
        step!((aa & cc) | (bb & !cc), dd, aa, bb, cc, 2, 9, 0xfcefa3f8);
        step!((dd & bb) | (aa & !bb), cc, dd, aa, bb, 7, 14, 0x676f02d9);
        step!((cc & aa) | (dd & !aa), bb, cc, dd, aa, 12, 20, 0x8d2a4c8a);

        // Round 3
        step!(bb ^ cc ^ dd, aa, bb, cc, dd, 5, 4, 0xfffa3942);
        step!(aa ^ bb ^ cc, dd, aa, bb, cc, 8, 11, 0x8771f681);
        step!(dd ^ aa ^ bb, cc, dd, aa, bb, 11, 16, 0x6d9d6122);
        step!(cc ^ dd ^ aa, bb, cc, dd, aa, 14, 23, 0xfde5380c);
        step!(bb ^ cc ^ dd, aa, bb, cc, dd, 1, 4, 0xa4beea44);
        step!(aa ^ bb ^ cc, dd, aa, bb, cc, 4, 11, 0x4bdecfa9);
        step!(dd ^ aa ^ bb, cc, dd, aa, bb, 7, 16, 0xf6bb4b60);
        step!(cc ^ dd ^ aa, bb, cc, dd, aa, 10, 23, 0xbebfbc70);
        step!(bb ^ cc ^ dd, aa, bb, cc, dd, 13, 4, 0x289b7ec6);
        step!(aa ^ bb ^ cc, dd, aa, bb, cc, 0, 11, 0xeaa127fa);
        step!(dd ^ aa ^ bb, cc, dd, aa, bb, 3, 16, 0xd4ef3085);
        step!(cc ^ dd ^ aa, bb, cc, dd, aa, 6, 23, 0x04881d05);
        step!(bb ^ cc ^ dd, aa, bb, cc, dd, 9, 4, 0xd9d4d039);
        step!(aa ^ bb ^ cc, dd, aa, bb, cc, 12, 11, 0xe6db99e5);
        step!(dd ^ aa ^ bb, cc, dd, aa, bb, 15, 16, 0x1fa27cf8);
        step!(cc ^ dd ^ aa, bb, cc, dd, aa, 2, 23, 0xc4ac5665);

        // Round 4
        step!(cc ^ (bb | !dd), aa, bb, cc, dd, 0, 6, 0xf4292244);
        step!(bb ^ (aa | !cc), dd, aa, bb, cc, 7, 10, 0x432aff97);
        step!(aa ^ (dd | !bb), cc, dd, aa, bb, 14, 15, 0xab9423a7);
        step!(dd ^ (cc | !aa), bb, cc, dd, aa, 5, 21, 0xfc93a039);
        step!(cc ^ (bb | !dd), aa, bb, cc, dd, 12, 6, 0x655b59c3);
        step!(bb ^ (aa | !cc), dd, aa, bb, cc, 3, 10, 0x8f0ccc92);
        step!(aa ^ (dd | !bb), cc, dd, aa, bb, 10, 15, 0xffeff47d);
        step!(dd ^ (cc | !aa), bb, cc, dd, aa, 1, 21, 0x85845dd1);
        step!(cc ^ (bb | !dd), aa, bb, cc, dd, 8, 6, 0x6fa87e4f);
        step!(bb ^ (aa | !cc), dd, aa, bb, cc, 15, 10, 0xfe2ce6e0);
        step!(aa ^ (dd | !bb), cc, dd, aa, bb, 6, 15, 0xa3014314);
        step!(dd ^ (cc | !aa), bb, cc, dd, aa, 13, 21, 0x4e0811a1);
        step!(cc ^ (bb | !dd), aa, bb, cc, dd, 4, 6, 0xf7537e82);
        step!(bb ^ (aa | !cc), dd, aa, bb, cc, 11, 10, 0xbd3af235);
        step!(aa ^ (dd | !bb), cc, dd, aa, bb, 2, 15, 0x2ad7d2bb);
        step!(dd ^ (cc | !aa), bb, cc, dd, aa, 9, 21, 0xeb86d391);

        a = a.wrapping_add(aa);
        b = b.wrapping_add(bb);
        c = c.wrapping_add(cc);
        d = d.wrapping_add(dd);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a.to_le_bytes());
    out[4..8].copy_from_slice(&b.to_le_bytes());
    out[8..12].copy_from_slice(&c.to_le_bytes());
    out[12..16].copy_from_slice(&d.to_le_bytes());

    let mut hex = String::with_capacity(32);
    for byte in out {
        use std::fmt::Write;
        let _ = write!(hex, "{:02x}", byte);
    }
    hex
}
