//! Built-in assertions and the fork-per-test runner (`pith test`).

use super::cstring::{pith_cstring_eq, pith_strdup_string};
use super::primitives::cstr_to_display;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

// one verdict for the whole process. a built-in assertion ends the process at
// the point it fails, so it never needs to consult this; std.testing's checks
// report and carry on, and this is how one of them that failed still reaches
// the exit code. the test runner forks per test, so "the process" is the child
// running a single test and the flag cannot leak into the next one.
static TEST_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[no_mangle]
pub extern "C" fn pith_assert(cond: i64) {
    if cond == 0 {
        TEST_FAILED.store(true, Ordering::Relaxed);
        let message = "assertion failed";
        record_failure_detail(message);
        eprintln!("{}", message);
    }
}

/// Set or clear the process verdict. `std.testing` records a failure through
/// here so a check it counted still fails the run; clearing is for the tally's
/// own tests, which provoke failures deliberately.
#[no_mangle]
pub extern "C" fn pith_test_set_failed(failed: i64) {
    TEST_FAILED.store(failed != 0, Ordering::Relaxed);
}

// --- test value equality ----------------------------------------------------
//
// assert_eq compares by value. bytes and lists are heap handles, so comparing
// the raw handles would report two equal-content collections as different; these
// walk the contents instead. a handle that does not resolve falls back to
// identity so a malformed value still gives a definite answer.

