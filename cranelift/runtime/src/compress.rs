use crate::bytes::{forge_bytes_from_vec, forge_bytes_ref};

const GZIP_HEADER: [u8; 10] = [0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff];
const GZIP_FLAG_TEXT: u8 = 0x01;
const GZIP_FLAG_HEADER_CRC: u8 = 0x02;
const GZIP_FLAG_EXTRA: u8 = 0x04;
const GZIP_FLAG_NAME: u8 = 0x08;
const GZIP_FLAG_COMMENT: u8 = 0x10;
const GZIP_FLAG_RESERVED: u8 = 0xe0;
const STORED_BLOCK_SIZE: usize = u16::MAX as usize;

#[no_mangle]
pub unsafe extern "C" fn forge_gzip_compress(handle: i64) -> i64 {
    let Some(bytes) = forge_bytes_ref(handle) else {
        return forge_bytes_from_vec(Vec::new());
    };
    forge_bytes_from_vec(gzip_compress_stored(&bytes.data))
}

#[no_mangle]
pub unsafe extern "C" fn forge_gzip_decompress(handle: i64) -> i64 {
    let Some(bytes) = forge_bytes_ref(handle) else {
        return 0;
    };
    match gzip_decompress_stored(&bytes.data) {
        Some(data) => forge_bytes_from_vec(data),
        None => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_gzip_validate(handle: i64) -> i64 {
    let Some(bytes) = forge_bytes_ref(handle) else {
        return 0;
    };
    if gzip_decompress_stored(&bytes.data).is_some() {
        1
    } else {
        0
    }
}

fn gzip_compress_stored(data: &[u8]) -> Vec<u8> {
    let block_count = if data.is_empty() {
        1
    } else {
        data.len().div_ceil(STORED_BLOCK_SIZE)
    };
    let mut out = Vec::with_capacity(GZIP_HEADER.len() + data.len() + block_count * 5 + 8);
    out.extend_from_slice(&GZIP_HEADER);

    if data.is_empty() {
        write_stored_block(&mut out, true, data);
    } else {
        for (index, chunk) in data.chunks(STORED_BLOCK_SIZE).enumerate() {
            let is_final = index + 1 == block_count;
            write_stored_block(&mut out, is_final, chunk);
        }
    }

    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

fn write_stored_block(out: &mut Vec<u8>, is_final: bool, data: &[u8]) {
    let len = data.len() as u16;
    out.push(if is_final { 0x01 } else { 0x00 });
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes());
    out.extend_from_slice(data);
}

fn gzip_decompress_stored(input: &[u8]) -> Option<Vec<u8>> {
    if input.len() < GZIP_HEADER.len() + 8 {
        return None;
    }
    if input[0] != 0x1f || input[1] != 0x8b || input[2] != 0x08 {
        return None;
    }
    let flags = input[3];
    if flags & GZIP_FLAG_RESERVED != 0 {
        return None;
    }

    let mut pos = GZIP_HEADER.len();
    if flags & GZIP_FLAG_EXTRA != 0 {
        if pos + 2 > input.len() {
            return None;
        }
        let extra_len = u16::from_le_bytes([input[pos], input[pos + 1]]) as usize;
        pos = pos.checked_add(2)?.checked_add(extra_len)?;
        if pos > input.len() {
            return None;
        }
    }
    if flags & GZIP_FLAG_NAME != 0 {
        pos = skip_null_terminated(input, pos)?;
    }
    if flags & GZIP_FLAG_COMMENT != 0 {
        pos = skip_null_terminated(input, pos)?;
    }
    if flags & GZIP_FLAG_HEADER_CRC != 0 {
        pos = pos.checked_add(2)?;
        if pos > input.len() {
            return None;
        }
    }
    let _is_probably_text = flags & GZIP_FLAG_TEXT != 0;

    let mut out = Vec::new();
    loop {
        if pos + 1 > input.len().saturating_sub(8) {
            return None;
        }
        let block_header = input[pos];
        pos += 1;
        let is_final = block_header & 0x01 != 0;
        let block_type = (block_header >> 1) & 0x03;
        if block_type != 0 {
            return None;
        }
        if pos + 4 > input.len().saturating_sub(8) {
            return None;
        }
        let len = u16::from_le_bytes([input[pos], input[pos + 1]]);
        let nlen = u16::from_le_bytes([input[pos + 2], input[pos + 3]]);
        if len != !nlen {
            return None;
        }
        pos += 4;
        let end = pos.checked_add(len as usize)?;
        if end > input.len().saturating_sub(8) {
            return None;
        }
        out.extend_from_slice(&input[pos..end]);
        pos = end;
        if is_final {
            break;
        }
    }

    if pos + 8 != input.len() {
        return None;
    }
    let expected_crc =
        u32::from_le_bytes([input[pos], input[pos + 1], input[pos + 2], input[pos + 3]]);
    let expected_size = u32::from_le_bytes([
        input[pos + 4],
        input[pos + 5],
        input[pos + 6],
        input[pos + 7],
    ]);
    if crc32(&out) != expected_crc {
        return None;
    }
    if out.len() as u32 != expected_size {
        return None;
    }
    Some(out)
}

fn skip_null_terminated(input: &[u8], mut pos: usize) -> Option<usize> {
    while pos < input.len() {
        if input[pos] == 0 {
            return Some(pos + 1);
        }
        pos += 1;
    }
    None
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}
