// grammar-directed pith program generator. builds a typed program tree over a
// symbol table (structs, enums, interfaces, fns, locals) and prints it, so
// references are valid by construction. the deliberate exceptions are the edge
// dials: optionals of type parameters, empty literals in argument position,
// bare `none` in typed positions, and enum variants reached through a module
// alias — the places compiler bugs have historically lived.

use crate::eval::{lookup, ArmSem, BaseSem, Body, CmpOp, Env, PickSem, Stmt, Val, E};
use crate::rng::Rng;

/// an emitted expression: the code text plus its semantic tree
pub struct Ex {
    pub code: String,
    pub e: E,
}

fn ex(code: String, e: E) -> Ex {
    Ex { code, e }
}

/// one line of predicted stdout. a wildcard line matches anything.
#[derive(Clone)]
pub struct ExpLine {
    pub text: String,
    pub wildcard: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Ty {
    Int,
    Str,
    Bool,
    Opt(Box<Ty>),
    List(Box<Ty>),
    Map(Box<Ty>, Box<Ty>),
    Struct(usize, Vec<Ty>),
    Enum(usize),
    Param(String),
    Chan(Box<Ty>),
    FnT(Vec<Ty>, Box<Ty>),
}

#[derive(Clone)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
    pub weak: bool,
}

#[derive(Clone)]
pub struct StructDef {
    pub name: String,
    pub module: usize,
    pub generics: Vec<String>,
    pub fields: Vec<Field>,
}

#[derive(Clone)]
pub struct EnumDef {
    pub name: String,
    pub module: usize,
    pub variants: Vec<(String, Vec<Ty>)>,
}

#[derive(Clone, PartialEq)]
pub enum Special {
    Plain,
    Generic,     // generic helper fn, called from dedicated main blocks
    Worker,      // channel pump for the concurrency template
    Apply,       // takes a fn value
    Fallible,    // -> Int!, called with catch
    CrossBlend,  // generic body using cross-module types (or module-local, re-emitted)
}

#[derive(Clone)]
pub struct FnDef {
    pub name: String,
    pub module: usize,
    pub generics: Vec<String>,
    pub params: Vec<(String, Ty)>,
    pub ret: Option<Ty>,
    pub special: Special,
    pub body: Body,
}

pub struct IfaceDef {
    pub name: String,
    pub opt_method: bool,
}

pub struct ImplDef {
    pub iface: usize,
    pub struct_idx: usize,
    pub item: Ty,
    pub first: E,
    pub pick: Option<PickSem>,
}

#[derive(Clone, Copy, Default)]
pub struct Feats {
    pub n_modules: usize,
    pub generic_structs: bool,
    pub self_ref: bool,
    pub opt_param_field: bool,
    pub enums: bool,
    pub interfaces: bool,
    pub iface_enum_bind: bool,
    pub iface_opt_method: bool,
    pub iface_generic_fn: bool,
    pub generic_fns: bool,
    pub optionals: bool,
    pub collections: bool,
    pub closures: bool,
    pub concurrency: bool,
    pub spawn_generic: bool,
    pub spawn_closure: bool,
    pub alias_dial: bool,
    pub alias_nopayload: bool,
    pub weakrefs: bool,
    pub results: bool,
    pub cross_generic: bool,
    pub ownership: bool,
}

struct Scope {
    vars: Vec<(String, Ty, bool)>, // name, type, mutable
    module: usize,
}

impl Scope {
    fn new(module: usize) -> Scope {
        Scope { vars: Vec::new(), module }
    }
}

enum Print {
    Interp(String),
    Concat(String),
}

pub struct Gen {
    rng: Rng,
    opts: GenOpts,
    pub feats: Feats,
    structs: Vec<StructDef>,
    enums: Vec<EnumDef>,
    fns: Vec<FnDef>,
    ifaces: Vec<IfaceDef>,
    impls: Vec<ImplDef>,
    decl_text: Vec<String>, // one buffer per module
    counter: u32,
    emitted_fns: usize, // bodies rendered so far; general calls only reach these
    pluck_fn: Option<(usize, bool)>,
    env: Env,               // main-scope values, parallel to main's Scope
    expected: Vec<ExpLine>, // predicted stdout, in program order
}

const STRUCT_POOL: &[&str] = &["Pack", "Crate", "Badge", "Relay", "Prism", "Vault", "Ledger", "Spool"];
const ENUM_POOL: &[&str] = &["Phase", "Route", "Verdict", "Pulse", "Grade", "Stage"];
const IFACE_POOL: &[&str] = &["Source", "Gauge", "Sink"];
const FN_POOL: &[&str] = &["blend", "carry", "weigh", "stamp", "tally", "probe", "gather", "settle"];
const FIELD_POOL: &[&str] = &["label", "weight", "count", "tag", "note", "rank", "extra", "inner"];
/// the ownership shapes by id. a shape prints lines tagged `own-<name>:` (the
/// shrink shape prints own-grow half the time), and `--shapes` takes names or
/// ids. ids below OWN_SHAPES_SETTLED run clean on main's toolchain; the rest
/// are pending (see GenOpts::pending). when their fixes land, set
/// OWN_SHAPES_SETTLED to OWN_SHAPES and every program draws them.
const OWN_SHAPE_NAMES: &[&str] = &[
    "clear", "remove", "set", "call", "struct", "global", "field", "method", "element", "optional",
    "weaktmp", "weaklive", "iflet", "closure", "optbind", "ifletclear", "match", "return", "shrink",
    "tmpfield", "closurefield", "nested", "cut",
    // pending: closures called out of a holder
    "fnfield", "fnself", "fnelem", "fnelemself",
    // pending: a catch-all or tuple arm that drops its borrowed subject
    "viewfield", "viewstring", "viewenum", "viewelem", "viewtuple", "viewtail", "viewloop",
    // pending: a sibling operand drops what an earlier one read
    "sibcat", "sibinterp", "sibtuple", "sibmapkey", "sibarg", "sibassign", "sibindex",
    // pending: interpolating an optional struct, list or map
    "optstruct", "optlist", "optmap",
    // pending: a kept getter optional outlives its container's hold
    "getkept", "getkeptmap", "getkeptstr",
    // pending: map, filter and reduce whose function changes what they walk
    "walkfield", "walkfilter", "walkclear", "walkpush", "walkreplace",
    // pending: consumers of a weak read
    "weakunwrap", "weakmatch", "weakstore", "weakinterp",
];
const OWN_SHAPES_SETTLED: u32 = 23;
const OWN_SHAPES: u32 = OWN_SHAPE_NAMES.len() as u32;

/// the ownership shape's name for an id
pub fn shape_name(k: u32) -> &'static str {
    OWN_SHAPE_NAMES.get(k as usize).copied().unwrap_or("?")
}

/// generation options shared by gen, run and reduce. a seed and the same
/// options always give the same program.
#[derive(Clone)]
pub struct GenOpts {
    /// `--pending-shapes`: draw the pending ownership shapes too. they
    /// reproduce bugs main's toolchain still has, so they stay out of the
    /// default programs (the deterministic gate, the nightly hunt) until the
    /// fixes land. without it a seed gives the program it gave before they
    /// were added.
    pub pending: bool,
    /// emit exactly these ownership shapes in every program, to attribute
    /// findings to one shape
    pub only: Option<Vec<u32>>,
    /// leak mode: each ownership shape runs in a loop of PITH_LEAK_ROUNDS
    /// rounds (one by default), and the program prints its peak resident set
    /// after each shape for the leak oracle
    pub rounds: bool,
}

impl GenOpts {
    pub fn from_args(args: &[String]) -> GenOpts {
        let only = args
            .iter()
            .position(|a| a == "--shapes")
            .and_then(|i| args.get(i + 1))
            .map(|list| parse_shapes(list));
        GenOpts {
            pending: args.iter().any(|a| a == "--pending-shapes"),
            only,
            rounds: args.iter().any(|a| a == "--leak"),
        }
    }
}

/// a comma list of shape names, ids and `a..b` id ranges
fn parse_shapes(list: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for item in list.split(',').filter(|s| !s.is_empty()) {
        if let Some((a, b)) = item.split_once("..") {
            let a: u32 = a.parse().expect("bad --shapes range");
            let b: u32 = b.parse().expect("bad --shapes range");
            out.extend(a..b.min(OWN_SHAPES));
        } else if let Ok(k) = item.parse::<u32>() {
            assert!(k < OWN_SHAPES, "--shapes: no shape {}", k);
            out.push(k);
        } else {
            match OWN_SHAPE_NAMES.iter().position(|n| *n == item) {
                Some(k) => out.push(k as u32),
                None => panic!("--shapes: no shape named {}", item),
            }
        }
    }
    out
}
const VARIANT_POOL: &[&str] = &["Alpha", "Beta", "Gamma", "Delta", "Omega", "Zed"];

pub struct Program {
    pub files: Vec<(String, String)>, // filename, content
    pub expected: Vec<ExpLine>,       // predicted stdout for a clean run
}

pub fn generate(seed: u64, opts: &GenOpts) -> Program {
    let mut g = Gen::new(seed, opts.clone());
    g.decide_features();
    if g.opts.only.is_some() {
        g.feats.ownership = true;
    }
    for m in 1..g.feats.n_modules {
        g.gen_module_decls(m);
    }
    g.gen_module_decls(0);
    if g.feats.ownership {
        g.emit_ownership_decls();
    }
    let main_body = g.gen_main();
    g.render(main_body)
}

impl Gen {
    fn new(seed: u64, opts: GenOpts) -> Gen {
        Gen {
            rng: Rng::new(seed),
            opts,
            feats: Feats::default(),
            structs: Vec::new(),
            enums: Vec::new(),
            fns: Vec::new(),
            ifaces: Vec::new(),
            impls: Vec::new(),
            decl_text: vec![String::new(), String::new(), String::new()],
            counter: 0,
            emitted_fns: 0,
            pluck_fn: None,
            env: Vec::new(),
            expected: Vec::new(),
        }
    }

    fn expect(&mut self, text: String) {
        self.expected.push(ExpLine { text, wildcard: false });
    }

    fn expect_wild(&mut self) {
        self.expected.push(ExpLine { text: String::new(), wildcard: true });
    }

    // ---------- evaluation ----------

    /// resolve a dotted path against an environment: longest env key first,
    /// then remaining segments as struct field reads
    fn resolve(&self, path: &str, env: &Env) -> Val {
        if let Some(v) = lookup(env, path) {
            return v.clone();
        }
        if let Some((prefix, seg)) = path.rsplit_once('.') {
            let base = self.resolve(prefix, env);
            return self.field_val(&base, seg);
        }
        panic!("pithgen eval: unresolved path '{}'", path);
    }

    fn field_val(&self, v: &Val, field: &str) -> Val {
        match v {
            Val::St(si, fields) => {
                let pos = self.structs[*si]
                    .fields
                    .iter()
                    .position(|f| f.name == field)
                    .unwrap_or_else(|| panic!("pithgen eval: no field '{}' on {}", field, self.structs[*si].name));
                fields[pos].clone()
            }
            other => panic!("pithgen eval: field '{}' read on {:?}", field, other),
        }
    }

    fn eval(&self, e: &E, env: &Env) -> Val {
        match e {
            E::Lit(v) => v.clone(),
            E::Path(p) => self.resolve(p, env),
            E::Add(a, b) => Val::I(self.eval(a, env).as_int().wrapping_add(self.eval(b, env).as_int())),
            E::Mul(a, b) => Val::I(self.eval(a, env).as_int().wrapping_mul(self.eval(b, env).as_int())),
            E::Concat(a, b) => {
                let mut s = self.eval(a, env).as_str().to_string();
                s.push_str(self.eval(b, env).as_str());
                Val::S(s)
            }
            E::ToStr(a) => Val::S(self.eval(a, env).as_int().to_string()),
            E::Len(a) => Val::I(self.eval(a, env).len_of()),
            E::Cmp(op, a, b) => {
                let x = self.eval(a, env).as_int();
                let y = self.eval(b, env).as_int();
                Val::B(match op {
                    CmpOp::Lt => x < y,
                    CmpOp::Gt => x > y,
                    CmpOp::Eq => x == y,
                    CmpOp::Ne => x != y,
                })
            }
            E::IsNone(a, negated) => {
                let none = self.eval(a, env) == Val::NoneV;
                Val::B(if *negated { !none } else { none })
            }
            E::ListL(items) => Val::L(items.iter().map(|it| self.eval(it, env)).collect()),
            E::MapL(pairs) => {
                // literal insert semantics: unique keys, later value wins
                let mut out: Vec<(Val, Val)> = Vec::new();
                for (k, v) in pairs {
                    let kv = self.eval(k, env);
                    let vv = self.eval(v, env);
                    if let Some(slot) = out.iter_mut().find(|(ek, _)| *ek == kv) {
                        slot.1 = vv;
                    } else {
                        out.push((kv, vv));
                    }
                }
                Val::M(out)
            }
            E::StructL(si, fields) => Val::St(*si, fields.iter().map(|f| self.eval(f, env)).collect()),
            E::EnumL(ei, vi, payload) => {
                Val::En(*ei, *vi, payload.iter().map(|p| self.eval(p, env)).collect())
            }
            E::Call(fi, args) => {
                let argv: Vec<Val> = args.iter().map(|a| self.eval(a, env)).collect();
                self.eval_call(*fi, argv)
            }
        }
    }

    fn eval_in_main(&self, e: &E) -> Val {
        self.eval(e, &self.env)
    }

    fn eval_call(&self, fi: usize, args: Vec<Val>) -> Val {
        let f = &self.fns[fi];
        match &f.body {
            Body::Stmts(stmts) => {
                let mut env: Env = f
                    .params
                    .iter()
                    .map(|(n, _)| n.clone())
                    .zip(args.into_iter())
                    .collect();
                for st in stmts {
                    match st {
                        Stmt::Let(name, e) => {
                            let v = self.eval(e, &env);
                            env.push((name.clone(), v));
                        }
                        Stmt::IfRet(cond, ret) => {
                            if self.eval(cond, &env).as_bool() {
                                return self.eval(ret, &env);
                            }
                        }
                        Stmt::Ret(e) => return self.eval(e, &env),
                    }
                }
                panic!("pithgen eval: fn body without return");
            }
            Body::Identity => args.into_iter().next().unwrap(),
            Body::FirstOr => {
                let mut it = args.into_iter();
                let xs = it.next().unwrap();
                let fb = it.next().unwrap();
                match xs {
                    Val::L(items) if !items.is_empty() => items[0].clone(),
                    _ => fb,
                }
            }
            Body::OptProbe => {
                let tag = args[0].as_str().to_string();
                if args[1] == Val::NoneV {
                    Val::S(tag + "-none")
                } else {
                    Val::S(tag + "-some")
                }
            }
            Body::WrapStruct(si, tail) => {
                let mut fields = vec![args.into_iter().next().unwrap()];
                fields.extend(tail.iter().cloned());
                Val::St(*si, fields)
            }
            Body::UnwrapOr7 => match &args[1] {
                Val::NoneV => Val::I(7),
                other => Val::I(other.as_int()),
            },
            Body::CrossBlend(arms, base) => {
                let bonus = match &args[2] {
                    Val::En(_, vi, payload) => match &arms[*vi] {
                        ArmSem::Const(n) => *n,
                        ArmSem::B0Int => payload[0].as_int(),
                        ArmSem::B0Len => payload[0].len_of(),
                        ArmSem::B0FieldInt(fx) => match &payload[0] {
                            Val::St(_, fs) => fs[*fx].as_int(),
                            other => panic!("pithgen eval: cross-blend payload {:?}", other),
                        },
                        ArmSem::B0FieldStrLen(fx) => match &payload[0] {
                            Val::St(_, fs) => fs[*fx].len_of(),
                            other => panic!("pithgen eval: cross-blend payload {:?}", other),
                        },
                        ArmSem::B0OptIntOr(k) => match &payload[0] {
                            Val::NoneV => *k,
                            v => v.as_int(),
                        },
                    },
                    other => panic!("pithgen eval: cross-blend enum arg {:?}", other),
                };
                let base_v = match (base, &args[1]) {
                    (BaseSem::FieldInt(fx), Val::St(_, fs)) => fs[*fx].as_int(),
                    (BaseSem::FieldStrLen(fx), Val::St(_, fs)) => fs[*fx].len_of(),
                    (BaseSem::One, _) => 1,
                    (b, v) => panic!("pithgen eval: cross-blend base {:?} on {:?}", b, v),
                };
                Val::I(base_v.wrapping_add(bonus))
            }
            Body::Pluck(optional) => {
                let recv = &args[0];
                let si = match recv {
                    Val::St(si, _) => *si,
                    other => panic!("pithgen eval: pluck receiver {:?}", other),
                };
                let ii = self
                    .impls
                    .iter()
                    .position(|im| im.iface == 0 && im.struct_idx == si)
                    .expect("pithgen eval: pluck receiver has no impl");
                if *optional {
                    self.eval_impl_pick(ii, recv)
                } else {
                    self.eval_impl_first(ii, recv)
                }
            }
            Body::Opaque => panic!("pithgen eval: call into opaque body '{}'", f.name),
        }
    }

