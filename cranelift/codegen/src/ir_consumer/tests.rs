use super::dce::*;
use super::fast_paths::*;
use super::instrumentation::*;
use super::ir::*;
use super::lower::{compile_ir_function, create_label_blocks, ModuleCtx};
use super::module::*;
use super::passes::scans::*;
use super::passes::string_fusion::*;
use cranelift::prelude::*;
use cranelift_module::{DataId, FuncId, Linkage, Module};
use pith_runtime::runtime_core::STRUCT_SHARED_MASK;
use std::collections::{HashMap, HashSet};


fn fuse(body: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = body.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    fuse_concat_chains(&lines, &[], &HashMap::new(), &HashMap::new())
}

/// `chr(123) + q + "id" + q`, as the emitter lowers it.
const CHAIN: &str = "
    iconst 25 123
    call 26 chr string 1 25
    load 27 q
    concat 28 26 27
    call 29 pith_cstring_release void 1 26
    strref 30 m0s1
    concat 31 28 30
    call 32 pith_cstring_release void 1 28
    call 33 pith_cstring_release void 1 30
    load 34 q
    concat 35 31 34
    call 36 pith_cstring_release void 1 31
    store body 35
";

fn appends(body: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = body.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    fuse_self_appends(&lines, &HashMap::new())
}

/// `s = s + "ab"`, as the emitter lowers it.
const SELF_APPEND: &str = "
    load 14 s
    load 15 s
    strref 16 m0s0
    concat 17 15 16
    call 18 pith_cstring_release void 1 16
    call 24 pith_cstring_release void 1 14
    store s 17
";

#[test]
fn a_self_append_takes_the_locals_count() {
    let rewritten = appends(SELF_APPEND).expect("the append is rewritten");
    assert!(rewritten.contains(&"appendn 17 1 15 16".to_string()));
    assert!(!rewritten.iter().any(|l| l == "call 24 pith_cstring_release void 1 14"));
    let fused = appends(&SELF_APPEND.replace("concat 17 15 16", "concatn 17 3 15 16 20"))
        .expect("a fused chain is rewritten too");
    assert!(fused.contains(&"appendn 17 2 15 16 20".to_string()));
}

#[test]
fn a_self_append_that_is_not_plain_keeps_its_concat() {
    // a global may be read by another task at the same time
    assert!(appends(&SELF_APPEND.replace(" s", " __g_s")).is_none());
    // another read of the local, a branch, or another use of the result
    assert!(appends(&SELF_APPEND.replace("strref 16 m0s0", "load 16 s")).is_none());
    assert!(appends(&SELF_APPEND.replace("strref 16 m0s0", "strref 16 m0s0\nlabel L1")).is_none());
    assert!(appends(&SELF_APPEND.replace("store s 17", "store s 17\nstore t 17")).is_none());
    // no release of the old value: the local does not own it
    assert!(appends(&SELF_APPEND.replace("call 24 pith_cstring_release void 1 14\n", "")).is_none());
}

fn int_parts(body: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = body.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    fuse_int_parts(&lines, &[], &HashMap::new(), &HashMap::new())
}

/// `"value-" + n.to_string()`, as the emitter lowers it.
const INT_CONCAT: &str = "
    strref 4 m0s9
    load 5 n
    call 6 int_to_string string 1 5
    concat 7 4 6
    call 8 pith_cstring_release void 1 4
    call 9 pith_cstring_release void 1 6
    store label 7
";

#[test]
fn a_converted_int_joins_as_a_number() {
    let fused = int_parts(INT_CONCAT).expect("the conversion is folded in");
    // two parts, the second an Int: 2 | 0b10 << 8
    assert_eq!(
        fused,
        vec![
            "strref 4 m0s9",
            "load 5 n",
            "concatn 7 514 4 5",
            "call 8 pith_cstring_release void 1 4",
            "store label 7",
        ]
    );
}

#[test]
fn int_parts_fold_into_a_fused_chain_and_an_append() {
    let chain = "
        call 4 int_to_string string 1 3
        call 22 int_to_string string 1 21
        concatn 23 5 2 4 8 12 22
        call 7 pith_cstring_release void 1 4
        call 25 pith_cstring_release void 1 22
    ";
    // parts 1 and 4 are Ints: 5 | 0b10010 << 8
    assert_eq!(int_parts(chain).unwrap(), vec!["concatn 23 4613 2 3 8 12 21"]);
    let append = "
        call 16 int_to_string string 1 12
        appendn 17 2 15 16 19
        call 18 pith_cstring_release void 1 16
    ";
    // bit 0 is the first part after the base: 2 | 0b01 << 8
    assert_eq!(int_parts(append).unwrap(), vec!["appendn 17 258 15 12 19"]);
}

#[test]
fn a_long_join_count_carries_no_int_bits() {
    let shift = pith_runtime::JOIN_INT_SHIFT;
    let max = pith_runtime::CONCAT_MANY_MAX;
    // 514 is two parts with part 1 an Int
    assert_eq!(join_int_bits("514", 2, max), 2 << shift);
    // 256 and 300 parts: the count spills past the low byte
    assert_eq!(join_int_bits("256", 256, max), 0);
    assert_eq!(join_int_bits("300", 300, max), 0);
    assert_eq!(join_int_bits("256", 256, pith_runtime::APPEND_MANY_MAX), 0);
}

#[test]
fn a_converted_int_with_another_reader_stays_a_string() {
    // stored as well as joined, so the string has to exist
    let stored = INT_CONCAT.replace("store label 7", "store label 7\nstore digits 6");
    assert!(int_parts(&stored).is_none());
    // released on the far side of a branch
    let branched = INT_CONCAT.replace(
        "call 9 pith_cstring_release void 1 6",
        "brif 7 L1 L2\nlabel L1\ncall 9 pith_cstring_release void 1 6",
    );
    assert!(int_parts(&branched).is_none());
    // more parts than one call takes
    let long = "
        call 4 int_to_string string 1 3
        concatn 23 9 2 4 8 12 13 14 15 16 17
        call 7 pith_cstring_release void 1 4
    ";
    assert!(int_parts(long).is_none());
}

