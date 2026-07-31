//! Argon2 (RFC 9106) memory-hard password hashing.
//!
//! Hand-written on top of the BLAKE2b module for the same reason BLAKE2b is:
//! the runtime keeps its dependency set small, and `ring` offers neither
//! primitive. Only Argon2id is exposed to pith — it is the variant RFC 9106
//! recommends for password hashing — but the core supports all three
//! variants so the official test vectors for each can pin down the indexing
//! logic, which is where a subtly wrong implementation would still produce
//! plausible-looking output.
//!
//! The memory matrix is filled single-threaded. The `lanes` parameter still
//! changes the output as the RFC requires; it just doesn't fan out across
//! threads. For the parameter sizes a password hash uses, correctness and
//! simplicity beat the modest wall-clock win.

use crate::blake2b;

/// Ceiling on the memory cost, in KiB (1 GiB). The memory parameter is an
/// allocation request from whatever config supplied it, so the runtime bounds
/// it and fails cleanly instead of letting a bad value take down the process.
pub const MAX_MEMORY_KIB: u32 = 1 << 20;

/// Smallest salt accepted, per RFC 9106's recommendation for password hashing.
pub const MIN_SALT_LEN: usize = 8;

/// Largest salt accepted.
pub const MAX_SALT_LEN: usize = 1024;

/// Smallest tag (output) length, from RFC 9106.
pub const MIN_TAG_LEN: usize = 4;

/// Largest tag (output) length accepted.
pub const MAX_TAG_LEN: usize = 1024;

/// Ceiling on the pass count (RFC 9106 allows up to 2^32-1; nothing sane
/// needs more than this).
pub const MAX_PASSES: u32 = 1024;

/// Ceiling on parallelism.
pub const MAX_LANES: u32 = 64;

/// A memory block: 1024 bytes as 128 little-endian words.
const BLOCK_WORDS: usize = 128;
type Block = [u64; BLOCK_WORDS];
const ZERO_BLOCK: Block = [0u64; BLOCK_WORDS];

/// Each lane is filled in four slices, with a synchronisation point between
/// slices (RFC 9106 section 3.4).
const SYNC_POINTS: usize = 4;

const VERSION: u64 = 0x13;

// argon2d and argon2i are never built outside of tests: they exist so the
// official vectors can pin the indexing logic on every addressing path,
// while only argon2id is exposed for real use
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq)]
enum Variant {
    Argon2d = 0,
    Argon2i = 1,
    Argon2id = 2,
}

/// Compute an Argon2id tag. `passes` is the time cost, `memory_kib` the
/// memory cost in KiB, `lanes` the parallelism degree. `secret` and `ad` are
/// the optional keyed-secret and associated-data inputs; pass empty slices
/// when unused.
#[allow(clippy::too_many_arguments)]
pub fn argon2id(
    password: &[u8],
    salt: &[u8],
    secret: &[u8],
    ad: &[u8],
    passes: u32,
    memory_kib: u32,
    lanes: u32,
    tag_len: usize,
) -> Result<Vec<u8>, &'static str> {
    argon2(
        Variant::Argon2id,
        password,
        salt,
        secret,
        ad,
        passes,
        memory_kib,
        lanes,
        tag_len,
    )
}