    fn impl_env(&self, si: usize, recv: &Val) -> Env {
        let fields = match recv {
            Val::St(_, fs) => fs,
            other => panic!("pithgen eval: impl receiver {:?}", other),
        };
        self.structs[si]
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.weak)
            .map(|(i, f)| (format!("self.{}", f.name), fields[i].clone()))
            .collect()
    }

    fn eval_impl_first(&self, ii: usize, recv: &Val) -> Val {
        let im = &self.impls[ii];
        let env = self.impl_env(im.struct_idx, recv);
        self.eval(&im.first, &env)
    }

    fn eval_impl_pick(&self, ii: usize, recv: &Val) -> Val {
        let im = &self.impls[ii];
        let pick = im.pick.as_ref().expect("pithgen eval: pick without opt method");
        let env = self.impl_env(im.struct_idx, recv);
        let guard = match recv {
            Val::St(_, fs) => fs[pick.field_idx].as_int(),
            other => panic!("pithgen eval: pick receiver {:?}", other),
        };
        if guard > pick.lim {
            self.eval(&pick.some, &env)
        } else {
            Val::NoneV
        }
    }

    fn fresh(&mut self, pool: &[&str]) -> String {
        let base = pool[self.rng.below(pool.len())];
        let n = self.counter;
        self.counter += 1;
        format!("{}{}", base, n)
    }

    fn decide_features(&mut self) {
        let r = &mut self.rng;
        let mut f = Feats::default();
        f.n_modules = match r.weighted(&[45, 35, 20]) {
            0 => 1,
            1 => 2,
            _ => 3,
        };
        f.generic_structs = r.chance(55);
        if f.generic_structs {
            f.self_ref = r.chance(45);
            f.opt_param_field = r.chance(55);
        }
        f.enums = r.chance(75);
        f.interfaces = r.chance(35);
        if f.interfaces {
            f.iface_enum_bind = r.chance(55);
            f.iface_opt_method = r.chance(60);
            f.iface_generic_fn = r.chance(45);
            if f.iface_enum_bind {
                f.enums = true;
            }
        }
        f.generic_fns = r.chance(60);
        f.optionals = r.chance(80);
        f.collections = r.chance(60);
        f.closures = r.chance(40);
        f.concurrency = r.chance(35);
        if f.concurrency {
            f.spawn_generic = r.chance(35);
            f.spawn_closure = r.chance(35);
        }
        if f.n_modules > 1 {
            f.alias_dial = r.chance(70);
            f.alias_nopayload = f.alias_dial && r.chance(50);
            f.cross_generic = r.chance(60);
            if f.alias_nopayload || f.cross_generic {
                f.enums = true;
            }
        }
        f.weakrefs = r.chance(25);
        f.results = r.chance(30);
        // the ownership shapes are the whole point of the sanitize hunt, so
        // they land in the large majority of programs
        f.ownership = r.chance(85);
        // keep every program exercising something
        let majors = [f.generic_structs, f.enums, f.interfaces, f.generic_fns, f.collections, f.concurrency]
            .iter()
            .filter(|b| **b)
            .count();
        if majors < 2 {
            f.enums = true;
            f.collections = true;
        }
        self.feats = f;
    }

    // ---------- type helpers ----------

    fn ty_name(&self, ty: &Ty) -> String {
        match ty {
            Ty::Int => "Int".into(),
            Ty::Str => "String".into(),
            Ty::Bool => "Bool".into(),
            Ty::Opt(t) => format!("{}?", self.ty_name(t)),
            Ty::List(t) => format!("List[{}]", self.ty_name(t)),
            Ty::Map(k, v) => format!("Map[{}, {}]", self.ty_name(k), self.ty_name(v)),
            Ty::Struct(i, targs) => {
                if targs.is_empty() {
                    self.structs[*i].name.clone()
                } else {
                    let args: Vec<String> = targs.iter().map(|t| self.ty_name(t)).collect();
                    format!("{}[{}]", self.structs[*i].name, args.join(", "))
                }
            }
            Ty::Enum(i) => self.enums[*i].name.clone(),
            Ty::Param(p) => p.clone(),
            Ty::Chan(t) => format!("Channel[{}]", self.ty_name(t)),
            Ty::FnT(ps, r) => {
                let args: Vec<String> = ps.iter().map(|t| self.ty_name(t)).collect();
                format!("fn({}) -> {}", args.join(", "), self.ty_name(r))
            }
        }
    }

    fn subst(&self, ty: &Ty, map: &[(String, Ty)]) -> Ty {
        match ty {
            Ty::Param(p) => {
                for (name, t) in map {
                    if name == p {
                        return t.clone();
                    }
                }
                ty.clone()
            }
            Ty::Opt(t) => Ty::Opt(Box::new(self.subst(t, map))),
            Ty::List(t) => Ty::List(Box::new(self.subst(t, map))),
            Ty::Map(k, v) => Ty::Map(Box::new(self.subst(k, map)), Box::new(self.subst(v, map))),
            Ty::Struct(i, targs) => {
                Ty::Struct(*i, targs.iter().map(|t| self.subst(t, map)).collect())
            }
            Ty::Chan(t) => Ty::Chan(Box::new(self.subst(t, map))),
            Ty::FnT(ps, r) => Ty::FnT(
                ps.iter().map(|t| self.subst(t, map)).collect(),
                Box::new(self.subst(r, map)),
            ),
            _ => ty.clone(),
        }
    }

    fn module_visible(&self, decl_module: usize, from: usize) -> bool {
        decl_module == from || from == 0 || (from == 2 && decl_module == 1)
    }

    /// how a helper fn is spelled from a caller module (types are always
    /// from-imported and spelled plain, fns go through the module alias).
    fn fn_call_name(&self, fi: usize, from: usize) -> String {
        let f = &self.fns[fi];
        if f.module == from {
            f.name.clone()
        } else {
            format!("{}.{}", module_alias(f.module, from), f.name)
        }
    }

    /// a concrete type usable for instantiating a generic
    fn concrete_ty(&mut self, allow_struct: bool) -> Ty {
        let plain_structs: Vec<usize> = self
            .structs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.generics.is_empty())
            .map(|(i, _)| i)
            .collect();
        let w = if allow_struct && !plain_structs.is_empty() {
            self.rng.weighted(&[45, 35, 20])
        } else {
            self.rng.weighted(&[55, 45])
        };
        match w {
            0 => Ty::Int,
            1 => Ty::Str,
            _ => Ty::Struct(plain_structs[self.rng.below(plain_structs.len())], vec![]),
        }
    }

    /// a random concrete field/local type drawn from what exists so far
    fn random_ty(&mut self, module: usize, depth: u32) -> Ty {
        let structs: Vec<usize> = self
            .structs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.generics.is_empty() && self.module_visible(s.module, module))
            .map(|(i, _)| i)
            .collect();
        let enums: Vec<usize> = self
            .enums
            .iter()
            .enumerate()
            .filter(|(_, e)| self.module_visible(e.module, module))
            .map(|(i, _)| i)
            .collect();
        let mut weights = vec![28u32, 18, 8];
        weights.push(if depth > 0 { 10 } else { 0 }); // Opt
        weights.push(if depth > 0 { 10 } else { 0 }); // List
        weights.push(if depth > 0 { 5 } else { 0 }); // Map
        weights.push(if structs.is_empty() { 0 } else { 12 });
        weights.push(if enums.is_empty() { 0 } else { 9 });
        match self.rng.weighted(&weights) {
            0 => Ty::Int,
            1 => Ty::Str,
            2 => Ty::Bool,
            3 => {
                let inner = self.random_ty(module, 0);
                Ty::Opt(Box::new(inner))
            }
            4 => {
                let inner = self.random_ty(module, 0);
                Ty::List(Box::new(inner))
            }
            5 => {
                let k = if self.rng.chance(60) { Ty::Str } else { Ty::Int };
                let v = self.random_ty(module, 0);
                Ty::Map(Box::new(k), Box::new(v))
            }
            6 => Ty::Struct(structs[self.rng.below(structs.len())], vec![]),
            _ => Ty::Enum(enums[self.rng.below(enums.len())]),
        }
    }

    // ---------- declaration generation ----------

    fn gen_module_decls(&mut self, m: usize) {
        // plain structs first so later decls can reference them
        let n_structs = 1 + self.rng.below(2);
        for _ in 0..n_structs {
            self.gen_plain_struct(m);
        }
        if self.feats.enums {
            let n_enums = 1 + self.rng.below(if m == 0 { 2 } else { 1 });
            for _ in 0..n_enums {
                self.gen_enum(m);
            }
        }
        if m == 0 && self.feats.generic_structs {
            self.gen_generic_struct();
        }
        if m == 0 && self.feats.interfaces {
            self.gen_interface();
        }
        // plain helper fns
        let n_fns = 1 + self.rng.below(2);
        for _ in 0..n_fns {
            self.gen_plain_fn(m);
        }
        if self.feats.generic_fns && (m == 0 || self.rng.chance(40)) {
            self.gen_generic_fn(m);
        }
        if self.feats.cross_generic && m > 0 {
            self.gen_cross_blend(m);
        }
        if m == 0 {
            if self.feats.results {
                self.gen_fallible_fn();
            }
            if self.feats.closures || self.feats.spawn_closure {
                self.gen_apply_fn();
            }
            if self.feats.concurrency {
                self.gen_workers();
            }
            if self.feats.iface_generic_fn && !self.ifaces.is_empty() {
                self.gen_iface_pluck();
            }
        }
    }

    fn gen_plain_struct(&mut self, m: usize) {
        let name = self.fresh(STRUCT_POOL);
        let mut fields = Vec::new();
        // the first field is always printable
        let first_ty = if self.rng.chance(55) { Ty::Int } else { Ty::Str };
        let fname = self.fresh(FIELD_POOL);
        fields.push(Field { name: fname, ty: first_ty, weak: false });
        let extra = 1 + self.rng.below(2);
        for _ in 0..extra {
            let ty = self.random_ty(m, 1);
            let fname = self.fresh(FIELD_POOL);
            fields.push(Field { name: fname, ty, weak: false });
        }
        // a weak back-pointer to an earlier struct in this module, sometimes
        if self.feats.weakrefs && m == 0 && self.rng.chance(40) {
            let earlier: Vec<usize> = self
                .structs
                .iter()
                .enumerate()
                .filter(|(_, s)| s.module == m && s.generics.is_empty())
                .map(|(i, _)| i)
                .collect();
            if !earlier.is_empty() {
                let t = earlier[self.rng.below(earlier.len())];
                let fname = self.fresh(FIELD_POOL);
                fields.push(Field {
                    name: fname,
                    ty: Ty::Opt(Box::new(Ty::Struct(t, vec![]))),
                    weak: true,
                });
            }
        }
        let vis = if m == 0 { "" } else { "pub " };
        let mut text = format!("{}struct {}:\n", vis, name);
        for f in &fields {
            let w = if f.weak { "weak " } else { "" };
            text.push_str(&format!("    {}{}: {}\n", w, f.name, self.ty_name(&f.ty)));
        }
        text.push('\n');
        self.decl_text[m].push_str(&text);
        self.structs.push(StructDef { name, module: m, generics: vec![], fields });
    }

    fn gen_generic_struct(&mut self) {
        let name = self.fresh(STRUCT_POOL);
        let idx = self.structs.len();
        let mut fields = Vec::new();
        fields.push(Field { name: "inner".into(), ty: Ty::Param("T".into()), weak: false });
        // always one concrete printable field
        let tag_ty = if self.rng.chance(50) { Ty::Str } else { Ty::Int };
        fields.push(Field { name: "tag".into(), ty: tag_ty, weak: false });
        if self.feats.opt_param_field {
            // the M? dial: an optional of the type parameter
            fields.push(Field {
                name: "peer".into(),
                ty: Ty::Opt(Box::new(Ty::Param("T".into()))),
                weak: false,
            });
        }
        if self.feats.self_ref {
            // the self-referential dial: Node[T]? mentioning the struct itself
            fields.push(Field {
                name: "next".into(),
                ty: Ty::Opt(Box::new(Ty::Struct(idx, vec![Ty::Param("T".into())]))),
                weak: false,
            });
        }
        let mut text = format!("struct {}[T]:\n", name);
        // the self-ref field prints its own name, so register the def first
        self.structs.push(StructDef {
            name: name.clone(),
            module: 0,
            generics: vec!["T".into()],
            fields: fields.clone(),
        });
        for f in &fields {
            text.push_str(&format!("    {}: {}\n", f.name, self.ty_name(&f.ty)));
        }
        text.push('\n');
        self.decl_text[0].push_str(&text);
    }

    fn gen_enum(&mut self, m: usize) {
        let name = self.fresh(ENUM_POOL);
        let n_variants = 2 + self.rng.below(2);
        let mut variants = Vec::new();
        let mut order: Vec<&str> = VARIANT_POOL.to_vec();
        // rotate so variant names differ across enums
        let rot = self.rng.below(order.len());
        order.rotate_left(rot);
        let mut has_nopayload = false;
        for vi in 0..n_variants {
            let vname = order[vi].to_string();
            let payload = if self.rng.chance(45) || (vi == n_variants - 1 && !has_nopayload) {
                has_nopayload = true;
                vec![]
            } else {
                let n = 1 + self.rng.below(2) as usize;
                let mut tys = Vec::new();
                for _ in 0..n {
                    let choice = self.rng.weighted(&[35, 30, 15, 10, 10]);
                    let ty = match choice {
                        0 => Ty::Int,
                        1 => Ty::Str,
                        2 => Ty::List(Box::new(Ty::Int)),
                        3 => {
                            let cands: Vec<usize> = self
                                .structs
                                .iter()
                                .enumerate()
                                .filter(|(_, s)| s.generics.is_empty() && self.module_visible(s.module, m))
                                .map(|(i, _)| i)
                                .collect();
                            if cands.is_empty() {
                                Ty::Int
                            } else {
                                Ty::Struct(cands[self.rng.below(cands.len())], vec![])
                            }
                        }
                        _ => Ty::Opt(Box::new(Ty::Int)),
                    };
                    tys.push(ty);
                }
                tys
            };
            variants.push((vname, payload));
        }
        let vis = if m == 0 { "" } else { "pub " };
        let mut text = format!("{}enum {}:\n", vis, name);
        for (vname, payload) in &variants {
            if payload.is_empty() {
                text.push_str(&format!("    {}\n", vname));
            } else {
                let tys: Vec<String> = payload.iter().map(|t| self.ty_name(t)).collect();
                text.push_str(&format!("    {}({})\n", vname, tys.join(", ")));
            }
        }
        text.push('\n');
        self.decl_text[m].push_str(&text);
        self.enums.push(EnumDef { name, module: m, variants });
    }

    fn gen_interface(&mut self) {
        let name = self.fresh(IFACE_POOL);
        let opt_method = self.feats.iface_opt_method;
        let mut text = format!("interface {}:\n    type Item\n    fn first() -> Item\n", name);
        if opt_method {
            text.push_str("    fn pick() -> Item?\n");
        }
        text.push('\n');
        self.decl_text[0].push_str(&text);
        let iface_idx = self.ifaces.len();
        self.ifaces.push(IfaceDef { name: name.clone(), opt_method });

        // impls: pick 1-2 root structs that carry an Int field for the guard
        let cands: Vec<usize> = self
            .structs
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.module == 0 && s.generics.is_empty() && s.fields.iter().any(|f| f.ty == Ty::Int)
            })
            .map(|(i, _)| i)
            .collect();
        if cands.is_empty() {
            return;
        }
        let n_impls = 1 + self.rng.below(cands.len().min(2));
        for k in 0..n_impls {
            let si = cands[k % cands.len()];
            let item = if k == 0 && self.feats.iface_enum_bind && !self.enums.is_empty() {
                let ecands: Vec<usize> = self
                    .enums
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| self.module_visible(e.module, 0))
                    .map(|(i, _)| i)
                    .collect();
                Ty::Enum(ecands[self.rng.below(ecands.len())])
            } else if self.rng.chance(50) {
                Ty::Int
            } else {
                Ty::Str
            };
            let (int_field_idx, int_field) = self.structs[si]
                .fields
                .iter()
                .enumerate()
                .find(|(_, f)| f.ty == Ty::Int)
                .map(|(i, f)| (i, f.name.clone()))
                .unwrap();
            let mut sc = Scope::new(0);
            for f in &self.structs[si].fields {
                if !f.weak {
                    sc.vars.push((format!("self.{}", f.name), f.ty.clone(), false));
                }
            }
            let first_expr = self.expr(&item, 1, &sc);
            let mut text = format!(
                "impl {} for {}:\n    type Item = {}\n    fn first() -> Item:\n        return {}\n",
                name,
                self.structs[si].name,
                self.ty_name(&item),
                first_expr.code
            );
            let mut pick_sem = None;
            if opt_method {
                let some_expr = self.expr(&item, 1, &sc);
                let lim = self.rng.range(0, 5);
                text.push_str(&format!(
                    "    fn pick() -> Item?:\n        if self.{} > {}:\n            return {}\n        return none\n",
                    int_field, lim, some_expr.code
                ));
                pick_sem = Some(PickSem { field_idx: int_field_idx, lim, some: some_expr.e });
            }
            text.push('\n');
            self.decl_text[0].push_str(&text);
            self.impls.push(ImplDef {
                iface: iface_idx,
                struct_idx: si,
                item,
                first: first_expr.e,
                pick: pick_sem,
            });
        }
    }

    fn gen_plain_fn(&mut self, m: usize) {
        let name = self.fresh(FN_POOL);
        let n_params = 1 + self.rng.below(2);
        let mut params = Vec::new();
        for _ in 0..n_params {
            let ty = if self.feats.optionals && self.rng.chance(25) {
                Ty::Opt(Box::new(if self.rng.chance(60) { Ty::Int } else { Ty::Str }))
            } else {
                self.random_ty(m, 1)
            };
            let pname = format!("a{}", self.counter);
            self.counter += 1;
            params.push((pname, ty));
        }
        let ret = self.random_ty(m, 1);
        let mut sc = Scope::new(m);
        for (p, t) in &params {
            sc.vars.push((p.clone(), t.clone(), false));
        }
        let (body, stmts) = self.fn_body(&mut sc, &ret);
        let sig_params: Vec<String> = params
            .iter()
            .map(|(p, t)| format!("{}: {}", p, self.ty_name(t)))
            .collect();
        let vis = if m == 0 { "" } else { "pub " };
        let text = format!(
            "{}fn {}({}) -> {}:\n{}\n",
            vis,
            name,
            sig_params.join(", "),
            self.ty_name(&ret),
            body
        );
        self.decl_text[m].push_str(&text);
        self.fns.push(FnDef {
            name,
            module: m,
            generics: vec![],
            params,
            ret: Some(ret),
            special: Special::Plain,
            body: Body::Stmts(stmts),
        });
        self.emitted_fns = self.fns.len();
    }

    fn fn_body(&mut self, sc: &mut Scope, ret: &Ty) -> (String, Vec<Stmt>) {
        let mut out = String::new();
        let mut stmts = Vec::new();
        let n_lets = self.rng.below(3);
        for _ in 0..n_lets {
            let ty = self.random_ty(sc.module, 1);
            let code = self.expr(&ty, 1, sc);
            let vname = format!("t{}", self.counter);
            self.counter += 1;
            let annotate = code.code == "[]"
                || code.code == "{}"
                || code.code == "none"
                || matches!(ty, Ty::Opt(_));
            if annotate {
                out.push_str(&format!("    {}: {} := {}\n", vname, self.ty_name(&ty), code.code));
            } else {
                out.push_str(&format!("    {} := {}\n", vname, code.code));
            }
            stmts.push(Stmt::Let(vname.clone(), code.e));
            sc.vars.push((vname, ty, false));
        }
        if self.rng.chance(35) {
            let cond = self.expr(&Ty::Bool, 1, sc);
            let early = self.expr(ret, 1, sc);
            out.push_str(&format!("    if {}:\n        return {}\n", cond.code, early.code));
            stmts.push(Stmt::IfRet(cond.e, early.e));
        }
        let fin = self.expr(ret, 2, sc);
        out.push_str(&format!("    return {}\n", fin.code));
        stmts.push(Stmt::Ret(fin.e));
        (out, stmts)
    }

    fn gen_generic_fn(&mut self, m: usize) {
        let name = self.fresh(FN_POOL);
        let kind = self.rng.weighted(&[25, 25, 25, 25]);
        let vis = if m == 0 { "" } else { "pub " };
        let (text, params, ret, body): (String, Vec<(String, Ty)>, Ty, Body) = match kind {
            0 => {
                // identity with a detour through a local
                let text = format!(
                    "{}fn {}[T](v: T) -> T:\n    held := v\n    return held\n\n",
                    vis, name
                );
                (
                    text,
                    vec![("v".into(), Ty::Param("T".into()))],
                    Ty::Param("T".into()),
                    Body::Identity,
                )
            }
            1 => {
                // first-or-fallback over a list of T
                let text = format!(
                    "{}fn {}[T](xs: List[T], fb: T) -> T:\n    if xs.len() > 0:\n        return xs[0]\n    return fb\n\n",
                    vis, name
                );
                (
                    text,
                    vec![
                        ("xs".into(), Ty::List(Box::new(Ty::Param("T".into())))),
                        ("fb".into(), Ty::Param("T".into())),
                    ],
                    Ty::Param("T".into()),
                    Body::FirstOr,
                )
            }
            2 => {
                // optional-of-T probe: `v == none` inside a generic body
                let text = format!(
                    "{}fn {}[T](tag: String, v: T?) -> String:\n    if v == none:\n        return tag + \"-none\"\n    return tag + \"-some\"\n\n",
                    vis, name
                );
                (
                    text,
                    vec![
                        ("tag".into(), Ty::Str),
                        ("v".into(), Ty::Opt(Box::new(Ty::Param("T".into())))),
                    ],
                    Ty::Str,
                    Body::OptProbe,
                )
            }
            _ => {
                // wrap into a generic struct when one exists, else a concrete Int? probe
                let gs: Vec<usize> = self
                    .structs
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| !s.generics.is_empty() && self.module_visible(s.module, m))
                    .map(|(i, _)| i)
                    .collect();
                if let Some(&si) = gs.first() {
                    let sd = self.structs[si].clone();
                    let mut args = vec!["v".to_string()];
                    let mut tail = Vec::new();
                    for f in sd.fields.iter().skip(1) {
                        match &f.ty {
                            Ty::Opt(_) => {
                                args.push("none".into());
                                tail.push(Val::NoneV);
                            }
                            Ty::Str => {
                                args.push("\"w\"".into());
                                tail.push(Val::S("w".into()));
                            }
                            Ty::Int => {
                                args.push("1".into());
                                tail.push(Val::I(1));
                            }
                            _ => {
                                args.push("none".into());
                                tail.push(Val::NoneV);
                            }
                        }
                    }
                    let text = format!(
                        "{}fn {}[T](v: T) -> {}[T]:\n    return {}({})\n\n",
                        vis,
                        name,
                        sd.name,
                        sd.name,
                        args.join(", ")
                    );
                    (
                        text,
                        vec![("v".into(), Ty::Param("T".into()))],
                        Ty::Struct(si, vec![Ty::Param("T".into())]),
                        Body::WrapStruct(si, tail),
                    )
                } else {
                    let text = format!(
                        "{}fn {}[T](a: T, b: Int?) -> Int:\n    return b.unwrap_or(7)\n\n",
                        vis, name
                    );
                    (
                        text,
                        vec![("a".into(), Ty::Param("T".into())), ("b".into(), Ty::Opt(Box::new(Ty::Int)))],
                        Ty::Int,
                        Body::UnwrapOr7,
                    )
                }
            }
        };
        self.decl_text[m].push_str(&text);
        self.fns.push(FnDef {
            name,
            module: m,
            generics: vec!["T".into()],
            params,
            ret: Some(ret),
            special: Special::Generic,
            body,
        });
        self.emitted_fns = self.fns.len();
    }

    /// a generic fn in a helper module whose body works on that module's own
    /// concrete types (match with payload bindings + field reads). when a
    /// specialization is re-emitted for the importing module, those types
    /// cross the module boundary inside the generic body.
    fn gen_cross_blend(&mut self, m: usize) {
        let scands: Vec<usize> = self
            .structs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.module == m && s.generics.is_empty())
            .map(|(i, _)| i)
            .collect();
        let ecands: Vec<usize> = self
            .enums
            .iter()
            .enumerate()
            .filter(|(_, e)| e.module == m)
            .map(|(i, _)| i)
            .collect();
        if scands.is_empty() || ecands.is_empty() {
            return;
        }
        let si = scands[self.rng.below(scands.len())];
        let ei = ecands[self.rng.below(ecands.len())];
        let name = self.fresh(FN_POOL);
        let sd = self.structs[si].clone();
        let ed = self.enums[ei].clone();
        // an int expression per variant, using payload bindings where possible.
        // a payload binding has the payload's declared type, so an optional
        // payload arrives as the optional and reads through unwrap_or.
        let mut arms = String::new();
        let mut arm_sems = Vec::new();
        for (vname, payload) in &ed.variants {
            if payload.is_empty() {
                let n = self.rng.range(1, 9);
                arm_sems.push(ArmSem::Const(n));
                arms.push_str(&format!("        {}.{} => {}\n", ed.name, vname, n));
            } else {
                let binds: Vec<String> = (0..payload.len()).map(|k| format!("b{}", k)).collect();
                let (use_expr, sem) = match &payload[0] {
                    Ty::Int => ("b0".to_string(), ArmSem::B0Int),
                    Ty::Str => ("b0.len()".to_string(), ArmSem::B0Len),
                    Ty::List(_) => ("b0.len()".to_string(), ArmSem::B0Len),
                    Ty::Opt(inner) if **inner == Ty::Int => {
                        let k = self.rng.range(1, 9);
                        (format!("b0.unwrap_or({})", k), ArmSem::B0OptIntOr(k))
                    }
                    Ty::Struct(fsi, _) => {
                        let fsd = &self.structs[*fsi];
                        match fsd
                            .fields
                            .iter()
                            .enumerate()
                            .find(|(_, f)| f.ty == Ty::Int || f.ty == Ty::Str)
                        {
                            Some((fx, f)) if f.ty == Ty::Int => {
                                (format!("b0.{}", f.name), ArmSem::B0FieldInt(fx))
                            }
                            Some((fx, f)) => (format!("b0.{}.len()", f.name), ArmSem::B0FieldStrLen(fx)),
                            None => ("3".to_string(), ArmSem::Const(3)),
                        }
                    }
                    _ => ("4".to_string(), ArmSem::Const(4)),
                };
                arm_sems.push(sem);
                arms.push_str(&format!(
                    "        {}.{}({}) => {}\n",
                    ed.name,
                    vname,
                    binds.join(", "),
                    use_expr
                ));
            }
        }
        let sfield = sd
            .fields
            .iter()
            .enumerate()
            .find(|(_, f)| f.ty == Ty::Int)
            .map(|(i, f)| (i, f.name.clone()));
        let (base, base_sem) = match sfield {
            Some((fx, f)) => (format!("p.{}", f), BaseSem::FieldInt(fx)),
            None => {
                let f = sd
                    .fields
                    .iter()
                    .enumerate()
                    .find(|(_, f)| f.ty == Ty::Str)
                    .map(|(i, f)| (i, f.name.clone()));
                match f {
                    Some((fx, f)) => (format!("p.{}.len()", f), BaseSem::FieldStrLen(fx)),
                    None => ("1".to_string(), BaseSem::One),
                }
            }
        };
        let text = format!(
            "pub fn {}[T](v: T, p: {}, e: {}) -> Int:\n    bonus := match e:\n{}    return {} + bonus\n\n",
            name, sd.name, ed.name, arms, base
        );
        self.decl_text[m].push_str(&text);
        self.fns.push(FnDef {
            name,
            module: m,
            generics: vec!["T".into()],
            params: vec![
                ("v".into(), Ty::Param("T".into())),
                ("p".into(), Ty::Struct(si, vec![])),
                ("e".into(), Ty::Enum(ei)),
            ],
            ret: Some(Ty::Int),
            special: Special::CrossBlend,
            body: Body::CrossBlend(arm_sems, base_sem),
        });
        self.emitted_fns = self.fns.len();
    }

    fn gen_fallible_fn(&mut self) {
        let name = self.fresh(FN_POOL);
        let text = format!(
            "fn {}(n: Int) -> Int!:\n    if n < 0:\n        fail \"negative input\"\n    return n * 2\n\n",
            name
        );
        self.decl_text[0].push_str(&text);
        self.fns.push(FnDef {
            name,
            module: 0,
            generics: vec![],
            params: vec![("n".into(), Ty::Int)],
            ret: Some(Ty::Int),
            special: Special::Fallible,
            body: Body::Opaque,
        });
        self.emitted_fns = self.fns.len();
    }

    fn gen_apply_fn(&mut self) {
        let name = self.fresh(FN_POOL);
        let text = format!(
            "fn {}(f: fn(Int) -> Int, x: Int) -> Int:\n    return f(x)\n\n",
            name
        );
        self.decl_text[0].push_str(&text);
        self.fns.push(FnDef {
            name,
            module: 0,
            generics: vec![],
            params: vec![
                ("f".into(), Ty::FnT(vec![Ty::Int], Box::new(Ty::Int))),
                ("x".into(), Ty::Int),
            ],
            ret: Some(Ty::Int),
            special: Special::Apply,
            body: Body::Opaque,
        });
        self.emitted_fns = self.fns.len();
    }

    fn channel_payload(&mut self) -> Ty {
        let enums: Vec<usize> = self
            .enums
            .iter()
            .enumerate()
            .filter(|(_, e)| self.module_visible(e.module, 0))
            .map(|(i, _)| i)
            .collect();
        let structs: Vec<usize> = self
            .structs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.generics.is_empty() && self.module_visible(s.module, 0))
            .map(|(i, _)| i)
            .collect();
        let mut weights = vec![35u32, 20];
        weights.push(if enums.is_empty() { 0 } else { 25 });
        weights.push(if structs.is_empty() { 0 } else { 20 });
        match self.rng.weighted(&weights) {
            0 => Ty::Int,
            1 => Ty::Str,
            2 => Ty::Enum(enums[self.rng.below(enums.len())]),
            _ => Ty::Struct(structs[self.rng.below(structs.len())], vec![]),
        }
    }

    fn gen_workers(&mut self) {
        // a concrete pump for a chosen payload type
        let payload = self.channel_payload();
        let name = self.fresh(FN_POOL);
        let mut sc = Scope::new(0);
        sc.vars.push(("n".into(), Ty::Int, false));
        let send_expr = self.expr(&payload, 1, &sc);
        let text = format!(
            "fn {}(ch: Channel[{}], n: Int):\n    mut i := 0\n    while i < n:\n        ch.send({})\n        i = i + 1\n\n",
            name,
            self.ty_name(&payload),
            send_expr.code
        );
        self.decl_text[0].push_str(&text);
        self.fns.push(FnDef {
            name,
            module: 0,
            generics: vec![],
            params: vec![
                ("ch".into(), Ty::Chan(Box::new(payload.clone()))),
                ("n".into(), Ty::Int),
            ],
            ret: None,
            special: Special::Worker,
            body: Body::Opaque,
        });
        self.emitted_fns = self.fns.len();
        if self.feats.spawn_generic {
            // a generic pump: spawn of a generic-capturing call
            let gname = self.fresh(FN_POOL);
            let text = format!(
                "fn {}[T](ch: Channel[T], v: T, n: Int):\n    mut i := 0\n    while i < n:\n        ch.send(v)\n        i = i + 1\n\n",
                gname
            );
            self.decl_text[0].push_str(&text);
            self.fns.push(FnDef {
                name: gname,
                module: 0,
                generics: vec!["T".into()],
                params: vec![
                    ("ch".into(), Ty::Chan(Box::new(Ty::Param("T".into())))),
                    ("v".into(), Ty::Param("T".into())),
                    ("n".into(), Ty::Int),
                ],
                ret: None,
                special: Special::Worker,
                body: Body::Opaque,
            });
            self.emitted_fns = self.fns.len();
        }
    }

    fn gen_iface_pluck(&mut self) {
        let iface = 0usize;
        let iname = self.ifaces[iface].name.clone();
        let name = self.fresh(FN_POOL);
        let optional = self.ifaces[iface].opt_method && self.rng.chance(50);
        let text = if optional {
            format!(
                "fn {}[T: {}](c: T) -> T.Item?:\n    return c.pick()\n\n",
                name, iname
            )
        } else {
            format!(
                "fn {}[T: {}](c: T) -> T.Item:\n    return c.first()\n\n",
                name, iname
            )
        };
        self.decl_text[0].push_str(&text);
        // tracked outside the general fn table: T.Item returns need special
        // handling at each call site, done in the interface main block
        self.fns.push(FnDef {
            name,
            module: 0,
            generics: vec!["T".into()],
            params: vec![("c".into(), Ty::Param("T".into()))],
            ret: None,
            special: Special::Generic,
            body: Body::Pluck(optional),
        });
        // remember which pluck fn exists via impls; store optional flag in ifaces
        let last = self.fns.len() - 1;
        self.pluck_fn = Some((last, optional));
        self.emitted_fns = self.fns.len();
    }

    // ---------- expressions ----------

    fn vars_of(&self, sc: &Scope, ty: &Ty) -> Vec<String> {
        sc.vars
            .iter()
            .filter(|(_, t, _)| t == ty)
            .map(|(n, _, _)| n.clone())
            .collect()
    }

    /// field paths (one level) on in-scope struct vars that yield `ty`
    fn fields_of(&self, sc: &Scope, ty: &Ty) -> Vec<String> {
        let mut out = Vec::new();
        for (name, t, _) in &sc.vars {
            if let Ty::Struct(i, targs) = t {
                let sd = &self.structs[*i];
                let map: Vec<(String, Ty)> = sd
                    .generics
                    .iter()
                    .cloned()
                    .zip(targs.iter().cloned())
                    .collect();
                for f in &sd.fields {
                    if f.weak {
                        continue;
                    }
                    if &self.subst(&f.ty, &map) == ty {
                        out.push(format!("{}.{}", name, f.name));
                    }
                }
            }
        }
        out
    }

    fn callable_fns(&self, ty: &Ty, sc: &Scope) -> Vec<usize> {
        (0..self.emitted_fns)
            .filter(|&i| {
                let f = &self.fns[i];
                f.special == Special::Plain
                    && f.generics.is_empty()
                    && f.ret.as_ref() == Some(ty)
                    && self.module_visible(f.module, sc.module)
            })
            .collect()
    }

    fn expr(&mut self, ty: &Ty, depth: u32, sc: &Scope) -> Ex {
        let vars = self.vars_of(sc, ty);
        let fields = if depth > 0 { self.fields_of(sc, ty) } else { vec![] };
        let calls = if depth > 0 { self.callable_fns(ty, sc) } else { vec![] };
        // shared productions: var / field / call, else a per-type form
        if !vars.is_empty() && self.rng.chance(40) {
            let v = vars[self.rng.below(vars.len())].clone();
            return ex(v.clone(), E::Path(v));
        }
        if !fields.is_empty() && self.rng.chance(25) {
            let p = fields[self.rng.below(fields.len())].clone();
            return ex(p.clone(), E::Path(p));
        }
        if !calls.is_empty() && self.rng.chance(25) {
            let fi = calls[self.rng.below(calls.len())];
            let f = self.fns[fi].clone();
            let cname = self.fn_call_name(fi, sc.module);
            let mut acode = Vec::new();
            let mut aes = Vec::new();
            for (_, pt) in f.params.iter() {
                let a = self.expr(pt, depth.saturating_sub(1), sc);
                acode.push(a.code);
                aes.push(a.e);
            }
            return ex(format!("{}({})", cname, acode.join(", ")), E::Call(fi, aes));
        }
        match ty {
            Ty::Int => {
                if depth > 0 && self.rng.chance(30) {
                    let a = self.expr(&Ty::Int, depth - 1, sc);
                    let b = self.expr(&Ty::Int, depth - 1, sc);
                    let op = if self.rng.chance(70) { "+" } else { "*" };
                    let e = if op == "+" {
                        E::Add(Box::new(a.e), Box::new(b.e))
                    } else {
                        E::Mul(Box::new(a.e), Box::new(b.e))
                    };
                    ex(format!("({} {} {})", a.code, op, b.code), e)
                } else if depth > 0 && self.rng.chance(20) {
                    // a length read off something in scope
                    let mut lens = Vec::new();
                    for (n, t, _) in &sc.vars {
                        match t {
                            Ty::List(_) | Ty::Map(_, _) | Ty::Str => lens.push(n.clone()),
                            _ => {}
                        }
                    }
                    if lens.is_empty() {
                        let n = self.rng.range(0, 99);
                        ex(format!("{}", n), E::Lit(Val::I(n)))
                    } else {
                        let v = lens[self.rng.below(lens.len())].clone();
                        ex(format!("{}.len()", v), E::Len(Box::new(E::Path(v))))
                    }
                } else {
                    let n = self.rng.range(0, 99);
                    ex(format!("{}", n), E::Lit(Val::I(n)))
                }
            }
            Ty::Str => {
                if depth > 0 && self.rng.chance(25) {
                    let a = self.expr(&Ty::Str, depth - 1, sc);
                    let b = self.expr(&Ty::Str, depth - 1, sc);
                    ex(
                        format!("({} + {})", a.code, b.code),
                        E::Concat(Box::new(a.e), Box::new(b.e)),
                    )
                } else if depth > 0 && self.rng.chance(20) {
                    let n = self.expr(&Ty::Int, depth - 1, sc);
                    ex(format!("{}.to_string()", n.code), E::ToStr(Box::new(n.e)))
                } else {
                    let n = self.counter;
                    self.counter += 1;
                    ex(format!("\"s{}\"", n), E::Lit(Val::S(format!("s{}", n))))
                }
            }
            Ty::Bool => {
                let w = self.rng.weighted(&[25, 30, 20, 25]);
                match w {
                    0 => {
                        let b = self.rng.chance(50);
                        ex((if b { "true" } else { "false" }).into(), E::Lit(Val::B(b)))
                    }
                    1 => {
                        let a = self.expr(&Ty::Int, depth.saturating_sub(1), sc);
                        let b = self.expr(&Ty::Int, depth.saturating_sub(1), sc);
                        let op = *self.rng.pick(&["<", ">", "==", "!="]);
                        let cop = match op {
                            "<" => CmpOp::Lt,
                            ">" => CmpOp::Gt,
                            "==" => CmpOp::Eq,
                            _ => CmpOp::Ne,
                        };
                        ex(
                            format!("({} {} {})", a.code, op, b.code),
                            E::Cmp(cop, Box::new(a.e), Box::new(b.e)),
                        )
                    }
                    2 => {
                        // optional-vs-none probe on anything optional in scope
                        let opts: Vec<String> = sc
                            .vars
                            .iter()
                            .filter(|(_, t, _)| matches!(t, Ty::Opt(_)))
                            .map(|(n, _, _)| n.clone())
                            .collect();
                        if opts.is_empty() {
                            ex("true".into(), E::Lit(Val::B(true)))
                        } else {
                            let v = opts[self.rng.below(opts.len())].clone();
                            let eq = self.rng.chance(50);
                            let op = if eq { "==" } else { "!=" };
                            ex(
                                format!("({} {} none)", v, op),
                                E::IsNone(Box::new(E::Path(v)), !eq),
                            )
                        }
                    }
                    _ => {
                        let a = self.expr(&Ty::Str, depth.saturating_sub(1), sc);
                        let k = self.rng.range(0, 3);
                        ex(
                            format!("({}.len() > {})", a.code, k),
                            E::Cmp(
                                CmpOp::Gt,
                                Box::new(E::Len(Box::new(a.e))),
                                Box::new(E::Lit(Val::I(k))),
                            ),
                        )
                    }
                }
            }
            Ty::Opt(inner) => {
                if depth == 0 {
                    return ex("none".into(), E::Lit(Val::NoneV));
                }
                if self.rng.chance(22) {
                    ex("none".into(), E::Lit(Val::NoneV))
                } else {
                    // implicit T -> T? coercion in a typed position
                    self.expr(inner, depth - 1, sc)
                }
            }
            Ty::List(inner) => {
                if depth == 0 || self.rng.chance(15) {
                    ex("[]".into(), E::ListL(vec![]))
                } else {
                    let n = 1 + self.rng.below(3);
                    let mut codes = Vec::new();
                    let mut es = Vec::new();
                    for _ in 0..n {
                        let it = self.expr(inner, depth - 1, sc);
                        codes.push(it.code);
                        es.push(it.e);
                    }
                    ex(format!("[{}]", codes.join(", ")), E::ListL(es))
                }
            }
            Ty::Map(k, v) => {
                if depth == 0 || self.rng.chance(30) {
                    ex("{}".into(), E::MapL(vec![]))
                } else {
                    let n = 1 + self.rng.below(2);
                    let mut pairs = Vec::new();
                    let mut pes = Vec::new();
                    for _ in 0..n {
                        let kk = self.expr(k, 0, sc);
                        let vv = self.expr(v, depth - 1, sc);
                        pairs.push(format!("{}: {}", kk.code, vv.code));
                        pes.push((kk.e, vv.e));
                    }
                    ex(format!("{{{}}}", pairs.join(", ")), E::MapL(pes))
                }
            }
            Ty::Struct(i, targs) => {
                let sd = self.structs[*i].clone();
                let map: Vec<(String, Ty)> = sd
                    .generics
                    .iter()
                    .cloned()
                    .zip(targs.iter().cloned())
                    .collect();
                let mut args = Vec::new();
                let mut aes = Vec::new();
                for f in &sd.fields {
                    if f.weak {
                        args.push("none".to_string());
                        aes.push(E::Lit(Val::NoneV));
                        continue;
                    }
                    let fty = self.subst(&f.ty, &map);
                    let a = self.expr(&fty, depth.saturating_sub(1), sc);
                    args.push(a.code);
                    aes.push(a.e);
                }
                let head = if targs.is_empty() {
                    sd.name.clone()
                } else if self.rng.chance(75) {
                    let ta: Vec<String> = targs.iter().map(|t| self.ty_name(t)).collect();
                    format!("{}[{}]", sd.name, ta.join(", "))
                } else {
                    sd.name.clone()
                };
                let code = if targs.is_empty() && self.rng.chance(25) {
                    let named: Vec<String> = sd
                        .fields
                        .iter()
                        .zip(args.iter())
                        .map(|(f, a)| format!("{}: {}", f.name, a))
                        .collect();
                    format!("{}({})", head, named.join(", "))
                } else {
                    format!("{}({})", head, args.join(", "))
                };
                ex(code, E::StructL(*i, aes))
            }
            Ty::Enum(i) => {
                let ed = self.enums[*i].clone();
                let vi = self.rng.below(ed.variants.len());
                let (vname, payload) = &ed.variants[vi];
                if payload.is_empty() {
                    ex(format!("{}.{}", ed.name, vname), E::EnumL(*i, vi, vec![]))
                } else {
                    // an optional payload element does not accept a bare inner
                    // value at construction, so those slots use a real optional
                    // (a `none`, or an in-scope optional var of that type)
                    let mut args = Vec::new();
                    let mut aes = Vec::new();
                    for t in payload.iter() {
                        if matches!(t, Ty::Opt(_)) {
                            let vs = self.vars_of(sc, t);
                            if !vs.is_empty() && self.rng.chance(50) {
                                let v = vs[self.rng.below(vs.len())].clone();
                                args.push(v.clone());
                                aes.push(E::Path(v));
                            } else {
                                args.push("none".to_string());
                                aes.push(E::Lit(Val::NoneV));
                            }
                        } else {
                            let a = self.expr(t, depth.saturating_sub(1), sc);
                            args.push(a.code);
                            aes.push(a.e);
                        }
                    }
                    ex(
                        format!("{}.{}({})", ed.name, vname, args.join(", ")),
                        E::EnumL(*i, vi, aes),
                    )
                }
            }
            Ty::Param(_) => {
                // only reachable through in-scope values of that parameter type
                if let Some(v) = vars.first() {
                    ex(v.clone(), E::Path(v.clone()))
                } else {
                    // should not happen; a harmless fallback
                    ex("0".into(), E::Lit(Val::I(0)))
                }
            }
            Ty::Chan(inner) => ex(
                format!("Channel[{}]({})", self.ty_name(inner), self.rng.range(1, 4)),
                E::Lit(Val::Opaque),
            ),
            Ty::FnT(ps, r) => {
                let pnames: Vec<String> = (0..ps.len()).map(|k| format!("q{}", k)).collect();
                let sig: Vec<String> = pnames
                    .iter()
                    .zip(ps.iter())
                    .map(|(n, t)| format!("{}: {}", n, self.ty_name(t)))
                    .collect();
                let mut inner_sc = Scope::new(sc.module);
                for (n, t) in pnames.iter().zip(ps.iter()) {
                    inner_sc.vars.push((n.clone(), t.clone(), false));
                }
                // captures: primitives from the outer scope stay visible
                for (n, t, _) in &sc.vars {
                    if matches!(t, Ty::Int | Ty::Str | Ty::Bool) {
                        inner_sc.vars.push((n.clone(), t.clone(), false));
                    }
                }
                let body = self.expr(r, depth.saturating_sub(1), &inner_sc);
                ex(
                    format!("fn({}) => {}", sig.join(", "), body.code),
                    E::Lit(Val::Opaque),
                )
            }
        }
    }

    // ---------- printing values ----------

    fn printable(&self, code: &str, ty: &Ty) -> Option<Print> {
        match ty {
            Ty::Int | Ty::Str => Some(Print::Interp(code.into())),
            Ty::Bool => Some(Print::Concat(format!("{}.to_string()", code))),
            Ty::List(_) | Ty::Map(_, _) => Some(Print::Interp(format!("{}.len()", code))),
            Ty::Opt(inner) => match **inner {
                Ty::Int => Some(Print::Interp(format!("{}.unwrap_or(0)", code))),
                Ty::Str => Some(Print::Concat(format!("{}.unwrap_or(\"?\")", code))),
                _ => None,
            },
            Ty::Struct(i, targs) => {
                let sd = &self.structs[*i];
                let map: Vec<(String, Ty)> = sd
                    .generics
                    .iter()
                    .cloned()
                    .zip(targs.iter().cloned())
                    .collect();
                for f in &sd.fields {
                    if f.weak {
                        continue;
                    }
                    let fty = self.subst(&f.ty, &map);
                    if fty == Ty::Int || fty == Ty::Str {
                        return Some(Print::Interp(format!("{}.{}", code, f.name)));
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// the rendered value part of a print for `ty`, or None when the type is
    /// not printable and the emitter falls back to a fixed "ok" line. this
    /// mirrors `printable` branch for branch — the two must stay in lockstep.
    fn print_value_text(&self, ty: &Ty, val: &Val) -> Option<String> {
        match ty {
            Ty::Int | Ty::Str | Ty::Bool => Some(val.show()),
            Ty::List(_) | Ty::Map(_, _) => Some(val.len_of().to_string()),
            Ty::Opt(inner) => match **inner {
                Ty::Int => Some(match val {
                    Val::NoneV => "0".to_string(),
                    other => other.show(),
                }),
                Ty::Str => Some(match val {
                    Val::NoneV => "?".to_string(),
                    other => other.show(),
                }),
                _ => None,
            },
            Ty::Struct(i, targs) => {
                let sd = &self.structs[*i];
                let map: Vec<(String, Ty)> = sd
                    .generics
                    .iter()
                    .cloned()
                    .zip(targs.iter().cloned())
                    .collect();
                for (fx, f) in sd.fields.iter().enumerate() {
                    if f.weak {
                        continue;
                    }
                    let fty = self.subst(&f.ty, &map);
                    if fty == Ty::Int || fty == Ty::Str {
                        return match val {
                            Val::St(_, fields) => Some(fields[fx].show()),
                            other => panic!("pithgen eval: struct print on {:?}", other),
                        };
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// emit a labeled print of `code` and record the line it must produce.
    /// `live` is false when the print sits in a branch that will not run.
    fn emit_print(&mut self, out: &mut String, ind: usize, code: &str, ty: &Ty, val: &Val, live: bool) {
        let label = format!("p{}", self.counter);
        self.counter += 1;
        let pad = "    ".repeat(ind);
        match self.printable(code, ty) {
            Some(Print::Interp(frag)) if !frag.contains('"') => {
                out.push_str(&format!("{}print(\"{}: {{{}}}\")\n", pad, label, frag));
            }
            Some(Print::Interp(frag)) => {
                // a quote inside interpolation is legal but noisy; sidestep it
                out.push_str(&format!("{}print(\"{}: \" + {}.to_string())\n", pad, label, frag));
            }
            Some(Print::Concat(cexpr)) => {
                out.push_str(&format!("{}print(\"{}: \" + {})\n", pad, label, cexpr));
            }
            None => {
                out.push_str(&format!("{}print(\"{} ok\")\n", pad, label));
            }
        }
        if live {
            let text = match self.print_value_text(ty, val) {
                Some(v) => format!("{}: {}", label, v),
                None => format!("{} ok", label),
            };
            self.expect(text);
        }
    }

    // ---------- main generation ----------

    fn gen_main(&mut self) -> String {
        let mut sc = Scope::new(0);
        let mut blocks: Vec<String> = Vec::new();

        let mut order: Vec<u32> = (0..13).collect();
        // fisher-yates over the feature blocks so ordering varies by seed
        for i in (1..order.len()).rev() {
            let j = self.rng.below(i + 1);
            order.swap(i, j);
        }
        for k in order {
            let mut b = String::new();
            match k {
                0 => self.block_plain_structs(&mut b, &mut sc),
                1 => {
                    if self.feats.generic_structs {
                        self.block_generic_structs(&mut b, &mut sc)
                    }
                }
                2 => {
                    if self.feats.enums {
                        self.block_enums(&mut b, &mut sc)
                    }
                }
                3 => {
                    if self.feats.interfaces && !self.impls.is_empty() {
                        self.block_interfaces(&mut b, &mut sc)
                    }
                }
                4 => {
                    if self.feats.generic_fns {
                        self.block_generic_fns(&mut b, &mut sc)
                    }
                }
                5 => {
                    if self.feats.optionals {
                        self.block_optionals(&mut b, &mut sc)
                    }
                }
                6 => {
                    if self.feats.collections {
                        self.block_collections(&mut b, &mut sc)
                    }
                }
                7 => {
                    if self.feats.closures {
                        self.block_closures(&mut b, &mut sc)
                    }
                }
                8 => {
                    if self.feats.concurrency {
                        self.block_concurrency(&mut b, &mut sc)
                    }
                }
                9 => {
                    if self.feats.alias_dial {
                        self.block_alias(&mut b, &mut sc)
                    }
                }
                10 => {
                    if self.feats.weakrefs {
                        self.block_weak(&mut b, &mut sc)
                    }
                }
                11 => {
                    if self.feats.results {
                        self.block_results(&mut b, &mut sc)
                    }
                }
                _ => {
                    if self.feats.ownership {
                        self.block_ownership(&mut b, &mut sc)
                    }
                }
            }
            if !b.is_empty() {
                blocks.push(b);
            }
        }
        // a little filler so plain expression paths get exercised too
        let mut filler = String::new();
        let n = 1 + self.rng.below(3);
        for _ in 0..n {
            let ty = self.random_ty(0, 2);
            let code = self.expr(&ty, 2, &sc);
            let val = self.eval_in_main(&code.e);
            let name = self.let_var(&mut filler, &mut sc, &ty, &code.code, val.clone(), "v");
            if self.rng.chance(70) {
                self.emit_print(&mut filler, 1, &name, &ty, &val, true);
            }
        }
        blocks.push(filler);

        let mut out = String::from("fn main():\n");
        for b in blocks {
            out.push_str(&b);
        }
        out.push_str("    print(\"done\")\n");
        self.expect("done".to_string());
        out
    }

    fn let_var(
        &mut self,
        out: &mut String,
        sc: &mut Scope,
        ty: &Ty,
        code: &str,
        val: Val,
        prefix: &str,
    ) -> String {
        let name = format!("{}{}", prefix, self.counter);
        self.counter += 1;
        // an optional local assigned a bare inner value would infer the inner
        // type and lose its `?`, so optionals (and empty literals) are always
        // annotated with their declared type
        let annotate = code == "[]"
            || code == "{}"
            || code == "none"
            || matches!(ty, Ty::Opt(_));
        if annotate {
            out.push_str(&format!("    {}: {} := {}\n", name, self.ty_name(ty), code));
        } else {
            out.push_str(&format!("    {} := {}\n", name, code));
        }
        sc.vars.push((name.clone(), ty.clone(), false));
        self.env.push((name.clone(), val));
        name
    }

    fn block_plain_structs(&mut self, out: &mut String, sc: &mut Scope) {
        let cands: Vec<usize> = self
            .structs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.generics.is_empty())
            .map(|(i, _)| i)
            .collect();
        if cands.is_empty() {
            return;
        }
        let n = 1 + self.rng.below(2);
        for _ in 0..n {
            let si = cands[self.rng.below(cands.len())];
            let ty = Ty::Struct(si, vec![]);
            let code = self.expr(&ty, 2, sc);
            let val = self.eval_in_main(&code.e);
            let name = self.let_var(out, sc, &ty, &code.code, val.clone(), "s");
            self.emit_print(out, 1, &name, &ty, &val, true);
        }
    }

    fn block_generic_structs(&mut self, out: &mut String, sc: &mut Scope) {
        let cands: Vec<usize> = self
            .structs
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.generics.is_empty())
            .map(|(i, _)| i)
            .collect();
        if cands.is_empty() {
            return;
        }
        let si = cands[0];
        let n_inst = 1 + self.rng.below(2);
        for _ in 0..n_inst {
            let targ = self.concrete_ty(true);
            let ty = Ty::Struct(si, vec![targ.clone()]);
            let code = self.expr(&ty, 2, sc);
            let val = self.eval_in_main(&code.e);
            let name = self.let_var(out, sc, &ty, &code.code, val.clone(), "g");
            self.emit_print(out, 1, &name, &ty, &val, true);
            let sd = self.structs[si].clone();
            let map = vec![("T".to_string(), targ.clone())];
            // read the interesting fields back
            for f in &sd.fields {
                let fty = self.subst(&f.ty, &map);
                match &f.ty {
                    Ty::Opt(inner) if **inner == Ty::Param("T".into()) => {
                        // the M? field, probed through unwrap_or / == none / match
                        let path = format!("{}.peer", name);
                        let peer_val = self.field_val(&val, "peer");
                        let w = self.rng.weighted(&[35, 30, 35]);
                        match w {
                            0 => {
                                let pv = self.let_var(out, sc, &fty, &path, peer_val.clone(), "m");
                                self.emit_print(out, 1, &pv, &fty, &peer_val, true);
                            }
                            1 => {
                                out.push_str(&format!(
                                    "    if {} == none:\n        print(\"peer none\")\n",
                                    path
                                ));
                                if peer_val == Val::NoneV {
                                    self.expect("peer none".to_string());
                                }
                            }
                            _ => {
                                let inner_print = match self.printable("pv", &targ) {
                                    Some(Print::Interp(fr)) if !fr.contains('"') => {
                                        format!("print(\"peer {{{}}}\")", fr)
                                    }
                                    _ => "print(\"peer set\")".to_string(),
                                };
                                out.push_str(&format!(
                                    "    match {}:\n        pv => {}\n        none => print(\"peer empty\")\n",
                                    path, inner_print
                                ));
                                if peer_val == Val::NoneV {
                                    self.expect("peer empty".to_string());
                                } else {
                                    match self.print_value_text(&targ, &peer_val) {
                                        Some(t) => self.expect(format!("peer {}", t)),
                                        None => self.expect("peer set".to_string()),
                                    }
                                }
                            }
                        }
                    }
                    Ty::Opt(inner) => {
                        if let Ty::Struct(ssi, _) = **inner {
                            if ssi == si {
                                // the self-referential next pointer
                                let path = format!("{}.next", name);
                                out.push_str(&format!(
                                    "    match {}:\n        nx => print(\"next {{nx.tag}}\")\n        none => print(\"next end\")\n",
                                    path
                                ));
                                let next_val = self.field_val(&val, "next");
                                if next_val == Val::NoneV {
                                    self.expect("next end".to_string());
                                } else {
                                    let tag = self.field_val(&next_val, "tag");
                                    self.expect(format!("next {}", tag.show()));
                                }
                            }
                        }
                    }
                    _ => {}
                }
                let _ = fty;
            }
        }
    }

    fn block_enums(&mut self, out: &mut String, sc: &mut Scope) {
        let cands: Vec<usize> = (0..self.enums.len()).collect();
        if cands.is_empty() {
            return;
        }
        let ei = cands[self.rng.below(cands.len())];
        let ty = Ty::Enum(ei);
        let code = self.expr(&ty, 2, sc);
        let val = self.eval_in_main(&code.e);
        let name = self.let_var(out, sc, &ty, &code.code, val.clone(), "e");
        self.emit_match_enum(out, sc, &name, ei, 1, &val, true);
    }

    /// emit an exhaustive match over `subject` and record the arm line the
    /// known value takes. `live` is false when the match sits in a dead branch
    /// (the value is then allowed to be a placeholder).
    fn emit_match_enum(
        &mut self,
        out: &mut String,
        _sc: &Scope,
        subject: &str,
        ei: usize,
        ind: usize,
        val: &Val,
        live: bool,
    ) {
        let ed = self.enums[ei].clone();
        let pad = "    ".repeat(ind);
        out.push_str(&format!("{}match {}:\n", pad, subject));
        for (vname, payload) in &ed.variants {
            let arm_pad = "    ".repeat(ind + 1);
            if payload.is_empty() {
                out.push_str(&format!(
                    "{}{}.{} => print(\"{} hit\")\n",
                    arm_pad, ed.name, vname, vname
                ));
            } else {
                let binds: Vec<String> = (0..payload.len()).map(|k| format!("x{}", k)).collect();
                // a payload binding has the payload's declared type, so an
                // optional payload routes through printable's unwrap_or forms
                // like any other optional value
                let frag = match self.printable(&binds[0], &payload[0]) {
                    Some(Print::Interp(fr)) if !fr.contains('"') => {
                        format!("print(\"{} {{{}}}\")", vname, fr)
                    }
                    Some(Print::Concat(ce)) => {
                        format!("print(\"{} \" + {})", vname, ce)
                    }
                    _ => format!("print(\"{} bound\")", vname),
                };
                out.push_str(&format!(
                    "{}{}.{}({}) => {}\n",
                    arm_pad,
                    ed.name,
                    vname,
                    binds.join(", "),
                    frag
                ));
            }
        }
        if live {
            let (vi, pvals) = match val {
                Val::En(e2, vi, pvals) => {
                    assert_eq!(*e2, ei, "pithgen eval: enum value/type mismatch");
                    (*vi, pvals)
                }
                other => panic!("pithgen eval: match subject {:?}", other),
            };
            let (vname, payload) = &ed.variants[vi];
            let text = if payload.is_empty() {
                format!("{} hit", vname)
            } else {
                match self.print_value_text(&payload[0], &pvals[0]) {
                    Some(t) => format!("{} {}", vname, t),
                    None => format!("{} bound", vname),
                }
            };
            self.expect(text);
        }
    }

    fn block_interfaces(&mut self, out: &mut String, sc: &mut Scope) {
        let impls: Vec<usize> = (0..self.impls.len()).collect();
        for &ii in impls.iter().take(2) {
            let (si, item) = (self.impls[ii].struct_idx, self.impls[ii].item.clone());
            let ty = Ty::Struct(si, vec![]);
            let code = self.expr(&ty, 2, sc);
            let objv = self.eval_in_main(&code.e);
            let obj = self.let_var(out, sc, &ty, &code.code, objv.clone(), "o");
            let fcall = format!("{}.first()", obj);
            let fval = self.eval_impl_first(ii, &objv);
            let fv = self.let_var(out, sc, &item, &fcall, fval.clone(), "f");
            if let Ty::Enum(ei) = item {
                self.emit_match_enum(out, sc, &fv, ei, 1, &fval, true);
            } else {
                self.emit_print(out, 1, &fv, &item, &fval, true);
            }
            if self.ifaces[self.impls[ii].iface].opt_method {
                // the optional-of-associated-type extraction path
                let pcall = format!("{}.pick()", obj);
                let opt_ty = Ty::Opt(Box::new(item.clone()));
                let pickv = self.eval_impl_pick(ii, &objv);
                let ov = self.let_var(out, sc, &opt_ty, &pcall, pickv.clone(), "u");
                if let Ty::Enum(ei) = item {
                    let got = format!("got{}", self.counter);
                    self.counter += 1;
                    out.push_str(&format!("    if let {} = {}:\n", got, ov));
                    self.emit_match_enum(out, sc, &got, ei, 2, &pickv, pickv != Val::NoneV);
                    out.push_str(&format!(
                        "    if {} == none:\n        print(\"pick none\")\n",
                        ov
                    ));
                    if pickv == Val::NoneV {
                        self.expect("pick none".to_string());
                    }
                } else {
                    match self.printable(&ov, &opt_ty) {
                        Some(Print::Interp(fr)) if !fr.contains('"') => {
                            out.push_str(&format!("    print(\"picked: {{{}}}\")\n", fr));
                            let t = self.print_value_text(&opt_ty, &pickv).unwrap();
                            self.expect(format!("picked: {}", t));
                        }
                        Some(Print::Concat(ce)) => {
                            out.push_str(&format!("    print(\"picked: \" + {})\n", ce));
                            let t = self.print_value_text(&opt_ty, &pickv).unwrap();
                            self.expect(format!("picked: {}", t));
                        }
                        _ => {}
                    }
                }
            }
            // route the object through the generic bound fn when it exists
            if let Some((fi, optional)) = self.pluck_fn {
                if self.impls[ii].iface == 0 && self.rng.chance(70) {
                    let fname = self.fns[fi].name.clone();
                    let call = format!("{}({})", fname, obj);
                    let rty = if optional {
                        Ty::Opt(Box::new(item.clone()))
                    } else {
                        item.clone()
                    };
                    let rval = if optional {
                        self.eval_impl_pick(ii, &objv)
                    } else {
                        self.eval_impl_first(ii, &objv)
                    };
                    let rv = self.let_var(out, sc, &rty, &call, rval.clone(), "k");
                    if let Ty::Enum(ei) = item {
                        if optional {
                            let plucked = format!("got{}", self.counter);
                            self.counter += 1;
                            out.push_str(&format!("    if let {} = {}:\n", plucked, rv));
                            self.emit_match_enum(out, sc, &plucked, ei, 2, &rval, rval != Val::NoneV);
                        } else {
                            self.emit_match_enum(out, sc, &rv, ei, 1, &rval, true);
                        }
                    } else {
                        self.emit_print(out, 1, &rv, &rty, &rval, true);
                    }
                }
            }
        }
    }

    fn block_generic_fns(&mut self, out: &mut String, sc: &mut Scope) {
        let gens: Vec<usize> = (0..self.fns.len())
            .filter(|&i| {
                let f = &self.fns[i];
                (f.special == Special::Generic || f.special == Special::CrossBlend)
                    && !f.generics.is_empty()
                    && f.ret.is_some()
                    && self.module_visible(f.module, 0)
            })
            .collect();
        for &fi in gens.iter() {
            if self.rng.chance(25) {
                continue;
            }
            let f = self.fns[fi].clone();
            let n_calls = 1 + self.rng.below(2);
            for _ in 0..n_calls {
                let targ = self.concrete_ty(true);
                let map = vec![("T".to_string(), targ.clone())];
                let cname = self.fn_call_name(fi, 0);
                // coercion of a bare inner value into an optional parameter
                // does not fire inside a generic specialization, so every
                // optional argument is materialized as a typed optional local
                // first. a `T?` parameter also cannot pin T from a plain value,
                // so any param mentioning a type parameter under an optional
                // forces explicit type arguments.
                let mut explicit = self.rng.chance(45);
                let mut args: Vec<String> = Vec::new();
                let mut argvals: Vec<Val> = Vec::new();
                for (_, pt) in f.params.iter() {
                    let concrete = self.subst(pt, &map);
                    if matches!(concrete, Ty::Opt(_)) {
                        let code = self.expr(&concrete, 2, sc);
                        let v = self.eval_in_main(&code.e);
                        let ov = self.let_var(out, sc, &concrete, &code.code, v.clone(), "oa");
                        args.push(ov);
                        argvals.push(v);
                        if param_under_opt(pt) {
                            explicit = true;
                        }
                    } else {
                        let a = self.expr(&concrete, 2, sc);
                        argvals.push(self.eval_in_main(&a.e));
                        args.push(a.code);
                    }
                }
                let call = if explicit {
                    format!("{}[{}]({})", cname, self.ty_name(&targ), args.join(", "))
                } else {
                    format!("{}({})", cname, args.join(", "))
                };
                let rty = self.subst(f.ret.as_ref().unwrap(), &map);
                let rval = self.eval_call(fi, argvals);
                let rv = self.let_var(out, sc, &rty, &call, rval.clone(), "r");
                if let Ty::Enum(ei) = rty {
                    self.emit_match_enum(out, sc, &rv, ei, 1, &rval, true);
                } else {
                    self.emit_print(out, 1, &rv, &rty, &rval, true);
                }
            }
        }
    }

    fn block_optionals(&mut self, out: &mut String, sc: &mut Scope) {
        // build a couple of optional locals, then probe them every way
        let inner = if self.rng.chance(55) { Ty::Int } else { Ty::Str };
        let oty = Ty::Opt(Box::new(inner.clone()));
        let some_code = self.expr(&inner, 1, sc);
        let some_val = self.eval_in_main(&some_code.e);
        let a = self.let_var(out, sc, &oty, &some_code.code, some_val, "op");
        let b_name = format!("op{}", self.counter);
        self.counter += 1;
        out.push_str(&format!("    {}: {} := none\n", b_name, self.ty_name(&oty)));
        sc.vars.push((b_name.clone(), oty.clone(), false));
        self.env.push((b_name.clone(), Val::NoneV));
        for v in [a.clone(), b_name.clone()] {
            let vv = self.resolve(&v, &self.env);
            match self.rng.weighted(&[30, 30, 20, 20]) {
                0 => {
                    out.push_str(&format!(
                        "    if {} != none:\n        print(\"{} set\")\n    if {} == none:\n        print(\"{} unset\")\n",
                        v, v, v, v
                    ));
                    if vv != Val::NoneV {
                        self.expect(format!("{} set", v));
                    } else {
                        self.expect(format!("{} unset", v));
                    }
                }
                1 => {
                    self.emit_print(out, 1, &v, &oty, &vv, true);
                }
                2 => {
                    out.push_str(&format!("    if let got{} = {}:\n", self.counter, v));
                    let gname = format!("got{}", self.counter);
                    self.counter += 1;
                    self.emit_print(out, 2, &gname, &inner, &vv, vv != Val::NoneV);
                }
                _ => {
                    let frag = match self.printable("w", &inner) {
                        Some(Print::Interp(fr)) if !fr.contains('"') => format!("print(\"some {{{}}}\")", fr),
                        _ => "print(\"some\")".to_string(),
                    };
                    out.push_str(&format!(
                        "    match {}:\n        w => {}\n        none => print(\"none arm\")\n",
                        v, frag
                    ));
                    if vv == Val::NoneV {
                        self.expect("none arm".to_string());
                    } else {
                        match self.print_value_text(&inner, &vv) {
                            Some(t) => self.expect(format!("some {}", t)),
                            None => self.expect("some".to_string()),
                        }
                    }
                }
            }
        }
    }

    fn block_collections(&mut self, out: &mut String, sc: &mut Scope) {
        // a mut list with pushes, reads by index and len
        let elem = match self.rng.weighted(&[40, 30, 30]) {
            0 => Ty::Int,
            1 => Ty::Str,
            _ => self.concrete_ty(true),
        };
        let lname = format!("xs{}", self.counter);
        self.counter += 1;
        out.push_str(&format!(
            "    mut {}: List[{}] := []\n",
            lname,
            self.ty_name(&elem)
        ));
        let n_push = 2 + self.rng.below(2);
        let mut elem_vals = Vec::new();
        for _ in 0..n_push {
            let e = self.expr(&elem, 1, sc);
            elem_vals.push(self.eval_in_main(&e.e));
            out.push_str(&format!("    {}.push({})\n", lname, e.code));
        }
        out.push_str(&format!("    print(\"len: {{{}.len()}}\")\n", lname));
        self.expect(format!("len: {}", n_push));
        let idx = self.rng.below(2);
        let idx_code = format!("{}[{}]", lname, idx);
        let iv_val = elem_vals[idx].clone();
        let iv = self.let_var(out, sc, &elem, &idx_code, iv_val.clone(), "el");
        self.emit_print(out, 1, &iv, &elem, &iv_val, true);
        sc.vars
            .push((lname.clone(), Ty::List(Box::new(elem.clone())), true));
        self.env.push((lname.clone(), Val::L(elem_vals)));

        // an empty literal straight into an argument position
        let list_fns: Vec<usize> = (0..self.emitted_fns)
            .filter(|&i| {
                let f = &self.fns[i];
                f.special == Special::Plain
                    && f.generics.is_empty()
                    && self.module_visible(f.module, 0)
                    && f.params.iter().any(|(_, t)| matches!(t, Ty::List(_)))
                    && f.ret.is_some()
            })
            .collect();
        if let Some(&fi) = list_fns.first() {
            let f = self.fns[fi].clone();
            let cname = self.fn_call_name(fi, 0);
            let mut args: Vec<String> = Vec::new();
            let mut argvals: Vec<Val> = Vec::new();
            for (_, t) in f.params.iter() {
                if matches!(t, Ty::List(_)) {
                    args.push("[]".to_string());
                    argvals.push(Val::L(vec![]));
                } else {
                    let a = self.expr(t, 1, sc);
                    argvals.push(self.eval_in_main(&a.e));
                    args.push(a.code);
                }
            }
            let rty = f.ret.clone().unwrap();
            let rval = self.eval_call(fi, argvals);
            let rv = self.let_var(
                out,
                sc,
                &rty,
                &format!("{}({})", cname, args.join(", ")),
                rval.clone(),
                "z",
            );
            self.emit_print(out, 1, &rv, &rty, &rval, true);
        }

        // a map with inserts and get_default
        if self.rng.chance(60) {
            let vt = if self.rng.chance(60) { Ty::Int } else { Ty::Str };
            let mname = format!("mp{}", self.counter);
            self.counter += 1;
            out.push_str(&format!(
                "    mut {}: Map[String, {}] := {{}}\n",
                mname,
                self.ty_name(&vt)
            ));
            let val = self.expr(&vt, 1, sc);
            let vval = self.eval_in_main(&val.e);
            out.push_str(&format!("    {}.insert(\"k1\", {})\n", mname, val.code));
            let dflt = self.expr(&vt, 0, sc);
            let gv = self.let_var(
                out,
                sc,
                &vt,
                &format!("{}.get_default(\"k1\", {})", mname, dflt.code),
                vval.clone(),
                "mv",
            );
            self.emit_print(out, 1, &gv, &vt, &vval, true);
        }

        // a nested list, read through len
        if self.rng.chance(40) {
            let nl = format!("nn{}", self.counter);
            self.counter += 1;
            let inner1 = self.expr(&Ty::List(Box::new(Ty::Int)), 1, sc);
            let inner2 = self.expr(&Ty::List(Box::new(Ty::Int)), 1, sc);
            let l1 = self.eval_in_main(&inner1.e);
            out.push_str(&format!("    {} := [{}, {}]\n", nl, inner1.code, inner2.code));
            out.push_str(&format!(
                "    print(\"nested: {{{}.len()}} {{{}[0].len()}}\")\n",
                nl, nl
            ));
            self.expect(format!("nested: 2 {}", l1.len_of()));
        }
    }

    fn block_closures(&mut self, out: &mut String, sc: &mut Scope) {
        let capn = self.rng.range(1, 20);
        let cap = self.let_var(out, sc, &Ty::Int, &format!("{}", capn), Val::I(capn), "c");
        let fname = format!("fx{}", self.counter);
        self.counter += 1;
        let block_form = !self.rng.chance(60);
        if !block_form {
            out.push_str(&format!("    {} := fn(x: Int) => x + {}\n", fname, cap));
        } else {
            // block lambda: no return annotation on purpose (not allowed)
            out.push_str(&format!(
                "    {} := fn(x: Int):\n        mut acc := x + {}\n        if acc > 50:\n            acc = acc + 1\n        return acc\n",
                fname, cap
            ));
        }
        let lam = |x: i64| -> i64 {
            let acc = x + capn;
            if block_form && acc > 50 {
                acc + 1
            } else {
                acc
            }
        };
        let dvn = self.rng.range(0, 9);
        let dval = lam(dvn);
        let dv = self.let_var(
            out,
            sc,
            &Ty::Int,
            &format!("{}({})", fname, dvn),
            Val::I(dval),
            "d",
        );
        self.emit_print(out, 1, &dv, &Ty::Int, &Val::I(dval), true);
        // hand a lambda to the apply fn when it exists
        let apply: Vec<usize> = (0..self.fns.len())
            .filter(|&i| self.fns[i].special == Special::Apply)
            .collect();
        if let Some(&fi) = apply.first() {
            let cname = self.fns[fi].name.clone();
            let avn = self.rng.range(0, 9);
            let aval = lam(avn);
            let av = self.let_var(
                out,
                sc,
                &Ty::Int,
                &format!("{}({}, {})", cname, fname, avn),
                Val::I(aval),
                "d",
            );
            self.emit_print(out, 1, &av, &Ty::Int, &Val::I(aval), true);
            if self.rng.chance(50) {
                let bvn = self.rng.range(0, 5);
                let bval = bvn * 2 + capn;
                let bv = self.let_var(
                    out,
                    sc,
                    &Ty::Int,
                    &format!("{}(fn(y: Int) => y * 2 + {}, {})", cname, cap, bvn),
                    Val::I(bval),
                    "d",
                );
                self.emit_print(out, 1, &bv, &Ty::Int, &Val::I(bval), true);
            }
        }
    }

    fn block_concurrency(&mut self, out: &mut String, sc: &mut Scope) {
        let workers: Vec<usize> = (0..self.fns.len())
            .filter(|&i| self.fns[i].special == Special::Worker)
            .collect();
        for &wi in &workers {
            let f = self.fns[wi].clone();
            let n_msgs = self.rng.range(2, 5);
            if f.generics.is_empty() {
                let payload = match &f.params[0].1 {
                    Ty::Chan(t) => (**t).clone(),
                    _ => Ty::Int,
                };
                let ch = format!("ch{}", self.counter);
                self.counter += 1;
                let cap = self.rng.range(1, 4);
                out.push_str(&format!(
                    "    {} := Channel[{}]({})\n",
                    ch,
                    self.ty_name(&payload),
                    cap
                ));
                let task = format!("tk{}", self.counter);
                self.counter += 1;
                out.push_str(&format!("    {} := spawn {}({}, {})\n", task, f.name, ch, n_msgs));
                let seen = format!("sn{}", self.counter);
                self.counter += 1;
                let i = format!("i{}", self.counter);
                self.counter += 1;
                out.push_str(&format!("    mut {} := 0\n    mut {} := 0\n", seen, i));
                out.push_str(&format!("    while {} < {}:\n", i, n_msgs));
                let rv = format!("rc{}", self.counter);
                self.counter += 1;
                out.push_str(&format!("        {} := {}.recv()\n", rv, ch));
                out.push_str(&format!(
                    "        if {} != none:\n            {} = {} + 1\n",
                    rv, seen, seen
                ));
                out.push_str(&format!("        {} = {} + 1\n", i, i));
                out.push_str(&format!("    await {}\n", task));
                out.push_str(&format!("    print(\"seen: {{{}}}\")\n", seen));
                self.expect(format!("seen: {}", n_msgs));
                // a last look at the payload through match, for enum payloads
                if let Ty::Enum(_) = payload {
                    let one = format!("lp{}", self.counter);
                    self.counter += 1;
                    let e = self.expr(&payload, 1, sc);
                    let eval = self.eval_in_main(&e.e);
                    out.push_str(&format!("    {} := {}\n", one, e.code));
                    if let Ty::Enum(ei) = payload {
                        self.emit_match_enum(out, sc, &one, ei, 1, &eval, true);
                    }
                }
            } else {
                // spawn of a generic call: the channel and the captured value
                // share the same substitution
                let payload = self.channel_payload();
                let ch = format!("gc{}", self.counter);
                self.counter += 1;
                out.push_str(&format!(
                    "    {} := Channel[{}]({})\n",
                    ch,
                    self.ty_name(&payload),
                    n_msgs
                ));
                let val = self.expr(&payload, 1, sc);
                let task = format!("gt{}", self.counter);
                self.counter += 1;
                let explicit = self.rng.chance(50);
                let call = if explicit {
                    format!("{}[{}]({}, {}, {})", f.name, self.ty_name(&payload), ch, val.code, n_msgs)
                } else {
                    format!("{}({}, {}, {})", f.name, ch, val.code, n_msgs)
                };
                out.push_str(&format!("    {} := spawn {}\n", task, call));
                let i = format!("i{}", self.counter);
                self.counter += 1;
                let seen = format!("sn{}", self.counter);
                self.counter += 1;
                out.push_str(&format!("    mut {} := 0\n    mut {} := 0\n", seen, i));
                out.push_str(&format!("    while {} < {}:\n", i, n_msgs));
                let rv = format!("rc{}", self.counter);
                self.counter += 1;
                out.push_str(&format!("        {} := {}.recv()\n", rv, ch));
                out.push_str(&format!(
                    "        if {} != none:\n            {} = {} + 1\n",
                    rv, seen, seen
                ));
                out.push_str(&format!("        {} = {} + 1\n", i, i));
                out.push_str(&format!("    await {}\n", task));
                out.push_str(&format!("    print(\"gseen: {{{}}}\")\n", seen));
                self.expect(format!("gseen: {}", n_msgs));
            }
        }
        // spawn of a call that takes a capturing closure
        if self.feats.spawn_closure {
            let apply: Vec<usize> = (0..self.fns.len())
                .filter(|&i| self.fns[i].special == Special::Apply)
                .collect();
            if let Some(&fi) = apply.first() {
                let cname = self.fns[fi].name.clone();
                let basen = self.rng.range(1, 30);
                let base = self.let_var(out, sc, &Ty::Int, &format!("{}", basen), Val::I(basen), "b");
                let task = format!("ct{}", self.counter);
                self.counter += 1;
                let argn = self.rng.range(0, 9);
                out.push_str(&format!(
                    "    {} := spawn {}(fn(y: Int) => y + {}, {})\n",
                    task, cname, base, argn
                ));
                let got = format!("cw{}", self.counter);
                self.counter += 1;
                out.push_str(&format!("    {} := await {}\n", got, task));
                out.push_str(&format!("    print(\"closure task: {{{}}}\")\n", got));
                self.expect(format!("closure task: {}", basen + argn));
            }
        }
    }

    fn block_alias(&mut self, out: &mut String, sc: &mut Scope) {
        // call a helper fn through its module alias
        let helper_fns: Vec<usize> = (0..self.emitted_fns)
            .filter(|&i| {
                let f = &self.fns[i];
                f.module > 0 && f.special == Special::Plain && f.generics.is_empty() && f.ret.is_some()
            })
            .collect();
        if let Some(&fi) = helper_fns.first() {
            let f = self.fns[fi].clone();
            let cname = self.fn_call_name(fi, 0);
            let mut args: Vec<String> = Vec::new();
            let mut argvals: Vec<Val> = Vec::new();
            for (_, t) in f.params.iter() {
                let a = self.expr(t, 1, sc);
                argvals.push(self.eval_in_main(&a.e));
                args.push(a.code);
            }
            let rty = f.ret.clone().unwrap();
            let rval = self.eval_call(fi, argvals);
            let rv = self.let_var(
                out,
                sc,
                &rty,
                &format!("{}({})", cname, args.join(", ")),
                rval.clone(),
                "av",
            );
            self.emit_print(out, 1, &rv, &rty, &rval, true);
        }
        // reference enum variants through the alias — payload form, and the
        // no-payload form when the dial is on
        let helper_enums: Vec<usize> = (0..self.enums.len())
            .filter(|&i| self.enums[i].module > 0)
            .collect();
        if let Some(&ei) = helper_enums.first() {
            let ed = self.enums[ei].clone();
            let alias = module_alias(ed.module, 0);
            if self.feats.alias_nopayload {
                if let Some(vi) = ed.variants.iter().position(|(_, p)| p.is_empty()) {
                    let vname = &ed.variants[vi].0;
                    let name = format!("an{}", self.counter);
                    self.counter += 1;
                    out.push_str(&format!(
                        "    {} := {}.{}.{}\n",
                        name, alias, ed.name, vname
                    ));
                    let val = Val::En(ei, vi, vec![]);
                    sc.vars.push((name.clone(), Ty::Enum(ei), false));
                    self.env.push((name.clone(), val.clone()));
                    self.emit_match_enum(out, sc, &name, ei, 1, &val, true);
                }
            }
            if self.rng.chance(60) {
                if let Some(vi) = ed.variants.iter().position(|(_, p)| !p.is_empty()) {
                    let (vname, payload) = ed.variants[vi].clone();
                    let mut args: Vec<String> = Vec::new();
                    let mut argvals: Vec<Val> = Vec::new();
                    for t in payload.iter() {
                        if matches!(t, Ty::Opt(_)) {
                            args.push("none".into());
                            argvals.push(Val::NoneV);
                        } else {
                            let tt = t.clone();
                            let a = self.expr(&tt, 1, sc);
                            argvals.push(self.eval_in_main(&a.e));
                            args.push(a.code);
                        }
                    }
                    let name = format!("ap{}", self.counter);
                    self.counter += 1;
                    out.push_str(&format!(
                        "    {} := {}.{}.{}({})\n",
                        name,
                        alias,
                        ed.name,
                        vname,
                        args.join(", ")
                    ));
                    let val = Val::En(ei, vi, argvals);
                    sc.vars.push((name.clone(), Ty::Enum(ei), false));
                    self.env.push((name.clone(), val.clone()));
                    self.emit_match_enum(out, sc, &name, ei, 1, &val, true);
                }
            }
        }
    }

    fn block_weak(&mut self, out: &mut String, sc: &mut Scope) {
        let cands: Vec<usize> = self
            .structs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.generics.is_empty() && s.module == 0)
            .map(|(i, _)| i)
            .collect();
        if cands.is_empty() {
            return;
        }
        let si = cands[self.rng.below(cands.len())];
        let ty = Ty::Struct(si, vec![]);
        let code = self.expr(&ty, 1, sc);
        let sval = self.eval_in_main(&code.e);
        let strong = self.let_var(out, sc, &ty, &code.code, sval.clone(), "st");
        let w = format!("wk{}", self.counter);
        self.counter += 1;
        out.push_str(&format!("    weak {} := {}\n", w, strong));
        let frag = match self.printable("alive", &ty) {
            Some(Print::Interp(fr)) if !fr.contains('"') => format!("print(\"live {{{}}}\")", fr),
            _ => "print(\"live\")".to_string(),
        };
        out.push_str(&format!(
            "    match {}:\n        alive => {}\n        none => print(\"gone\")\n",
            w, frag
        ));
        // the strong ref is still in scope, so the weak ref is always alive
        match self.print_value_text(&ty, &sval) {
            Some(t) => self.expect(format!("live {}", t)),
            None => self.expect("live".to_string()),
        }
    }

    fn block_results(&mut self, out: &mut String, sc: &mut Scope) {
        let risky: Vec<usize> = (0..self.fns.len())
            .filter(|&i| self.fns[i].special == Special::Fallible)
            .collect();
        if let Some(&fi) = risky.first() {
            let cname = self.fns[fi].name.clone();
            let okn = self.rng.range(0, 20);
            // the fallible body doubles a non-negative input, and a negative
            // one lands in the catch arm
            let ok = self.let_var(
                out,
                sc,
                &Ty::Int,
                &format!("{}({}) catch (0 - 1)", cname, okn),
                Val::I(okn * 2),
                "rr",
            );
            self.emit_print(out, 1, &ok, &Ty::Int, &Val::I(okn * 2), true);
            let badn = self.rng.range(1, 5);
            let bad = self.let_var(
                out,
                sc,
                &Ty::Int,
                &format!("{}((0 - {})) catch (0 - 9)", cname, badn),
                Val::I(-9),
                "rr",
            );
            self.emit_print(out, 1, &bad, &Ty::Int, &Val::I(-9), true);
        }
    }

    // ---------- ownership shapes (the sanitize hunt) ----------
    //
    // these target the emitter's borrow-vs-own decision: whether a value
    // carries its own count. the three shapes that broke in #1270 were a loop
    // whose body drops the element it walks, an argument read out of a field
    // or an element that the callee overwrites, and a weak field read off a
    // temporary bound to a name. the runtime toolchain runs these clean; the
    // sanitize oracle links an asan runtime and runs them with the struct
    // freelist off, where a stale read is a use-after-free. each printed value
    // is computed here, so the differential oracle also catches a silent wrong
    // answer when a slot is recycled with new contents.

    /// a fresh, distinctive heap string. an over-release that frees it would
    /// read back as filler of a similar size, never this.
    fn olit(&mut self, tag: char) -> String {
        let n = self.counter;
        self.counter += 1;
        format!("{}-{}-ownpayload-xxxxxxxx", tag, n)
    }

    /// the fixed helper declarations the ownership block leans on, appended to
    /// module 0 once per program. unused helpers are harmless.
    fn emit_ownership_decls(&mut self) {
        let decls = r#"
struct ONode:
    mut name: String
    mut val: Int
    mut tags: List[String]
    mut next: ONode?
    weak back: ONode?

struct OHolder:
    mut child: ONode?

enum OSlot:
    Empty
    Holds(ONode)

mut og_xs: List[String] := []

fn ofirst_after_clear(xs: List[String]) -> String:
    for s in xs:
        xs.clear()
        ochurn()
        return s
    return "empty"

fn ochurn():
    mut junk: List[String] := []
    mut i := 0
    while i < 40:
        junk.push("ofiller-{i}-ownpayload-x")
        i = i + 1

fn orefill(xs: List[String]):
    mut i := 0
    while i < 40:
        xs.push("ofiller-{i}-ownpayload-x")
        i = i + 1

fn orefill_nodes(xs: List[ONode]):
    mut i := 0
    while i < 40:
        xs.push(ONode(name: "ofiller-{i}-ownpayload-x", val: i, tags: ["oft-{i}-ownpayload-x"], next: none, back: none))
        i = i + 1

fn owipe(xs: List[String]):
    xs.clear()
    orefill(xs)

fn oclear_global():
    og_xs.clear()
    orefill(og_xs)

fn opair(an: String, bn: String) -> ONode:
    a := ONode(name: an, val: 1, tags: [], next: none, back: none)
    b := ONode(name: bn, val: 2, tags: [], next: none, back: none)
    a.next = b
    b.back = a
    return a

fn ooverwrite_field(o: ONode, s: String) -> String:
    o.name = "oreplaced-ownpayload-xx"
    ochurn()
    return s

fn oclear_list(xs: List[String], s: String) -> String:
    xs.clear()
    ochurn()
    return s

fn odrop_child(h: OHolder, c: ONode?) -> String:
    h.child = none
    ochurn()
    return c.value().name

fn orun_then_read(f: fn(), s: String) -> String:
    f()
    ochurn()
    return s

impl ONode:
    fn rename_and_read(s: String) -> String:
        self.name = "orenamed-ownpayload-xx"
        ochurn()
        return s
"#;
        self.decl_text[0].push_str(decls);
        if self.opts.pending || self.opts.only.is_some() {
            self.decl_text[0].push_str(PENDING_DECLS);
        }
        if self.opts.rounds {
            self.decl_text[0].push_str(ROUNDS_DECLS);
        }
    }

    fn block_ownership(&mut self, out: &mut String, _sc: &mut Scope) {
        // pick a varied subset so programs differ by seed, but keep the odds
        // high that any given shape appears across a batch
        let n = if self.opts.pending { OWN_SHAPES } else { OWN_SHAPES_SETTLED };
        let mut ids: Vec<u32> = (0..n).collect();
        for i in (1..ids.len()).rev() {
            let j = self.rng.below(i + 1);
            ids.swap(i, j);
        }
        if let Some(only) = self.opts.only.clone() {
            ids = only;
        }
        let mut emitted = 0;
        for k in ids {
            // force the first couple regardless so the block is never empty
            if self.opts.only.is_none() && emitted >= 2 && !self.rng.chance(60) {
                continue;
            }
            if self.opts.rounds {
                self.emit_shape_rounds(out, k);
            } else {
                self.emit_ownership_shape(out, k);
            }
            emitted += 1;
        }
    }

    /// leak mode: the shape becomes the body of its own fn, which main calls
    /// PITH_LEAK_ROUNDS times. only the first round prints, so the expected
    /// output is the same at any round count, and the peak resident set
    /// printed after the loop is a wildcard line the leak oracle reads.
    fn emit_shape_rounds(&mut self, out: &mut String, k: u32) {
        let f = format!("oround{}", self.counter);
        let r = format!("ow{}", self.counter + 1);
        self.counter += 2;
        let mut body = String::new();
        self.emit_ownership_shape(&mut body, k);
        // every print in a shape is a statement of its own, and no literal
        // a shape emits contains this text
        let body = body.replace("print(", "oput(");
        self.decl_text[0].push_str(&format!("\nfn {}():\n{}", f, body));
        out.push_str("    og_quiet = false\n");
        out.push_str(&format!("    mut {} := 0\n", r));
        out.push_str(&format!("    while {} < orounds():\n", r));
        out.push_str(&format!("        {}()\n", f));
        out.push_str("        og_quiet = true\n");
        out.push_str(&format!("        {} = {} + 1\n", r, r));
        out.push_str(&format!("    print(\"own-kb {}: {{opeak_kb()}}\")\n", shape_name(k)));
        self.expect_wild();
    }

    fn emit_ownership_shape(&mut self, out: &mut String, k: u32) {
        let v = format!("ow{}", self.counter);
        self.counter += 1;
        match k {
            0 => {
                // for loop, body clears the iterable directly then refills
                let a = self.olit('a');
                let b = self.olit('b');
                out.push_str(&format!("    mut {}: List[String] := [\"{}\", \"{}\"]\n", v, a, b));
                out.push_str(&format!("    for s in {}:\n", v));
                out.push_str(&format!("        {}.clear()\n", v));
                out.push_str(&format!("        orefill({})\n", v));
                out.push_str("        print(\"own-clear: {s}\")\n");
                out.push_str("        break\n");
                self.expect(format!("own-clear: {}", a));
            }
            1 => {
                // for loop, body removes the walked element then refills
                let a = self.olit('a');
                let b = self.olit('b');
                out.push_str(&format!("    mut {}: List[String] := [\"{}\", \"{}\"]\n", v, a, b));
                out.push_str(&format!("    for s in {}:\n", v));
                out.push_str(&format!("        {}.remove(0)\n", v));
                out.push_str(&format!("        orefill({})\n", v));
                out.push_str("        print(\"own-remove: {s}\")\n");
                out.push_str("        break\n");
                self.expect(format!("own-remove: {}", a));
            }
            2 => {
                // for loop, body overwrites the walked slot then refills
                let a = self.olit('a');
                let b = self.olit('b');
                out.push_str(&format!("    mut {}: List[String] := [\"{}\", \"{}\"]\n", v, a, b));
                out.push_str(&format!("    for s in {}:\n", v));
                out.push_str(&format!("        {}[0] = \"oset-ownpayload-xxxxxx\"\n", v));
                out.push_str(&format!("        orefill({})\n", v));
                out.push_str("        print(\"own-set: {s}\")\n");
                out.push_str("        break\n");
                self.expect(format!("own-set: {}", a));
            }
            3 => {
                // for loop, the mutation reaches the iterable through a call
                let a = self.olit('a');
                let b = self.olit('b');
                out.push_str(&format!("    mut {}: List[String] := [\"{}\", \"{}\"]\n", v, a, b));
                out.push_str(&format!("    for s in {}:\n", v));
                out.push_str(&format!("        owipe({})\n", v));
                out.push_str("        print(\"own-call: {s}\")\n");
                out.push_str("        break\n");
                self.expect(format!("own-call: {}", a));
            }
            4 => {
                // for loop over structs, body clears and refills the list
                let a = self.olit('a');
                let t = self.olit('t');
                out.push_str(&format!(
                    "    mut {}: List[ONode] := [ONode(name: \"{}\", val: 1, tags: [\"{}\"], next: none, back: none)]\n",
                    v, a, t
                ));
                out.push_str(&format!("    for p in {}:\n", v));
                out.push_str(&format!("        {}.clear()\n", v));
                out.push_str(&format!("        orefill_nodes({})\n", v));
                out.push_str("        print(\"own-struct: {p.name} {p.tags.len()} {p.tags[0]}\")\n");
                out.push_str("        break\n");
                self.expect(format!("own-struct: {} 1 {}", a, t));
            }
            5 => {
                // a global list mutated through a call inside the loop
                let a = self.olit('g');
                let b = self.olit('g');
                out.push_str(&format!("    og_xs = [\"{}\", \"{}\"]\n", a, b));
                out.push_str("    for s in og_xs:\n");
                out.push_str("        oclear_global()\n");
                out.push_str("        print(\"own-global: {s}\")\n");
                out.push_str("        break\n");
                self.expect(format!("own-global: {}", a));
            }
            6 => {
                // a field passed alongside its owner, overwritten in the callee
                let a = self.olit('o');
                out.push_str(&format!(
                    "    {} := ONode(name: \"{}\", val: 3, tags: [], next: none, back: none)\n",
                    v, a
                ));
                out.push_str(&format!("    print(\"own-field: {{ooverwrite_field({}, {}.name)}} {{{}.name}}\")\n", v, v, v));
                self.expect(format!("own-field: {} oreplaced-ownpayload-xx", a));
            }
            7 => {
                // the same through a method that overwrites self's field
                let a = self.olit('o');
                out.push_str(&format!(
                    "    {} := ONode(name: \"{}\", val: 3, tags: [], next: none, back: none)\n",
                    v, a
                ));
                out.push_str(&format!("    print(\"own-method: {{{}.rename_and_read({}.name)}} {{{}.name}}\")\n", v, v, v));
                self.expect(format!("own-method: {} orenamed-ownpayload-xx", a));
            }
            8 => {
                // a list element passed alongside its owner, evicted in the callee
                let a = self.olit('e');
                out.push_str(&format!("    mut {}: List[String] := [\"{}\"]\n", v, a));
                out.push_str(&format!("    print(\"own-element: {{oclear_list({}, {}[0])}} {{{}.len()}}\")\n", v, v, v));
                self.expect(format!("own-element: {} 0", a));
            }
            9 => {
                // an optional field passed alongside its owner, dropped in the callee
                let a = self.olit('c');
                out.push_str(&format!(
                    "    {} := OHolder(child: ONode(name: \"{}\", val: 4, tags: [], next: none, back: none))\n",
                    v, a
                ));
                out.push_str(&format!("    print(\"own-optional: {{odrop_child({}, {}.child)}}\")\n", v, v));
                self.expect(format!("own-optional: {}", a));
            }
            10 => {
                // a weak field read off a temporary bound to a name: the
                // temporary owner is gone by the next statement, so the weak
                // edge must read none rather than dangle
                let a = self.olit('a');
                let b = self.olit('b');
                out.push_str(&format!("    {} := opair(\"{}\", \"{}\").next.value().back\n", v, a, b));
                out.push_str("    ochurn()\n");
                out.push_str(&format!("    print(\"own-weaktmp: {{{} == none}}\")\n", v));
                self.expect("own-weaktmp: true".to_string());
            }
            11 => {
                // a weak field read off a live owner bound to a name
                let a = self.olit('a');
                let b = self.olit('b');
                let p = format!("ow{}", self.counter);
                self.counter += 1;
                out.push_str(&format!("    {} := opair(\"{}\", \"{}\")\n", p, a, b));
                out.push_str(&format!("    {} := {}.next.value().back\n", v, p));
                out.push_str("    ochurn()\n");
                out.push_str(&format!("    print(\"own-weaklive: {{{}.value().name}}\")\n", v));
                self.expect(format!("own-weaklive: {}", a));
            }
            12 => {
                // an if-let payload binding off a call result's weak edge
                let a = self.olit('a');
                let b = self.olit('b');
                out.push_str(&format!("    {} := opair(\"{}\", \"{}\")\n", v, a, b));
                out.push_str(&format!("    if let z = {}.next.value().back:\n", v));
                out.push_str("        ochurn()\n");
                out.push_str("        print(\"own-iflet: {z.name}\")\n");
                self.expect(format!("own-iflet: {}", a));
            }
            14 => {
                // an optional bound from a field, then the field cleared
                let a = self.olit('c');
                let c = format!("ow{}", self.counter);
                self.counter += 1;
                out.push_str(&format!(
                    "    {} := OHolder(child: ONode(name: \"{}\", val: 4, tags: [], next: none, back: none))\n",
                    v, a
                ));
                out.push_str(&format!("    {} := {}.child\n", c, v));
                out.push_str(&format!("    {}.child = none\n", v));
                out.push_str("    ochurn()\n");
                out.push_str(&format!("    print(\"own-optbind: {{{}.value().name}}\")\n", c));
                self.expect(format!("own-optbind: {}", a));
            }
            15 => {
                // an if-let payload whose source field is cleared in the body
                let a = self.olit('c');
                out.push_str(&format!(
                    "    {} := OHolder(child: ONode(name: \"{}\", val: 4, tags: [], next: none, back: none))\n",
                    v, a
                ));
                out.push_str(&format!("    if let c = {}.child:\n", v));
                out.push_str(&format!("        {}.child = none\n", v));
                out.push_str("        ochurn()\n");
                out.push_str("        print(\"own-ifletclear: {c.name}\")\n");
                self.expect(format!("own-ifletclear: {}", a));
            }
            16 => {
                // a match payload whose subject is rebound inside the arm
                let a = self.olit('m');
                out.push_str(&format!(
                    "    mut {}: OSlot := OSlot.Holds(ONode(name: \"{}\", val: 5, tags: [], next: none, back: none))\n",
                    v, a
                ));
                out.push_str(&format!("    match {}:\n", v));
                out.push_str("        OSlot.Holds(n) =>\n");
                out.push_str(&format!("            {} = OSlot.Empty\n", v));
                out.push_str("            ochurn()\n");
                out.push_str("            print(\"own-match: {n.name}\")\n");
                out.push_str("        OSlot.Empty =>\n");
                out.push_str("            print(\"own-match: empty\")\n");
                self.expect(format!("own-match: {}", a));
            }
            17 => {
                // early return of a loop variable after the body cleared the list
                let a = self.olit('r');
                let b = self.olit('r');
                out.push_str(&format!("    mut {}: List[String] := [\"{}\", \"{}\"]\n", v, a, b));
                out.push_str(&format!("    print(\"own-return: {{ofirst_after_clear({})}} {{{}.len()}}\")\n", v, v));
                self.expect(format!("own-return: {} 0", a));
            }
            18 => {
                // remove and push while walking, with no break: the walk stops
                // at the shrunken length and a push does not extend it
                let names: Vec<String> = (0..4).map(|_| self.olit('w')).collect();
                let quoted: Vec<String> = names.iter().map(|s| format!("\"{}\"", s)).collect();
                let n = format!("ow{}", self.counter);
                self.counter += 1;
                if self.rng.chance(50) {
                    out.push_str(&format!("    mut {}: List[String] := [{}]\n", v, quoted.join(", ")));
                    out.push_str(&format!("    mut {} := 0\n", n));
                    out.push_str(&format!("    for s in {}:\n", v));
                    out.push_str(&format!("        {}.remove({}.len() - 1)\n", v, v));
                    out.push_str(&format!("        {} = {} + 1\n", n, n));
                    out.push_str("        if s.len() == 0:\n            continue\n");
                    out.push_str(&format!("    print(\"own-shrink: {{{}}} {{{}.len()}}\")\n", n, v));
                    self.expect("own-shrink: 2 2".to_string());
                } else {
                    out.push_str(&format!("    mut {}: List[String] := [{}, {}]\n", v, quoted[0], quoted[1]));
                    out.push_str(&format!("    mut {} := 0\n", n));
                    out.push_str(&format!("    for s in {}:\n", v));
                    out.push_str(&format!("        {}.push(s + s)\n", v));
                    out.push_str(&format!("        {} = {} + 1\n", n, n));
                    out.push_str(&format!("    print(\"own-grow: {{{}}} {{{}.len()}} {{{}[3]}}\")\n", n, v, v));
                    self.expect(format!("own-grow: 2 4 {}{}", names[1], names[1]));
                }
            }
            19 => {
                // a strong field read off a call result's chain, bound to a name
                let a = self.olit('a');
                let b = self.olit('b');
                out.push_str(&format!("    {} := opair(\"{}\", \"{}\").next.value().name\n", v, a, b));
                out.push_str("    ochurn()\n");
                out.push_str(&format!("    print(\"own-tmpfield: {{{}}}\")\n", v));
                self.expect(format!("own-tmpfield: {}", b));
            }
            20 => {
                // a closure overwrites a field that is passed alongside it
                let a = self.olit('o');
                let cl = format!("ow{}", self.counter);
                self.counter += 1;
                out.push_str(&format!(
                    "    {} := ONode(name: \"{}\", val: 3, tags: [], next: none, back: none)\n",
                    v, a
                ));
                out.push_str(&format!("    {} := fn():\n        {}.name = \"oclosure-ownpayload-xx\"\n", cl, v));
                out.push_str(&format!("    print(\"own-closurefield: {{orun_then_read({}, {}.name)}} {{{}.name}}\")\n", cl, v, v));
                self.expect(format!("own-closurefield: {} oclosure-ownpayload-xx", a));
            }
            21 => {
                // a nested loop whose inner body clears the outer element's list
                let a = self.olit('n');
                let t = self.olit('t');
                out.push_str(&format!(
                    "    {}: List[ONode] := [ONode(name: \"{}\", val: 1, tags: [\"{}\"], next: none, back: none)]\n",
                    v, a, t
                ));
                out.push_str(&format!("    for p in {}:\n", v));
                out.push_str("        for t in p.tags:\n");
                out.push_str("            p.tags.clear()\n");
                out.push_str("            orefill(p.tags)\n");
                out.push_str("            print(\"own-nested: {p.name} {t}\")\n");
                out.push_str("            break\n");
                out.push_str("        break\n");
                self.expect(format!("own-nested: {} {}", a, t));
            }
            22 => {
                // the next node bound by name, then cut off from its owner
                let a = self.olit('a');
                let b = self.olit('b');
                let p = format!("ow{}", self.counter);
                self.counter += 1;
                out.push_str(&format!("    {} := opair(\"{}\", \"{}\")\n", p, a, b));
                out.push_str(&format!("    {} := {}.next.value()\n", v, p));
                out.push_str(&format!("    {}.next = none\n", p));
                out.push_str("    ochurn()\n");
                out.push_str(&format!("    print(\"own-cut: {{{}.name}} {{{}.back.value().name}}\")\n", v, v));
                self.expect(format!("own-cut: {} {}", b, a));
            }
            13 => {
                // a closure captures a mutable list and clears it inside the loop
                let a = self.olit('c');
                let b = self.olit('c');
                let cl = format!("ow{}", self.counter);
                self.counter += 1;
                out.push_str(&format!("    mut {}: List[String] := [\"{}\", \"{}\"]\n", v, a, b));
                out.push_str(&format!("    {} := fn():\n        {}.clear()\n", cl, v));
                out.push_str(&format!("    for s in {}:\n", v));
                out.push_str(&format!("        orun_then_read({}, s)\n", cl));
                out.push_str("        print(\"own-closure: {s}\")\n");
                out.push_str("        break\n");
                self.expect(format!("own-closure: {}", a));
            }
            _ => self.emit_pending_shape(out, k, &v),
        }
    }

    /// a fresh heap string: the code that builds it at run time, and the text
    /// it prints. a literal is a static the runtime never frees, so a stale
    /// read of one would go unseen.
    fn oheap(&mut self, tag: char) -> (String, String) {
        let lit = self.olit(tag);
        (format!("\"{}\" + \"-h\"", lit), format!("{}-h", lit))
    }

    fn oname(&mut self) -> String {
        let n = format!("ow{}", self.counter);
        self.counter += 1;
        n
    }

    /// an OView whose every field holds a fresh heap value: the construction
    /// and the printed names of c, s, cs[0] and the slot's node
    fn oview(&mut self) -> (String, Vec<String>) {
        let parts: Vec<(String, String)> = "csen".chars().map(|t| self.oheap(t)).collect();
        let code = format!(
            "OView(c: OLeaf(name: {}), s: {}, cs: [OLeaf(name: {})], slot: OSlot.Holds(ONode(name: {}, val: 1, tags: [], next: none, back: none)))",
            parts[0].0, parts[1].0, parts[2].0, parts[3].0
        );
        (code, parts.into_iter().map(|p| p.1).collect())
    }

    /// the pending shapes: the classes #1277 fixes, which main's toolchain
    /// still gets wrong. each drops the owner of a value it read (or keeps a
    /// count it should not) and reads the value after an oscrub, so a freed
    /// value reads back as filler and a stranded count shows as a live weak
    /// edge.
    fn emit_pending_shape(&mut self, out: &mut String, k: u32, v: &str) {
        let name = shape_name(k);
        match name {
            "fnfield" => {
                // a closure in a field called with an argument read from the
                // same holder, overwriting that field while it runs
                let (ac, a) = self.oheap('f');
                out.push_str(&format!("    mut {} := OFn(name: {}, f: oplain)\n", v, ac));
                out.push_str(&format!("    {}.f = fn(s: String) -> String:\n", v));
                out.push_str(&format!("        {}.name = \"oreplaced\" + \"-ownpayload-x\"\n", v));
                out.push_str("        oscrub()\n        return s\n");
                out.push_str(&format!("    print(\"own-fnfield: {{{v}.f({v}.name)}} {{{v}.name}}\")\n", v = v));
                // the closure captures its holder; put a plain fn back to
                // break the cycle
                out.push_str(&format!("    {}.f = oplain\n", v));
                self.expect(format!("own-fnfield: {} oreplaced-ownpayload-x", a));
            }
            "fnself" => {
                // a closure in a field that replaces itself while it runs
                let (tc, t) = self.oheap('f');
                out.push_str(&format!("    {} := OFn(name: \"\", f: oplain)\n", v));
                out.push_str(&format!("    oinstall_self({}, {})\n", v, tc));
                out.push_str(&format!("    print(\"own-fnself: {{{v}.f(\"x\")}} {{{v}.f(\"y\")}}\")\n", v = v));
                self.expect(format!("own-fnself: {}x y", t));
            }
            "fnelem" => {
                // a closure in a list element that empties its list
                let (tc, t) = self.oheap('f');
                out.push_str(&format!("    {} := OFs(fs: [])\n", v));
                out.push_str(&format!("    {}.fs.push(oclearing({}, {}))\n", v, v, tc));
                out.push_str(&format!("    print(\"own-fnelem: {{{v}.fs[0](1)}} {{{v}.fs.len()}}\")\n", v = v));
                self.expect(format!("own-fnelem: {} 0", t));
            }
            "fnelemself" => {
                // a closure in a list element that overwrites its own slot
                let (tc, t) = self.oheap('f');
                out.push_str(&format!("    {} := OFs(fs: [])\n", v));
                out.push_str(&format!("    {}.fs.push(oreplacing({}, {}))\n", v, v, tc));
                out.push_str(&format!("    print(\"own-fnelemself: {{{v}.fs[0](1)}} {{{v}.fs[0](2)}}\")\n", v = v));
                self.expect(format!("own-fnelemself: {} oplain", t));
            }
            "viewfield" | "viewstring" | "viewenum" | "viewelem" | "viewtuple" | "viewtail" => {
                // a catch-all or tuple arm over a field or an element, whose
                // body drops the subject's owner before reading the binding
                if name == "viewtuple" {
                    // a struct of its own: a tuple field strands its
                    // elements when the struct is freed, and the leak oracle
                    // should pin that on this shape alone
                    let (ac, a) = self.oheap('a');
                    let (bc, b) = self.oheap('b');
                    out.push_str(&format!("    {} := OPair(pair: ({}, OLeaf(name: {})))\n", v, ac, bc));
                    out.push_str(&format!(
                        "    match {}.pair:\n        (a, b) =>\n            {}.pair = (\"ox\", OLeaf(name: \"oy\"))\n            oscrub()\n            print(\"own-viewtuple: {{a}} {{b.name}}\")\n",
                        v, v
                    ));
                    self.expect(format!("own-viewtuple: {} {}", a, b));
                    return;
                }
                let (code, names) = self.oview();
                out.push_str(&format!("    {} := {}\n", v, code));
                let (subject, arms, line) = match name {
                    "viewfield" => (
                        format!("{}.c", v),
                        format!("        x =>\n            {}.c = OLeaf(name: \"oother\")\n            oscrub()\n            print(\"own-viewfield: {{x.name}}\")\n", v),
                        format!("own-viewfield: {}", names[0]),
                    ),
                    "viewstring" => (
                        format!("{}.s", v),
                        format!("        \"onope\" =>\n            print(\"own-viewstring: nope\")\n        x =>\n            {}.s = \"oother\" + \"-x\"\n            oscrub()\n            print(\"own-viewstring: {{x}}\")\n", v),
                        format!("own-viewstring: {}", names[1]),
                    ),
                    "viewenum" => (
                        format!("{}.slot", v),
                        format!("        OSlot.Empty =>\n            print(\"own-viewenum: empty\")\n        x =>\n            {}.slot = OSlot.Empty\n            oscrub()\n            print(\"own-viewenum: {{oslot_name(x)}}\")\n", v),
                        format!("own-viewenum: {}", names[3]),
                    ),
                    "viewelem" => (
                        format!("{}.cs[0]", v),
                        format!("        x =>\n            {}.cs.clear()\n            oscrub()\n            print(\"own-viewelem: {{x.name}}\")\n", v),
                        format!("own-viewelem: {}", names[2]),
                    ),
                    _ => {
                        // a tail match in a callee, every arm returning
                        out.push_str(&format!("    print(\"own-viewtail: {{otake_view({})}}\")\n", v));
                        self.expect(format!("own-viewtail: {}", names[0]));
                        return;
                    }
                };
                out.push_str(&format!("    match {}:\n{}", subject, arms));
                self.expect(line);
            }
            "viewloop" => {
                // the same in a loop, leaving the arm by continue and break
                let (code, _) = self.oview();
                let seen = self.oname();
                let i = self.oname();
                out.push_str(&format!("    {} := {}\n", v, code));
                out.push_str(&format!("    mut {}: List[String] := []\n", seen));
                out.push_str(&format!("    mut {} := 0\n", i));
                out.push_str(&format!("    while {} < 3:\n", i));
                out.push_str(&format!("        {}.c = OLeaf(name: \"oloop\" + \"-{{{}}}\")\n", v, i));
                out.push_str(&format!("        {} = {} + 1\n", i, i));
                out.push_str(&format!("        match {}.c:\n            x =>\n", v));
                out.push_str(&format!("                {}.c = OLeaf(name: \"oother\")\n", v));
                out.push_str("                oscrub()\n");
                out.push_str(&format!("                {}.push(x.name)\n", seen));
                out.push_str(&format!("                if {} == 1:\n                    continue\n", i));
                out.push_str(&format!("                if {} == 2:\n                    break\n", i));
                out.push_str(&format!("    print(\"own-viewloop: {{{}.join(\" \")}}\")\n", seen));
                self.expect("own-viewloop: oloop-0 oloop-1".to_string());
            }
            "sibcat" | "sibinterp" | "sibtuple" | "sibmapkey" | "sibarg" => {
                // a field (or element) read in one operand while a later
                // operand, a call, overwrites or evicts it
                let (ac, a) = self.oheap('s');
                if name == "sibcat" && self.rng.chance(50) {
                    out.push_str(&format!("    mut {}: List[String] := [{}]\n", v, ac));
                    out.push_str(&format!("    print(\"own-sibcat: {{{v}[0] + oevict({v})}}\")\n", v = v));
                    self.expect(format!("own-sibcat: {}!", a));
                    return;
                }
                out.push_str(&format!(
                    "    {} := ONode(name: {}, val: 1, tags: [], next: none, back: none)\n",
                    v, ac
                ));
                match name {
                    "sibcat" => {
                        out.push_str(&format!("    print(\"own-sibcat: {{{v}.name + orename({v})}}\")\n", v = v));
                        self.expect(format!("own-sibcat: {}!", a));
                    }
                    "sibinterp" => {
                        out.push_str(&format!("    print(\"own-sibinterp: {{{v}.name}}{{orename({v})}}\")\n", v = v));
                        self.expect(format!("own-sibinterp: {}!", a));
                    }
                    "sibtuple" => {
                        let t = self.oname();
                        out.push_str(&format!("    {} := ({}.name, orename({}))\n", t, v, v));
                        out.push_str(&format!("    print(\"own-sibtuple: {{{t}.0}} {{{t}.1}}\")\n", t = t));
                        self.expect(format!("own-sibtuple: {} !", a));
                    }
                    "sibmapkey" => {
                        let m = self.oname();
                        if self.rng.chance(50) {
                            out.push_str(&format!("    mut {}: Map[String, String] := {{}}\n", m));
                            out.push_str(&format!("    {}.insert({}.name, orename({}))\n", m, v, v));
                        } else {
                            out.push_str(&format!("    {}: Map[String, String] := {{{}.name: orename({})}}\n", m, v, v));
                        }
                        out.push_str(&format!("    print(\"own-sibmapkey: {{{}.keys()[0]}}\")\n", m));
                        self.expect(format!("own-sibmapkey: {}", a));
                    }
                    _ => {
                        out.push_str(&format!("    print(\"own-sibarg: {{otwo({v}.name, orename({v}))}}\")\n", v = v));
                        self.expect(format!("own-sibarg: {}!", a));
                    }
                }
            }
            "sibassign" => {
                // a store whose value, a call, overwrites the field, the
                // global or the receiver being stored to
                match self.rng.below(3) {
                    0 => {
                        let (ac, _) = self.oheap('s');
                        out.push_str(&format!(
                            "    {} := ONode(name: {}, val: 1, tags: [], next: none, back: none)\n",
                            v, ac
                        ));
                        out.push_str(&format!("    {}.name = orename({})\n", v, v));
                        out.push_str(&format!("    print(\"own-sibassign: {{{}.name}}\")\n", v));
                        self.expect("own-sibassign: !".to_string());
                    }
                    1 => {
                        out.push_str("    og_name = oset_global()\n");
                        out.push_str("    print(\"own-sibassign: {og_name}\")\n");
                        self.expect("own-sibassign: oouter-global".to_string());
                    }
                    _ => {
                        let (ac, _) = self.oheap('s');
                        out.push_str(&format!(
                            "    {} := OBox(o: ONode(name: {}, val: 1, tags: [], next: none, back: none))\n",
                            v, ac
                        ));
                        out.push_str(&format!("    {}.o.name = oswap_o({})\n", v, v));
                        out.push_str(&format!("    print(\"own-sibassign: {{{}.o.name}}\")\n", v));
                        // the store lands in the node the call replaced
                        self.expect("own-sibassign: oswapped".to_string());
                    }
                }
            }
            "sibindex" => {
                // an index store or read whose index, a call, replaces the list
                let (ac, a) = self.oheap('s');
                out.push_str(&format!("    {} := OItems(items: [{}], f: oplain)\n", v, ac));
                if self.rng.chance(50) {
                    let (bc, _) = self.oheap('s');
                    out.push_str(&format!("    {}.items[oswap_items({})] = {}\n", v, v, bc));
                    out.push_str(&format!("    print(\"own-sibindex: {{{}.items[0]}}\")\n", v));
                    // the store lands in the list the call replaced
                    self.expect("own-sibindex: oswapped-ownpayload-x".to_string());
                } else {
                    let r = self.oname();
                    out.push_str(&format!("    {} := {}.items[oswap_items({})]\n", r, v, v));
                    out.push_str(&format!("    print(\"own-sibindex: {{{}}}\")\n", r));
                    self.expect(format!("own-sibindex: {}", a));
                }
            }
            "optstruct" => {
                let (ac, a) = self.oheap('p');
                out.push_str(&format!("    {} := [OLeaf(name: {})]\n", v, ac));
                out.push_str(&format!("    print(\"own-optstruct: {{{v}.get(0)}} {{{v}.get(3)}}\")\n", v = v));
                self.expect(format!("own-optstruct: OLeaf(name: {}) none", a));
            }
            "optlist" => {
                let (ac, a) = self.oheap('p');
                out.push_str(&format!("    {} := [[{}, \"oy\"]]\n", v, ac));
                out.push_str(&format!("    print(\"own-optlist: {{{}.get(0)}}\")\n", v));
                self.expect(format!("own-optlist: [{}, oy]", a));
            }
            "optmap" => {
                let (ac, a) = self.oheap('p');
                out.push_str(&format!("    mut {}: List[Map[String, Int]] := []\n", v));
                out.push_str(&format!("    {}.push({{{}: 7}})\n", v, ac));
                out.push_str(&format!("    print(\"own-optmap: {{{}.get(0)}}\")\n", v));
                self.expect(format!("own-optmap: {{{}: 7}}", a));
            }
            "getkept" | "getkeptstr" => {
                // a getter's optional kept in a local while the list lets go
                // of the element, then read
                let (ac, a) = self.oheap('g');
                let g = self.oname();
                let getter = ["get(0)", "first()", "last()"][self.rng.below(3)];
                let (elem_ty, other, read) = if name == "getkept" {
                    ("OLeaf", "OLeaf(name: \"oother\")".to_string(), format!("{}.value().name", g))
                } else {
                    ("String", "\"oother\" + \"-x\"".to_string(), format!("{}.value()", g))
                };
                let elem = if name == "getkept" { format!("OLeaf(name: {})", ac) } else { ac };
                out.push_str(&format!("    mut {}: List[{}] := [{}]\n", v, elem_ty, elem));
                out.push_str(&format!("    {} := {}.{}\n", g, v, getter));
                match self.rng.below(3) {
                    0 => out.push_str(&format!("    {}.clear()\n", v)),
                    1 => out.push_str(&format!("    {} = []\n", v)),
                    _ => out.push_str(&format!("    {}[0] = {}\n", v, other)),
                }
                out.push_str("    oscrub()\n");
                out.push_str(&format!("    if {} != none:\n        print(\"own-{}: {{{}}}\")\n", g, name, read));
                self.expect(format!("own-{}: {}", name, a));
            }
            "getkeptmap" => {
                let (ac, a) = self.oheap('g');
                let g = self.oname();
                out.push_str(&format!("    mut {}: Map[String, OLeaf] := {{}}\n", v));
                out.push_str(&format!("    {}.insert(\"k\", OLeaf(name: {}))\n", v, ac));
                out.push_str(&format!("    {} := {}.get(\"k\")\n", g, v));
                if self.rng.chance(50) {
                    out.push_str(&format!("    {}.remove(\"k\")\n", v));
                } else {
                    out.push_str(&format!("    {}.insert(\"k\", OLeaf(name: \"oother\"))\n", v));
                }
                out.push_str("    oscrub()\n");
                out.push_str(&format!("    if {} != none:\n        print(\"own-getkeptmap: {{{}.value().name}}\")\n", g, g));
                self.expect(format!("own-getkeptmap: {}", a));
            }
            "walkfield" | "walkfilter" => {
                // map or filter over a field, with a function read from the
                // same holder that replaces both the function and the list
                let items: Vec<(String, String)> = (0..3).map(|_| self.oheap('w')).collect();
                let codes: Vec<&str> = items.iter().map(|p| p.0.as_str()).collect();
                let w = self.oname();
                out.push_str(&format!("    {} := OItems(items: [{}], f: oplain)\n", v, codes.join(", ")));
                out.push_str(&format!("    oinstall_walk({})\n", v));
                if name == "walkfield" {
                    out.push_str(&format!("    {} := {}.items.map({}.f)\n", w, v, v));
                    let vals: Vec<String> = items.iter().map(|p| format!("{}!", p.1)).collect();
                    self.expect(format!("own-walkfield: {}", vals.join(",")));
                } else {
                    out.push_str(&format!("    {} := {}.items.filter(fn(s: String) => {}.f(s).len() > 0)\n", w, v, v));
                    let vals: Vec<String> = items.iter().map(|p| p.1.clone()).collect();
                    self.expect(format!("own-walkfilter: {}", vals.join(",")));
                }
                out.push_str(&format!("    print(\"own-{}: {{{}.join(\",\")}}\")\n", name, w));
            }
            "walkclear" | "walkpush" | "walkreplace" => {
                // map, filter or reduce over a local list whose function
                // empties it, grows it or replaces an element, then reads its
                // argument
                let (ac, a) = self.oheap('w');
                let (bc, b) = self.oheap('w');
                let f = self.oname();
                let w = self.oname();
                let change = match name {
                    "walkclear" => format!("{}.clear()", v),
                    "walkpush" => format!("{}.push(\"ogrow\" + \"-x\")", v),
                    _ => format!("{}[0] = \"oother\" + \"-x\"", v),
                };
                let len = if name == "walkclear" { 0 } else if name == "walkpush" { 4 } else { 2 };
                out.push_str(&format!("    mut {}: List[String] := [{}, {}]\n", v, ac, bc));
                let (sig, ret, call, result) = match self.rng.below(3) {
                    0 => ("s: String) -> String", "s + \"?\"", format!("{}.map({})", v, f), format!("{}?,{}?", a, b)),
                    1 => ("s: String) -> Bool", "s.len() > 0", format!("{}.filter({})", v, f), format!("{},{}", a, b)),
                    _ => (
                        "acc: String, s: String) -> String",
                        "acc + s",
                        format!("{}.reduce(\"\", {})", v, f),
                        format!("{}{}", a, b),
                    ),
                };
                out.push_str(&format!("    {} := fn({}:\n        {}\n        oscrub()\n        return {}\n", f, sig, change, ret));
                out.push_str(&format!("    {} := {}\n", w, call));
                if call.contains(".reduce(") {
                    out.push_str(&format!("    print(\"own-{}: {{{}}} {{{}.len()}}\")\n", name, w, v));
                } else {
                    out.push_str(&format!("    print(\"own-{}: {{{}.join(\",\")}} {{{}.len()}}\")\n", name, w, v));
                }
                self.expect(format!("own-{}: {} {}", name, result, len));
            }
            "weakunwrap" | "weakmatch" | "weakstore" | "weakinterp" => {
                // a weak read handed to a consumer, then the strong owner
                // dropped: a consumer that kept the count it was handed keeps
                // the target alive, and the weak edge reads live
                let (ac, a) = self.oheap('k');
                let r = self.oname();
                out.push_str(&format!("    {} := OWeak(held: OLeaf(name: {}), w: none, w2: none)\n", v, ac));
                out.push_str(&format!("    {}.w = {}.held\n", v, v));
                let shown = match name {
                    "weakunwrap" => {
                        out.push_str(&format!("    {} := {}.w.unwrap_or(OLeaf(name: \"ofallback\")).name\n", r, v));
                        a.clone()
                    }
                    "weakmatch" => {
                        // in a callee: a match keeps its subject in a slot
                        // until the function exits, so inline in main the
                        // edge would read live at the check
                        out.push_str(&format!("    {} := oweak_match({})\n", r, v));
                        a.clone()
                    }
                    "weakstore" => {
                        out.push_str(&format!("    {}.w2 = {}.w\n", v, v));
                        out.push_str(&format!("    {} := {}.w2 != none\n", r, v));
                        "true".to_string()
                    }
                    _ => {
                        out.push_str(&format!("    {} := \"{{{}.w}}\"\n", r, v));
                        format!("OLeaf(name: {})", a)
                    }
                };
                out.push_str(&format!("    {}.held = none\n", v));
                out.push_str("    oscrub()\n");
                out.push_str(&format!(
                    "    print(\"own-{}: {{{}}} {{{}.w == none}} {{{}.w2 == none}}\")\n",
                    name, r, v, v
                ));
                self.expect(format!("own-{}: {} true true", name, shown));
            }
            _ => unreachable!("ownership shape {} out of range", k),
        }
    }

    // ---------- rendering ----------

    fn render(&self, main_body: String) -> Program {
        let mut files = Vec::new();
        let mod_file = |m: usize| -> &'static str {
            match m {
                1 => "genmod_a",
                _ => "genmod_b",
            }
        };
        for m in 1..self.feats.n_modules {
            let mut content = String::new();
            content.push_str("# generated helper module\n");
            if m == 2 {
                // helper b may lean on helper a
                content.push_str("import genmod_a as mx\n");
                let types: Vec<String> = self
                    .structs
                    .iter()
                    .filter(|s| s.module == 1)
                    .map(|s| s.name.clone())
                    .chain(self.enums.iter().filter(|e| e.module == 1).map(|e| e.name.clone()))
                    .collect();
                if !types.is_empty() {
                    content.push_str(&format!("from genmod_a import {}\n", types.join(", ")));
                }
                content.push('\n');
            }
            content.push_str(&self.decl_text[m]);
            files.push((format!("{}.pith", mod_file(m)), content));
        }
        let mut main = String::new();
        main.push_str("# generated program\n");
        for m in 1..self.feats.n_modules {
            let alias = module_alias(m, 0);
            main.push_str(&format!("import {} as {}\n", mod_file(m), alias));
            let types: Vec<String> = self
                .structs
                .iter()
                .filter(|s| s.module == m)
                .map(|s| s.name.clone())
                .chain(
                    self.enums
                        .iter()
                        .filter(|e| e.module == m)
                        .map(|e| e.name.clone()),
                )
                .collect();
            if !types.is_empty() {
                main.push_str(&format!("from {} import {}\n", mod_file(m), types.join(", ")));
            }
        }
        if self.feats.n_modules > 1 {
            main.push('\n');
        }
        main.push_str(&self.decl_text[0]);
        main.push_str(&main_body);
        files.push(("main.pith".into(), main));
        Program {
            files,
            expected: self.expected.clone(),
        }
    }
}

/// the helpers the pending ownership shapes lean on, appended after the
/// settled ones. every callee here drops something its caller may still be
/// reading, then oscrubs, so a freed struct or string is reused before the
/// caller looks at it again.
const PENDING_DECLS: &str = r#"
struct OLeaf:
    name: String

struct OFn:
    mut name: String
    mut f: fn(String) -> String

struct OFs:
    mut fs: List[fn(Int) -> String]

struct OView:
    mut c: OLeaf
    mut s: String
    mut cs: List[OLeaf]
    mut slot: OSlot

struct OPair:
    mut pair: (String, OLeaf)

struct OWeak:
    mut held: OLeaf?
    weak w: OLeaf?
    weak w2: OLeaf?

struct OItems:
    mut items: List[String]
    mut f: fn(String) -> String

struct OBox:
    mut o: ONode

mut og_name := "ostart"

fn oscrub():
    mut junk: List[OLeaf] := []
    mut i := 0
    while i < 40:
        junk.push(OLeaf(name: "ofiller-{i}-ownpayload-x"))
        i = i + 1
    ochurn()

fn oplain(s: String) -> String:
    return s

fn oplain_at(i: Int) -> String:
    return "oplain"

fn oinstall_self(o: OFn, tag: String):
    o.f = fn(s: String) -> String:
        o.f = oplain
        oscrub()
        return tag + s

fn oclearing(b: OFs, tag: String) -> fn(Int) -> String:
    return fn(i: Int) -> String:
        b.fs = []
        oscrub()
        return tag

fn oreplacing(b: OFs, tag: String) -> fn(Int) -> String:
    return fn(i: Int) -> String:
        b.fs[0] = oplain_at
        oscrub()
        return tag

fn oslot_name(s: OSlot) -> String:
    return match s:
        OSlot.Holds(n) => n.name
        OSlot.Empty => "empty"

fn otake_view(h: OView) -> String:
    match h.c:
        x =>
            h.c = OLeaf(name: "oother")
            oscrub()
            return x.name

fn orename(o: ONode) -> String:
    o.name = "orenamed" + "-ownpayload-x"
    oscrub()
    return "!"

fn otwo(a: String, b: String) -> String:
    return a + b

fn oevict(xs: List[String]) -> String:
    xs.clear()
    oscrub()
    return "!"

fn oswap_items(o: OItems) -> Int:
    o.items = ["oswapped" + "-ownpayload-x"]
    oscrub()
    return 0

fn oswap_o(h: OBox) -> String:
    h.o = ONode(name: "oswapped", val: 0, tags: [], next: none, back: none)
    oscrub()
    return "ov" + "-x"

fn oset_global() -> String:
    og_name = "oinner" + "-global"
    oscrub()
    return "oouter" + "-global"

fn oweak_match(v: OWeak) -> String:
    return match v.w:
        none => "onone"
        m => m.name

fn oinstall_walk(o: OItems):
    o.f = fn(s: String) -> String:
        o.f = oplain
        o.items = ["oother" + "-list"]
        oscrub()
        return s + "!"
"#;

/// leak mode's helpers: the round count, a print that only the first round
/// reaches, and the process's peak resident set (the reading
/// tests/leaks/leakprobe makes). builtins only, so a generated program needs
/// no import and finds no std from outside a checkout.
const ROUNDS_DECLS: &str = r#"
mut og_quiet := false

fn orounds() -> Int:
    return parse_int(env_opt("PITH_LEAK_ROUNDS").unwrap_or("1")) catch 1

fn oput(s: String):
    if og_quiet == false:
        print(s)

fn opeak_kb() -> Int:
    status := read_file("/proc/self/status") catch ""
    for line in status.split(chr(10)):
        if line.starts_with("VmHWM:"):
            parsed := parse_int(line.replace("VmHWM:", "").replace("kB", "").trim())
            if parsed.is_ok:
                return parsed.ok
    return 0
"#;

fn ty_mentions_param(ty: &Ty) -> bool {
    match ty {
        Ty::Param(_) => true,
        Ty::Opt(t) | Ty::List(t) | Ty::Chan(t) => ty_mentions_param(t),
        Ty::Map(k, v) => ty_mentions_param(k) || ty_mentions_param(v),
        Ty::Struct(_, targs) => targs.iter().any(ty_mentions_param),
        Ty::FnT(ps, r) => ps.iter().any(ty_mentions_param) || ty_mentions_param(r),
        _ => false,
    }
}

/// true when a type parameter appears somewhere under an optional, so the
/// parameter can't be inferred from a plain (non-optional) argument value
fn param_under_opt(ty: &Ty) -> bool {
    match ty {
        Ty::Opt(t) => ty_mentions_param(t),
        Ty::List(t) | Ty::Chan(t) => param_under_opt(t),
        Ty::Map(k, v) => param_under_opt(k) || param_under_opt(v),
        Ty::Struct(_, targs) => targs.iter().any(param_under_opt),
        Ty::FnT(ps, r) => ps.iter().any(param_under_opt) || param_under_opt(r),
        _ => false,
    }
}

fn module_alias(decl_module: usize, from: usize) -> &'static str {
    if from == 2 || (from == 0 && decl_module == 1) {
        if from == 2 {
            "mx"
        } else {
            "ma"
        }
    } else {
        "mb"
    }
}
