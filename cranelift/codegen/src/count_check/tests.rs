use super::loop_borrows::runtime_call_writes;
use super::*;

fn check(ir: &str) -> CountReport {
    let lines: Vec<&str> = ir.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    check_counts(&lines)
}

// fn first_name(xs: List[Node]) -> String: for n in xs: return n.name
const FIRST_NAME: &str = "
    func first_name 1 string
    param xs
    load 1 xs
    call 2 pith_auto_len int 1 1
    iconst 3 0
    store __for_idx_0 3
    store __for_len_0 2
    store __for_iter_0 1
    label L0
    load 4 __for_idx_0
    load 5 __for_len_0
    lt 6 4 5
    brif 6 L1 L3
    label L1
    load 7 __for_iter_0
    load 8 __for_idx_0
    call 9 pith_list_get_value unknown 2 7 8
    store __loopvar_0_n 9
    load 10 __loopvar_0_n
    field 11 10 0 string name
    call 12 pith_cstring_retain void 1 11
    ret 11
    label L3
    strref 16 m0s1
    ret 16
    mark kind __for_idx_0 int
    mark kind __for_len_0 int
    mark kind __for_iter_0 list
    mark kind __loopvar_0_n struct
    mark borrow __loopvar_0_n __for_iter_0
    mark kind xs list
    endfunc
";


// `return b.to_string_utf8() catch ""`: the payload slot holds the fresh
// string on the success path and a literal on the failure path, and the
// catch slot copies it on success and releases it on failure
const CATCH_NULLABLE: &str = "
func plain 1 string
param b
load 1 b
call 2 bytes_to_string_utf8 string 1 1
iconst 3 0
neq 4 2 3
brif 4 L0 L1
label L0
iconst 5 1
store __lp_flag_3_0 5
store __lp_val_3_0 2
jmp L2
label L1
iconst 6 0
store __lp_flag_3_0 6
strref 7 m1s56
store __lp_val_3_0 7
label L2
load 8 __lp_flag_3_0
load 9 __lp_val_3_0
brif 8 L3 L4
label L3
store __catch_10_3 9
jmp L5
label L4
call 10 pith_cstring_release void 1 9
strref 11 m1s13
store __catch_10_3 11
label L5
load 12 __catch_10_3
ret 12
iconst 13 0
ret 13
mark kind b bytes
endfunc
";

#[test]
fn a_byte_buffer_call_drops_no_stored_value() {
    assert!(!runtime_call_writes("byte_buffer_take_bytes"));
    assert!(!runtime_call_writes("byte_buffer_clear"));
    assert!(runtime_call_writes("list_clear"));
}

#[test]
fn a_catch_over_a_nullable_string_balances() {
    let r = check(CATCH_NULLABLE);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
}


// `return zstd_compress(d) catch bytes.empty()`: the catch slot holds the
// payload on one path and a fresh default on the other
const CATCH_FRESH_DEFAULT: &str = "
func make_default 1 string
param b
load 1 b
call 2 bytes_to_string_utf8 string 1 1
ret 2
mark kind b bytes
endfunc
func compress 1 string
param data
load 1 data
call 3 bytes_to_string_utf8 string 1 1
iconst 4 0
neq 5 3 4
brif 5 L9 L10
label L9
iconst 6 1
store __lp_flag_4_9 6
store __lp_val_4_9 3
jmp L11
label L10
iconst 7 0
store __lp_flag_4_9 7
strref 8 m12s93
store __lp_val_4_9 8
label L11
load 9 __lp_flag_4_9
load 10 __lp_val_4_9
brif 9 L12 L13
label L12
store __catch_11_12 10
jmp L14
label L13
call 11 pith_cstring_release void 1 10
load 15 data
call 12 make_default string 1 15
store __catch_11_12 12
label L14
load 13 __catch_11_12
ret 13
iconst 14 0
ret 14
mark kind data bytes
endfunc
";

#[test]
fn a_catch_with_a_fresh_default_balances() {
    let r = check(CATCH_FRESH_DEFAULT);
    assert_eq!((r.passed, r.failed, r.undecided), (2, 0, 0), "{:?}", r);
}

#[test]
fn a_catch_with_a_fresh_default_that_drops_the_payload_is_a_leak() {
    let r = check(&CATCH_FRESH_DEFAULT.replace("store __catch_11_12 10\n", ""));
    assert_eq!(r.failed, 1, "{:?}", r);
}


// a select receive arm: the shell is there only when the flag is set
const SELECT_RECV: &str = "
func pick 1 int
param ch
load 14 ch
call 15 pith_channel_try_recv tuple 1 14
optflag 16 15
brif 16 L11 L12
label L11
store v 15
load 18 v
call 20 pith_struct_release void 1 15
iconst 19 1
ret 19
label L12
iconst 21 0
ret 21
mark kind ch channel
endfunc
";

#[test]
fn a_received_shell_released_where_it_came_balances() {
    let r = check(SELECT_RECV);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
}

#[test]
fn a_received_shell_never_released_is_a_leak() {
    let r = check(&SELECT_RECV.replace("call 20 pith_struct_release void 1 15\n", ""));
    assert_eq!(r.failed, 1, "{:?}", r);
}


// `x := if c: fresh() else: p`, then another branch before x is handed
// off: the register reading x crosses the second join
const IF_VALUE_ACROSS_JOIN: &str = "
func pick 1 string
param p
iconst 1 1
brif 1 L1 L2
label L1
call 3 int_to_string string 1 1
store __ifexpr_1 3
jmp L3
label L2
load 4 p
call 5 pith_cstring_retain void 1 4
store __ifexpr_1 4
label L3
load 6 __ifexpr_1
iconst 7 1
brif 7 L4 L5
label L4
jmp L6
label L5
jmp L6
label L6
ret 6
mark kind p string
endfunc
";

#[test]
fn a_branch_value_read_across_a_later_join_balances() {
    let r = check(IF_VALUE_ACROSS_JOIN);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
}