#[allow(clippy::too_many_arguments)]
fn argon2(
    variant: Variant,
    password: &[u8],
    salt: &[u8],
    secret: &[u8],
    ad: &[u8],
    passes: u32,
    memory_kib: u32,
    lanes: u32,
    tag_len: usize,
) -> Result<Vec<u8>, &'static str> {
    if !(MIN_TAG_LEN..=MAX_TAG_LEN).contains(&tag_len) {
        return Err("argon2 tag length out of range");
    }
    if !(MIN_SALT_LEN..=MAX_SALT_LEN).contains(&salt.len()) {
        return Err("argon2 salt length out of range");
    }
    if passes == 0 || passes > MAX_PASSES {
        return Err("argon2 pass count out of range");
    }
    if lanes == 0 || lanes > MAX_LANES {
        return Err("argon2 parallelism out of range");
    }
    if memory_kib > MAX_MEMORY_KIB {
        return Err("argon2 memory cost above the runtime cap");
    }
    if memory_kib < 8 * lanes {
        return Err("argon2 memory cost below 8 blocks per lane");
    }

    let lanes = lanes as usize;
    // round the block count down to a multiple of 4 * lanes (RFC section 3.2:
    // m' = 4 * p * floor(m / 4p))
    let segment_len = memory_kib as usize / (SYNC_POINTS * lanes);
    let lane_len = segment_len * SYNC_POINTS;
    let block_count = lane_len * lanes;

    // this is the caller-sized allocation; fail cleanly instead of aborting
    // on out-of-memory
    let mut memory: Vec<Block> = Vec::new();
    if memory.try_reserve_exact(block_count).is_err() {
        return Err("argon2 memory allocation failed");
    }
    memory.resize(block_count, ZERO_BLOCK);

    let h0 = initial_hash(
        variant, password, salt, secret, ad, passes, memory_kib, lanes, tag_len,
    )?;

    let mut instance = Instance {
        memory,
        lanes,
        lane_len,
        segment_len,
        passes,
        variant,
    };
    instance.fill_first_blocks(&h0)?;
    for pass in 0..passes as usize {
        for slice in 0..SYNC_POINTS {
            for lane in 0..lanes {
                instance.fill_segment(pass, lane, slice);
            }
        }
    }
    let tag = instance.extract_tag(tag_len);
    // best-effort scrubbing of password-derived state
    instance.memory.fill(ZERO_BLOCK);
    tag
}

/// The initial 64-byte digest H_0 (RFC 9106 section 3.2).
#[allow(clippy::too_many_arguments)]
fn initial_hash(
    variant: Variant,
    password: &[u8],
    salt: &[u8],
    secret: &[u8],
    ad: &[u8],
    passes: u32,
    memory_kib: u32,
    lanes: usize,
    tag_len: usize,
) -> Result<[u8; 64], &'static str> {
    if password.len() > u32::MAX as usize
        || secret.len() > u32::MAX as usize
        || ad.len() > u32::MAX as usize
    {
        return Err("argon2 input too long");
    }
    let mut input = Vec::with_capacity(40 + password.len() + salt.len() + secret.len() + ad.len());
    for fixed in [
        lanes as u32,
        tag_len as u32,
        memory_kib,
        passes,
        VERSION as u32,
        variant as u32,
    ] {
        input.extend_from_slice(&fixed.to_le_bytes());
    }
    for var in [password, salt, secret, ad] {
        input.extend_from_slice(&(var.len() as u32).to_le_bytes());
        input.extend_from_slice(var);
    }
    let Some(digest) = blake2b::hash(64, b"", &input) else {
        return Err("argon2 initial hash failed");
    };
    input.fill(0);
    let mut h0 = [0u8; 64];
    h0.copy_from_slice(&digest);
    Ok(h0)
}

/// The variable-length hash H' (RFC 9106 section 3.3): plain BLAKE2b up to
/// 64 bytes, and a 32-byte chain of BLAKE2b digests beyond that.
fn h_prime(out: &mut [u8], input: &[u8]) -> Result<(), &'static str> {
    let total = out.len();
    let mut prefixed = Vec::with_capacity(4 + input.len());
    prefixed.extend_from_slice(&(total as u32).to_le_bytes());
    prefixed.extend_from_slice(input);
    if total <= 64 {
        let Some(digest) = blake2b::hash(total, b"", &prefixed) else {
            return Err("argon2 h' failed");
        };
        out.copy_from_slice(&digest);
        return Ok(());
    }
    let Some(mut v) = blake2b::hash(64, b"", &prefixed) else {
        return Err("argon2 h' failed");
    };
    let mut pos = 0;
    let mut remaining = total;
    while remaining > 64 {
        out[pos..pos + 32].copy_from_slice(&v[..32]);
        pos += 32;
        remaining -= 32;
        let next_len = remaining.min(64);
        let Some(next) = blake2b::hash(next_len, b"", &v) else {
            return Err("argon2 h' failed");
        };
        v = next;
    }
    out[pos..].copy_from_slice(&v);
    Ok(())
}

struct Instance {
    memory: Vec<Block>,
    lanes: usize,
    lane_len: usize,
    segment_len: usize,
    passes: u32,
    variant: Variant,
}