fn list_eq_scalar_impl(
    x: &crate::collections::list::ListImpl,
    y: &crate::collections::list::ListImpl,
) -> i64 {
    if x.len() != y.len() {
        return 0;
    }
    for i in 0..x.len() {
        if x.get_value(i) != y.get_value(i) {
            return 0;
        }
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_eq_scalar(a: i64, b: i64) -> i64 {
    // SAFETY: `a` and `b` are validated by `with_list_ref2_handle`; shared borrows permit `a == b`.
    unsafe { crate::collections::list::with_list_ref2_handle(a, b, list_eq_scalar_impl) }
        .unwrap_or((a == b) as i64)
}

unsafe fn list_eq_string_impl(
    x: &crate::collections::list::ListImpl,
    y: &crate::collections::list::ListImpl,
) -> i64 {
    if x.len() != y.len() {
        return 0;
    }
    for i in 0..x.len() {
        let pa = x.get_value(i).unwrap_or(0) as *const i8;
        let pb = y.get_value(i).unwrap_or(0) as *const i8;
        // SAFETY: Caller guarantees both lists hold valid C string handles (or null).
        if unsafe { pith_cstring_eq(pa, pb) } == 0 {
            return 0;
        }
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_eq_string(a: i64, b: i64) -> i64 {
    // SAFETY: `a` and `b` are validated by `with_list_ref2_handle`; elements are string handles.
    unsafe { crate::collections::list::with_list_ref2_handle(a, b, |x, y| list_eq_string_impl(x, y)) }
        .unwrap_or((a == b) as i64)
}

fn format_bytes_hex(data: &[u8]) -> String {
    let mut s = String::from("0x");
    for byte in data {
        s.push_str(&format!("{:02x}", byte));
    }
    s
}

unsafe fn bytes_hex(handle: i64) -> String {
    // SAFETY: `handle` is validated by `with_bytes` before borrowing its slice.
    unsafe { crate::bytes::with_bytes(handle, format_bytes_hex) }
        .unwrap_or_else(|| format!("<bytes {}>", handle))
}

fn list_values_copy(list: &crate::collections::list::ListImpl) -> Vec<i64> {
    (0..list.len()).map(|i| list.get_value(i).unwrap_or(0)).collect()
}

unsafe fn list_display(handle: i64, string_elems: bool) -> String {
    // SAFETY: `handle` is validated by `with_list_ref_handle` before reading elements.
    let Some(values) = (unsafe { crate::collections::list::with_list_ref_handle(handle, list_values_copy) }) else {
        return format!("<list {}>", handle);
    };
    let mut s = String::from("[");
    for (i, v) in values.into_iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        if string_elems {
            // SAFETY: Caller guarantees elements are valid C string handles when `string_elems`.
            let text = unsafe { cstr_to_display(v as *const i8) };
            s.push_str(&format!("\"{}\"", text));
        } else {
            s.push_str(&v.to_string());
        }
    }
    s.push(']');
    s
}

/// One side of an assertion failure, decoded per `kind`
/// (0 int, 1 float, 2 string, 3 bytes, 4 bool, 5 int-list, 6 string-list).
unsafe fn assert_operand(v: i64, kind: i64) -> String {
    // SAFETY: Caller guarantees `v` is a valid value of the given `kind`.
    match kind {
        1 => f64::from_bits(v as u64).to_string(),
        2 => format!("\"{}\"", unsafe { cstr_to_display(v as *const i8) }),
        3 => unsafe { bytes_hex(v) },
        4 => if v != 0 { "true".to_string() } else { "false".to_string() },
        5 => unsafe { list_display(v, false) },
        6 => unsafe { list_display(v, true) },
        _ => v.to_string(),
    }
}

/// Report an assert_eq failure with both sides decoded per `kind`.
///
/// # Safety
/// `a`/`b` must be valid values of the given `kind` for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_assert_eq_fail(a: i64, b: i64, kind: i64) {
    TEST_FAILED.store(true, Ordering::Relaxed);
    // SAFETY: Preconditions forwarded from caller.
    let (lhs, rhs) = unsafe { (assert_operand(a, kind), assert_operand(b, kind)) };
    let message = format!("assertion failed: {} != {}", lhs, rhs);
    record_failure_detail(&message);
    eprintln!("{}", message);
}

/// Report an assert_ne failure. Both sides are shown even though they compared
/// equal: which of two supposedly different values the pair collapsed to is the
/// first thing anyone reading the failure wants to know.
///
/// # Safety
/// `a`/`b` must be valid values of the given `kind` for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_assert_ne_fail(a: i64, b: i64, kind: i64) {
    TEST_FAILED.store(true, Ordering::Relaxed);
    // SAFETY: Preconditions forwarded from caller.
    let (lhs, rhs) = unsafe { (assert_operand(a, kind), assert_operand(b, kind)) };
    let message = format!("assertion failed: {} == {}", lhs, rhs);
    record_failure_detail(&message);
    eprintln!("{}", message);
}

// --- test runner ------------------------------------------------------------
//
// each `test "..."` block compiles to a `__test_i` function; the generated main
// is a pure dispatcher that forks once per test and records the outcome. running
// each test in its own child means a failing assert's exit(1), or even a crash,
// ends only that test. the others still run, and the parent reports every one.
//
// a run reports in one of two shapes. the default is the prose a person reads in
// a terminal. with PITH_TEST_JSON set (`pith test --json`) every result is one
// json object on a line of its own instead, because a run is a stream produced
// across process boundaries: the child prints its own rows and its own skip, the
// parent prints the verdict once the child is gone, and a test that crashes
// still leaves every record written before it. one json document could not be
// closed by a process that died, and buffering the whole run to close it would
// throw away the streaming the fork-per-test design is built on.

static TEST_PASS: AtomicUsize = AtomicUsize::new(0);
static TEST_FAIL: AtomicUsize = AtomicUsize::new(0);
static TEST_FILTERED: AtomicUsize = AtomicUsize::new(0);
static TEST_SKIPPED: AtomicUsize = AtomicUsize::new(0);

// the name of the test running in the current child, so skip_test can print a
// full result line. set once at the top of each child.
static CURRENT_TEST: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

// the tags of the test being considered, comma-joined, and the file the build
// was made from. both are set in the parent before it forks, so a child sees
// them without being told again.
static CURRENT_TAGS: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
static TEST_FILE: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

// every tag any test in this build carries. a run that selects or excludes a
// tag no test has is a mistake in the invocation. a typo that silently runs
// nothing is the failure mode a tagged suite invites, so the summary names it
// and the run fails.
static SEEN_TAGS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

// when the current test started, stamped before the fork so the child measures
// the same span the parent does.
static TEST_START: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

// where the assertion about to fail sits, recorded by the emitted code on the
// failing branch. only a test build emits it, so an ordinary program pays
// nothing for it.
static FAIL_SITE: std::sync::Mutex<Option<(String, i64, i64)>> = std::sync::Mutex::new(None);

// the pipe carrying one failure's detail from the child back to the parent.
// the message and its position are known where the assertion failed, and the
// record is written where the exit status is known; a pipe is the shortest
// path between the two that does not touch the filesystem. the read end is
// non-blocking and the child is already gone when it is read, so nothing here
// can wedge the run.
static DETAIL_WRITE_FD: AtomicI32 = AtomicI32::new(-1);
static DETAIL_READ_FD: AtomicI32 = AtomicI32::new(-1);
static DETAIL_WRITTEN: AtomicBool = AtomicBool::new(false);
const DETAIL_MAX: usize = 4096;

// a child that skipped exits with this code; the parent tells it apart from a
// pass (0) or a failure (anything else).
const TEST_SKIP_CODE: i32 = 42;

fn locked_string(cell: &std::sync::Mutex<String>) -> String {
    cell.lock().map(|value| value.clone()).unwrap_or_default()
}

/// Whether this run reports json records rather than prose.
fn test_json_mode() -> bool {
    match std::env::var("PITH_TEST_JSON") {
        Ok(value) => !value.is_empty() && value != "0",
        Err(_) => false,
    }
}

fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn json_string(text: &str) -> String {
    format!("\"{}\"", json_escape(text))
}

fn json_string_array(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|item| json_string(item)).collect();
    format!("[{}]", quoted.join(","))
}

