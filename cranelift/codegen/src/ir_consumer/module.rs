//! Module-level IR compilation driver: Pass 1/1.5/2 orchestration, `main`
//! startup prologue, post-lowering small-function inlining, loop header
//! hoisting, and verifier error formatting.

use super::dce::{collect_live_globals_and_strings, filter_tracer_reg_body, live_functions};
use super::fast_paths::string_lit_addr;
use super::instrumentation::{green_preempt_enabled, quarantine_build, sanitize_hooks};
use super::ir::{
    decode_string_literal, ir_functions, is_count_mark, parse_i64_operand, push_return_types,
};
use super::lower::{compile_ir_function, ModuleCtx};
use super::passes::scans::detect_bits_wrapper;
use super::passes::string_fusion::{fuse_concat_chains, fuse_int_parts, fuse_self_appends};
use crate::{loop_hoist, CodeGen, CompileError};
use cranelift::codegen::ir::{FuncRef, Function};
use cranelift::prelude::*;
use cranelift_module::{DataId, FuncId, Linkage, Module};
use std::collections::{HashMap, HashSet};

// the count checker's findings on stderr. the compiler only reports them;
// a test or a tooling pass reads the lines and decides whether to fail.
fn report_counts(report: &crate::count_check::CountReport) {
    eprintln!("{}", report.summary());
    for p in &report.problems {
        eprintln!("  unbalanced: {p}");
    }
    if !report.undecided_reasons.is_empty() {
        let reasons: Vec<String> = report
            .undecided_reasons
            .iter()
            .map(|(r, n)| format!("{n} {r}"))
            .collect();
        eprintln!("  undecided: {}", reasons.join(", "));
    }
    if std::env::var("PITH_CHECK_COUNTS").ok().as_deref() == Some("2") {
        for d in &report.join_details {
            eprintln!("  join: {d}");
        }
        for u in &report.undecided_names {
            eprintln!("  undecided fn: {u}");
        }
    }
}