// a field read retained for a call and released after it
const FIELD_READ_ARG: &str = "
func show 1 int
param settings
load 13 settings
field 14 13 8 string issuer
call 15 pith_cstring_retain void 1 14
call 16 pith_cstring_len int 1 14
call 17 pith_cstring_release void 1 14
ret 16
mark kind settings struct
endfunc
";

#[test]
fn a_retained_field_read_released_once_balances() {
    let r = check(FIELD_READ_ARG);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
}

#[test]
fn a_retained_field_read_released_twice_is_reported() {
    let r = check(&FIELD_READ_ARG.replace("call 17 pith_cstring_release void 1 14\n", "call 17 pith_cstring_release void 1 14\ncall 18 pith_cstring_release void 1 14\n"));
    assert_eq!(r.failed, 1, "{:?}", r);
}

// a struct read out of a fresh optional from a thread-local map,
// retained for a pair call and released after it
const OPT_PAYLOAD_ARG: &str = "
func flush_state 1 result_reg
param state
iconst 1 0
iconst 2 1
ret2 2 1
mark kind state State
endfunc
func flush_internal 1 result_reg
param writer
iconst 13 11
funcref 14 tlsinit
iconst 15 3
call 12 pith_tls_get_or_init_tagged map_int 3 13 14 15
load 16 writer
field 17 16 0 int handle
call 19 map_get_ikey_opt tuple 2 12 17
optflag 20 19
brif 20 L1127 L1128
label L1127
field 21 19 8 State value
call 22 pith_struct_release void 1 19
jmp L1129
label L1128
call 23 pith_struct_release void 1 19
strref 24 m3s210
iconst 25 0
ret2 25 24
label L1129
call 26 pith_struct_retain void 1 21
rcall 28 29 flush_state int 1 21
call 30 pith_struct_release void 1 21
ret2 28 29
mark kind writer Writer
endfunc
";

#[test]
fn a_retained_optional_payload_released_once_balances() {
    let r = check(OPT_PAYLOAD_ARG);
    assert_eq!((r.passed, r.failed, r.undecided), (2, 0, 0), "{:?}", r);
}

#[test]
fn a_retained_optional_payload_never_released_is_reported() {
    let r = check(&OPT_PAYLOAD_ARG.replace("call 30 pith_struct_release void 1 21\n", ""));
    assert_eq!(r.failed, 1, "{:?}", r);
}

#[test]
fn a_retained_optional_payload_released_twice_is_reported() {
    let r = check(&OPT_PAYLOAD_ARG.replace("call 30 pith_struct_release void 1 21\n", "call 30 pith_struct_release void 1 21\ncall 31 pith_struct_release void 1 21\n"));
    assert_eq!(r.failed, 1, "{:?}", r);
}

// a tuple that holds a count on one path and gets a destructor, and is
// built without one holding a null on the other: freeing it hands its
// payload to nobody, so a read of it taken before a later block holds
// no count
const DTOR_AFTER_STORE: &str = "
func pick 1 int
param flag
load 1 flag
brif 1 L1 L2
label L1
call 2 int_to_string string 1 1
iconst 3 2
call 4 pith_struct_alloc struct:tuple 1 3
sstore 4 1 2
funcref 5 dtor
call 6 pith_struct_set_dtor void 2 4 5
store picked 4
jmp L3
label L2
iconst 7 0
iconst 8 2
call 9 pith_struct_alloc struct:tuple 1 8
sstore 9 1 7
store picked 9
label L3
load 10 picked
field 11 10 8 string 1
iconst 12 0
brif 12 L4 L5
label L4
jmp L5
label L5
load 13 picked
call 14 pith_struct_release void 1 13
iconst 15 0
ret 15
mark kind picked tuple
mark owns picked tuple
endfunc
";

#[test]
fn a_destructor_takes_over_a_shells_payload() {
    let r = check(DTOR_AFTER_STORE);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
}

// a struct of the program's own with a field named `ok`, read across a
// block boundary: it is not a result box, so freeing the struct hands
// the read nothing
const STRUCT_FIELD_NAMED_OK: &str = "
func tally 1 int
param n
call 1 make_metrics struct:Metrics 0
store metrics 1
load 2 metrics
field 3 2 8 Counter ok
iconst 4 1
brif 4 L1 L2
label L1
jmp L2
label L2
call 5 pith_struct_retain void 1 3
call 6 record void 1 3
call 7 pith_struct_release void 1 3
load 8 metrics
call 9 pith_struct_release void 1 8
iconst 10 0
ret 10
mark kind metrics Metrics
mark owns metrics struct
endfunc
func make_metrics 0 struct:Metrics
iconst 1 1
call 2 pith_struct_alloc struct:Metrics 1 1
ret 2
endfunc
func record 1 void
param c
iconst 1 0
ret 1
mark kind c Counter
endfunc
";

// a borrowed result box returned whole goes through the unshare helper,
// which takes the count the return retained and hands back an owned box
const UNSHARE_RETURN: &str = "
func first 1 tuple
param rs
load 1 rs
iconst 2 0
call 3 pith_list_get_value tuple 2 1 2
call 4 pith_struct_retain void 1 3
call 5 __result_unshare_s0_m0 tuple 1 3
ret 5
mark kind rs list
endfunc
";

#[test]
fn the_unshare_helper_takes_its_argument_and_hands_back_a_count() {
    let r = check(UNSHARE_RETURN);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
    // without the retain the helper takes a count the function never had
    let r = check(&UNSHARE_RETURN.replace("call 4 pith_struct_retain void 1 3\n", ""));
    assert_eq!(r.failed, 1, "{:?}", r);
}