fn json_message(message: Option<&str>) -> String {
    match message {
        Some(text) => json_string(text),
        None => "null".to_string(),
    }
}

fn json_position(site: &Option<(String, i64, i64)>) -> String {
    match site {
        Some((file, line, col)) => format!(
            "{{\"file\":{},\"line\":{},\"col\":{}}}",
            json_string(file),
            line,
            col
        ),
        None => "null".to_string(),
    }
}

/// The tags in a comma-joined list, empties dropped.
fn tag_list(tags: &str) -> Vec<String> {
    tags.split(',')
        .map(|tag| tag.trim())
        .filter(|tag| !tag.is_empty())
        .map(|tag| tag.to_string())
        .collect()
}

fn env_tag_list(name: &str) -> Vec<String> {
    match std::env::var(name) {
        Ok(value) => tag_list(&value),
        Err(_) => Vec::new(),
    }
}

fn note_seen_tags(tags: &[String]) {
    if let Ok(mut seen) = SEEN_TAGS.lock() {
        for tag in tags {
            if !seen.contains(tag) {
                seen.push(tag.clone());
            }
        }
    }
}

/// Whether the tags a test carries pass the tags this run selected and
/// excluded. With nothing selected every test qualifies; with a selection a
/// test needs one of the selected tags; an excluded tag removes it either way,
/// so an exclusion beats a selection when a test carries both.
fn tags_admit(own: &[String]) -> bool {
    let selected = env_tag_list("PITH_TEST_TAGS");
    let excluded = env_tag_list("PITH_TEST_EXCLUDE_TAGS");
    if !selected.is_empty() && !own.iter().any(|tag| selected.contains(tag)) {
        return false;
    }
    !own.iter().any(|tag| excluded.contains(tag))
}

/// The tags this run named that no test in the build carries.
fn unmatched_tags() -> Vec<String> {
    let seen = SEEN_TAGS.lock().map(|tags| tags.clone()).unwrap_or_default();
    let mut named = env_tag_list("PITH_TEST_TAGS");
    for tag in env_tag_list("PITH_TEST_EXCLUDE_TAGS") {
        if !named.contains(&tag) {
            named.push(tag);
        }
    }
    named.retain(|tag| !seen.contains(tag));
    named
}