/// Compile IR text to native code via Cranelift.
pub fn compile_from_ir(
    codegen: &mut CodeGen,
    ir_text: &str,
    runtime_funcs: &HashMap<String, FuncId>,
) -> Result<HashMap<String, FuncId>, CompileError> {
    // the count checker reads the marks, so it runs before they are dropped
    let count_mode = std::env::var("PITH_CHECK_COUNTS").unwrap_or_default();
    if matches!(count_mode.as_str(), "1" | "2" | "only") {
        let raw: Vec<&str> = ir_text.lines().collect();
        let (report, loop_problems, untagged_problems) = crate::count_check::check_all(&raw);
        report_counts(&report);
        for p in loop_problems {
            eprintln!("  borrow: {p}");
        }
        for p in untagged_problems {
            eprintln!("  borrow: {p}");
        }
        // `only` is for tools that want the report and nothing else, such as
        // the mutation run that checks the checker
        if count_mode == "only" {
            return Err(CompileError::ModuleError(
                "stopped after the count check (PITH_CHECK_COUNTS=only)".into(),
            ));
        }
    }
    // `mark` lines are notes for the count checker and generate nothing. they
    // go before any pass reads the text, so they can't split a pattern the
    // rewrites below look for and change the code (see docs/ir-contract.md).
    let lines: Vec<&str> = ir_text.lines().filter(|line| !is_count_mark(line)).collect();
    let functions = ir_functions(&lines);
    let live = live_functions(&functions);
    let is_live = |name: &str| live.as_ref().map_or(true, |set| set.contains(name));
    let (live_globals, mut live_strings) =
        collect_live_globals_and_strings(&functions, live.as_ref());

    let mut declared_funcs: HashMap<String, FuncId> = HashMap::new();
    let mut string_data: Vec<(String, String)> = Vec::new();
    // the struct names (and aliases) the IR declares: a call to one is its
    // constructor, and a load of one is a zero
    let mut struct_names: HashSet<String> = HashSet::new();
    let mut global_data: HashMap<String, DataId> = HashMap::new();
    let mut str_globals: Vec<(String, String)> = Vec::new(); // (global_name, string_id)
    let mut string_global_names: HashSet<String> = HashSet::new();

    // green preemption safe-points are emitted by default. `PITH_GREEN_PREEMPT=0`
    // is the opt-out for a build that will never run green.
    let preempt_flag = if green_preempt_enabled() {
        Some(
            codegen
                .module
                .declare_data("PITH_PREEMPT_REQUESTED", Linkage::Import, false, false)
                .map_err(|e| CompileError::ModuleError(e.to_string()))?,
        )
    } else {
        None
    };
    let sanitize = sanitize_hooks(codegen)?;
    // Compute quarantine mode once per module instead of calling `env::var` per function.
    let quarantine = quarantine_build();
    let fast_paths = sanitize.is_none() && !quarantine;

    // Pass 1: collect string data and declare globals & functions
    for line in &lines {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        match parts[0] {
            "string" if parts.len() >= 3 => {
                string_data.push((parts[1].to_string(), decode_string_literal(line)));
            }
            "struct" if parts.len() >= 2 => {
                struct_names.insert(parts[1].to_string());
            }
            "global" if parts.len() >= 3 => {
                let gname = parts[1].to_string();
                let global_is_live = live_globals
                    .as_ref()
                    .map_or(true, |set| set.contains(&gname));
                if global_is_live && !global_data.contains_key(&gname) {
                    let init_kind = parts[2];
                    use cranelift_module::DataDescription;
                    // Rename global if it conflicts with a function name
                    let data_name = if declared_funcs.contains_key(&gname) {
                        format!("__g_{}", gname)
                    } else {
                        gname.clone()
                    };
                    let data_id = codegen
                        .module
                        .declare_data(&data_name, Linkage::Local, true, false)
                        .map_err(|e| CompileError::ModuleError(e.to_string()))?;
                    let mut desc = DataDescription::new();
                    let init_val: i64 = if init_kind == "list"
                        || init_kind == "map"
                        || init_kind == "set"
                        || init_kind == "set_int"
                    {
                        0
                    } else if init_kind.starts_with("str:") {
                        0 // will be patched in __init_globals
                    } else {
                        parse_i64_operand(init_kind, "global initializer", line, &gname)?
                    };
                    desc.define(init_val.to_le_bytes().to_vec().into_boxed_slice());
                    codegen
                        .module
                        .define_data(data_id, &desc)
                        .map_err(|e| CompileError::ModuleError(e.to_string()))?;
                    global_data.insert(gname.clone(), data_id);
                    if init_kind.starts_with("str:") {
                        let str_id = &init_kind[4..]; // e.g., "m0s0"
                        if let Some(ref mut s_used) = live_strings {
                            s_used.insert(str_id.to_string());
                        }
                        str_globals.push((gname.clone(), str_id.to_string()));
                        string_global_names.insert(gname);
                    }
                }
            }
            "struct_alias" if parts.len() >= 3 => {
                if struct_names.contains(parts[2]) {
                    struct_names.insert(parts[1].to_string());
                }
            }
            "func" if parts.len() >= 4 => {
                let name = parts[1];
                if !declared_funcs.contains_key(name) && is_live(name) {
                    let nparam: usize = parts[2].parse().map_err(|_| {
                        CompileError::ModuleError(format!(
                            "IR consumer: invalid parameter count '{}' in function '{}'",
                            parts[2], name
                        ))
                    })?;
                    let mut sig = codegen.module.make_signature();
                    for _ in 0..nparam {
                        sig.params.push(AbiParam::new(types::I64));
                    }
                    push_return_types(&mut sig.returns, parts.get(3).copied().unwrap_or(""));
                    let linkage = if name == "main" {
                        Linkage::Export
                    } else {
                        Linkage::Local
                    };
                    if codegen.module.get_name(name).is_some() {
                        return Err(CompileError::ModuleError(format!(
                            "function '{name}' has the name of a runtime symbol; rename it"
                        )));
                    }
                    let func_id = codegen
                        .module
                        .declare_function(name, linkage, &sig)
                        .map_err(|e| CompileError::ModuleError(e.to_string()))?;
                    declared_funcs.insert(name.to_string(), func_id);
                }
            }
            _ => {}
        }
    }

    // Declare live string literal data, deduplicating identical static `.rodata`
    // payloads across modules (literals are immutable with static magic).
    let mut string_lits: HashMap<String, DataId> = HashMap::new();
    let mut content_to_data: HashMap<&str, DataId> = HashMap::new();
    for (idx, content) in &string_data {
        if let Some(ref s_used) = live_strings {
            if !s_used.contains(idx.as_str()) {
                continue;
            }
        }
        if !string_lits.contains_key(idx) {
            if let Some(&existing_id) = content_to_data.get(content.as_str()) {
                string_lits.insert(idx.clone(), existing_id);
            } else {
                let name = format!("__irstr_{}", idx);
                let data_id = crate::declare_string_data(&mut codegen.module, &name, content)
                    .map_err(|e| CompileError::ModuleError(format!("string data: {:?}", e)))?;
                content_to_data.insert(content.as_str(), data_id);
                string_lits.insert(idx.clone(), data_id);
            }
        }
    }

    // Pass 1.5: find bit-wrapper functions (std.bits and anything shaped like
    // it) whose calls can lower to a single native instruction. This must see
    // every function before any body compiles, since callers usually precede
    // the wrappers in the stream.
    let mut bits_aliases: HashMap<String, &'static str> = HashMap::new();
    for function in &functions {
        if let Some(op) = detect_bits_wrapper(
            &function.params,
            &function.body,
            &declared_funcs,
            &global_data,
        ) {
            bits_aliases.entry(function.name.to_string()).or_insert(op);
        }
    }

    let module_ctx = ModuleCtx {
        runtime_funcs,
        declared_funcs: &declared_funcs,
        string_lits: &string_lits,
        struct_names: &struct_names,
        global_data: &global_data,
        str_globals: &str_globals,
        string_global_names: &string_global_names,
        preempt_flag,
        sanitize,
        quarantine,
        fast_paths,
        bits_aliases: &bits_aliases,
    };

    // Pass 2: compile function bodies (first definition wins for duplicates)
    let mut compiled_funcs: HashSet<String> = HashSet::new();
    let mut built: Vec<(FuncId, String, Function)> = Vec::new();
    for function in &functions {
        let func_name = function.name.to_string();
        let func_retkind = function.retkind.to_string();
        let param_names = &function.params;
        let param_kinds = &function.param_kinds;
        let body_lines: Vec<&str> = match &live {
            Some(live_set) if func_name.contains("__cc_register_tracers") => {
                filter_tracer_reg_body(&function.body, live_set)
            }
            _ => function.body.clone(),
        };

        // Compile this function (skip if already compiled from an earlier module)
        if compiled_funcs.contains(&func_name) {
            continue;
        }
        compiled_funcs.insert(func_name.clone());
        // Pre-lowering fusion order MUST stay:
        // fuse_concat_chains -> fuse_self_appends -> fuse_int_parts.
        let fused = fuse_concat_chains(&body_lines, param_names, &declared_funcs, &global_data);
        let body_lines: Vec<&str> = match &fused {
            Some(lines) => lines.iter().map(String::as_str).collect(),
            None => body_lines,
        };
        let appended = fuse_self_appends(&body_lines, &global_data);
        let body_lines: Vec<&str> = match &appended {
            Some(lines) => lines.iter().map(String::as_str).collect(),
            None => body_lines,
        };
        let with_ints = fuse_int_parts(&body_lines, param_names, &declared_funcs, &global_data);
        let body_lines: Vec<&str> = match &with_ints {
            Some(lines) => lines.iter().map(String::as_str).collect(),
            None => body_lines,
        };
        if let Some(&func_id) = declared_funcs.get(&func_name) {
            let func = compile_ir_function(
                codegen,
                &module_ctx,
                &func_name,
                &func_retkind,
                param_names,
                param_kinds,
                &body_lines,
            )?;
            built.push((func_id, func_name, func));
        }
    }

    let keep_calls: Vec<u32> = LOOP_KEEP_CALLS
        .iter()
        .filter_map(|name| runtime_funcs.get(*name))
        .map(|id| id.as_u32())
        .collect();
    define_with_inlining(codegen, built, &keep_calls)?;
    Ok(declared_funcs)
}