impl Instance {
    /// B[lane][0] and B[lane][1] seed each lane from H_0 (RFC section 3.2).
    fn fill_first_blocks(&mut self, h0: &[u8; 64]) -> Result<(), &'static str> {
        for lane in 0..self.lanes {
            for col in 0..2usize {
                let mut seed = [0u8; 72];
                seed[..64].copy_from_slice(h0);
                seed[64..68].copy_from_slice(&(col as u32).to_le_bytes());
                seed[68..72].copy_from_slice(&(lane as u32).to_le_bytes());
                let mut block_bytes = [0u8; 1024];
                h_prime(&mut block_bytes, &seed)?;
                self.memory[lane * self.lane_len + col] = block_from_le_bytes(&block_bytes);
            }
        }
        Ok(())
    }

    /// Fill one segment of one lane (RFC section 3.4). Argon2id switches from
    /// data-independent to data-dependent addressing after the first two
    /// slices of the first pass.
    fn fill_segment(&mut self, pass: usize, lane: usize, slice: usize) {
        let data_independent = match self.variant {
            Variant::Argon2d => false,
            Variant::Argon2i => true,
            Variant::Argon2id => pass == 0 && slice < 2,
        };

        // data-independent addressing draws J_1 || J_2 from a pseudo-random
        // block regenerated every 128 references
        let mut address_block = ZERO_BLOCK;
        let mut input_block = ZERO_BLOCK;
        if data_independent {
            input_block[0] = pass as u64;
            input_block[1] = lane as u64;
            input_block[2] = slice as u64;
            input_block[3] = self.memory.len() as u64;
            input_block[4] = self.passes as u64;
            input_block[5] = self.variant as u64;
            // input_block[6] is the counter; next_addresses bumps it
        }

        // the first two blocks of every lane are the seed blocks
        let mut starting_index = 0;
        if pass == 0 && slice == 0 {
            starting_index = 2;
            if data_independent {
                next_addresses(&mut address_block, &mut input_block);
            }
        }

        for index in starting_index..self.segment_len {
            let col = slice * self.segment_len + index;
            let prev_col = if col == 0 { self.lane_len - 1 } else { col - 1 };
            let prev_idx = lane * self.lane_len + prev_col;

            let pseudo_rand = if data_independent {
                if index % BLOCK_WORDS == 0 {
                    next_addresses(&mut address_block, &mut input_block);
                }
                address_block[index % BLOCK_WORDS]
            } else {
                self.memory[prev_idx][0]
            };

            let ref_lane = if pass == 0 && slice == 0 {
                lane
            } else {
                ((pseudo_rand >> 32) as usize) % self.lanes
            };
            let ref_index = self.index_alpha(
                pass,
                slice,
                index,
                pseudo_rand as u32,
                ref_lane == lane,
            );

            let prev = self.memory[prev_idx];
            let reference = self.memory[ref_lane * self.lane_len + ref_index];
            let with_xor = pass > 0;
            fill_block(
                &prev,
                &reference,
                &mut self.memory[lane * self.lane_len + col],
                with_xor,
            );
        }
    }

    /// Map J_1 to a reference block index (RFC section 3.4.1.2): compute the
    /// size of the reachable window, then pick a position skewed towards the
    /// window's recent end via the x^2 mapping.
    fn index_alpha(&self, pass: usize, slice: usize, index: usize, j1: u32, same_lane: bool) -> usize {
        let reference_area = if pass == 0 {
            if slice == 0 {
                // only this lane's earlier blocks in this segment (index is
                // at least 2 here, so this never underflows)
                index - 1
            } else if same_lane {
                slice * self.segment_len + index - 1
            } else {
                slice * self.segment_len - if index == 0 { 1 } else { 0 }
            }
        } else if same_lane {
            self.lane_len - self.segment_len + index - 1
        } else {
            self.lane_len - self.segment_len - if index == 0 { 1 } else { 0 }
        };

        let area = reference_area as u64;
        let x = (j1 as u64 * j1 as u64) >> 32;
        let relative = area - 1 - ((area * x) >> 32);

        // after the first pass the window starts just past the current slice
        // and wraps around the lane
        let start = if pass == 0 || slice == SYNC_POINTS - 1 {
            0
        } else {
            (slice + 1) * self.segment_len
        };
        (start + relative as usize) % self.lane_len
    }

    /// XOR the last column together and hash it into the tag (RFC 3.2 step 7).
    fn extract_tag(&self, tag_len: usize) -> Result<Vec<u8>, &'static str> {
        let mut last = self.memory[self.lane_len - 1];
        for lane in 1..self.lanes {
            let block = &self.memory[lane * self.lane_len + self.lane_len - 1];
            for (acc, word) in last.iter_mut().zip(block.iter()) {
                *acc ^= word;
            }
        }
        let mut block_bytes = [0u8; 1024];
        for (i, word) in last.iter().enumerate() {
            block_bytes[i * 8..i * 8 + 8].copy_from_slice(&word.to_le_bytes());
        }
        let mut tag = vec![0u8; tag_len];
        h_prime(&mut tag, &block_bytes)?;
        Ok(tag)
    }
}