fn elapsed_ms() -> u64 {
    let started = TEST_START.lock().map(|start| *start).unwrap_or(None);
    match started {
        Some(instant) => instant.elapsed().as_millis() as u64,
        None => 0,
    }
}

/// One test's result as a json object on a line of its own.
fn print_test_record(
    name: &str,
    outcome: &str,
    duration_ms: u64,
    message: Option<&str>,
    site: &Option<(String, i64, i64)>,
) {
    println!(
        "{{\"type\":\"test\",\"name\":{},\"file\":{},\"tags\":{},\"outcome\":\"{}\",\"duration_ms\":{},\"message\":{},\"position\":{}}}",
        json_string(name),
        json_string(&locked_string(&TEST_FILE)),
        json_string_array(&tag_list(&locked_string(&CURRENT_TAGS))),
        outcome,
        duration_ms,
        json_message(message),
        json_position(site)
    );
}

/// One check or table row as a json object. A row belongs to the test that
/// holds it, which is why it names that test and carries its file and tags: a
/// reader grouping records by test needs no second lookup, and `name` alone
/// would not say which test a row labeled `[3] gamma` came from. Outside a
/// test (`std.testing` in a plain `main()`), the test name is empty.
fn print_case_record(name: &str, ok: bool, detail: &str) {
    let outcome = if ok { "passed" } else { "failed" };
    let message = if ok { None } else { Some(detail) };
    println!(
        "{{\"type\":\"case\",\"name\":{},\"test\":{},\"file\":{},\"tags\":{},\"outcome\":\"{}\",\"message\":{}}}",
        json_string(name),
        json_string(&locked_string(&CURRENT_TEST)),
        json_string(&locked_string(&TEST_FILE)),
        json_string_array(&tag_list(&locked_string(&CURRENT_TAGS))),
        outcome,
        json_message(message)
    );
}

/// Send one failure's message and position to the parent. The first failure
/// wins: a child stops at its first built-in assertion anyway, and a run of
/// recorded checks is best summarised by the one that failed first.
fn record_failure_detail(message: &str) {
    if DETAIL_WRITTEN.swap(true, Ordering::Relaxed) {
        return;
    }
    let fd = DETAIL_WRITE_FD.load(Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    let site = FAIL_SITE.lock().map(|site| site.clone()).unwrap_or(None);
    let (file, line, col) = site.unwrap_or((String::new(), 0, 0));
    // file, line and col first, message last: the message is whatever the
    // assertion printed and may hold anything, including newlines, and reading
    // it as the remainder needs no escaping on either side.
    let mut payload = format!("{}\n{}\n{}\n{}", file, line, col, message);
    if payload.len() > DETAIL_MAX {
        let mut cut = DETAIL_MAX;
        while cut > 0 && !payload.is_char_boundary(cut) {
            cut -= 1;
        }
        payload.truncate(cut);
    }
    unsafe {
        libc::write(fd, payload.as_ptr() as *const libc::c_void, payload.len());
    }
}

/// Parent side: the failure detail the child sent, if it sent one. Always
/// called once per test, pass or fail, because it is also what closes the read
/// end; a suite of a few hundred tests would otherwise run out of descriptors.
fn read_failure_detail() -> Option<(String, Option<(String, i64, i64)>)> {
    let fd = DETAIL_READ_FD.swap(-1, Ordering::Relaxed);
    if fd < 0 {
        return None;
    }
    let mut buf = [0u8; DETAIL_MAX];
    let mut text: Vec<u8> = Vec::new();
    loop {
        let read = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if read <= 0 {
            break;
        }
        text.extend_from_slice(&buf[..read as usize]);
        if text.len() >= DETAIL_MAX {
            break;
        }
    }
    unsafe {
        libc::close(fd);
    }
    if text.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&text).to_string();
    let mut parts = text.splitn(4, '\n');
    let file = parts.next().unwrap_or("").to_string();
    let line: i64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let col: i64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let message = parts.next().unwrap_or("").to_string();
    let site = if file.is_empty() {
        None
    } else {
        Some((file, line, col))
    };
    Some((message, site))
}

