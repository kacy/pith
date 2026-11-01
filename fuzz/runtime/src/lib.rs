//! Shared pieces of the runtime fuzz targets: a reader that splits the
//! fuzzer's bytes into choices, integers and payloads, constructors for the
//! string and bytes values compiled code hands the runtime, and the entry
//! point macro.
//!
//! Every value is built through the runtime's own constructors and released
//! through its own release functions, so a leak, a double free or a read
//! after free inside the runtime is visible to the sanitizer.

use pith_runtime::bytes::{
    pith_byte_buffer_free, pith_byte_buffer_string, pith_byte_buffer_take_bytes,
    pith_byte_buffer_with_capacity, pith_byte_buffer_write_word, pith_bytes_len, pith_bytes_release,
};
use pith_runtime::string::pith_cstring_len;

/// Define `LLVMFuzzerTestOneInput` (and an initializer) around a body that
/// takes the input as `&[u8]`.
#[macro_export]
macro_rules! fuzz_target {
    (|$data:ident| $body:block) => {
        #[no_mangle]
        pub extern "C" fn LLVMFuzzerInitialize(_argc: *mut i32, _argv: *mut *mut *mut u8) -> i32 {
            $crate::init();
            0
        }

        #[no_mangle]
        pub extern "C" fn LLVMFuzzerTestOneInput(data: *const u8, size: usize) -> i32 {
            let $data: &[u8] = if size == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(data, size) }
            };
            $body;
            0
        }
    };
}

/// Process setup before the first input: the struct free list recycles
/// freed structs, which would hide a use after free from the sanitizer,
/// and the sanitizer report hook below decides which reports end the run.
pub fn init() {
    std::env::set_var("PITH_STRUCT_FREELIST", "0");
    unsafe { __asan_set_error_report_callback(on_asan_report) };
}

extern "C" {
    fn __asan_set_error_report_callback(callback: extern "C" fn(*const std::ffi::c_char));
}

/// The targets are built with `-Zsanitizer-recover=address` and run with
/// `halt_on_error=0`, so this hook sees every report and aborts on all but
/// one known pattern: the length lookup reading the 16 bytes in front of a
/// headerless runtime static (the shared empty string) to look for a
/// header. That read stays inside mapped data and is tracked separately;
/// letting it end the run would stop every input that makes an empty
/// string. The abort is what libFuzzer records as the crash.
extern "C" fn on_asan_report(report: *const std::ffi::c_char) {
    let text = unsafe { std::ffi::CStr::from_ptr(report) }.to_string_lossy();
    let known = text.contains("global-buffer-overflow")
        && text.contains("global variable 'alloc_")
        && (text.contains("cstring_header_len") || text.contains("cstring_base"));
    if !known {
        std::process::abort();
    }
}

/// Integers that sit on a boundary of some range a runtime function checks.
const EDGES: [i64; 16] = [
    0,
    1,
    -1,
    i64::MIN,
    i64::MAX,
    i64::MIN + 1,
    i64::MAX - 1,
    i32::MAX as i64,
    i32::MIN as i64,
    u32::MAX as i64,
    1 << 32,
    1 << 62,
    -(1 << 62),
    255,
    256,
    65536,
];

/// Reads choices off the front of the fuzzer's input. Running out of input
/// reads zeros, so every input is a valid program for the harness.
pub struct Input<'a> {
    data: &'a [u8],
}

impl<'a> Input<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Input { data }
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn byte(&mut self) -> u8 {
        match self.data.split_first() {
            Some((&b, rest)) => {
                self.data = rest;
                b
            }
            None => 0,
        }
    }

    pub fn bool(&mut self) -> bool {
        self.byte() & 1 == 1
    }

    /// Up to `n` bytes, fewer at the end of the input.
    pub fn take(&mut self, n: usize) -> &'a [u8] {
        let n = n.min(self.data.len());
        let (head, rest) = self.data.split_at(n);
        self.data = rest;
        head
    }

    fn raw_i64(&mut self, n: usize) -> i64 {
        let mut le = [0u8; 8];
        let src = self.take(n);
        le[..src.len()].copy_from_slice(src);
        let value = i64::from_le_bytes(le);
        // sign-extend the narrow forms
        let shift = 64 - 8 * n as u32;
        (value << shift) >> shift
    }

    /// An arbitrary integer, biased towards small values and range edges.
    pub fn int(&mut self) -> i64 {
        match self.byte() % 8 {
            0..=3 => self.raw_i64(1),
            4 | 5 => EDGES[(self.byte() % EDGES.len() as u8) as usize],
            6 => self.raw_i64(2),
            _ => self.raw_i64(8),
        }
    }

    /// An index into something `len` long: mostly in or next to the range,
    /// sometimes negative or huge.
    pub fn index(&mut self, len: usize) -> i64 {
        match self.byte() % 8 {
            0..=4 => (self.byte() as usize % (len + 3)) as i64 - 1,
            5 => (len as i64).wrapping_add(self.raw_i64(1)),
            6 => EDGES[(self.byte() % EDGES.len() as u8) as usize],
            _ => self.raw_i64(8),
        }
    }

    /// A length-prefixed chunk: one length byte, 255 meaning the rest.
    pub fn chunk(&mut self) -> &'a [u8] {
        match self.byte() {
            255 => self.rest(),
            n => self.take(n as usize),
        }
    }

    pub fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.data)
    }
}

/// A string value as compiled code holds one. `Heap` is a counted runtime
/// string, `Literal` is laid out like a string literal the compiler emits
/// (a static header and no count), and `Raw` is a headerless C string at an
/// odd address, the shape of a pointer that came from outside the runtime.
pub enum PStr {
    Heap(*mut i8),
    Literal(Vec<u64>),
    Raw(Vec<u8>),
}