/// The start of `main`: `pith_runtime_start`, which arms the stack overflow
/// report, then the static string globals (so any global initializer sees a
/// valid pointer and any `mut` write survives), then every module's tracer
/// registration, then each unprefixed `__init_globals_N`, then the main
/// module's `__init_globals` (skipped when `main`'s body already calls it).
pub(super) fn emit_main_prologue(
    codegen: &mut CodeGen,
    builder: &mut FunctionBuilder<'_>,
    ctx: &ModuleCtx<'_>,
    body_lines: &[&str],
) {
    if let Some(&start_id) = ctx.runtime_funcs.get("pith_runtime_start") {
        let start_ref = codegen
            .module
            .declare_func_in_func(start_id, builder.func);
        builder.ins().call(start_ref, &[]);
    }
    // Initialize static str: globals FIRST so any global initializer reading
    // them sees a valid pointer and any `mut` string global keeps writes made
    // during `__init_globals`.
    for (gname, str_id) in ctx.str_globals {
        if let (Some(&g_data_id), Some(&s_data_id)) =
            (ctx.global_data.get(gname), ctx.string_lits.get(str_id))
        {
            let s_addr = string_lit_addr(codegen, builder, s_data_id);
            let g_gv = codegen
                .module
                .declare_data_in_func(g_data_id, builder.func);
            let g_addr = builder.ins().global_value(types::I64, g_gv);
            builder.ins().store(MemFlags::new(), s_addr, g_addr, 0);
        }
    }
    let mut tracer_regs: Vec<(&str, FuncId)> = ctx
        .declared_funcs
        .iter()
        .filter(|(name, _)| name.contains("__cc_register_tracers"))
        .map(|(name, &id)| (name.as_str(), id))
        .collect();
    tracer_regs.sort_by_key(|(name, _)| (name.len(), *name));
    for (_, fid) in tracer_regs {
        let fref = codegen.module.declare_func_in_func(fid, builder.func);
        builder.ins().call(fref, &[]);
    }
    let mut init_funcs: Vec<(&str, FuncId)> = ctx
        .declared_funcs
        .iter()
        .filter(|(name, _)| name.starts_with("__init_globals_"))
        .map(|(name, &id)| (name.as_str(), id))
        .collect();
    init_funcs.sort_by_key(|(name, _)| (name.len(), *name));
    for (_, fid) in init_funcs {
        let fref = codegen.module.declare_func_in_func(fid, builder.func);
        builder.ins().call(fref, &[]);
    }
    let body_calls_init = body_lines
        .iter()
        .any(|line| line.split_whitespace().nth(2) == Some("__init_globals"));
    if !body_calls_init {
        if let Some(&init_id) = ctx.declared_funcs.get("__init_globals") {
            let init_ref = codegen.module.declare_func_in_func(init_id, builder.func);
            builder.ins().call(init_ref, &[]);
        }
    }
}