/// Record the name of the test about to run in this child.
///
/// # Safety
/// `name` must point to a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_test_enter(name: *const i8) -> i64 {
    // SAFETY: Caller guarantees `name` is a valid NUL-terminated string.
    let name = unsafe { cstr_to_display(name) };
    if let Ok(mut current) = CURRENT_TEST.lock() {
        *current = name;
    }
    0
}

/// Record the source file this test build was made from. The generated main
/// calls it once, before any test runs: the binary has no other way to know
/// what it was built from, and every record names the file.
///
/// # Safety
/// `file` must point to a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_test_set_file(file: *const i8) -> i64 {
    // SAFETY: Caller guarantees `file` is a valid NUL-terminated string.
    let file = unsafe { cstr_to_display(file) };
    if let Ok(mut current) = TEST_FILE.lock() {
        *current = file;
    }
    0
}

/// Record where the assertion about to fail sits, so its record carries a
/// position and not only a message.
///
/// # Safety
/// `file` must point to a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_test_fail_site(file: *const i8, line: i64, col: i64) {
    // SAFETY: Caller guarantees `file` is a valid NUL-terminated string.
    let file = unsafe { cstr_to_display(file) };
    if let Ok(mut site) = FAIL_SITE.lock() {
        *site = Some((file, line, col));
    }
}

/// Report one check or one table row. `std.testing` counts its own results and
/// calls this to print them, so the prose and the json shapes of a row are
/// decided in the one place the test's own result is decided.
///
/// # Safety
/// `name` and `detail` must point to valid NUL-terminated strings for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_test_report_case(name: *const i8, ok: i64, detail: *const i8) {
    // SAFETY: Caller guarantees `name` and `detail` are valid NUL-terminated strings.
    let (name, detail) = unsafe { (cstr_to_display(name), cstr_to_display(detail)) };
    if ok == 0 {
        record_failure_detail(&format!("{} -- {}", name, detail));
    }
    if test_json_mode() {
        print_case_record(&name, ok != 0, &detail);
        return;
    }
    if ok != 0 {
        println!("  ok   {}", name);
    } else {
        println!("  FAIL {} -- {}", name, detail);
    }
}

/// Skip the current test with a reason: print the result line and exit the child
/// with the skip code, which the parent tallies as skipped rather than run.
///
/// # Safety
/// `reason` must point to a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_test_skip(reason: *const i8) -> i64 {
    use std::io::Write;
    let name = locked_string(&CURRENT_TEST);
    // SAFETY: Caller guarantees `reason` is a valid NUL-terminated string.
    let reason = unsafe { cstr_to_display(reason) };
    if test_json_mode() {
        print_test_record(&name, "skipped", elapsed_ms(), Some(&reason), &None);
    } else {
        println!("  {} ... skipped ({})", name, reason);
    }
    let _ = std::io::stdout().flush();
    // panic-guard: a skipped test exits with the skip code the harness reads.
    std::process::exit(TEST_SKIP_CODE);
}

/// The name of the test running in this child, or the empty string outside one.
/// `std.testing` builds a table row's identity on top of it, which is what lets
/// a row name itself in the output and be reachable by `--filter`.
#[no_mangle]
pub extern "C" fn pith_test_current_name() -> *const i8 {
    let name = locked_string(&CURRENT_TEST);
    pith_strdup_string(&name)
}

/// The separator between a test's name and the label of one case inside it.
/// A filter naming a case begins with the test's name and this, which is how
/// the parent knows to run a test whose own name does not match.
pub const TEST_CASE_SEPARATOR: &str = " / ";

fn filtered_out(name: &str) -> i64 {
    TEST_FILTERED.fetch_add(1, Ordering::Relaxed);
    if test_json_mode() {
        print_test_record(name, "filtered", 0, None, &None);
    }
    0
}