impl PStr {
    /// Build `bytes` as the kind `kind` selects. A raw string ends at its
    /// first NUL, so `content` is what the runtime will see.
    pub fn new(bytes: &[u8], kind: u8) -> PStr {
        match kind % 4 {
            0 | 1 => PStr::Heap(heap_cstring(bytes)),
            2 => PStr::Literal(literal_words(bytes)),
            _ => {
                let mut raw = Vec::with_capacity(bytes.len() + 2);
                raw.push(b'#');
                raw.extend(bytes.iter().copied().take_while(|&b| b != 0));
                raw.push(0);
                PStr::Raw(raw)
            }
        }
    }

    pub fn literal(bytes: &[u8]) -> PStr {
        PStr::Literal(literal_words(bytes))
    }

    pub fn ptr(&self) -> *const i8 {
        match self {
            PStr::Heap(p) => *p,
            PStr::Literal(words) => unsafe { (words.as_ptr() as *const u8).add(16) as *const i8 },
            PStr::Raw(raw) => unsafe { raw.as_ptr().add(1) as *const i8 },
        }
    }

    pub fn content(&self) -> Vec<u8> {
        unsafe { cstr_vec(self.ptr()).unwrap_or_default() }
    }
}

impl Drop for PStr {
    fn drop(&mut self) {
        if let PStr::Heap(p) = self {
            unsafe { pith_runtime::pith_cstring_release(*p) };
        }
    }
}

/// [magic][0][len u64][bytes][NUL] in a buffer of exactly that many words,
/// what `cranelift/codegen/src/lib.rs` writes for a literal.
fn literal_words(bytes: &[u8]) -> Vec<u64> {
    let total = 16 + bytes.len() + 1;
    let mut words = vec![0u64; total.div_ceil(8)];
    unsafe {
        let base = words.as_mut_ptr() as *mut u8;
        (base as *mut u32).write(pith_runtime::CSTRING_STATIC_MAGIC);
        (base.add(8) as *mut u64).write(bytes.len() as u64);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), base.add(16), bytes.len());
    }
    words
}

/// A fresh counted runtime string holding exactly `bytes`, built through a
/// byte buffer, the path `std.io`'s StringBuffer takes.
pub fn heap_cstring(bytes: &[u8]) -> *mut i8 {
    unsafe {
        let buffer = fill_buffer(bytes);
        let s = pith_byte_buffer_string(buffer);
        pith_byte_buffer_free(buffer);
        s
    }
}

/// A fresh bytes value holding exactly `data`.
pub fn bytes_value(data: &[u8]) -> i64 {
    unsafe {
        let handle = pith_byte_buffer_take_bytes(fill_buffer(data));
        assert_eq!(pith_bytes_len(handle), data.len() as i64);
        handle
    }
}

unsafe fn fill_buffer(data: &[u8]) -> i64 {
    let buffer = pith_byte_buffer_with_capacity(data.len() as i64);
    for word in data.chunks(8) {
        let mut le = [0u8; 8];
        le[..word.len()].copy_from_slice(word);
        assert_eq!(pith_byte_buffer_write_word(buffer, i64::from_le_bytes(le), word.len() as i64), 1);
    }
    buffer
}

/// A bytes value released when it goes out of scope.
pub struct PBytes(pub i64);

impl PBytes {
    pub fn new(data: &[u8]) -> PBytes {
        PBytes(bytes_value(data))
    }
}

impl Drop for PBytes {
    fn drop(&mut self) {
        unsafe { pith_bytes_release(self.0) };
    }
}

/// The bytes of a runtime string, or None for null.
///
/// # Safety
/// `p` must be null or a string the runtime can read.
pub unsafe fn cstr_vec(p: *const i8) -> Option<Vec<u8>> {
    if p.is_null() {
        return None;
    }
    let len = pith_cstring_len(p) as usize;
    let bytes = std::slice::from_raw_parts(p as *const u8, len).to_vec();
    // every runtime string is NUL-terminated past its length
    assert_eq!(*p.add(len), 0, "string is not NUL-terminated at its length");
    Some(bytes)
}

/// Read a string the caller owns, then release it.
///
/// # Safety
/// As `cstr_vec`, and the caller must own one count of `p`.
pub unsafe fn take_cstr(p: *mut i8) -> Option<Vec<u8>> {
    let bytes = cstr_vec(p);
    pith_runtime::pith_cstring_release(p);
    bytes
}

/// The bytes of a bytes value, without taking it, read through the data
/// pointer and length the codegen's inlined indexing reads.
///
/// # Safety
/// `handle` must be a live bytes value.
pub unsafe fn bytes_vec(handle: i64) -> Vec<u8> {
    use pith_runtime::bytes::{BYTES_DATA_LEN_OFFSET, BYTES_DATA_PTR_OFFSET};
    let base = handle as *const u8;
    let ptr = (base.add(BYTES_DATA_PTR_OFFSET as usize) as *const *const u8).read();
    let len = (base.add(BYTES_DATA_LEN_OFFSET as usize) as *const usize).read();
    assert_eq!(len as i64, pith_bytes_len(handle));
    std::slice::from_raw_parts(ptr, len).to_vec()
}

/// Read a bytes value the caller owns, then release it. 0 reads as None.
///
/// # Safety
/// `handle` must be 0 or a bytes value the caller owns one count of.
pub unsafe fn take_bytes(handle: i64) -> Option<Vec<u8>> {
    if handle == 0 {
        return None;
    }
    let data = bytes_vec(handle);
    pith_bytes_release(handle);
    Some(data)
}