fn block_from_le_bytes(bytes: &[u8; 1024]) -> Block {
    let mut block = ZERO_BLOCK;
    for (i, word) in block.iter_mut().enumerate() {
        let mut chunk = [0u8; 8];
        chunk.copy_from_slice(&bytes[i * 8..i * 8 + 8]);
        *word = u64::from_le_bytes(chunk);
    }
    block
}

/// Generate the next block of data-independent reference addresses:
/// addresses = G(zero, G(zero, input)) with the counter bumped first.
fn next_addresses(address: &mut Block, input: &mut Block) {
    input[6] += 1;
    let input_copy = *input;
    fill_block(&ZERO_BLOCK, &input_copy, address, false);
    let address_copy = *address;
    fill_block(&ZERO_BLOCK, &address_copy, address, false);
}

/// The compression function G (RFC section 3.5): XOR the inputs, run the
/// BLAKE2b-style permutation over rows then columns, and XOR the result back.
fn fill_block(prev: &Block, reference: &Block, next: &mut Block, with_xor: bool) {
    let mut r = ZERO_BLOCK;
    for (i, word) in r.iter_mut().enumerate() {
        *word = prev[i] ^ reference[i];
    }
    let mut z = r;

    // rows: 8 rows of 16 consecutive words
    for row in 0..8 {
        permute(&mut z, |i| row * 16 + i);
    }
    // columns: the block is an 8x8 grid of 16-byte cells; column `col`
    // gathers the two words of each cell in that column
    for col in 0..8 {
        permute(&mut z, |i| (i / 2) * 16 + col * 2 + i % 2);
    }

    for (i, word) in z.iter().enumerate() {
        let value = r[i] ^ word;
        if with_xor {
            next[i] ^= value;
        } else {
            next[i] = value;
        }
    }
}

/// The permutation P over 16 words selected by `idx`, mirroring a BLAKE2b
/// round with G_B replaced by the multiply-hardened G (RFC section 3.5).
fn permute(z: &mut Block, idx: impl Fn(usize) -> usize) {
    let mut v = [0u64; 16];
    for (i, word) in v.iter_mut().enumerate() {
        *word = z[idx(i)];
    }
    for (a, b, c, d) in [
        (0, 4, 8, 12),
        (1, 5, 9, 13),
        (2, 6, 10, 14),
        (3, 7, 11, 15),
        (0, 5, 10, 15),
        (1, 6, 11, 12),
        (2, 7, 8, 13),
        (3, 4, 9, 14),
    ] {
        quarter_round(&mut v, a, b, c, d);
    }
    for (i, word) in v.iter().enumerate() {
        z[idx(i)] = *word;
    }
}