/// Whether a test should run under this invocation's tags and
/// `PITH_TEST_FILTER`. With nothing selected every test runs; otherwise only
/// names containing the substring do, and the rest are tallied as filtered out.
///
/// A filter can also name one case inside a table-driven test, as `"the test
/// name / a case label"`. The test's own name does not contain that, so
/// matching on the substring alone would filter the test out and the case
/// would never get the chance to select itself. A filter that starts with this
/// test's name and the case separator is aimed inside it, so the test runs and
/// `std.testing`'s `each` applies the same filter to each row's identity.
///
/// Tags and the filter compose as one conjunction: the tags say which tests
/// this run is about, the filter picks a name out of those. Neither widens the
/// other, so `--tag slow --filter parse` is the slow tests whose name contains
/// parse and nothing else.
///
/// # Safety
/// `name` and `tags` must point to valid NUL-terminated strings for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_test_should_run(name: *const i8, tags: *const i8) -> i64 {
    // SAFETY: Caller guarantees `name` and `tags` are valid NUL-terminated strings.
    let (name, tags) = unsafe { (cstr_to_display(name), cstr_to_display(tags)) };
    if let Ok(mut current) = CURRENT_TAGS.lock() {
        *current = tags.clone();
    }
    let own = tag_list(&tags);
    note_seen_tags(&own);
    if !tags_admit(&own) {
        return filtered_out(&name);
    }
    let filter = match std::env::var("PITH_TEST_FILTER") {
        Ok(f) if !f.is_empty() => f,
        _ => return 1,
    };
    let aimed_inside =
        !name.is_empty() && filter.starts_with(&format!("{}{}", name, TEST_CASE_SEPARATOR));
    if name.contains(&filter) || aimed_inside {
        1
    } else {
        filtered_out(&name)
    }
}

/// Flush pending output and fork. Returns the child pid to the parent and 0 to
/// the child, matching the C fork contract. Flushing first keeps the child from
/// inheriting (and re-emitting) the parent's buffered stdout. The clock starts
/// and the failure-detail pipe opens here, before the fork, so both sides of it
/// measure and report the same test.
#[no_mangle]
pub extern "C" fn pith_test_fork() -> i64 {
    use std::io::Write;
    if let Ok(mut start) = TEST_START.lock() {
        *start = Some(std::time::Instant::now());
    }
    let mut fds: [libc::c_int; 2] = [-1, -1];
    let piped = unsafe { libc::pipe(fds.as_mut_ptr()) } == 0;
    if piped {
        unsafe {
            // a subprocess a test starts must not inherit the write end, or the
            // parent's read would wait on a descriptor nobody is going to close
            libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK);
        }
    }
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    let pid = unsafe { libc::fork() as i64 };
    if !piped {
        return pid;
    }
    if pid < 0 {
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        DETAIL_READ_FD.store(-1, Ordering::Relaxed);
        return -1;
    }
    if pid == 0 {
        unsafe {
            libc::close(fds[0]);
        }
        DETAIL_READ_FD.store(-1, Ordering::Relaxed);
        DETAIL_WRITTEN.store(false, Ordering::Relaxed);
        DETAIL_WRITE_FD.store(fds[1], Ordering::Relaxed);
    } else {
        unsafe {
            libc::close(fds[1]);
        }
        DETAIL_READ_FD.store(fds[0], Ordering::Relaxed);
    }
    pid
}

/// Called at the end of a test in the child process: flush and exit with the
/// process verdict. A built-in assertion that failed has already exited(1)
/// before reaching here; a `std.testing` check that failed only recorded it,
/// and this is where that recording becomes a failed test rather than a line
/// of output nobody counts.
#[no_mangle]
pub extern "C" fn pith_test_child_ok() -> i64 {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    if TEST_FAILED.load(Ordering::Relaxed) {
        // panic-guard: a failing test child exits non-zero for the harness.
        std::process::exit(1);
    }
    // panic-guard: a passing test child exits zero for the harness.
    std::process::exit(0);
}