#[test]
fn a_concat_chain_becomes_one_concatn() {
    let fused = fuse(CHAIN).expect("the chain is fused");
    assert_eq!(
        fused,
        vec![
            "iconst 25 123",
            "call 26 chr string 1 25",
            "load 27 q",
            "strref 30 m0s1",
            "load 34 q",
            "concatn 35 4 26 27 30 34",
            "call 29 pith_cstring_release void 1 26",
            "call 33 pith_cstring_release void 1 30",
            "store body 35",
        ]
    );
}

#[test]
fn an_intermediate_read_twice_ends_the_chain() {
    // 28 is stored as well as extended, so it has to exist; the chain
    // starts at it instead.
    let body = CHAIN.replace("store body 35", "store body 35\nstore head 28");
    let fused = fuse(&body).expect("the rest of the chain is fused");
    assert!(fused.contains(&"concat 28 26 27".to_string()));
    assert!(fused.contains(&"concatn 35 3 28 30 34".to_string()));
    assert!(fused.contains(&"call 32 pith_cstring_release void 1 28".to_string()));
}

#[test]
fn a_branch_inside_a_chain_ends_it() {
    let body = CHAIN.replace("load 34 q", "label L1\nload 34 q");
    let fused = fuse(&body).expect("the part before the label is fused");
    assert!(fused.contains(&"concatn 31 3 26 27 30".to_string()));
    assert!(fused.contains(&"concat 35 31 34".to_string()));
}

#[test]
fn a_body_without_a_chain_is_left_alone() {
    assert!(fuse("load 1 a\nload 2 b\nconcat 3 1 2\nret 3").is_none());
    assert!(fuse("iconst 1 2\nret 1").is_none());
}

