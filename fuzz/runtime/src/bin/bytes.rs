//! A sequence of bytes.rs calls over a small pool of bytes values and one
//! byte buffer, with arbitrary indexes, each checked against a Vec model.
//! Values are shared and released the way compiled code does it, so a
//! count the runtime gets wrong shows up as a leak or a use after free.
#![no_main]

use pith_runtime::bytes::*;
use pith_runtime::ffi_util::ResultPair;
use pith_runtime_fuzz::{bytes_value, bytes_vec, cstr_vec, fuzz_target, take_cstr, Input, PStr};

/// The largest single growth the harness asks for; past it a call is an
/// allocation the program asked for, not a runtime defect.
const MAX_GROW: i64 = 1 << 16;
const POOL: usize = 6;
const MAX_OPS: usize = 96;

struct State {
    pool: Vec<(i64, Vec<u8>)>,
    buffer: i64,
    model: Vec<u8>,
}

impl State {
    /// Hand a fresh owned value to the pool, dropping the oldest when full.
    unsafe fn adopt(&mut self, handle: i64, want: Vec<u8>) {
        assert_ne!(handle, 0);
        assert_eq!(bytes_vec(handle), want);
        if self.pool.len() == POOL {
            let (old, _) = self.pool.remove(0);
            pith_bytes_release(old);
        }
        self.pool.push((handle, want));
    }

    fn pick(&self, choice: u8) -> Option<(i64, Vec<u8>)> {
        if self.pool.is_empty() {
            return None;
        }
        Some(self.pool[choice as usize % self.pool.len()].clone())
    }
}

unsafe fn take_pair(pair: ResultPair) -> Result<i64, Vec<u8>> {
    if pair.is_ok == 1 {
        Ok(pair.payload)
    } else {
        Err(take_cstr(pair.payload as *mut i8).expect("error message"))
    }
}

fn clamp(len: usize, start: i64, end: i64) -> (usize, usize) {
    let len = len as i64;
    let lo = start.max(0).min(len);
    let hi = end.max(lo).min(len);
    (lo as usize, hi as usize)
}

