// value model for the differential output oracle. the generator knows every
// value it prints, so alongside each emitted expression it builds a small
// semantic tree (`E`); evaluating that tree against an environment of known
// locals yields the exact line the program must print. this is bookkeeping
// over choices the generator itself made, not an interpreter for pith: only
// the constructs the generator emits have a node here, and evaluation is
// expected to be total — an unresolved path is a pithgen bug and panics.

/// a runtime value. optionals are transparent: a present value is stored as
/// itself and an absent one as `NoneV` (the generator never nests optionals).
#[derive(Clone, PartialEq, Debug)]
pub enum Val {
    I(i64),
    S(String),
    B(bool),
    NoneV,
    L(Vec<Val>),
    M(Vec<(Val, Val)>),          // insertion-ordered, keys unique
    St(usize, Vec<Val>),         // struct index, field values in decl order
    En(usize, usize, Vec<Val>),  // enum index, variant index, payload values
    Opaque,                      // channels, fn values: never printed
}

#[derive(Clone, Debug)]
pub enum CmpOp {
    Lt,
    Gt,
    Eq,
    Ne,
}

/// a semantic expression tree mirroring the code string the generator emitted
#[derive(Clone, Debug)]
pub enum E {
    Lit(Val),
    Path(String),                // a local, or a dotted field path off one
    Add(Box<E>, Box<E>),
    Mul(Box<E>, Box<E>),
    Concat(Box<E>, Box<E>),
    ToStr(Box<E>),               // Int.to_string()
    Len(Box<E>),                 // String/List/Map .len()
    Cmp(CmpOp, Box<E>, Box<E>),  // int comparison
    IsNone(Box<E>, bool),        // `x == none` (false) / `x != none` (true)
    ListL(Vec<E>),
    MapL(Vec<(E, E)>),
    StructL(usize, Vec<E>),
    EnumL(usize, usize, Vec<E>),
    Call(usize, Vec<E>),         // fn table index, argument expressions
}

/// a statement in a recorded plain-fn body
#[derive(Clone, Debug)]
pub enum Stmt {
    Let(String, E),
    IfRet(E, E),
    Ret(E),
}

/// per-variant semantics of a cross-blend match arm
#[derive(Clone, Debug)]
pub enum ArmSem {
    Const(i64),
    B0Int,               // payload[0]: Int
    B0Len,               // payload[0]: String or List -> .len()
    B0FieldInt(usize),   // payload[0]: struct, read Int field by index
    B0FieldStrLen(usize),
}

/// how a cross-blend computes its base term from the struct param
#[derive(Clone, Debug)]
pub enum BaseSem {
    FieldInt(usize),
    FieldStrLen(usize),
    One,
}

/// the callable semantics of a generated fn, keyed off its fixed template
#[derive(Clone, Debug)]
pub enum Body {
    Opaque,                       // workers, apply, fallible: never reached via eval
    Stmts(Vec<Stmt>),             // plain fns: recorded statement list
    Identity,                     // fn[T](v: T) -> T
    FirstOr,                      // fn[T](xs: List[T], fb: T) -> T
    OptProbe,                     // fn[T](tag, v: T?) -> String
    WrapStruct(usize, Vec<Val>),  // fn[T](v: T) -> S[T], fixed tail fields
    UnwrapOr7,                    // fn[T](a: T, b: Int?) -> Int
    CrossBlend(Vec<ArmSem>, BaseSem),
    Pluck(bool),                  // fn[T: Iface](c: T) -> T.Item / T.Item?
}

/// the interface-impl semantics needed to evaluate first()/pick()
#[derive(Clone, Debug)]
pub struct PickSem {
    pub field_idx: usize, // the Int guard field
    pub lim: i64,
    pub some: E,
}

pub type Env = Vec<(String, Val)>;

pub fn lookup<'a>(env: &'a Env, name: &str) -> Option<&'a Val> {
    env.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v)
}

impl Val {
    pub fn as_int(&self) -> i64 {
        match self {
            Val::I(n) => *n,
            other => panic!("pithgen eval: expected Int, got {:?}", other),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Val::S(s) => s,
            other => panic!("pithgen eval: expected String, got {:?}", other),
        }
    }

    pub fn as_bool(&self) -> bool {
        match self {
            Val::B(b) => *b,
            other => panic!("pithgen eval: expected Bool, got {:?}", other),
        }
    }

    pub fn len_of(&self) -> i64 {
        match self {
            Val::S(s) => s.len() as i64,
            Val::L(xs) => xs.len() as i64,
            Val::M(ps) => ps.len() as i64,
            other => panic!("pithgen eval: no len on {:?}", other),
        }
    }

    /// how the value renders inside string interpolation
    pub fn show(&self) -> String {
        match self {
            Val::I(n) => n.to_string(),
            Val::S(s) => s.clone(),
            Val::B(b) => b.to_string(),
            other => panic!("pithgen eval: {:?} is not printable", other),
        }
    }
}