/// Callees with more instructions than this keep their call.
const INLINE_MAX_INSTS: usize = 24;

/// Runtime functions a loop may call and still have its list header reads hoisted.
const LOOP_KEEP_CALLS: &[&str] = &[
    "pith_green_maybe_yield",
    "pith_list_get_value_strict",
    "pith_list_len",
    "pith_list_set_scalar",
    "pith_list_set_scalar_strict",
    "pith_list_set_scalar_checked",
    "byte_at",
    "string_len",
    "pith_cstring_eq",
    "pith_cstring_compare",
    "pith_cstring_lt",
    "pith_cstring_gt",
    "pith_cstring_lte",
    "pith_cstring_gte",
];

/// Define every built function, first inlining the small functions it calls,
/// then hoisting loop-invariant list header reads.
fn define_with_inlining(
    codegen: &mut CodeGen,
    built: Vec<(FuncId, String, Function)>,
    keep_calls: &[u32],
) -> Result<(), CompileError> {
    let enabled = std::env::var("PITH_INLINE").map_or(true, |v| v != "0");
    let mut bodies: HashMap<FuncId, Function> = HashMap::new();
    if enabled {
        for (id, name, func) in &built {
            if is_inline_candidate(name, func) {
                let mut ctx = codegen.module.make_context();
                ctx.func = func.clone();
                if ctx.legalize(codegen.module.isa()).is_ok() && !names_data(&ctx.func) {
                    bodies.insert(*id, ctx.func);
                }
            }
        }
    }
    for (func_id, func_name, func) in &built {
        let mut ctx = codegen.module.make_context();
        ctx.func = func.clone();
        if enabled {
            let inliner = SmallFnInliner {
                caller: *func_id,
                bodies: &bodies,
            };
            ctx.inline(inliner).map_err(|e| {
                CompileError::ModuleError(format!("IR consumer: inlining into {func_name}: {e}"))
            })?;
        }
        // after inlining, so a loop whose calls were all inlined counts as call-free
        loop_hoist::hoist_list_headers(&mut ctx.func, keep_calls);
        define_built_function(codegen, *func_id, func_name, &mut ctx)?;
    }
    Ok(())
}

