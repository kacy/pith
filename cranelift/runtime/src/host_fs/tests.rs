use super::env::*;
use super::handles::*;
use super::ops::*;
use std::sync::atomic::Ordering;
use crate::ffi_util::unbox_result;
use std::ffi::CString;
use std::io::Read;
use std::sync::atomic::AtomicU32;

/// a scratch path unique to the calling test, cleaned up when the returned
/// value drops. tests run in one process, so a shared name would race.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "pith-host-fs-{}-{}-{}",
            tag,
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        Scratch(std::env::temp_dir().join(name))
    }

    fn cstr(&self) -> CString {
        CString::new(self.0.to_str().unwrap()).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn cstring(text: &str) -> CString {
    CString::new(text).unwrap()
}

#[test]
fn create_new_refuses_a_planted_symlink_and_an_existing_file() {
    use std::os::unix::fs::PermissionsExt;

    let victim = Scratch::new("victim");
    std::fs::write(&victim.0, b"keep").unwrap();
    let link = Scratch::new("link");
    std::os::unix::fs::symlink(&victim.0, &link.0).unwrap();
    let link_path = link.0.to_str().unwrap();
    assert_eq!(
        create_new_file_at(link_path, b"evil", 0o600),
        libc::EEXIST as i64
    );
    assert_eq!(std::fs::read(&victim.0).unwrap(), b"keep");

    let victim_path = victim.0.to_str().unwrap();
    assert_eq!(
        create_new_file_at(victim_path, b"evil", 0o600),
        libc::EEXIST as i64
    );
    assert_eq!(std::fs::read(&victim.0).unwrap(), b"keep");

    let fresh = Scratch::new("fresh");
    assert_eq!(
        create_new_file_at(fresh.0.to_str().unwrap(), b"data", 0o600),
        0
    );
    assert_eq!(std::fs::read(&fresh.0).unwrap(), b"data");
    let mode = std::fs::metadata(&fresh.0).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn mkdir_new_refuses_a_planted_symlink_and_sets_the_mode() {
    use std::os::unix::fs::PermissionsExt;

    let target = Scratch::new("target-dir");
    std::fs::create_dir(&target.0).unwrap();
    let link = Scratch::new("dir-link");
    std::os::unix::fs::symlink(&target.0, &link.0).unwrap();
    unsafe {
        assert_eq!(
            pith_fs_mkdir_new(link.cstr().as_ptr(), 0o700),
            libc::EEXIST as i64
        );
        let fresh = Scratch::new("fresh-dir");
        assert_eq!(pith_fs_mkdir_new(fresh.cstr().as_ptr(), 0o700), 0);
        let mode = std::fs::metadata(&fresh.0).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}

#[test]
fn invalid_file_paths_return_safe_defaults() {
    let invalid = [0xffu8, 0x00];
    let ptr = invalid.as_ptr() as *const i8;

    unsafe {
        assert_eq!(pith_file_exists(ptr), 0);
        assert_eq!(pith_file_size(ptr), -1);
        assert_eq!(pith_file_mtime(ptr), -1);
        assert_eq!(pith_write_file(ptr, b"x\0".as_ptr() as *const i8), 0);
        assert!(pith_read_file(ptr).is_null());
        assert_eq!(pith_os_chdir(ptr), 0);
        assert_eq!(pith_remove_tree(ptr), 0);
        assert_eq!(pith_file_open_read(ptr), 0);
    }
}

#[test]
fn whole_file_write_append_and_read_round_trip() {
    let scratch = Scratch::new("round-trip");
    let path = scratch.cstr();
    let first = cstring("hello");
    let second = cstring(" world");

    unsafe {
        assert_eq!(pith_write_file(path.as_ptr(), first.as_ptr()), 1);
        assert_eq!(pith_append_file(path.as_ptr(), second.as_ptr()), 1);
    }
    assert_eq!(
        std::fs::read_to_string(&scratch.0).unwrap(),
        "hello world",
        "the two writes should land in order"
    );

    // read it back through the entry point that builds a pith C string.
    let read = unsafe { pith_read_file(path.as_ptr()) };
    assert!(!read.is_null());
    let text = unsafe { std::ffi::CStr::from_ptr(read) }.to_str().unwrap();
    assert_eq!(text, "hello world");
}

#[test]
fn reading_a_missing_file_reports_failure() {
    let scratch = Scratch::new("missing");
    let path = scratch.cstr();
    unsafe {
        assert!(pith_read_file(path.as_ptr()).is_null());
        assert_eq!(pith_read_file_bytes(path.as_ptr()), 0);
    }
}

#[test]
fn streaming_writes_then_reads_back_a_chunk_at_a_time() {
    let scratch = Scratch::new("stream");
    let path = scratch.cstr();

    unsafe {
        let out = pith_file_open_append(path.as_ptr());
        assert_ne!(out, 0);
        let chunk = cstring("abcdef");
        assert_eq!(unbox_result(pith_file_write(out, chunk.as_ptr())), Ok(6));
        pith_file_close(out);

        let input = pith_file_open_read(path.as_ptr());
        assert_ne!(input, 0);
        // two short reads must not both start at the beginning: the second
        // one continues where the first stopped.
        let first = pith_file_read(input, 3);
        let second = pith_file_read(input, 3);
        assert_eq!(
            std::ffi::CStr::from_ptr(first).to_str().unwrap(),
            "abc",
            "first chunk"
        );
        assert_eq!(
            std::ffi::CStr::from_ptr(second).to_str().unwrap(),
            "def",
            "second chunk should resume after the first"
        );
        // and the read past the end is eof, not an error.
        let third = pith_file_read(input, 3);
        assert!(!third.is_null());
        assert_eq!(std::ffi::CStr::from_ptr(third).to_str().unwrap(), "");
        pith_file_close(input);
    }
}

#[test]
fn unknown_handles_report_failure() {
    unsafe {
        assert!(pith_file_read(9_999_999, 16).is_null());
        assert_eq!(pith_file_read_bytes(9_999_999, 16), 0);
        let data = cstring("x");
        assert_eq!(
            unbox_result(pith_file_write(9_999_999, data.as_ptr())),
            Err("file_write failed: invalid or closed file handle".to_string())
        );
    }
    assert!(clone_file(9_999_999).is_none());
}

/// an empty write succeeds with a count of 0, where it used to share that
/// return value with every failure (#1153), and a failed write names the
/// os error.
#[test]
fn an_empty_write_succeeds_and_a_failed_one_names_the_error() {
    let scratch = Scratch::new("empty-write");
    let path = scratch.cstr();
    unsafe {
        let out = pith_file_open_write(path.as_ptr());
        assert_ne!(out, 0);
        let empty = cstring("");
        assert_eq!(unbox_result(pith_file_write(out, empty.as_ptr())), Ok(0));
        pith_file_close(out);
        assert_eq!(
            unbox_result(pith_file_write(out, empty.as_ptr())),
            Err("file_write failed: invalid or closed file handle".to_string())
        );

        let input = pith_file_open_read(path.as_ptr());
        assert_ne!(input, 0);
        let data = cstring("x");
        assert_eq!(unbox_result(pith_file_write(input, empty.as_ptr())), Ok(0));
        assert_eq!(
            unbox_result(pith_file_write(input, data.as_ptr())),
            Err("file_write failed: Bad file descriptor (os error 9)".to_string())
        );
        pith_file_close(input);

        let full = pith_file_open_write(c"/dev/full".as_ptr());
        assert_ne!(full, 0);
        assert_eq!(
            unbox_result(pith_file_write(full, data.as_ptr())),
            Err("file_write failed: No space left on device (os error 28)".to_string())
        );
        pith_file_close(full);
    }
}

#[test]
fn a_cloned_handle_shares_the_files_offset() {
    // the offloaded read and write paths run on a dup of the open file, so
    // this sharing is what keeps sequential reads sequential.
    let scratch = Scratch::new("clone");
    std::fs::write(&scratch.0, b"abcdef").unwrap();
    let path = scratch.cstr();

    let handle = unsafe { pith_file_open_read(path.as_ptr()) };
    assert_ne!(handle, 0);

    let mut clone = clone_file(handle).expect("open handle should clone");
    let mut first = [0u8; 3];
    clone.read_exact(&mut first).unwrap();
    assert_eq!(&first, b"abc");

    // reading through the original handle picks up after the clone's read.
    let next = read_handle_chunk(handle, 3).expect("read should succeed");
    assert_eq!(next, b"def");
    pith_file_close(handle);
}

#[test]
fn listing_a_directory_names_its_entries() {
    let scratch = Scratch::new("listing");
    std::fs::create_dir(&scratch.0).unwrap();
    std::fs::write(scratch.0.join("one.txt"), b"1").unwrap();
    std::fs::write(scratch.0.join("two.txt"), b"2").unwrap();

    let mut names = list_dir_at(scratch.0.to_str().unwrap()).expect("directory should read");
    names.sort();
    assert_eq!(names, vec!["one.txt", "two.txt"]);
    assert!(list_dir_at(&format!("{}/nope", scratch.0.display())).is_none());
}

#[test]
fn a_listed_name_has_one_owner() {
    let scratch = Scratch::new("owners");
    std::fs::create_dir(&scratch.0).unwrap();
    std::fs::write(scratch.0.join("a.txt"), b"1").unwrap();
    std::fs::write(scratch.0.join("b.txt"), b"2").unwrap();
    let path = CString::new(scratch.0.to_str().unwrap()).unwrap();
    unsafe {
        let handle = pith_list_dir(path.as_ptr());
        let list = crate::collections::list::PithList { ptr: handle as *mut () };
        for i in 0..2 {
            let name = crate::collections::list::pith_list_get_value(list, i) as *const i8;
            // the list's count and nothing else: a second one is a leak
            assert_eq!(crate::runtime_core::cstring_refcount_for_tests(name), Some(1));
        }
        crate::collections::list::pith_list_release_handle(handle);
    }
}

#[test]
fn removing_a_tree_takes_its_contents_with_it() {
    let scratch = Scratch::new("tree");
    std::fs::create_dir_all(scratch.0.join("nested")).unwrap();
    std::fs::write(scratch.0.join("nested/file.txt"), b"x").unwrap();

    let path = scratch.cstr();
    unsafe {
        assert_eq!(pith_remove_tree(path.as_ptr()), 1);
        assert_eq!(pith_dir_exists(path.as_ptr()), 0);
        // a second removal has nothing to do and says so.
        assert_eq!(pith_remove_tree(path.as_ptr()), 0);
    }
}
