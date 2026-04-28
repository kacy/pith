use crate::{ensure_perf_stats_registered, perf_count, PERF_BYTES_ALLOCS, PERF_BYTES_ALLOC_BYTES};
use crate::{PERF_BYTE_BUFFER_NEWS, PERF_BYTE_BUFFER_WRITES, PERF_BYTE_BUFFER_WRITE_BYTES};
use std::io::Read;

#[repr(C)]
pub(crate) struct PithBytes {
    pub(crate) data_ptr: *const u8,
    pub(crate) data_len: usize,
    pub(crate) data: Vec<u8>,
}

pub(crate) struct PithByteBuffer {
    pub(crate) data: Vec<u8>,
}

pub(crate) unsafe fn pith_bytes_ref<'a>(handle: i64) -> Option<&'a PithBytes> {
    if handle == 0 {
        return None;
    }
    Some(&*(handle as *const PithBytes))
}

unsafe fn pith_byte_buffer_mut<'a>(handle: i64) -> Option<&'a mut PithByteBuffer> {
    if handle == 0 {
        return None;
    }
    Some(&mut *(handle as *mut PithByteBuffer))
}

pub(crate) fn pith_bytes_from_vec(data: Vec<u8>) -> i64 {
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTES_ALLOCS, 1);
    perf_count(&PERF_BYTES_ALLOC_BYTES, data.len());
    let data_ptr = data.as_ptr();
    let data_len = data.len();
    Box::into_raw(Box::new(PithBytes {
        data_ptr,
        data_len,
        data,
    })) as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_from_string_utf8(s: *const i8) -> i64 {
    if s.is_null() {
        return pith_bytes_from_vec(Vec::new());
    }
    let len = crate::string::pith_cstring_len(s) as usize;
    let bytes = std::slice::from_raw_parts(s as *const u8, len);
    pith_bytes_from_vec(bytes.to_vec())
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_to_string_utf8(handle: i64) -> *mut i8 {
    let Some(bytes) = pith_bytes_ref(handle) else {
        return std::ptr::null_mut();
    };
    if std::str::from_utf8(&bytes.data).is_err() {
        return std::ptr::null_mut();
    }
    crate::pith_copy_bytes_to_cstring(&bytes.data)
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_len(handle: i64) -> i64 {
    let Some(bytes) = pith_bytes_ref(handle) else {
        return 0;
    };
    bytes.data.len() as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_is_empty(handle: i64) -> i64 {
    let Some(bytes) = pith_bytes_ref(handle) else {
        return 1;
    };
    if bytes.data.is_empty() {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_get(handle: i64, idx: i64) -> i64 {
    let Some(bytes) = pith_bytes_ref(handle) else {
        return 0;
    };
    if idx < 0 {
        return 0;
    }
    bytes.data.get(idx as usize).copied().unwrap_or(0) as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_slice(handle: i64, start: i64, end: i64) -> i64 {
    let Some(bytes) = pith_bytes_ref(handle) else {
        return 0;
    };
    let len = bytes.data.len() as i64;
    let mut start_idx = start.max(0).min(len);
    let mut end_idx = end.max(0).min(len);
    if end_idx < start_idx {
        std::mem::swap(&mut start_idx, &mut end_idx);
    }
    pith_bytes_from_vec(bytes.data[start_idx as usize..end_idx as usize].to_vec())
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_concat(a: i64, b: i64) -> i64 {
    let Some(a_bytes) = pith_bytes_ref(a) else {
        return 0;
    };
    let Some(b_bytes) = pith_bytes_ref(b) else {
        return 0;
    };
    let mut out = Vec::with_capacity(a_bytes.data.len() + b_bytes.data.len());
    out.extend_from_slice(&a_bytes.data);
    out.extend_from_slice(&b_bytes.data);
    pith_bytes_from_vec(out)
}

#[no_mangle]
pub unsafe extern "C" fn pith_bytes_eq(a: i64, b: i64) -> i64 {
    if a == 0 && b == 0 {
        return 1;
    }
    let Some(a_bytes) = pith_bytes_ref(a) else {
        return 0;
    };
    let Some(b_bytes) = pith_bytes_ref(b) else {
        return 0;
    };
    if a_bytes.data == b_bytes.data {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_crypto_constant_time_eq(a: i64, b: i64) -> i64 {
    let a_bytes = pith_bytes_ref(a).map(|bytes| bytes.data.as_slice()).unwrap_or(&[]);
    let b_bytes = pith_bytes_ref(b).map(|bytes| bytes.data.as_slice()).unwrap_or(&[]);
    let max_len = a_bytes.len().max(b_bytes.len());
    let mut diff = (a_bytes.len() ^ b_bytes.len()) as u8;

    for i in 0..max_len {
        let left = a_bytes.get(i).copied().unwrap_or(0);
        let right = b_bytes.get(i).copied().unwrap_or(0);
        diff |= left ^ right;
    }

    if diff == 0 { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn pith_secure_random_bytes(count: i64) -> i64 {
    let len = count.max(0) as usize;
    let mut out = vec![0_u8; len];
    if len == 0 {
        return pith_bytes_from_vec(out);
    }

    match std::fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut out)) {
        Ok(()) => pith_bytes_from_vec(out),
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "C" fn pith_byte_buffer_new() -> i64 {
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_NEWS, 1);
    Box::into_raw(Box::new(PithByteBuffer { data: Vec::new() })) as i64
}

#[no_mangle]
pub extern "C" fn pith_byte_buffer_with_capacity(capacity: i64) -> i64 {
    let cap = if capacity > 0 { capacity as usize } else { 0 };
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_NEWS, 1);
    Box::into_raw(Box::new(PithByteBuffer {
        data: Vec::with_capacity(cap),
    })) as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_write(handle: i64, data: i64) -> i64 {
    let Some(buffer) = pith_byte_buffer_mut(handle) else {
        return 0;
    };
    let Some(bytes) = pith_bytes_ref(data) else {
        return 0;
    };
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_WRITES, 1);
    perf_count(&PERF_BYTE_BUFFER_WRITE_BYTES, bytes.data.len());
    buffer.data.extend_from_slice(&bytes.data);
    bytes.data.len() as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_write_string_utf8(handle: i64, s: *const i8) -> i64 {
    let Some(buffer) = pith_byte_buffer_mut(handle) else {
        return 0;
    };
    if s.is_null() {
        return 0;
    }
    let len = crate::string::pith_cstring_len(s) as usize;
    let bytes = std::slice::from_raw_parts(s as *const u8, len);
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_WRITES, 1);
    perf_count(&PERF_BYTE_BUFFER_WRITE_BYTES, bytes.len());
    buffer.data.extend_from_slice(bytes);
    bytes.len() as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_write_byte(handle: i64, value: i64) -> i64 {
    let Some(buffer) = pith_byte_buffer_mut(handle) else {
        return 0;
    };
    if !(0..=255).contains(&value) {
        return 0;
    }
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_WRITES, 1);
    perf_count(&PERF_BYTE_BUFFER_WRITE_BYTES, 1);
    buffer.data.push(value as u8);
    1
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_bytes(handle: i64) -> i64 {
    let Some(buffer) = pith_byte_buffer_mut(handle) else {
        return 0;
    };
    pith_bytes_from_vec(buffer.data.clone())
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_len(handle: i64) -> i64 {
    let Some(buffer) = pith_byte_buffer_mut(handle) else {
        return 0;
    };
    buffer.data.len() as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_get(handle: i64, index: i64) -> i64 {
    let Some(buffer) = pith_byte_buffer_mut(handle) else {
        return 0;
    };
    if index < 0 {
        return 0;
    }
    buffer.data.get(index as usize).copied().unwrap_or(0) as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_set(handle: i64, index: i64, value: i64) -> i64 {
    let Some(buffer) = pith_byte_buffer_mut(handle) else {
        return 0;
    };
    if index < 0 || !(0..=255).contains(&value) {
        return 0;
    }
    let idx = index as usize;
    if idx >= buffer.data.len() {
        return 0;
    }
    buffer.data[idx] = value as u8;
    1
}

#[no_mangle]
pub unsafe extern "C" fn pith_byte_buffer_clear(handle: i64) {
    if let Some(buffer) = pith_byte_buffer_mut(handle) {
        buffer.data.clear();
    }
}