// a local box whose `ok` was read, then returned through the unshare: the
// payload leaves inside the box, so the read is handed no count
const UNSHARE_AFTER_OK_READ: &str = "
func kept 1 tuple
param i
call 1 mk_pair tuple 1 i
store pf 1
load 2 pf
field 3 2 8 tuple ok
field 4 3 8 string 1
call 5 pith_print_cstr unknown 1 4
load 6 pf
call 7 __result_unshare_ps_m0 tuple 1 6
ret 7
mark kind i int
mark kind pf tuple
mark owns pf tuple
endfunc
func mk_pair 1 tuple
param i
iconst 1 3
call 2 pith_struct_alloc struct:tuple 1 1
ret 2
endfunc
";

#[test]
fn the_unshare_hands_the_payload_on_inside_the_box() {
    let r = check(UNSHARE_AFTER_OK_READ);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
}

// clearing a box's destructor (a null one) leaves it a shell: freeing it
// still hands the read payload to the reader, who must give it up
const DTOR_CLEARED: &str = "
func take 1 string
param i
call 1 mk tuple 1 i
field 2 1 8 string ok
iconst 3 0
call 4 pith_struct_set_dtor void 2 1 3
call 5 pith_struct_release void 1 1
ret 2
mark kind i int
endfunc
func mk 1 tuple
param i
iconst 1 3
call 2 pith_struct_alloc struct:tuple 1 1
ret 2
endfunc
";

#[test]
fn a_cleared_destructor_leaves_the_box_a_shell() {
    let r = check(DTOR_CLEARED);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
}

#[test]
fn a_struct_field_named_ok_is_not_a_result_box() {
    let r = check(STRUCT_FIELD_NAMED_OK);
    assert_eq!((r.passed, r.failed, r.undecided), (2, 0, 0), "{:?}", r);
}

// a checked owned store keeps the value only when it reports success:
// the miss arm releases what the store left behind
const CHECKED_OWNED_STORE: &str = "
func put 2 result_reg
param xs
param i
call 1 int_to_string string 1 i
load 2 xs
load 3 i
call 4 pith_list_set_value_owned_checked int 3 2 3 1
brif 4 L1 L2
label L2
call 5 pith_cstring_release void 1 1
strref 6 m1s1
iconst 7 0
ret2 7 6
label L1
iconst 8 1
iconst 9 0
ret2 8 9
mark kind xs list
endfunc
";

#[test]
fn a_checked_owned_store_gives_the_value_back_on_a_miss() {
    let r = check(CHECKED_OWNED_STORE);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
    let r = check(&CHECKED_OWNED_STORE.replace("call 5 pith_cstring_release void 1 1\n", ""));
    assert_eq!(r.failed, 1, "{:?}", r);
}

// a fixed-width integer named by its source type is a plain word
const SIZED_INT_RETURN: &str = "
func larger 2 UInt64
param a
param b
load 2 a
load 3 b
ugt 4 2 3
brif 4 L1 L2
label L1
load 5 a
ret 5
label L2
load 6 b
ret 6
mark kind a UInt64
mark kind b UInt64
endfunc
";

#[test]
fn a_sized_integer_is_not_counted() {
    let r = check(SIZED_INT_RETURN);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
}

// an owned local released on one path of a branch only
const BRANCH_RELEASE: &str = "
func pick 1 int
param n
iconst 1 0
store s 1
load 2 s
call 3 int_to_string string 1 2
call 4 pith_cstring_release void 1 2
store s 3
load 5 n
brif 5 L1 L2
label L1
load 6 s
call 7 pith_cstring_retain void 1 6
call 8 pith_cstring_release void 1 6
jmp L3
label L2
jmp L3
label L3
load 9 s
call 10 pith_cstring_release void 1 9
iconst 11 0
ret 11
mark kind n int
mark kind s string
mark owns s string
endfunc
";

#[test]
fn a_balanced_branch_balances() {
    let r = check(BRANCH_RELEASE);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
}

#[test]
fn a_release_missing_on_one_branch_is_reported() {
    let r = check(&BRANCH_RELEASE.replace("call 8 pith_cstring_release void 1 6\n", ""));
    assert_eq!(r.failed, 1, "{:?}", r);
}


// a list retained and released around a call on every loop pass
const LOOP_RETAIN: &str = "
func drain 1 int
param xs
label L1
load 2 xs
call 3 pith_list_len int 1 2
brif 3 L2 L3
label L2
load 4 xs
call 5 pith_list_retain_handle void 1 4
call 6 pith_list_len int 1 4
call 7 pith_list_release_handle void 1 4
jmp L1
label L3
iconst 8 0
ret 8
mark kind xs list
endfunc
";

#[test]
fn a_loop_that_gives_back_what_it_takes_balances() {
    let r = check(LOOP_RETAIN);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
}

#[test]
fn a_loop_that_keeps_a_count_each_pass_is_reported() {
    let r = check(&LOOP_RETAIN.replace("call 7 pith_list_release_handle void 1 4\n", ""));
    assert_eq!(r.failed, 1, "{:?}", r);
}


// a closure parameter named like a runtime function, called for a result
// that is only tested and then released
const CLOSURE_NAMED_WRITE: &str = "
func pump 2 int
param write
param n
load 2 n
call 3 write tuple 1 2
field 4 3 0 bool is_ok
iconst 5 0
eq 6 4 5
call 7 __result_arg_release_0s_m12 void 1 3
brif 6 L14 L15
label L14
iconst 8 1
ret 8
label L15
iconst 9 0
ret 9
mark kind write closure
mark kind n int
endfunc
";

#[test]
fn a_closure_named_like_a_runtime_function_hands_back_an_owned_result() {
    let r = check(CLOSURE_NAMED_WRITE);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
}

#[test]
fn a_closure_result_never_released_is_a_leak() {
    let r = check(&CLOSURE_NAMED_WRITE.replace("call 7 __result_arg_release_0s_m12 void 1 3\n", ""));
    assert_eq!(r.failed, 1, "{:?}", r);
}