fn quarter_round(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize) {
    v[a] = mul_add(v[a], v[b]);
    v[d] = (v[d] ^ v[a]).rotate_right(32);
    v[c] = mul_add(v[c], v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(24);
    v[a] = mul_add(v[a], v[b]);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = mul_add(v[c], v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(63);
}

/// a + b + 2 * lower32(a) * lower32(b), the multiplication that makes the
/// permutation compute-hard on 64-bit cores.
#[inline(always)]
fn mul_add(a: u64, b: u64) -> u64 {
    let product = (a as u32 as u64).wrapping_mul(b as u32 as u64);
    a.wrapping_add(b).wrapping_add(product.wrapping_add(product))
}

#[cfg(test)]
mod tests {
    use super::{argon2, argon2id, Variant};

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }

    // the three test vectors from RFC 9106 section 5 share these inputs:
    // m=32 KiB, t=3, p=4, 32-byte tag, and fixed password/salt/secret/ad
    fn rfc_vector(variant: Variant) -> Result<String, &'static str> {
        let password = [0x01u8; 32];
        let salt = [0x02u8; 16];
        let secret = [0x03u8; 8];
        let ad = [0x04u8; 12];
        argon2(variant, &password, &salt, &secret, &ad, 3, 32, 4, 32).map(|tag| hex(&tag))
    }

    #[test]
    fn rfc_9106_argon2d_vector() {
        assert_eq!(
            rfc_vector(Variant::Argon2d),
            Ok("512b391b6f1162975371d30919734294f868e3be3984f3c1a13a4db9fabe4acb".to_string())
        );
    }

    #[test]
    fn rfc_9106_argon2i_vector() {
        assert_eq!(
            rfc_vector(Variant::Argon2i),
            Ok("c814d9d1dc7f37aa13f0d77f2494bda1c8de6b016dd388d29952a4c4672b6ce8".to_string())
        );
    }

    #[test]
    fn rfc_9106_argon2id_vector() {
        assert_eq!(
            rfc_vector(Variant::Argon2id),
            Ok("0d640df58d78766c08c037a34a8b53c9d01ef0452d75b65eb52520e96b01e659".to_string())
        );
    }

    #[test]
    fn deterministic_and_salt_sensitive() {
        let a = argon2id(b"password", b"somesalt", b"", b"", 1, 8, 1, 32);
        let b = argon2id(b"password", b"somesalt", b"", b"", 1, 8, 1, 32);
        let c = argon2id(b"password", b"othersalt", b"", b"", 1, 8, 1, 32);
        assert!(a.is_ok());
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn parameters_change_the_tag() {
        let base = argon2id(b"password", b"somesalt", b"", b"", 2, 64, 1, 32);
        assert_ne!(base, argon2id(b"password", b"somesalt", b"", b"", 3, 64, 1, 32));
        assert_ne!(base, argon2id(b"password", b"somesalt", b"", b"", 2, 128, 1, 32));
        assert_ne!(base, argon2id(b"password", b"somesalt", b"", b"", 2, 64, 2, 32));
    }

    #[test]
    fn rejects_out_of_range_parameters() {
        let salt = b"somesalt";
        // salt too short
        assert!(argon2id(b"pw", b"short", b"", b"", 1, 8, 1, 32).is_err());
        // zero passes and zero lanes
        assert!(argon2id(b"pw", salt, b"", b"", 0, 8, 1, 32).is_err());
        assert!(argon2id(b"pw", salt, b"", b"", 1, 8, 0, 32).is_err());
        // memory below 8 blocks per lane, and above the runtime cap
        assert!(argon2id(b"pw", salt, b"", b"", 1, 7, 1, 32).is_err());
        assert!(argon2id(b"pw", salt, b"", b"", 1, 15, 2, 32).is_err());
        assert!(argon2id(b"pw", salt, b"", b"", 1, super::MAX_MEMORY_KIB + 1, 1, 32).is_err());
        // tag length outside 4..=1024
        assert!(argon2id(b"pw", salt, b"", b"", 1, 8, 1, 3).is_err());
        assert!(argon2id(b"pw", salt, b"", b"", 1, 8, 1, 1025).is_err());
        // the smallest legal configuration works
        assert!(argon2id(b"pw", salt, b"", b"", 1, 8, 1, 4).is_ok());
    }

    #[test]
    fn long_tags_use_the_hash_chain() {
        // tags over 64 bytes exercise the iterative H' construction
        let long = argon2id(b"password", b"somesalt", b"", b"", 1, 8, 1, 100);
        assert!(matches!(&long, Ok(tag) if tag.len() == 100));
        let longer = argon2id(b"password", b"somesalt", b"", b"", 1, 8, 1, 128);
        assert!(matches!(&longer, Ok(tag) if tag.len() == 128));
        // the shorter tag is not a prefix of the longer one
        if let (Ok(long), Ok(longer)) = (long, longer) {
            assert_ne!(long[..], longer[..100]);
        }
    }

    // not run by default: prints wall-clock timings for candidate password
    // hashing defaults. run with:
    //   cargo test --release -p pith-runtime -- --ignored timing --nocapture
    #[test]
    #[ignore]
    fn timing_for_candidate_defaults() {
        let candidates = [
            ("owasp m=19 MiB t=2 p=1", 2u32, 19 * 1024, 1u32),
            ("rfc second choice m=64 MiB t=3 p=4", 3, 64 * 1024, 4),
            ("m=32 MiB t=3 p=1", 3, 32 * 1024, 1),
        ];
        for (label, passes, memory_kib, lanes) in candidates {
            let start = std::time::Instant::now();
            let result = argon2id(b"password", b"somesalt", b"", b"", passes, memory_kib, lanes, 32);
            let elapsed = start.elapsed();
            assert!(result.is_ok());
            println!("{}: {:?}", label, elapsed);
        }
    }
}
