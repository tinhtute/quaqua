//! Standard MD5 digest implementation (RFC 1321) in pure Rust without external dependencies.
//! Used by `quaqua worker prompt` for pane text and git diff fingerprints.

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

/// Standard SHA-256 digest implementation (FIPS 180-4) in pure Rust without external dependencies.
/// Used for layout topology snapshots and integrity verification.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    let mut h0: u32 = 0x6a09e667;
    let mut h1: u32 = 0xbb67ae85;
    let mut h2: u32 = 0x3c6ef372;
    let mut h3: u32 = 0xa54ff53a;
    let mut h4: u32 = 0x510e527f;
    let mut h5: u32 = 0x9b05688c;
    let mut h6: u32 = 0x1f83d9ab;
    let mut h7: u32 = 0x5be0cd19;

    let orig_len_bits = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::from(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&orig_len_bits.to_be_bytes());

    for chunk in padded.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, wi) in w.iter_mut().take(16).enumerate() {
            let start = i * 4;
            *wi = u32::from_be_bytes([
                chunk[start],
                chunk[start + 1],
                chunk[start + 2],
                chunk[start + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = h0;
        let mut b = h1;
        let mut c = h2;
        let mut d = h3;
        let mut e = h4;
        let mut f = h5;
        let mut g = h6;
        let mut h = h7;

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
        h5 = h5.wrapping_add(f);
        h6 = h6.wrapping_add(g);
        h7 = h7.wrapping_add(h);
    }

    let mut out = [0u8; 32];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    out[20..24].copy_from_slice(&h5.to_be_bytes());
    out[24..28].copy_from_slice(&h6.to_be_bytes());
    out[28..32].copy_from_slice(&h7.to_be_bytes());

    let mut hex = String::with_capacity(64);
    for byte in out {
        use std::fmt::Write;
        let _ = write!(hex, "{:02x}", byte);
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_md5_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"a"), "0cc175b9c0f1b6a831c399e269772661");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"message digest"),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        assert_eq!(
            md5_hex(b"The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn test_sha256_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"The quick brown fox jumps over the lazy dog"),
            "d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592"
        );
    }
}