// `await make_task(3)`: a task handle a call hands back carries no count
const AWAIT_RETURNED_TASK: &str = "
func main 0 void
iconst 1 3
call 2 make_task task 1 1
call 3 pith_await unknown 1 2
store total 3
iconst 8 0
ret 8
mark kind total int
mark untracked total
endfunc
func make_task 1 task
param n
iconst 2 0
ret 2
mark kind n int
endfunc
";

#[test]
fn an_awaited_task_handle_holds_no_count() {
    let r = check(AWAIT_RETURNED_TASK);
    assert_eq!(r.failed, 0, "{:?}", r);
}

#[test]
fn a_retained_borrow_returned_balances() {
    let r = check(FIRST_NAME);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
}

#[test]
fn a_borrow_returned_without_its_retain_is_an_over_release() {
    let r = check(&FIRST_NAME.replace("call 12 pith_cstring_retain void 1 11\n", ""));
    assert_eq!(r.failed, 1, "{:?}", r);
    assert!(r.problems[0].contains("more than it took"), "{:?}", r.problems);
}

// main: a list built, pushed into, passed along, released at exit
const MAIN: &str = "
    func main 0 void
    iconst 1 0
    store xs 1
    load 2 xs
    call 3 pith_list_new_struct list 0
    call 4 pith_list_release_handle void 1 2
    store xs 3
    load 5 xs
    strref 6 m0s0
    iconst 8 1
    call 7 pith_struct_alloc struct:Node 1 8
    sstore 7 0 6
    call 11 pith_list_push_value_owned void 2 5 7
    load 16 xs
    call 17 pith_list_release_handle void 1 16
    iconst 18 0
    ret 18
    mark kind xs list
    mark owns xs list
    endfunc
";

#[test]
fn an_owned_local_released_at_exit_balances() {
    let r = check(MAIN);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
}

#[test]
fn a_missing_release_is_a_leak() {
    let r = check(&MAIN.replace("call 17 pith_list_release_handle void 1 16\n", ""));
    assert_eq!(r.failed, 1, "{:?}", r);
    assert!(r.problems[0].contains("leak"), "{:?}", r.problems);
}

#[test]
fn a_doubled_release_gives_up_a_count_it_doesnt_hold() {
    let r = check(&MAIN.replace(
        "call 17 pith_list_release_handle void 1 16\n",
        "call 17 pith_list_release_handle void 1 16\ncall 19 pith_list_release_handle void 1 16\n",
    ));
    assert_eq!(r.failed, 1, "{:?}", r);
}

#[test]
fn a_rebind_without_releasing_the_old_value_leaks() {
    // the first rebind replaces a null, which needs no release; a
    // second one without a release drops the first list's count
    let rebind = "store xs 3\nload 20 xs\ncall 21 pith_list_new_struct list 0\nstore xs 21\n";
    let r = check(&MAIN.replace("store xs 3\n", rebind));
    assert_eq!(r.failed, 1, "{:?}", r);
    assert!(r.problems.iter().any(|p| p.contains("leak")), "{:?}", r.problems);
    // releasing a null before the first store is fine either way
    let r = check(&MAIN.replace("call 4 pith_list_release_handle void 1 2\n", ""));
    assert_eq!(r.passed, 1, "{:?}", r);
}

// fn set_value(s: String): self.value = s
const SET_VALUE: &str = "
    func Input_set_value 2 void
    param self
    param s
    load 1 s
    call 3 pith_cstring_retain void 1 1
    load 2 self
    field 5 2 0 string value
    call 6 pith_cstring_release void 1 5
    sstore 2 0 1
    iconst 7 0
    ret 7
    mark kind self Input
    mark kind s string
    endfunc
";

#[test]
fn a_field_replaced_through_its_read_balances() {
    let r = check(SET_VALUE);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
}

#[test]
fn a_field_released_and_never_stored_dangles() {
    let r = check(&SET_VALUE.replace("sstore 2 0 1\n", "call 8 pith_cstring_release void 1 1\n"));
    assert_eq!(r.failed, 1, "{:?}", r);
    assert!(r.problems.iter().any(|p| p.contains("dangles")), "{:?}", r.problems);
}

#[test]
fn a_store_then_a_release_of_the_old_value_balances() {
    // the order Buffer.reset compiles to: read, store, then release
    let store_first = SET_VALUE.replace(
        "call 6 pith_cstring_release void 1 5\nsstore 2 0 1\n",
        "sstore 2 0 1\ncall 6 pith_cstring_release void 1 5\n",
    );
    let r = check(&store_first);
    assert_eq!((r.passed, r.failed), (1, 0), "{:?}", r);
    // and the displaced value, never released, is a leak
    let r = check(&store_first.replace("call 6 pith_cstring_release void 1 5\n", ""));
    assert_eq!(r.failed, 1, "{:?}", r);
}

// name = value with a retain on one path, name = a fresh substring on the
// other, both released at exit: the shape parser_mark_guarded compiles to
const ALIASED: &str = "
    func mark 1 void
    param s
    iconst 1 0
    store value 1
    iconst 2 0
    store name 2
    load 3 s
    call 4 pith_cstring_trim string 1 3
    store value 4
    load 5 value
    call 6 pith_cstring_retain void 1 5
    store name 5
    iconst 7 1
    brif 7 L1 L2
    label L1
    load 8 value
    iconst 9 0
    call 10 pith_cstring_substring string 3 8 9 9
    load 11 name
    call 12 pith_cstring_release void 1 11
    store name 10
    jmp L2
    label L2
    load 13 value
    call 14 pith_cstring_release void 1 13
    load 15 name
    call 16 pith_cstring_release void 1 15
    iconst 17 0
    ret 17
    mark kind s string
    mark kind value string
    mark kind name string
    mark owns value string
    mark owns name string
    endfunc
";

#[test]
fn slots_sharing_a_value_join_slots_holding_their_own() {
    let r = check(ALIASED);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
}

