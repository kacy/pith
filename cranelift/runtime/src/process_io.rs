fn pith_read_process_stream<R: std::io::Read>(reader: &mut R, max_bytes: i64) -> *mut i8 {
    let size = if max_bytes > 0 { max_bytes as usize } else { 4096 };
    let mut buf = vec![0u8; size];
    match reader.read(&mut buf) {
        Ok(0) => unsafe { crate::pith_cstring_empty() },
        Ok(n) => {
            buf.truncate(n);
            unsafe { crate::pith_copy_bytes_to_cstring(&buf) }
        }
        Err(_) => std::ptr::null_mut(),
    }
}

fn pith_read_process_stream_bytes<R: std::io::Read>(reader: &mut R, max_bytes: i64) -> i64 {
    let size = if max_bytes > 0 { max_bytes as usize } else { 4096 };
    let mut buf = vec![0u8; size];
    match reader.read(&mut buf) {
        Ok(n) => {
            buf.truncate(n);
            crate::bytes::pith_bytes_from_vec(buf)
        }
        Err(_) => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_process_read(handle: i64, max_bytes: i64) -> *mut i8 {
    let mut handles = crate::process::process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return std::ptr::null_mut();
    };
    let Some(stdout) = entry.stdout.as_mut() else {
        return crate::pith_cstring_empty();
    };
    pith_read_process_stream(stdout, max_bytes)
}

#[no_mangle]
pub unsafe extern "C" fn pith_process_read_bytes(handle: i64, max_bytes: i64) -> i64 {
    let mut handles = crate::process::process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return 0;
    };
    let Some(stdout) = entry.stdout.as_mut() else {
        return crate::bytes::pith_bytes_from_vec(Vec::new());
    };
    pith_read_process_stream_bytes(stdout, max_bytes)
}

#[no_mangle]
pub unsafe extern "C" fn pith_process_read_err(handle: i64, max_bytes: i64) -> *mut i8 {
    let mut handles = crate::process::process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return std::ptr::null_mut();
    };
    let Some(stderr) = entry.stderr.as_mut() else {
        return crate::pith_cstring_empty();
    };
    pith_read_process_stream(stderr, max_bytes)
}

#[no_mangle]
pub unsafe extern "C" fn pith_process_read_err_bytes(handle: i64, max_bytes: i64) -> i64 {
    let mut handles = crate::process::process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return 0;
    };
    let Some(stderr) = entry.stderr.as_mut() else {
        return crate::bytes::pith_bytes_from_vec(Vec::new());
    };
    pith_read_process_stream_bytes(stderr, max_bytes)
}

#[no_mangle]
pub unsafe extern "C" fn pith_process_write(handle: i64, data: *const i8) -> i64 {
    use std::io::Write;

    if data.is_null() {
        return 0;
    }
    let mut handles = crate::process::process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return 0;
    };
    let Some(stdin) = entry.stdin.as_mut() else {
        return 0;
    };
    let text = std::ffi::CStr::from_ptr(data).to_str().unwrap_or("");
    match stdin.write(text.as_bytes()) {
        Ok(n) => {
            let _ = stdin.flush();
            n as i64
        }
        Err(_) => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_process_write_bytes(handle: i64, data: i64) -> i64 {
    use std::io::Write;

    let Some(bytes) = crate::bytes::pith_bytes_ref(data) else {
        return 0;
    };
    let mut handles = crate::process::process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return 0;
    };
    let Some(stdin) = entry.stdin.as_mut() else {
        return 0;
    };
    match stdin.write(&bytes.data) {
        Ok(n) => {
            let _ = stdin.flush();
            n as i64
        }
        Err(_) => 0,
    }
}