/// Whether the function refers to a global by symbol: a module global, a
/// string constant, the preemption flag.
fn names_data(func: &Function) -> bool {
    func.global_values.values().any(|gv| {
        matches!(
            gv,
            cranelift::codegen::ir::GlobalValueData::Symbol {
                name: cranelift::codegen::ir::ExternalName::User(_),
                ..
            }
        )
    })
}

/// Whether calls to this function may be inlined: small, and not one of the
/// entry points the runtime calls by name.
fn is_inline_candidate(name: &str, func: &Function) -> bool {
    if name == "main" || name.starts_with("__init_globals") {
        return false;
    }
    let mut insts = 0usize;
    for block in func.layout.blocks() {
        insts += func.layout.block_insts(block).count();
        if insts > INLINE_MAX_INSTS {
            return false;
        }
    }
    true
}

/// Directs Cranelift's inliner: a direct call to a small Pith function is
/// inlined, everything else keeps its call.
struct SmallFnInliner<'a> {
    caller: FuncId,
    bodies: &'a HashMap<FuncId, Function>,
}

impl cranelift::codegen::inline::Inline for SmallFnInliner<'_> {
    fn inline(
        &mut self,
        caller: &Function,
        _call_inst: cranelift::codegen::ir::Inst,
        _call_opcode: cranelift::codegen::ir::Opcode,
        callee: FuncRef,
        _call_args: &[Value],
    ) -> cranelift::codegen::inline::InlineCommand<'_> {
        use cranelift::codegen::inline::InlineCommand;
        use cranelift::codegen::ir::ExternalName;
        let ExternalName::User(name_ref) = caller.dfg.ext_funcs[callee].name else {
            return InlineCommand::KeepCall;
        };
        let name = &caller.params.user_named_funcs()[name_ref];
        if name.namespace != 0 {
            return InlineCommand::KeepCall;
        }
        let id = FuncId::from_u32(name.index);
        if id == self.caller {
            return InlineCommand::KeepCall;
        }
        match self.bodies.get(&id) {
            Some(body) => InlineCommand::Inline {
                callee: std::borrow::Cow::Borrowed(body),
                visit_callee: false,
            },
            None => InlineCommand::KeepCall,
        }
    }
}

/// Define one built function, naming the failing instruction when the
/// verifier rejects it.
fn define_built_function(
    codegen: &mut CodeGen,
    func_id: FuncId,
    func_name: &str,
    ctx: &mut cranelift::codegen::Context,
) -> Result<(), CompileError> {
    codegen
        .module
        .define_function(func_id, ctx)
        .map_err(|e| {
            let detail = match &e {
                cranelift_module::ModuleError::Compilation(
                    cranelift::codegen::CodegenError::Verifier(errors),
                ) => errors
                    .0
                    .iter()
                    .map(|err| format!("  {}", err))
                    .collect::<Vec<_>>()
                    .join("\n"),
                other => format!("  {}", other),
            };
            eprintln!(
                "IR consumer verifier error in '{}':\n{}\nIR:\n{}",
                func_name,
                detail,
                ctx.func.display()
            );
            CompileError::ModuleError(format!("IR consumer: {}\n{}", e, detail))
        })?;

    Ok(())
}