#[test]
fn a_missing_release_after_an_aliasing_join_is_still_a_leak() {
    let r = check(&ALIASED.replace("call 16 pith_cstring_release void 1 15\n", ""));
    assert_eq!(r.failed, 1, "{:?}", r);
    // and dropping the release of the old shared value on one path too
    let r = check(&ALIASED.replace("call 12 pith_cstring_release void 1 11\n", ""));
    assert_eq!(r.failed, 1, "{:?}", r);
}


// examples/graceful_shutdown.pith: a result rebound each loop pass, its old
// value released through a register read before a branch. the slot is
// null on the first pass and a value on later ones.
#[test]
fn a_result_rebound_in_a_loop_balances() {
    let ir = r#"func std_time_delay 1 void
param x
iconst 1 0
ret 1
endfunc
func await_listening 1 bool
param port
iconst 1 0
store probe 1
iconst 2 0
store waited 2
label L12
load 3 waited
iconst 4 10000
lt 5 3 4
brif 5 L13 L14
label L13
load 6 probe
strref 7 m53s59
load 8 port
call 9 tcp_connect result_int 2 7 8
call 10 pith_cstring_release void 1 7
iconst 11 0
neq 12 9 11
brif 12 L15 L16
label L15
iconst 13 1
sub 14 9 13
iconst 15 1
iconst 16 0
iconst 17 3
call 18 pith_struct_alloc struct:tuple 1 17
sstore 18 0 15
sstore 18 1 14
sstore 18 2 16
store __legacy_result_11_15 18
jmp L17
label L16
strref 19 m53s1109
iconst 20 0
iconst 21 3
call 22 pith_struct_alloc struct:tuple 1 21
sstore 22 0 20
sstore 22 1 20
sstore 22 2 19
store __legacy_result_11_15 22
label L17
load 23 __legacy_result_11_15
call 24 __tuple_cascade_0s_m53 void 1 6
store probe 23
load 25 probe
field 26 25 0 bool is_ok
brif 26 L19 L20
label L19
load 27 probe
field 28 27 8 int ok
call 29 tcp_close unknown 1 28
iconst 30 1
load 31 probe
call 32 __tuple_cascade_0s_m53 void 1 31
ret 30
label L20
label L18
iconst 33 20
call 34 std_time_delay void 1 33
load 35 waited
iconst 36 20
add 37 35 36
store waited 37
jmp L12
label L14
iconst 38 0
load 39 probe
call 40 __tuple_cascade_0s_m53 void 1 39
ret 38
load 41 probe
call 42 __tuple_cascade_0s_m53 void 1 41
iconst 43 0
ret 43
mark kind __legacy_result_11_15 tuple
mark kind waited int
mark kind port int
mark kind probe tuple
mark owns probe tuple
mark untracked waited
endfunc
"#;
    let r = check(ir);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
    // without the release of the old value, the next pass leaks it
    let r = check(&ir.replace("call 24 __tuple_cascade_0s_m53 void 1 6\n", ""));
    assert_eq!(r.failed, 1, "{:?}", r);
}


// fn total(xs: List[Node]) -> Int: for n in xs: sum = sum + n.name.len()
// the loop as the emitter writes it when the body keeps storage
const SUM_NAMES: &str = "
    func total 1 int
    param xs
    iconst 1 0
    store sum 1
    load 2 xs
    call 3 pith_auto_len int 1 2
    iconst 4 0
    store __for_idx_0 4
    store __for_len_0 3
    store __for_iter_0 2
    label L0
    load 5 __for_idx_0
    load 6 __for_len_0
    lt 7 5 6
    brif 7 L1 L3
    label L1
    load 8 __for_iter_0
    load 9 __for_idx_0
    call 10 pith_list_get_value unknown 2 8 9
    store __loopvar_0_n 10
    load 11 __loopvar_0_n
    field 12 11 0 string name
    call 13 pith_cstring_len int 1 12
    load 14 sum
    add 15 14 13
    store sum 15
    label L2
    load 16 __for_idx_0
    iconst 17 1
    add 18 16 17
    store __for_idx_0 18
    jmp L0
    label L3
    load 19 sum
    ret 19
    mark kind __for_idx_0 int
    mark kind __for_len_0 int
    mark kind __for_iter_0 list
    mark kind __loopvar_0_n struct
    mark borrow __loopvar_0_n __for_iter_0
    mark kind xs list
    mark kind sum int
    endfunc
";

fn borrow_problems(ir: &str) -> Vec<String> {
    let lines: Vec<&str> = ir.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    check_loop_borrows(&lines)
}

// the #1270 shape: the body clears the list, then reads the variable
#[test]
fn a_borrowed_loop_variable_read_after_a_clear_is_reported() {
    assert!(borrow_problems(SUM_NAMES).is_empty());
    let cleared = SUM_NAMES.replace(
        "load 11 __loopvar_0_n\n",
        "call 30 list_clear void 1 8\nload 11 __loopvar_0_n\n",
    );
    let problems = borrow_problems(&cleared);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("list_clear"), "{problems:?}");
}

#[test]
fn a_call_into_a_function_that_writes_a_global_counts_as_a_drop() {
    let writer = "global __g_items list\nfunc bump 0 void\nload 1 __g_items\nstore __g_items 1\niconst 2 0\nret 2\nendfunc\n";
    let keeper = "func peek 0 int\niconst 1 7\nret 1\nendfunc\n";
    let calls = |callee: &str| {
        format!(
            "{writer}{keeper}{}",
            SUM_NAMES.replace("load 11 __loopvar_0_n\n", &format!("call 30 {callee} int 0\nload 11 __loopvar_0_n\n"))
        )
    };
    assert_eq!(borrow_problems(&calls("bump")).len(), 1);
    // a function that writes nothing keeps the borrow good
    assert!(borrow_problems(&calls("peek")).is_empty());
}