/// Parent side: wait for a test's child, print its result line, and tally it. A
/// clean exit(0) passes; any non-zero exit or a killing signal fails.
///
/// # Safety
/// `name` must point to a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_test_record(name: *const i8, pid: i64) -> i64 {
    // SAFETY: Caller guarantees `name` is a valid NUL-terminated string.
    let name_str = unsafe { cstr_to_display(name) };
    let duration = elapsed_ms();
    let json = test_json_mode();
    if pid <= 0 {
        let _ = read_failure_detail();
        TEST_FAIL.fetch_add(1, Ordering::Relaxed);
        if json {
            print_test_record(&name_str, "failed", duration, Some("fork failed"), &None);
        } else {
            println!("  {} ... FAILED (fork failed)", name_str);
        }
        return 1;
    }
    let mut status: i32 = 0;
    // SAFETY: `status` is a valid out-pointer for `waitpid`.
    let waited = loop {
        let rc = unsafe { libc::waitpid(pid as i32, &mut status, 0) };
        if rc < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        break rc;
    };
    // read unconditionally: this is also what closes the read end of the pipe
    let detail = read_failure_detail();
    if waited < 0 {
        TEST_FAIL.fetch_add(1, Ordering::Relaxed);
        if json {
            print_test_record(&name_str, "failed", duration, Some("waitpid failed"), &None);
        } else {
            println!("  {} ... FAILED (waitpid failed)", name_str);
        }
        return 1;
    }
    if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == TEST_SKIP_CODE {
        // the child already reported its own skip, in whichever shape this run
        // is printing
        TEST_SKIPPED.fetch_add(1, Ordering::Relaxed);
        0
    } else if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
        TEST_PASS.fetch_add(1, Ordering::Relaxed);
        if json {
            print_test_record(&name_str, "passed", duration, None, &None);
        } else {
            println!("  {} ... ok", name_str);
        }
        0
    } else {
        TEST_FAIL.fetch_add(1, Ordering::Relaxed);
        let signalled = libc::WIFSIGNALED(status);
        if json {
            let (message, site) = match detail {
                Some((message, site)) => (message, site),
                None => {
                    let message = if signalled {
                        format!("killed by signal {}", libc::WTERMSIG(status))
                    } else {
                        format!("exited with status {}", libc::WEXITSTATUS(status))
                    };
                    (message, None)
                }
            };
            print_test_record(&name_str, "failed", duration, Some(&message), &site);
        } else if signalled {
            println!(
                "  {} ... FAILED (killed by signal {})",
                name_str,
                libc::WTERMSIG(status)
            );
        } else {
            println!("  {} ... FAILED", name_str);
        }
        1
    }
}

/// Print the final tally and return the process exit code (non-zero on any
/// failure, and on a tag this run named that no test carries).
#[no_mangle]
pub extern "C" fn pith_test_summary() -> i64 {
    let passed = TEST_PASS.load(Ordering::Relaxed);
    let failed = TEST_FAIL.load(Ordering::Relaxed);
    let filtered = TEST_FILTERED.load(Ordering::Relaxed);
    let skipped = TEST_SKIPPED.load(Ordering::Relaxed);
    let unmatched = unmatched_tags();
    if test_json_mode() {
        println!(
            "{{\"type\":\"summary\",\"passed\":{},\"failed\":{},\"skipped\":{},\"filtered_out\":{},\"unmatched_tags\":{}}}",
            passed,
            failed,
            skipped,
            filtered,
            json_string_array(&unmatched)
        );
    } else {
        println!();
        let mut line = format!("{} passed, {} failed", passed, failed);
        if skipped > 0 {
            line.push_str(&format!(", {} skipped", skipped));
        }
        if filtered > 0 {
            line.push_str(&format!(", {} filtered out", filtered));
        }
        println!("{}", line);
        for tag in &unmatched {
            println!("no test carries the tag \"{}\"", tag);
        }
    }
    if failed > 0 || !unmatched.is_empty() {
        1
    } else {
        0
    }
}
