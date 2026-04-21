use crate::{ensure_perf_stats_registered, perf_count, PERF_BYTES_ALLOCS, PERF_BYTES_ALLOC_BYTES};
use crate::{PERF_BYTE_BUFFER_NEWS, PERF_BYTE_BUFFER_WRITES, PERF_BYTE_BUFFER_WRITE_BYTES};

#[repr(C)]
pub(crate) struct ForgeBytes {
    pub(crate) data_ptr: *const u8,
    pub(crate) data_len: usize,
    pub(crate) data: Vec<u8>,
}

pub(crate) struct ForgeByteBuffer {
    pub(crate) data: Vec<u8>,
}

pub(crate) unsafe fn forge_bytes_ref<'a>(handle: i64) -> Option<&'a ForgeBytes> {
    if handle == 0 {
        return None;
    }
    Some(&*(handle as *const ForgeBytes))
}

unsafe fn forge_byte_buffer_mut<'a>(handle: i64) -> Option<&'a mut ForgeByteBuffer> {
    if handle == 0 {
        return None;
    }
    Some(&mut *(handle as *mut ForgeByteBuffer))
}

pub(crate) fn forge_bytes_from_vec(data: Vec<u8>) -> i64 {
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTES_ALLOCS, 1);
    perf_count(&PERF_BYTES_ALLOC_BYTES, data.len());
    let data_ptr = data.as_ptr();
    let data_len = data.len();
    Box::into_raw(Box::new(ForgeBytes {
        data_ptr,
        data_len,
        data,
    })) as i64
}

#[no_mangle]
pub unsafe extern "C" fn forge_bytes_from_string_utf8(s: *const i8) -> i64 {
    if s.is_null() {
        return forge_bytes_from_vec(Vec::new());
    }
    let len = crate::string::forge_cstring_len(s) as usize;
    let bytes = std::slice::from_raw_parts(s as *const u8, len);
    forge_bytes_from_vec(bytes.to_vec())
}

#[no_mangle]
pub unsafe extern "C" fn forge_bytes_to_string_utf8(handle: i64) -> *mut i8 {
    let Some(bytes) = forge_bytes_ref(handle) else {
        return std::ptr::null_mut();
    };
    if std::str::from_utf8(&bytes.data).is_err() {
        return std::ptr::null_mut();
    }
    crate::forge_copy_bytes_to_cstring(&bytes.data)
}

#[no_mangle]
pub unsafe extern "C" fn forge_bytes_len(handle: i64) -> i64 {
    let Some(bytes) = forge_bytes_ref(handle) else {
        return 0;
    };
    bytes.data.len() as i64
}

#[no_mangle]
pub unsafe extern "C" fn forge_bytes_is_empty(handle: i64) -> i64 {
    let Some(bytes) = forge_bytes_ref(handle) else {
        return 1;
    };
    if bytes.data.is_empty() {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_bytes_get(handle: i64, idx: i64) -> i64 {
    let Some(bytes) = forge_bytes_ref(handle) else {
        return 0;
    };
    if idx < 0 {
        return 0;
    }
    bytes.data.get(idx as usize).copied().unwrap_or(0) as i64
}

#[no_mangle]
pub unsafe extern "C" fn forge_bytes_slice(handle: i64, start: i64, end: i64) -> i64 {
    let Some(bytes) = forge_bytes_ref(handle) else {
        return 0;
    };
    let len = bytes.data.len() as i64;
    let mut start_idx = start.max(0).min(len);
    let mut end_idx = end.max(0).min(len);
    if end_idx < start_idx {
        std::mem::swap(&mut start_idx, &mut end_idx);
    }
    forge_bytes_from_vec(bytes.data[start_idx as usize..end_idx as usize].to_vec())
}

#[no_mangle]
pub unsafe extern "C" fn forge_bytes_concat(a: i64, b: i64) -> i64 {
    let Some(a_bytes) = forge_bytes_ref(a) else {
        return 0;
    };
    let Some(b_bytes) = forge_bytes_ref(b) else {
        return 0;
    };
    let mut out = Vec::with_capacity(a_bytes.data.len() + b_bytes.data.len());
    out.extend_from_slice(&a_bytes.data);
    out.extend_from_slice(&b_bytes.data);
    forge_bytes_from_vec(out)
}

#[no_mangle]
pub unsafe extern "C" fn forge_bytes_eq(a: i64, b: i64) -> i64 {
    if a == 0 && b == 0 {
        return 1;
    }
    let Some(a_bytes) = forge_bytes_ref(a) else {
        return 0;
    };
    let Some(b_bytes) = forge_bytes_ref(b) else {
        return 0;
    };
    if a_bytes.data == b_bytes.data {
        1
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn forge_byte_buffer_new() -> i64 {
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_NEWS, 1);
    Box::into_raw(Box::new(ForgeByteBuffer { data: Vec::new() })) as i64
}

#[no_mangle]
pub extern "C" fn forge_byte_buffer_with_capacity(capacity: i64) -> i64 {
    let cap = if capacity > 0 { capacity as usize } else { 0 };
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_NEWS, 1);
    Box::into_raw(Box::new(ForgeByteBuffer {
        data: Vec::with_capacity(cap),
    })) as i64
}

#[no_mangle]
pub unsafe extern "C" fn forge_byte_buffer_write(handle: i64, data: i64) -> i64 {
    let Some(buffer) = forge_byte_buffer_mut(handle) else {
        return 0;
    };
    let Some(bytes) = forge_bytes_ref(data) else {
        return 0;
    };
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_WRITES, 1);
    perf_count(&PERF_BYTE_BUFFER_WRITE_BYTES, bytes.data.len());
    buffer.data.extend_from_slice(&bytes.data);
    bytes.data.len() as i64
}

#[no_mangle]
pub unsafe extern "C" fn forge_byte_buffer_write_string_utf8(handle: i64, s: *const i8) -> i64 {
    let Some(buffer) = forge_byte_buffer_mut(handle) else {
        return 0;
    };
    if s.is_null() {
        return 0;
    }
    let len = crate::string::forge_cstring_len(s) as usize;
    let bytes = std::slice::from_raw_parts(s as *const u8, len);
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_WRITES, 1);
    perf_count(&PERF_BYTE_BUFFER_WRITE_BYTES, bytes.len());
    buffer.data.extend_from_slice(bytes);
    bytes.len() as i64
}

#[no_mangle]
pub unsafe extern "C" fn forge_byte_buffer_write_byte(handle: i64, value: i64) -> i64 {
    let Some(buffer) = forge_byte_buffer_mut(handle) else {
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
pub unsafe extern "C" fn forge_byte_buffer_bytes(handle: i64) -> i64 {
    let Some(buffer) = forge_byte_buffer_mut(handle) else {
        return 0;
    };
    forge_bytes_from_vec(buffer.data.clone())
}

#[no_mangle]
pub unsafe extern "C" fn forge_byte_buffer_clear(handle: i64) {
    if let Some(buffer) = forge_byte_buffer_mut(handle) {
        buffer.data.clear();
    }
}