#[test]
fn string_literals_decode_their_escapes() {
    assert_eq!(decode_string_literal(r#"string m0s0 "a\nb\tc""#), "a\nb\tc");
    assert_eq!(
        decode_string_literal(r#"string 1 "{{x}} \"q\" \\ \0""#),
        "{x} \"q\" \\ \0"
    );
    // a trailing escaped quote keeps its quote
    assert_eq!(decode_string_literal(r#"string 1 "say \"""#), "say \"");
    // an escaped brace is the brace; an unknown escape (which the lexer
    // reports before any ir exists) and a lone brace stay as written
    assert_eq!(decode_string_literal(r#"string 1 "\{x\}""#), "{x}");
    assert_eq!(decode_string_literal(r#"string 1 "\q { }""#), "\\q { }");
    assert_eq!(decode_string_literal("string 1 \"café\""), "café");
}

#[test]
fn a_part_shared_by_two_chains_keeps_both_unfused() {
    // 2 is a part of both chains, and its release sits inside the second.
    let body = "
        load 1 a
        load 2 b
        load 3 c
        concat 4 1 2
        concat 5 4 3
        call 6 pith_cstring_release void 1 4
        concat 7 2 3
        call 8 pith_cstring_release void 1 2
        concat 9 7 1
        call 10 pith_cstring_release void 1 7
        ret 9
    ";
    assert!(fuse(body).is_none());
}

#[test]
fn safe_points_are_on_unless_the_build_asks_them_off() {
    // unset, and anything that is not an off spelling, emits the check.
    assert!(preempt_from_env(None));
    assert!(preempt_from_env(Some("1")));
    assert!(preempt_from_env(Some("")));
    assert!(preempt_from_env(Some("please")));

    // the opt-out is as generous as PITH_GREEN's, for the same reason: a
    // build reaching for it must not silently keep the safe-points.
    for off in ["0", "off", "false", "no", "n", " OFF ", "False"] {
        assert!(!preempt_from_env(Some(off)), "{off} should turn them off");
    }
}

#[test]
fn sanitize_names_its_checks_and_rejects_unknown_values() {
    for off in [None, Some(""), Some("0")] {
        assert_eq!(sanitize_hook_names(off), Ok(None));
    }
    assert_eq!(
        sanitize_hook_names(Some("thread")),
        Ok(Some(("__tsan_read8", "__tsan_write8")))
    );
    assert_eq!(
        sanitize_hook_names(Some(" address ")),
        Ok(Some(("__asan_load8", "__asan_store8")))
    );
    // a typo must not quietly build without the checks
    assert!(sanitize_hook_names(Some("1")).is_err());
    assert!(sanitize_hook_names(Some("tsan")).is_err());
}

/// Compile the one function in `ir`, `main`, and display its CLIF, with
/// the thread sanitizer's hooks when `sanitize`. Also answers the runtime
/// functions it was compiled against, by name.
fn compile_main(ir: &str, sanitize: bool) -> (String, HashMap<String, FuncId>) {
    let mut codegen = crate::create_codegen().expect("create codegen");
    let runtime_funcs = crate::runtime_imports::declare_runtime_functions(&mut codegen.module)
        .expect("declare runtime functions");
    let hooks = if sanitize {
        let mut sig = codegen.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        let mut declare = |name: &str| {
            codegen.module.declare_function(name, Linkage::Import, &sig).expect("declare")
        };
        Some(SanitizeHooks { read: declare("__tsan_read8"), write: declare("__tsan_write8") })
    } else {
        None
    };
    let lines: Vec<&str> = ir.lines().skip(1).take_while(|l| *l != "endfunc").collect();
    let empty_funcs = HashMap::new();
    let empty_lits = HashMap::new();
    let empty_structs = HashSet::new();
    let empty_globals = HashMap::new();
    let empty_str_globals = HashSet::new();
    let empty_bits = HashMap::new();
    let mctx = ModuleCtx {
        runtime_funcs: &runtime_funcs,
        declared_funcs: &empty_funcs,
        string_lits: &empty_lits,
        struct_names: &empty_structs,
        global_data: &empty_globals,
        str_globals: &[],
        string_global_names: &empty_str_globals,
        preempt_flag: None,
        sanitize: hooks,
        quarantine: false,
        fast_paths: hooks.is_none(),
        bits_aliases: &empty_bits,
    };
    let func = compile_ir_function(&mut codegen, &mctx, "main", "int", &[], &[], &lines)
        .expect("compile");
    (func.display().to_string(), runtime_funcs)
}

// with a sanitizer named, field reads and writes report to its checks
// and the inline fast paths give way to their runtime calls.
#[test]
fn sanitized_build_checks_fields_and_calls_the_runtime() {
    let ir = "func main 0 int\niconst 1 0\ncall 2 pith_struct_alloc int 1 1\n\
              sstore 2 0 1\nfield 3 2 0\ncall 4 pith_list_len int 1 1\nret 3\nendfunc\n";
    let plain = compile_main(ir, false).0;
    let sanitized = compile_main(ir, true).0;
    let calls = |text: &str| text.matches(" call ").count();
    // after main's pith_runtime_start: the alloc and the fast path's
    // cold fallback, against the alloc, the two checks and a plain
    // list_len call
    assert_eq!(calls(&plain), 3, "{plain}");
    assert_eq!(calls(&sanitized), 5, "{sanitized}");
    assert!(!sanitized.contains("can_move"), "{sanitized}");
}

/// Each branch `clif` takes on a struct's shared mark, in order: the body
/// of the block it goes to when the mark is set, and of the one it goes
/// to when it is clear.
fn shared_mark_branches(clif: &str) -> Vec<(String, String)> {
    let mut blocks: HashMap<String, String> = HashMap::new();
    let mut current = String::new();
    let mut marks = HashSet::new();
    let mut branches = Vec::new();
    let mask = format!(", {STRUCT_SHARED_MASK}");
    for line in clif.lines().map(str::trim) {
        if line.starts_with("block") && line.ends_with(':') {
            current = line.split(['(', ' ', ':']).next().unwrap_or_default().to_string();
            continue;
        }
        blocks.entry(current.clone()).or_default().push_str(&format!("{line}\n"));
        if let Some((value, op)) = line.split_once(" = ") {
            if op.starts_with("band_imm") && op.ends_with(&mask) {
                marks.insert(value.to_string());
            }
        }
        let parts: Vec<&str> = line.split([' ', ',']).filter(|p| !p.is_empty()).collect();
        if let ["brif", cond, set, clear] = parts[..] {
            if marks.contains(cond) {
                branches.push((set.to_string(), clear.to_string()));
            }
        }
    }
    branches
        .into_iter()
        .map(|(set, clear)| (blocks[&set].clone(), blocks[&clear].clone()))
        .collect()
}

/// The `call` of the runtime function `name` as `clif` writes it, up to
/// its arguments: `call fn2(`.
fn clif_call(clif: &str, funcs: &HashMap<String, FuncId>, name: &str) -> String {
    let declared = format!(" = u0:{} ", funcs[name].as_u32());
    let fn_ref = clif
        .lines()
        .map(str::trim)
        .find_map(|line| line.contains(&declared).then(|| line.split(' ').next().unwrap_or_default()))
        .unwrap_or_else(|| panic!("{name} is never declared in\n{clif}"));
    format!("call {fn_ref}(")
}

// a weak field read or store takes the runtime's locked call when its
// holder is marked shared, and only then: the lock is what keeps a store
// in another task from freeing the old target's header between a read's
// load and its upgrade. a local holder's field is read and upgraded, or
// swapped and its old target's weak count dropped, without the call.
#[test]
fn weak_field_calls_take_the_lock_on_a_shared_holder() {
    let ir = "func main 0 int\niconst 1 2\ncall 2 pith_struct_alloc int 1 1\n\
              iconst 3 8\ncall 4 pith_struct_weak_store_field void 3 2 3 2\n\
              call 5 pith_struct_weak_load_field int 2 2 3\nret 5\nendfunc\n";
    let (clif, funcs) = compile_main(ir, false);
    let call = |name: &str| clif_call(&clif, &funcs, name);
    let branches = shared_mark_branches(&clif);
    assert_eq!(branches.len(), 2, "{clif}");
    let (shared_store, local_store) = &branches[0];
    assert!(shared_store.contains(&call("pith_struct_weak_store_field")), "{clif}");
    assert!(!local_store.contains(&call("pith_struct_weak_store_field")), "{clif}");
    assert!(local_store.contains(&call("pith_struct_weak_release")), "{clif}");
    let (shared_load, local_load) = &branches[1];
    assert!(shared_load.contains(&call("pith_struct_weak_load_field")), "{clif}");
    assert!(!local_load.contains(&call("pith_struct_weak_load_field")), "{clif}");
    assert!(local_load.contains(&call("pith_struct_weak_upgrade")), "{clif}");

    // a build without the fast paths makes the runtime calls, which
    // test the mark themselves
    let (sanitized, funcs) = compile_main(ir, true);
    assert!(shared_mark_branches(&sanitized).is_empty(), "{sanitized}");
    assert!(sanitized.contains(&clif_call(&sanitized, &funcs, "pith_struct_weak_store_field")), "{sanitized}");
    assert!(sanitized.contains(&clif_call(&sanitized, &funcs, "pith_struct_weak_load_field")), "{sanitized}");
}

#[test]
fn bits_wrapper_detection_matches_only_the_exact_shape() {
    let params = vec!["left".to_string(), "right".to_string()];
    let wrapper = vec![
        "load 2 left",
        "load 3 right",
        "call 4 bit_and int 2 2 3",
        "ret 4",
        "iconst 5 0",
        "ret 5",
    ];
    let no_funcs = HashMap::new();
    let no_globals = HashMap::new();
    assert_eq!(detect_bits_wrapper(&params, &wrapper, &no_funcs, &no_globals), Some("band"));

    // swapped operands are a different function; it must keep its call
    let swapped = vec![
        "load 2 left",
        "load 3 right",
        "call 4 bit_and int 2 3 2",
        "ret 4",
    ];
    assert_eq!(detect_bits_wrapper(&params, &swapped, &no_funcs, &no_globals), None);

    // extra work after the return means the body is not just the builtin
    let with_extra = vec![
        "load 2 left",
        "load 3 right",
        "call 4 bit_and int 2 2 3",
        "ret 4",
        "call 6 print void 1 4",
    ];
    assert_eq!(detect_bits_wrapper(&params, &with_extra, &no_funcs, &no_globals), None);

    // a non-bit builtin never aliases
    let other_call = vec![
        "load 2 left",
        "load 3 right",
        "call 4 map_get int 2 2 3",
        "ret 4",
    ];
    assert_eq!(detect_bits_wrapper(&params, &other_call, &no_funcs, &no_globals), None);

    // a shadowed bit_and user function must never alias to the native instruction
    let mut shadowed_funcs = HashMap::new();
    shadowed_funcs.insert("bit_and".to_string(), FuncId::from_u32(1));
    assert_eq!(
        detect_bits_wrapper(&params, &wrapper, &shadowed_funcs, &no_globals),
        None
    );
}

#[test]
fn list_get_opt_scan_accepts_the_unpack_pattern_and_nothing_else() {
    // the emitter's indexed-read shape: call, flag read, value read,
    // release on both arms
    let body = vec![
        "call 10 pith_list_get_opt tuple 2 4 5",
        "field 11 10 0 bool is_some",
        "brif 11 L1 L2",
        "label L1",
        "field 12 10 8 int value",
        "call 13 pith_struct_release void 1 10",
        "jmp L3",
        "label L2",
        "call 14 pith_struct_release void 1 10",
        "ret 0",
        "label L3",
    ];
    let regs = scan_unpackable_opt_regs(&body);
    assert!(regs.contains(&10));

    // an escaping optional (stored into a variable) must keep the call
    let escaping = vec![
        "call 10 pith_list_get_opt tuple 2 4 5",
        "store opt 10",
        "field 11 10 0 bool is_some",
    ];
    assert!(scan_unpackable_opt_regs(&escaping).is_empty());

    // passing the tuple to any other call disqualifies it too
    let passed = vec![
        "call 10 pith_list_get_opt tuple 2 4 5",
        "field 11 10 0 bool is_some",
        "call 12 some_fn int 1 10",
    ];
    assert!(scan_unpackable_opt_regs(&passed).is_empty());
}

#[test]
fn a_pair_getter_unpacked_in_place_needs_no_shell() {
    let body = vec![
        "call 6 pith_channel_recv tuple 1 5",
        "optflag 8 6",
        "brif 8 L1 L2",
        "label L1",
        "optval 10 6 int value",
        "call 11 pith_struct_release void 1 6",
        "label L2",
    ];
    assert!(scan_unpackable_opt_regs(&body).contains(&6));

    // a destructor attached to the shell means it owns the payload
    let owned = vec![
        "call 6 map_get_opt tuple 2 4 5",
        "funcref 7 __opt_dtor_string",
        "call 8 pith_struct_set_dtor void 2 6 7",
        "optflag 9 6",
    ];
    assert!(scan_unpackable_opt_regs(&owned).is_empty());

    // a getter with no pair form keeps its call
    let other = vec!["call 6 env_opt tuple 1 5", "optflag 7 6"];
    assert!(scan_unpackable_opt_regs(&other).is_empty());
}

#[test]
fn parse_call_shape_requires_explicit_retkind() {
    let old = vec!["call", "7", "print", "1", "3"];
    let new = vec!["call", "8", "char_at", "string", "2", "1", "2"];
    let imported_struct = vec!["call", "9", "advance_token", "struct:Token", "0"];

    assert_eq!(parse_call_shape(&old), None);
    assert_eq!(parse_call_shape(&new), Some(("char_at", "string", 2, 5)));
    assert_eq!(
        parse_call_shape(&imported_struct),
        Some(("advance_token", "struct:Token", 0, 5))
    );
}

fn compile_err_for_ir(ir: &str) -> String {
    let mut codegen = crate::create_codegen().expect("create codegen");
    let runtime_funcs = HashMap::new();
    let result = compile_from_ir(&mut codegen, ir, &runtime_funcs);
    assert!(result.is_err(), "expected malformed IR to fail");
    result.err().expect("compile error").to_string()
}

fn compile_ok_for_ir(ir: &str) {
    let mut codegen = crate::create_codegen().expect("create codegen");
    let runtime_funcs = HashMap::new();
    let result = compile_from_ir(&mut codegen, ir, &runtime_funcs);
    assert!(result.is_ok(), "expected IR to compile, got {:?}", result.err());
}

// the same, with the runtime table declared, for ir that calls builtins.
fn compile_with_runtime_for_ir(ir: &str) -> Result<(), String> {
    let mut codegen = crate::create_codegen().expect("create codegen");
    let runtime_funcs = crate::runtime_imports::declare_runtime_functions(&mut codegen.module)
        .expect("declare runtime functions");
    compile_from_ir(&mut codegen, ir, &runtime_funcs)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[test]
fn push_return_types_uses_two_returns_for_result_abi() {
    let mut r = Vec::new();
    push_return_types(&mut r, "int");
    assert_eq!(r.len(), 1);

    let mut r = Vec::new();
    push_return_types(&mut r, "result_reg");
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].value_type, types::I64);
    assert_eq!(r[1].value_type, types::I64);

    let mut r = Vec::new();
    push_return_types(&mut r, "result_reg_f");
    assert_eq!(r.len(), 2);
    assert_eq!(r[1].value_type, types::F64);
}

#[test]
fn result_abi_ret2_and_rcall_compile() {
    // A result-abi function returns (is_ok, payload) via ret2; a caller
    // binds both with rcall. Arity mismatches would trip the verifier, so
    // a clean compile validates the two-return plumbing end to end.
    let ir = "func const_ok 0 result_reg\n\
              iconst 1 1\n\
              iconst 2 42\n\
              ret2 1 2\n\
              endfunc\n\
              func main 0 int\n\
              rcall 3 4 const_ok int 0\n\
              ret 4\n\
              endfunc\n";
    compile_ok_for_ir(ir);
}

#[test]
fn result_abi_float_payload_compiles() {
    let ir = "func const_okf 0 result_reg_f\n\
              iconst 1 1\n\
              fconst 2 2.5\n\
              ret2 1 2\n\
              endfunc\n\
              func main 0 int\n\
              rcall 3 4 const_okf float 0\n\
              ret 3\n\
              endfunc\n";
    compile_ok_for_ir(ir);
}

#[test]
fn a_cold_label_marks_its_block_cold() {
    use cranelift::codegen::ir::{Function, UserFuncName};
    let sig = Signature::new(cranelift::codegen::isa::CallConv::SystemV);
    let mut func = Function::with_name_signature(UserFuncName::default(), sig);
    let mut fb_ctx = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fb_ctx);
    let body = ["brif 1 L1 L2", "label L1", "ret 0", "label L2 cold", "ret 0"];
    let (labels, _) = create_label_blocks(&mut builder, &body);
    assert!(!builder.func.layout.is_cold(labels["L1"]));
    assert!(builder.func.layout.is_cold(labels["L2"]));
}

#[test]
fn a_label_takes_cold_and_nothing_else() {
    let ir = |flag: &str| {
        format!("func main 0 int\niconst 1 1\nbrif 1 L1 L2\nlabel L1\nret 1\nlabel L2{flag}\niconst 2 0\nret 2\nendfunc\n")
    };
    compile_ok_for_ir(&ir(" cold"));
    let err = compile_err_for_ir(&ir(" hot"));
    assert!(err.contains("a label takes only its name"), "{err}");
}

#[test]
fn count_calls_on_null_and_literals_are_noops() {
    use cranelift::codegen::ir::{Function, UserFuncName};
    let sig = Signature::new(cranelift::codegen::isa::CallConv::SystemV);
    let mut func = Function::with_name_signature(UserFuncName::default(), sig);
    let mut fb_ctx = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fb_ctx);
    let block = builder.create_block();
    builder.switch_to_block(block);
    let null = builder.ins().iconst(types::I64, 0);
    let five = builder.ins().iconst(types::I64, 5);
    let literal = builder.ins().iadd_imm(five, 16);
    let literals: HashSet<Value> = [literal].into_iter().collect();
    let f = &*builder.func;
    assert!(is_noop_count_call("pith_cstring_release", null, &literals, f));
    assert!(is_noop_count_call("pith_list_release_handle", null, &literals, f));
    assert!(is_noop_count_call("pith_cstring_retain", literal, &literals, f));
    // a literal is not a list, and a nonzero constant is not null
    assert!(!is_noop_count_call("pith_list_release_handle", literal, &literals, f));
    assert!(!is_noop_count_call("pith_cstring_release", five, &literals, f));
    assert!(!is_noop_count_call("pith_list_retain", null, &literals, f));
}

#[test]
fn rcall_reaches_a_runtime_pair_builtin() {
    // parse_int and parse_float return their (is_ok, payload) pair in two
    // registers; rcall resolves them from the runtime table, the float
    // payload arriving as bits in an integer word.
    let ir = "string 0 \"12\"\n\
              func main 0 int\n\
              strref 1 0\n\
              rcall 2 3 parse_int int 1 1\n\
              rcall 4 5 parse_float float 1 1\n\
              ret 3\n\
              endfunc\n";
    let result = compile_with_runtime_for_ir(ir);
    assert!(result.is_ok(), "expected IR to compile, got {result:?}");
}

#[test]
fn a_plain_call_to_a_runtime_pair_builtin_is_an_error() {
    let err = compile_with_runtime_for_ir(
        "string 0 \"12\"\nfunc main 0 int\nstrref 1 0\ncall 2 parse_int tuple 1 1\nret 2\nendfunc\n",
    )
    .expect_err("a plain call to a pair builtin must not compile");
    assert!(err.contains("must be an rcall"), "{err}");
}

#[test]
fn rcall_to_a_single_return_function_is_an_error() {
    let err = compile_with_runtime_for_ir(
        "string 0 \"12\"\nfunc main 0 int\nstrref 1 0\nrcall 2 3 ord int 1 1\nret 2\nendfunc\n",
    )
    .expect_err("an rcall to a single-return builtin must not compile");
    assert!(err.contains("does not return a register pair"), "{err}");
}

#[test]
fn a_function_named_after_a_runtime_symbol_is_an_error() {
    // one signature matches the runtime's and one does not. without the
    // check the first replaces the runtime function and the second is
    // dropped in favour of it.
    for decl in ["func pith_list_get_value 2 int", "func pith_list_set_value 3 int"] {
        let err = compile_with_runtime_for_ir(&format!("{decl}\niconst 1 0\nret 1\nendfunc\n"))
            .expect_err("a function named after a runtime symbol must not compile");
        assert!(err.contains("has the name of a runtime symbol"), "{err}");
    }
}

#[test]
fn invalid_register_reference_returns_compile_error() {
    let err = compile_err_for_ir("func main 0 int\niconst 1 1\nadd 2 nope 1\nendfunc\n");
    assert!(err.contains("invalid register reference 'nope'"));
}

#[test]
fn missing_register_reference_returns_compile_error() {
    let err = compile_err_for_ir("func main 0 int\niconst 1 1\nadd 2 1 99\nendfunc\n");
    assert!(err.contains("missing register 99"));
}

#[test]
fn invalid_destination_register_returns_compile_error() {
    let err = compile_err_for_ir("func main 0 int\niconst nope 1\nendfunc\n");
    assert!(err.contains("invalid destination register 'nope'"));
}

#[test]
fn unknown_instruction_returns_compile_error() {
    let err = compile_err_for_ir("func main 0 int\nsurprise 1 2 3\nendfunc\n");
    assert!(err.contains("unknown or malformed instruction"));
}

#[test]
fn count_marks_are_skipped_anywhere() {
    compile_ok_for_ir(
        "mark global __g_x string\nfunc main 0 int\nparam p\nmark kind p string\niconst 1 0\nmark owns s string\nret 1\nmark mixed t\nendfunc\n",
    );
    // only the whole word: `marked` is still an unknown instruction
    let err = compile_err_for_ir("func main 0 int\nmarked 1\nendfunc\n");
    assert!(err.contains("unknown or malformed instruction"));
}

#[test]
fn malformed_store_returns_compile_error() {
    let err = compile_err_for_ir("func main 0 int\niconst 17 1\nstore  17\nendfunc\n");
    assert!(err.contains("unknown or malformed instruction"));
}

#[test]
fn call_to_unknown_function_returns_compile_error() {
    let err = compile_err_for_ir("func main 0 int\ncall 1 vanished int 0\nendfunc\n");
    assert!(err.contains("call to unknown function 'vanished'"));
}

// with the runtime declared, a call nothing resolves used to compile to
// a runtime error trap. a function main reaches fails the build, naming
// the callee and the caller.
#[test]
fn live_unknown_call_with_runtime_is_a_compile_error() {
    let err = compile_with_runtime_for_ir(
        "func main 0 int\ncall 1 helper int 0\nret 1\nendfunc\n\
         func helper 0 int\ncall 1 pith_no_such_runtime_fn int 0\nret 1\nendfunc\n",
    )
    .expect_err("a live unknown call must not build");
    assert!(
        err.contains("call to unknown function 'pith_no_such_runtime_fn' in helper"),
        "{err}"
    );
}

// a body nothing reaches, like the unspecialized template of a generic
// method, is not compiled, so what it calls does not matter.
#[test]
fn unreachable_function_with_unknown_call_is_skipped() {
    compile_with_runtime_for_ir(
        "func main 0 int\niconst 1 0\nret 1\nendfunc\n\
         func Box__show 1 string\nparam self\nload 1 self\n\
         call 2 to_string string 1 1\nret 2\nendfunc\n",
    )
    .expect("an unreachable body must not fail the build");
}

// reachability follows every name a body mentions, a funcref included,
// and starts from the startup functions main's prologue calls.
#[test]
fn functions_reached_by_reference_and_startup_stay_live() {
    let ir = "func main 0 int\nfuncref 1 cb\nret 1\nendfunc\n\
              func cb 0 int\ncall 1 deep int 0\nret 1\nendfunc\n\
              func deep 0 int\niconst 1 0\nret 1\nendfunc\n\
              func __init_globals 0 int\niconst 1 0\nret 1\nendfunc\n\
              func m___cc_register_tracers_m0 0 int\niconst 1 0\nret 1\nendfunc\n\
              func dead 0 int\ncall 1 deader int 0\nret 1\nendfunc\n\
              func deader 0 int\niconst 1 0\nret 1\nendfunc\n";
    let lines: Vec<&str> = ir.lines().collect();
    let live = live_functions(&ir_functions(&lines)).expect("main is present");
    let mut names: Vec<&str> = live.iter().map(String::as_str).collect();
    names.sort();
    assert_eq!(
        names,
        ["__init_globals", "cb", "deep", "m___cc_register_tracers_m0", "main"]
    );
    assert!(live_functions(&ir_functions(&["func helper 0 int", "endfunc"])).is_none());
}

#[test]
fn cc_tracer_registration_keeps_only_live_destructors() {
    let ir = "func main 0 int\nfuncref 1 __dtor_Used\nret 1\nendfunc\n\
              func __dtor_Used 1 void\nparam p\nret 0\nendfunc\n\
              func __cc_trace_Used 1 void\nparam p\nret 0\nendfunc\n\
              func __dtor_Unused 1 void\nparam p\nret 0\nendfunc\n\
              func __cc_trace_Unused 1 void\nparam p\nret 0\nendfunc\n\
              func m___cc_register_tracers_m0 0 int\n\
              funcref 1 __dtor_Used\n\
              funcref 2 __cc_trace_Used\n\
              call 3 pith_cc_register_tracer void 2 1 2\n\
              funcref 4 __dtor_Unused\n\
              funcref 5 __cc_trace_Unused\n\
              call 6 pith_cc_register_tracer void 2 4 5\n\
              funcref 7 __dtor_Unused\n\
              call 8 pith_cc_register_user_drop void 1 7\n\
              iconst 9 0\n\
              ret 9\n\
              endfunc\n";
    let lines: Vec<&str> = ir.lines().collect();
    let funcs = ir_functions(&lines);
    let live = live_functions(&funcs).expect("main is present");
    assert!(live.contains("__dtor_Used"));
    assert!(live.contains("__cc_trace_Used"));
    assert!(!live.contains("__dtor_Unused"));
    assert!(!live.contains("__cc_trace_Unused"));
    let reg = funcs
        .iter()
        .find(|f| f.name == "m___cc_register_tracers_m0")
        .unwrap();
    let filtered = filter_tracer_reg_body(&reg.body, &live);
    assert_eq!(filtered.len(), 5);
    assert!(filtered[0].contains("__dtor_Used"));
    assert!(filtered[1].contains("__cc_trace_Used"));
    assert!(filtered[2].contains("pith_cc_register_tracer"));
}

#[test]
fn dead_strings_pruned_and_duplicate_live_strings_compile() {
    compile_with_runtime_for_ir(
        "string m0s0 \"shared\"\n\
         string m1s0 \"shared\"\n\
         string m2s0 \"dead string never used\"\n\
         global g_dead 42\n\
         global g_dead_str str:m2s0\n\
         func main 0 int\n\
         strref 1 m0s0\n\
         strref 2 m1s0\n\
         iconst 3 0\n\
         ret 3\n\
         endfunc\n",
    )
    .expect("live deduplicated strings and pruned dead globals should compile cleanly");
}

// every IR-declared function returns i64, so only the runtime table can
// supply a callee that returns nothing, which is exactly where the
// emitter's idea of a call's result and the runtime's can drift apart.
#[test]
fn call_asking_a_value_of_a_void_runtime_function_returns_compile_error() {
    let mut codegen = crate::create_codegen().expect("create codegen");
    let mut sig = codegen.module.make_signature();
    sig.params.push(AbiParam::new(types::I64));
    let func_id = codegen
        .module
        .declare_function("pith_test_void_sink", Linkage::Import, &sig)
        .expect("declare void runtime function");
    let mut runtime_funcs = HashMap::new();
    runtime_funcs.insert("void_sink".to_string(), func_id);

    let result = compile_from_ir(
        &mut codegen,
        "func main 0 int\niconst 1 0\ncall 2 void_sink int 1 1\nret 2\nendfunc\n",
        &runtime_funcs,
    );
    let err = result
        .err()
        .expect("expected a value-returning call to a void runtime function to fail")
        .to_string();
    assert!(err.contains("asks for a int result, but the function returns nothing"));
}

#[test]
fn unknown_branch_label_returns_compile_error() {
    let err =
        compile_err_for_ir("func main 0 int\niconst 1 1\nbrif 1 then_l else_l\nendfunc\n");
    assert!(err.contains("unknown label 'then_l'"));
}

#[test]
fn unknown_jump_label_returns_compile_error() {
    let err = compile_err_for_ir("func main 0 int\njmp missing_l\nendfunc\n");
    assert!(err.contains("unknown label 'missing_l'"));
}

#[test]
fn malformed_integer_constant_returns_compile_error() {
    let err = compile_err_for_ir("func main 0 int\niconst 1 notanumber\nendfunc\n");
    assert!(err.contains("invalid integer constant 'notanumber'"));
}

#[test]
fn malformed_float_constant_returns_compile_error() {
    let err = compile_err_for_ir("func main 0 int\nfconst 1 notafloat\nendfunc\n");
    assert!(err.contains("invalid float constant 'notafloat'"));
}

#[test]
fn invalid_parameter_count_returns_compile_error() {
    let err = compile_err_for_ir("func main x int\nendfunc\n");
    assert!(err.contains("invalid parameter count 'x'"));
}

#[test]
fn concat_chain_does_not_fuse_across_mutating_call_or_non_string_release() {
    // C-06: `h.name + " " + mutate(h)` must not defer reading `h.name` past `mutate(h)`.
    let mutated = "
        field 1 10 0 string name
        strref 2 m0s0
        concat 3 1 2
        call 4 mutate string 1 10
        concat 5 3 4
        call 6 pith_cstring_release void 1 3
        call 7 pith_cstring_release void 1 4
        ret 5
    ";
    assert!(fuse(mutated).is_none());

    // Releasing a temporary struct between concats can free a borrowed field string.
    let temp_struct = "
        field 1 10 0 string name
        strref 2 m0s0
        concat 3 1 2
        call 4 pith_struct_release void 1 10
        load 5 b
        concat 6 3 5
        call 7 pith_cstring_release void 1 3
        ret 6
    ";
    assert!(fuse(temp_struct).is_none());

    // A user-defined function shadowing `chr` acts as a barrier too.
    let lines: Vec<&str> = CHAIN.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let mut declared = HashMap::new();
    declared.insert("chr".to_string(), FuncId::from_u32(1));
    // `chr` runs before the first concat in CHAIN, so add one between concats:
    let shadowed_chain = "
        load 1 a
        strref 2 m0s0
        concat 3 1 2
        iconst 4 65
        call 5 chr string 1 4
        concat 6 3 5
        call 7 pith_cstring_release void 1 3
        call 8 pith_cstring_release void 1 5
        ret 6
    ";
    let slines: Vec<&str> = shadowed_chain
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert!(fuse_concat_chains(&slines, &[], &declared, &HashMap::new()).is_none());
    assert!(fuse_concat_chains(&lines, &[], &HashMap::new(), &HashMap::new()).is_some());
}

#[test]
fn self_append_skips_module_prefixed_globals() {
    // C-07: imported module globals are named `<mod>___g_<name>`, not `__g_<name>`.
    let lines: Vec<String> = SELF_APPEND
        .replace(" s", " other___g_s")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let mut globals = HashMap::new();
    globals.insert("other___g_s".to_string(), DataId::from_u32(1));
    assert!(fuse_self_appends(&refs, &globals).is_none());
}

#[test]
fn scan_float_vars_does_not_poison_int_lambda_params() {
    // C-16: `fn(n: Int) -> Float: k := n * 2; return k.to_float() + 0.5`
    let params = vec!["__closure".to_string(), "n".to_string()];
    let kinds = vec![String::new(), String::new()];
    let body = vec![
        "load 1 n",
        "iconst 2 2",
        "mulc 3 1 2",
        "store k 3",
        "load 4 k",
        "call 5 int_to_float float 1 4",
        "fconst 6 0.5",
        "addc 7 5 6",
        "ret 7",
    ];
    let floats = scan_float_vars(&params, &kinds, &body);
    assert!(!floats.contains("__closure"));
    assert!(!floats.contains("n"));
    assert!(!floats.contains("k"));

    // A genuinely Float unannotated param paired with a float op IS inferred.
    let float_params = vec!["__closure".to_string(), "x".to_string(), "n".to_string()];
    let float_kinds = vec![String::new(), String::new(), String::new()];
    let mixed_body = vec![
        "load 1 x",
        "fconst 2 1.0",
        "addc 3 1 2",
        "store y 3",
        "load 4 n",
        "iconst 5 2",
        "mulc 6 4 5",
        "store k 6",
        "ret 3",
    ];
    let mixed_floats = scan_float_vars(&float_params, &float_kinds, &mixed_body);
    assert!(mixed_floats.contains("x"));
    assert!(mixed_floats.contains("y"));
    assert!(!mixed_floats.contains("n"));
    assert!(!mixed_floats.contains("k"));
}

#[test]
fn fuse_int_parts_respects_shadowed_int_to_string() {
    let lines: Vec<&str> = INT_CONCAT
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut declared = HashMap::new();
    declared.insert("int_to_string".to_string(), FuncId::from_u32(1));
    assert!(fuse_int_parts(&lines, &[], &declared, &HashMap::new()).is_none());

    let param_shadow = vec!["int_to_string".to_string()];
    assert!(fuse_int_parts(&lines, &param_shadow, &HashMap::new(), &HashMap::new()).is_none());
}

#[test]
fn rcall_float_abi_with_non_float_payload_kind_compiles() {
    // M-12: rcall on a result_reg_f callee when the IR's payload kind is non-float
    // must bitcast F64 -> I64 instead of passing an F64 to integer consumers.
    let ir = "\
func make_f 0 result_reg_f
fconst 1 1.5
iconst 2 1
ret2 2 1
endfunc
func main 0 int
rcall 1 2 make_f int 0
iconst 3 1
add 4 2 3
ret 4
endfunc
";
    compile_ok_for_ir(ir);
}

#[test]
fn parse_i64_operand_accepts_full_u64_and_i64_min_literals() {
    assert_eq!(
        parse_i64_operand("0xFFFFFFFFFFFFFFFF", "constant", "iconst", "main").unwrap(),
        -1i64
    );
    assert_eq!(
        parse_i64_operand("18446744073709551615", "constant", "iconst", "main").unwrap(),
        -1i64
    );
    assert_eq!(
        parse_i64_operand("-0x8000000000000000", "constant", "iconst", "main").unwrap(),
        i64::MIN
    );
    assert!(parse_i64_operand("-0x8000000000000001", "constant", "iconst", "main").is_err());
}

#[test]
fn malformed_offsets_and_tuple_stack_sizes_return_compile_error() {
    let neg_tuple = "func main 0 int\ntuple_stack 1 -1\niconst 2 0\nret 2\nendfunc\n";
    let huge_tuple = "func main 0 int\ntuple_stack 1 1000000000\niconst 2 0\nret 2\nendfunc\n";
    let neg_sstore = "func main 0 int\niconst 1 0\niconst 2 0\nsstore 1 -1 2\nret 2\nendfunc\n";
    let huge_sstore = "func main 0 int\niconst 1 0\niconst 2 0\nsstore 1 536870912 2\nret 2\nendfunc\n";
    let neg_field = "func main 0 int\niconst 1 0\nfield 2 1 -8 int x\nret 2\nendfunc\n";
    let huge_field = "func main 0 int\niconst 1 0\nfield 2 1 3000000000 int x\nret 2\nendfunc\n";
    for ir in [neg_tuple, huge_tuple, neg_sstore, huge_sstore, neg_field, huge_field] {
        let _ = compile_err_for_ir(ir);
    }
}

#[test]
fn closure_call_respects_fast_paths_and_sanitizes_global_closure() {
    let mut codegen = crate::create_codegen().expect("create codegen");
    let runtime_funcs =
        crate::runtime_imports::declare_runtime_functions(&mut codegen.module)
            .expect("declare runtime functions");
    let mut declare = |name: &str| {
        let mut sig = codegen.module.make_signature();
        sig.params.push(AbiParam::new(types::I64));
        codegen
            .module
            .declare_function(name, Linkage::Import, &sig)
            .expect("declare sanitizer hook")
    };
    let tsan_read = declare("__tsan_read8");
    let tsan_write = declare("__tsan_write8");
    let hooks = Some(SanitizeHooks {
        read: tsan_read,
        write: tsan_write,
    });
    let g_id = codegen
        .module
        .declare_data("__g_cb", Linkage::Local, true, false)
        .expect("declare global");
    let mut globals = HashMap::new();
    globals.insert("__g_cb".to_string(), g_id);
    let lines = vec!["call 2 __g_cb int 0", "ret 2"];
    let empty_funcs = HashMap::new();
    let empty_lits = HashMap::new();
    let empty_structs = HashSet::new();
    let empty_str_globals = HashSet::new();
    let empty_bits = HashMap::new();
    let mctx = ModuleCtx {
        runtime_funcs: &runtime_funcs,
        declared_funcs: &empty_funcs,
        string_lits: &empty_lits,
        struct_names: &empty_structs,
        global_data: &globals,
        str_globals: &[],
        string_global_names: &empty_str_globals,
        preempt_flag: None,
        sanitize: hooks,
        quarantine: false,
        fast_paths: hooks.is_none(),
        bits_aliases: &empty_bits,
    };
    let func = compile_ir_function(&mut codegen, &mctx, "main", "int", &[], &[], &lines)
        .expect("compile");
    let sanitized = func.display().to_string();
    assert!(
        sanitized.contains(&clif_call(&sanitized, &runtime_funcs, "pith_closure_get_fn")),
        "expected runtime pith_closure_get_fn when fast_paths=false:\n{sanitized}"
    );
    let read_decl = format!(" = u0:{} ", tsan_read.as_u32());
    assert!(
        sanitized.contains(&read_decl),
        "expected __tsan_read8 hook on global closure load:\n{sanitized}"
    );
}

#[test]
fn malformed_rcall_missing_args_returns_compile_error() {
    let ir = "
func callee 2 result_reg
param a
param b
iconst 1 1
iconst 2 42
ret2 1 2
endfunc
func main 0 int
iconst 1 7
rcall 2 3 callee int 2 1
ret 3
endfunc
";
    let mut codegen = crate::create_codegen().expect("codegen");
    let runtime = crate::declare_runtime_functions(&mut codegen.module).expect("runtime");
    let err = compile_from_ir(&mut codegen, ir, &runtime)
        .expect_err("rcall with fewer args than nargs must return an error, not panic");
    assert!(err.to_string().contains("rcall"), "{err}");
}