#[test]
fn a_function_without_marks_is_left_out() {
    let r = check("func helper 1 void\nparam p\niconst 1 0\nret 1\nendfunc\n");
    assert_eq!(r.unmarked, 1);
}

// the same IR with one line deleted or doubled
fn without(ir: &str, line: &str) -> String {
    assert!(ir.contains(&format!("{line}\n")), "{line}");
    ir.replacen(&format!("{line}\n"), "", 1)
}

fn doubled(ir: &str, line: &str) -> String {
    assert!(ir.contains(&format!("{line}\n")), "{line}");
    let mut parts: Vec<&str> = line.split(' ').collect();
    parts[1] = "999";
    ir.replacen(
        &format!("{line}\n"),
        &format!("{line}\n{}\n", parts.join(" ")),
        1,
    )
}

// a program's own `second` shadows the runtime's, which borrows its
// result: the call hands back a count like any user call
const USER_SHADOWS_RUNTIME: &str = "
func second 1 struct:Node
param a
load 1 a
call 2 pith_struct_retain void 1 1
ret 1
mark kind a Node
endfunc
func main 1 int
param a
load 2 a
call 3 second struct:Node 1 2
call 4 pith_struct_release void 1 3
iconst 5 0
ret 5
mark kind a Node
endfunc
";

#[test]
fn a_user_function_named_like_a_runtime_function_hands_back_a_count() {
    let r = check(USER_SHADOWS_RUNTIME);
    assert_eq!((r.passed, r.failed, r.undecided), (2, 0, 0), "{:?}", r);
    assert_eq!(
        check(&without(
            USER_SHADOWS_RUNTIME,
            "call 4 pith_struct_release void 1 3"
        ))
        .failed,
        1
    );
}

// fn apply(xs: List[Int], f: fn(Int) -> String) -> Int: an open-coded map
// keeps the function in an emitter temp and calls through it
const CALL_THROUGH_TEMP: &str = "
func apply 2 int
param x
param f
load 1 f
store __map_fn_L3 1
load 2 x
call 3 __map_fn_L3 string 1 2
call 4 pith_cstring_len int 1 3
call 5 pith_cstring_release void 1 3
ret 4
mark kind x int
mark kind f closure
endfunc
";

#[test]
fn a_call_through_a_temp_holding_a_closure_follows_the_user_convention() {
    let r = check(CALL_THROUGH_TEMP);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
    assert_eq!(
        check(&without(
            CALL_THROUGH_TEMP,
            "call 5 pith_cstring_release void 1 3"
        ))
        .failed,
        1
    );
    assert_eq!(
        check(&doubled(
            CALL_THROUGH_TEMP,
            "call 5 pith_cstring_release void 1 3"
        ))
        .failed,
        1
    );
}

// fn forward(x: T!) -> T! in a generic body: the payload's kind is `T`,
// and the retain says it is counted. without the retain the kind stays
// open and the function undecided, so only a doubled one is a finding
const GENERIC_FORWARD: &str = "
func forward 1 result_reg
param x word
load 1 x
field 2 1 0 bool is_ok
brif 2 L0 L1
label L0
field 3 1 8 unknown ok
call 4 pith_cstring_retain void 1 3
iconst 5 1
ret2 5 3
label L1
field 6 1 16 unknown err
call 7 pith_cstring_retain void 1 6
iconst 8 0
ret2 8 6
mark kind x tuple
endfunc
";

#[test]
fn a_value_of_an_open_kind_the_body_retains_is_counted() {
    let r = check(GENERIC_FORWARD);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
    assert_eq!(
        check(&doubled(
            GENERIC_FORWARD,
            "call 4 pith_cstring_retain void 1 3"
        ))
        .failed,
        1
    );
    assert_eq!(
        check(&doubled(
            GENERIC_FORWARD,
            "call 7 pith_cstring_retain void 1 6"
        ))
        .failed,
        1
    );
}

// `m.get(k)` kept as an optional: the payload is retained and the shell
// takes that count with its destructor
const KEPT_GETTER: &str = "
func known 1 tuple
param key word
load 1 __g_series
load 2 key
call 3 map_get_opt tuple 2 1 2
optval 4 3 unknown value
call 5 pith_struct_retain void 1 4
funcref 6 __opt_dtor_struct
call 7 pith_struct_set_dtor void 2 3 6
ret 3
mark kind key string
endfunc
";

#[test]
fn a_kept_optional_takes_the_count_of_its_retained_payload() {
    let r = check(&format!("mark global __g_series map\n{KEPT_GETTER}"));
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
    let doubled_retain = doubled(KEPT_GETTER, "call 5 pith_struct_retain void 1 4");
    assert_eq!(
        check(&format!("mark global __g_series map\n{doubled_retain}")).failed,
        1
    );
}

// a weak field read: the upgrade hands back a strong count or a null,
// and the optional built around it owns that count
const WEAK_FIELD_READ: &str = "
func parent_value 1 int
param c word
load 1 c
iconst 2 16
call 3 pith_struct_weak_load_field int 2 1 2
store __weak_read_1 3
brif 3 L1 L2
label L1
iconst 4 1
iconst 5 2
call 6 pith_struct_alloc tuple 1 5
sstore 6 0 4
sstore 6 1 3
funcref 7 __opt_dtor_struct
call 8 pith_struct_set_dtor void 2 6 7
store __weak_read_1 6
jmp L2
label L2
load 9 __weak_read_1
call 10 pith_struct_release void 1 9
iconst 11 0
ret 11
mark kind c Node
endfunc
";

#[test]
fn a_weak_read_owns_the_count_its_upgrade_took() {
    let r = check(WEAK_FIELD_READ);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
    assert_eq!(
        check(&without(
            WEAK_FIELD_READ,
            "call 10 pith_struct_release void 1 9"
        ))
        .failed,
        1
    );
    assert_eq!(
        check(&doubled(
            WEAK_FIELD_READ,
            "call 10 pith_struct_release void 1 9"
        ))
        .failed,
        1
    );
}