fuzz_target!(|data| {
    let mut input = Input::new(data);
    let mut st = State { pool: Vec::new(), buffer: pith_byte_buffer_new(), model: Vec::new() };
    unsafe {
        let mut ops = 0;
        while !input.is_empty() && ops < MAX_OPS {
            ops += 1;
            let op = input.byte();
            let choice = input.byte();
            match op % 18 {
                0 => {
                    let raw = input.chunk().to_vec();
                    st.adopt(bytes_value(&raw), raw);
                }
                1 => {
                    let Some((h, m)) = st.pick(choice) else { continue };
                    let i = input.index(m.len());
                    let want = usize::try_from(i).ok().and_then(|i| m.get(i)).copied();
                    assert_eq!(pith_bytes_get(h, i), want.unwrap_or(0) as i64);
                    if let Some(b) = want {
                        assert_eq!(pith_bytes_get_strict(h, i), b as i64);
                    }
                    assert_eq!(pith_bytes_len(h), m.len() as i64);
                    assert_eq!(pith_bytes_is_empty(h), m.is_empty() as i64);
                }
                2 => {
                    let Some((h, m)) = st.pick(choice) else { continue };
                    let a = input.index(m.len());
                    let b = input.index(m.len());
                    let (lo, hi) = clamp(m.len(), a, b);
                    let part = pith_bytes_slice(h, a, b);
                    if lo == 0 && hi == m.len() {
                        assert_eq!(part, h, "a whole-range slice shares the value");
                    }
                    st.adopt(part, m[lo..hi].to_vec());
                }
                3 => {
                    let (Some((a, am)), Some((b, bm))) = (st.pick(choice), st.pick(input.byte())) else { continue };
                    // repeated concats double a value per op
                    if (am.len() + bm.len()) as i64 > MAX_GROW {
                        continue;
                    }
                    let joined = pith_bytes_concat(a, b);
                    st.adopt(joined, [am, bm].concat());
                }
                4 => {
                    let (Some((a, am)), Some((b, bm))) = (st.pick(choice), st.pick(input.byte())) else { continue };
                    assert_eq!(pith_bytes_eq(a, b), (am == bm) as i64);
                    assert_eq!(pith_crypto_constant_time_eq(a, b), (am == bm) as i64);
                }
                5 => {
                    let Some((h, m)) = st.pick(choice) else { continue };
                    let off = input.index(m.len());
                    let count = input.int();
                    let want = match (usize::try_from(off), count) {
                        (Ok(off), 1..=8) if off.checked_add(count as usize).is_some_and(|end| end <= m.len()) => {
                            let mut le = [0u8; 8];
                            le[..count as usize].copy_from_slice(&m[off..off + count as usize]);
                            i64::from_le_bytes(le)
                        }
                        _ => 0,
                    };
                    assert_eq!(pith_bytes_read_word(h, off, count), want);
                }
                6 => {
                    let Some((h, m)) = st.pick(choice) else { continue };
                    let s = take_cstr(pith_bytes_to_string_utf8(h));
                    assert_eq!(s, std::str::from_utf8(&m).ok().map(|_| m.clone()));
                    // a second owner and its release leave the value as it was
                    pith_bytes_retain(h);
                    pith_bytes_release(h);
                    assert_eq!(bytes_vec(h), m);
                }
                7 => {
                    let v = input.int();
                    let ok = (0..=255).contains(&v);
                    assert_eq!(pith_byte_buffer_write_byte(st.buffer, v), ok as i64);
                    if ok {
                        st.model.push(v as u8);
                    }
                }
                8 => {
                    let Some((h, m)) = st.pick(choice) else { continue };
                    assert_eq!(take_pair(pith_byte_buffer_write(st.buffer, h)), Ok(m.len() as i64));
                    st.model.extend_from_slice(&m);
                }
                9 => {
                    let s = PStr::new(input.chunk(), choice);
                    let c = s.content();
                    assert_eq!(take_pair(pith_byte_buffer_write_string_utf8(st.buffer, s.ptr())), Ok(c.len() as i64));
                    st.model.extend_from_slice(&c);
                }
                10 => {
                    let src = input.index(st.model.len());
                    let len = input.index(64);
                    let valid = src >= 0 && len >= 0 && (src as usize) < st.model.len();
                    if valid && len > MAX_GROW {
                        continue;
                    }
                    assert_eq!(pith_byte_buffer_copy_within(st.buffer, src, len), valid as i64);
                    if valid {
                        for i in 0..len as usize {
                            let b = st.model[src as usize + i];
                            st.model.push(b);
                        }
                    }
                }
                11 => {
                    let Some((h, m)) = st.pick(choice) else { continue };
                    let start = input.index(m.len());
                    let len = input.index(m.len());
                    let range = match (usize::try_from(start), usize::try_from(len)) {
                        (Ok(s), Ok(l)) => s.checked_add(l).filter(|&e| e <= m.len()).map(|e| (s, e)),
                        _ => None,
                    };
                    assert_eq!(pith_byte_buffer_write_range(st.buffer, h, start, len), range.is_some() as i64);
                    if let Some((s, e)) = range {
                        st.model.extend_from_slice(&m[s..e]);
                    }
                }
                12 => {
                    let word = input.int();
                    let count = input.int();
                    let ok = (1..=8).contains(&count);
                    assert_eq!(pith_byte_buffer_write_word(st.buffer, word, count), ok as i64);
                    if ok {
                        st.model.extend_from_slice(&word.to_le_bytes()[..count as usize]);
                    }
                }
                13 => {
                    let i = input.index(st.model.len());
                    let v = input.int();
                    let at = usize::try_from(i).ok().filter(|&i| i < st.model.len());
                    assert_eq!(pith_byte_buffer_get(st.buffer, i), at.map_or(0, |i| st.model[i] as i64));
                    let ok = at.is_some() && (0..=255).contains(&v);
                    assert_eq!(pith_byte_buffer_set(st.buffer, i, v), ok as i64);
                    if ok {
                        st.model[i as usize] = v as u8;
                    }
                }
                14 => {
                    assert_eq!(pith_byte_buffer_len(st.buffer), st.model.len() as i64);
                    let copy = pith_byte_buffer_bytes(st.buffer);
                    let model = st.model.clone();
                    st.adopt(copy, model);
                    let s = pith_byte_buffer_string(st.buffer);
                    assert_eq!(cstr_vec(s), Some(st.model.clone()));
                    pith_runtime::pith_cstring_release(s);
                }
                15 => {
                    pith_byte_buffer_clear(st.buffer);
                    st.model.clear();
                }
                16 => {
                    let taken = pith_byte_buffer_take_bytes(st.buffer);
                    let model = std::mem::take(&mut st.model);
                    st.adopt(taken, model);
                    let cap = input.int();
                    st.buffer = if cap > MAX_GROW {
                        pith_byte_buffer_new()
                    } else {
                        pith_byte_buffer_with_capacity(cap)
                    };
                }
                _ => {
                    // drop a value early, the way a last use does
                    if !st.pool.is_empty() {
                        let (h, _) = st.pool.remove(choice as usize % st.pool.len());
                        pith_bytes_release(h);
                    }
                }
            }
        }
        for (h, m) in st.pool.drain(..) {
            assert_eq!(bytes_vec(h), m);
            pith_bytes_release(h);
        }
        pith_byte_buffer_free(st.buffer);
    }
});