// a weak field stored into a fresh struct keeps a weak count, not a
// strong one
const WEAK_FIELD_INIT: &str = "
func make_child 1 struct:Node
param p word
load 1 p
call 2 pith_struct_weak_retain void 1 1
iconst 3 1
call 4 pith_struct_alloc struct:Node 1 3
sstore 4 0 1
ret 4
mark kind p Node
endfunc
";

#[test]
fn a_weak_field_store_hands_over_no_strong_count() {
    let r = check(WEAK_FIELD_INIT);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
}

// `x := o.unwrap_or(fb)`: the temp holds one count on each path, and on
// the fallback path it shares the value with `fb`, which holds its own
const UNWRAP_OR_TEMP: &str = "
func pick 1 int
param o word
iconst 1 0
store fb 1
load 2 fb
call 3 int_to_string string 1 2
store fb 3
load 4 o
optflag 5 4
brif 5 L1 L2
label L1
optval 6 4 string value
call 7 pith_cstring_retain void 1 6
store __unwrap_or_1 6
jmp L3
label L2
load 8 fb
call 9 pith_cstring_retain void 1 8
store __unwrap_or_1 8
label L3
load 10 __unwrap_or_1
store x 10
load 11 x
call 12 pith_cstring_len int 1 11
load 13 fb
call 14 pith_cstring_release void 1 13
load 15 x
call 16 pith_cstring_release void 1 15
ret 12
mark kind o tuple
mark kind fb string
mark kind x string
mark owns fb string
mark owns x string
endfunc
";

#[test]
fn a_temp_holding_its_own_count_joins_a_path_where_it_shares_an_owned_value() {
    let r = check(UNWRAP_OR_TEMP);
    assert_eq!((r.passed, r.failed, r.undecided), (1, 0, 0), "{:?}", r);
    assert_eq!(
        check(&without(
            UNWRAP_OR_TEMP,
            "call 16 pith_cstring_release void 1 15"
        ))
        .failed,
        1
    );
    assert_eq!(
        check(&doubled(
            UNWRAP_OR_TEMP,
            "call 14 pith_cstring_release void 1 13"
        ))
        .failed,
        1
    );
}

// `mut cur := cursor` and `cur = advance(cur)` in a loop: `cursor` aliases
// `cur` before the loop and borrows on its own after a pass
const CURSOR_LOOP: &str = "
func walk 1 struct:Cursor
param cursor word
iconst 1 0
store cur 1
load 2 cur
load 3 cursor
call 4 pith_struct_retain void 1 3
call 5 pith_struct_release void 1 2
store cur 3
label L1
load 6 cur
call 7 done bool 1 6
brif 7 L3 L2
label L2
load 8 cur
call 9 advance struct:Cursor 1 8
load 10 cur
call 11 pith_struct_release void 1 10
store cur 9
jmp L1
label L3
load 12 cursor
call 13 pith_struct_retain void 1 12
load 14 cur
call 15 pith_struct_release void 1 14
ret 12
mark kind cursor Cursor
mark kind cur Cursor
mark owns cur struct
endfunc
func done 1 bool
param c word
iconst 1 1
ret 1
mark kind c Cursor
endfunc
func advance 1 struct:Cursor
param c word
load 1 c
call 2 pith_struct_retain void 1 1
ret 1
mark kind c Cursor
endfunc
";

#[test]
fn a_parameter_copied_into_a_rebound_local_stays_readable_past_the_loop() {
    let r = check(CURSOR_LOOP);
    assert_eq!((r.passed, r.failed, r.undecided), (3, 0, 0), "{:?}", r);
    assert_eq!(
        check(&without(
            CURSOR_LOOP,
            "call 11 pith_struct_release void 1 10"
        ))
        .failed,
        1
    );
    assert_eq!(
        check(&without(
            CURSOR_LOOP,
            "call 13 pith_struct_retain void 1 12"
        ))
        .failed,
        1
    );
}

// `mut data := arrived`, replaced on one path: the replace releases the
// parameter's retained copy. doubling that release made the paths
// disagree at the join, and the function was left undecided instead of
// reported (a mutation miss in std.io's buffered_text_decode)
const REPLACED_COPY: &str = "
func keep 1 int
param arrived word
iconst 1 0
store data 1
load 2 data
load 3 arrived
call 4 pith_bytes_retain void 1 3
call 5 pith_bytes_release void 1 2
store data 3
call 6 flag bool 0
brif 6 L1 L2
label L1
call 7 fresh bytes 0
load 8 data
call 9 pith_bytes_release void 1 8
store data 7
jmp L2
label L2
load 10 data
call 11 pith_bytes_release void 1 10
iconst 12 0
ret 12
mark kind arrived bytes
mark kind data bytes
mark owns data bytes
endfunc
func flag 0 bool
iconst 1 1
ret 1
mark kind x int
endfunc
func fresh 0 bytes
iconst 2 16
call 1 secure_random_bytes bytes 1 2
ret 1
mark kind x int
endfunc
";

#[test]
fn a_doubled_release_that_unsettles_a_later_join_is_still_reported() {
    let r = check(REPLACED_COPY);
    assert_eq!((r.passed, r.failed, r.undecided), (3, 0, 0), "{:?}", r);
    let r = check(&doubled(
        REPLACED_COPY,
        "call 9 pith_bytes_release void 1 8",
    ));
    assert_eq!((r.failed, r.undecided), (1, 0), "{:?}", r);
    assert_eq!(
        check(&without(
            REPLACED_COPY,
            "call 9 pith_bytes_release void 1 8"
        ))
        .failed,
        1
    );
}

#[test]
fn double_release_of_struct_field_is_reported() {
    // H-20(1): releasing a field twice before storing back into it is a double-free.
    let ir = "
func bad 1 void
param b word
load 1 b
field 2 1 0 string name
call 3 pith_cstring_release void 1 2
load 4 b
field 5 4 0 string name
call 6 pith_cstring_release void 1 5
call 7 int_to_string string 1 4
sstore 1 0 7
ret
mark kind b Box
endfunc
";
    let r = check(ir);
    assert_eq!(r.failed, 1, "{r:?}");
}

#[test]
fn released_error_string_returned_on_ret2_is_reported() {
    // H-20(2): returning a released error payload from a plain-ok rcall via ret2(0, val).
    let ir = "
func inner 0 result_reg
iconst 1 1
iconst 2 42
ret2 1 2
mark kind x int
endfunc
func bad 0 result_reg
rcall 1 2 inner int 0
brif 1 L1 L2
label L1
iconst 3 1
ret2 3 2
label L2
call 4 pith_cstring_release void 1 2
iconst 5 0
ret2 5 2
mark kind x int
endfunc
";
    let r = check(ir);
    assert_eq!(r.failed, 1, "{r:?}");
}

#[test]
fn shell_ownership_transfer_deduplicates_readers_and_catches_unread_fields() {
    // H-20(3): releasing a destructor-less shell with an unread owned field leaks.
    let unread_leak = "
func leak_shell 0 void
iconst 1 1
call 2 int_to_string string 1 1
iconst 3 2
call 4 pith_struct_alloc struct:tuple 1 3
sstore 4 1 2
call 5 pith_struct_release void 1 4
ret
mark kind x int
endfunc
";
    assert_eq!(check(unread_leak).failed, 1);

    // Two reads of the same shell field cannot both inherit a count.
    let dup_reader = "
func dup_shell 0 void
iconst 1 1
call 2 int_to_string string 1 1
iconst 3 2
call 4 pith_struct_alloc struct:tuple 1 3
sstore 4 1 2
field 5 4 8 string value
field 6 4 8 string value
call 7 pith_struct_release void 1 4
call 8 pith_cstring_release void 1 5
call 9 pith_cstring_release void 1 6
ret
mark kind x int
endfunc
";
    assert_eq!(check(dup_reader).failed, 1);
}

#[test]
fn stack_closure_with_counted_capture_is_reported() {
    // M-14: closure_stack is never released, so a counted capture leaks.
    let ir = "
func leak_cap 1 void
param s word
closure_stack 1 target 1
iconst 2 0
load 3 s
call 4 pith_cstring_retain void 1 3
iconst 5 1
call 6 pith_closure_set_env_rc void 4 1 2 3 5
ret
mark kind s string
endfunc
";
    let r = check(ir);
    assert_eq!(r.failed, 1, "{r:?}");
}

#[test]
fn appendn_consumes_its_base_string() {
    // M-14: appendn takes ownership of its base operand; releasing it again fails.
    let ir = "
func bad_append 0 string
iconst 1 7
call 2 int_to_string string 1 1
strref 3 m0s0
appendn 4 2 2 3
call 5 pith_cstring_release void 1 2
ret 4
mark kind x int
endfunc
";
    assert_eq!(check(ir).failed, 1);
}

#[test]
fn weak_retain_covers_only_one_weak_sstore() {
    // M-14: a single weak_retain covers one weak sstore, not a second strong sstore.
    let ir = "
func bad_strong 1 struct:Node
param p word
load 1 p
call 2 pith_struct_weak_retain void 1 1
iconst 3 2
call 4 pith_struct_alloc struct:Node 1 3
sstore 4 0 1
sstore 4 1 1
ret 4
mark kind p Node
endfunc
";
    assert_eq!(check(ir).failed, 1);
}

#[test]
fn loop_borrows_catches_map_insert_preloaded_reg_and_inner_backedge() {
    // H-20(4): map_insert eviction + preloaded register read after clear + inner loop back-edge.
    let map_evict = "
func evict 1 void
param m word
mark kind m map
mark kind __loopvar_1_v string
mark borrow __loopvar_1_v m
label L0
load 10 __for_idx_1
load 11 m
call 12 pith_list_get_value string 2 11 10
store __loopvar_1_v 12
load 1 m
load 2 __loopvar_1_v
strref 3 m0s0
call 4 map_insert void 3 1 3 2
load 5 __loopvar_1_v
call 6 print void 1 5
jmp L0
ret
endfunc
";
    let lines: Vec<&str> = map_evict.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    assert_eq!(check_loop_borrows(&lines).len(), 1);

    let preloaded = "
func preloaded 1 void
param xs word
mark kind xs list
mark kind __loopvar_1_item string
mark borrow __loopvar_1_item xs
label L0
load 10 __for_idx_1
load 11 xs
call 12 pith_list_get_value string 2 11 10
store __loopvar_1_item 12
load 1 __loopvar_1_item
load 2 xs
call 3 list_clear void 1 2
call 4 print void 1 1
jmp L0
ret
endfunc
";
    let lines: Vec<&str> = preloaded.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    assert_eq!(check_loop_borrows(&lines).len(), 1);

    let inner_loop = "
func inner 1 void
param xs word
mark kind xs list
mark kind __loopvar_1_item string
mark borrow __loopvar_1_item xs
label L0
load 10 __for_idx_1
load 11 xs
call 12 pith_list_get_value string 2 11 10
store __loopvar_1_item 12
label L1
load 1 __loopvar_1_item
call 2 print void 1 1
load 3 xs
call 4 list_clear void 1 3
iconst 5 1
brif 5 L1 L2
label L2
jmp L0
ret
endfunc
";
    let lines: Vec<&str> = inner_loop.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    assert_eq!(check_loop_borrows(&lines).len(), 1);
}

#[test]
fn zero_arg_retain_does_not_panic() {
    let ir = "
        func f 0 void
        mark kind x int
        call 1 pith_cstring_retain void 0
        ret
        endfunc
    ";
    let report = check(ir);
    assert_eq!(report.passed, 1);
    assert_eq!(report.failed, 0);
}
