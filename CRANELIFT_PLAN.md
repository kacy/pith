# Cranelift Migration Plan

## Overview
Migrate Forge from C transpilation to direct Cranelift-based native code generation.

## Why Cranelift?
- Better optimization than our custom C runtime
- Source-level debugging (DWARF)
- No C compiler dependency
- Better performance characteristics
- Foundation for future JIT/REPL features

## Architecture

### Current Flow
```
.fg → Lexer → Parser → Checker → C Codegen → C File → zig cc → Binary
```

### New Flow
```
.fg → Lexer → Parser → Checker → Cranelift IR → Machine Code → Binary
                                             ↓
                                      Rust Runtime (static link)
```

## Project Structure

```
cranelift/
├── Cargo.toml          # Rust workspace
├── runtime/            # Rust runtime library
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs          # Main exports
│       ├── string.rs       # String operations
│       ├── collections/    # List, Map, Set
│       │   ├── mod.rs
│       │   ├── list.rs
│       │   ├── map.rs
│       │   └── set.rs
│       ├── arc.rs          # Reference counting
│       ├── cycle.rs        # Cycle collection
│       └── concurrency/    # Tasks, channels, sync
│           ├── mod.rs
│           ├── task.rs
│           ├── channel.rs
│           ├── mutex.rs
│           └── sem.rs
├── codegen/            # Cranelift code generator
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── module.rs       # Module compilation
│       ├── function.rs     # Function compilation
│       ├── types.rs        # Type mapping
│       └── builtins.rs     # Runtime calls
└── cli/                # New CLI
    ├── Cargo.toml
    └── src/main.rs
```

## Phase 1: Foundation (Week 1)

### 1.1 Create Rust Project Structure
- [ ] Initialize Cargo workspace
- [ ] Add Cranelift dependencies
- [ ] Create runtime crate structure
- [ ] Set up FFI boundary

### 1.2 Port Core String Operations
- [ ] String struct (ptr + len, no null terminator)
- [ ] Allocation with ARC header
- [ ] Basic operations: concat, substring, split
- [ ] Comparison and search operations
- [ ] UTF-8 handling

### 1.3 Port ARC Infrastructure
- [ ] RC header structure
- [ ] Retain/release operations
- [ ] Global object list for cycle detection
- [ ] Mark-and-scan cycle collector

## Phase 2: Collections (Week 1-2)

### 2.1 List[T]
- [ ] Vec-backed implementation
- [ ] Push, pop, insert, remove
- [ ] Iteration support
- [ ] Indexing bounds checking

### 2.2 Map[K,V]
- [ ] HashMap-backed (hashbrown crate)
- [ ] Insert, remove, lookup
- [ ] Keys/values iteration
- [ ] Proper hashing for String keys

### 2.3 Set[T]
- [ ] HashSet-backed
- [ ] Add, remove, contains
- [ ] Union, intersection, difference

## Phase 3: Concurrency (Week 2)

### 3.1 Task System
- [ ] Task struct with join handle
- [ ] Thread pool executor
- [ ] Spawn and await
- [ ] Result propagation

### 3.2 Channels
- [ ] Bounded and unbounded channels
- [ ] Send/receive operations
- [ ] Select/multiplex (future)

### 3.3 Synchronization
- [ ] Mutex
- [ ] WaitGroup
- [ ] Semaphore
- [ ] Condition variables

## Phase 4: Cranelift Codegen (Week 3-4)

### 4.1 IR Generation
- [ ] Module builder
- [ ] Function translation from AST
- [ ] Type mapping (Forge types → Cranelift types)
- [ ] Control flow (if, while, for, match)

### 4.2 Runtime Integration
- [ ] FFI declarations for runtime calls
- [ ] String operations
- [ ] Collection operations
- [ ] Memory management (alloc/free)

### 4.3 Optimizations
- [ ] Basic block optimizations
- [ ] Register allocation
- [ ] Inline small functions

## Phase 5: Integration (Week 4)

### 5.1 CLI Integration
- [ ] New build command using Cranelift
- [ ] Runtime linking (static)
- [ ] Debug info generation

### 5.2 Testing
- [ ] Port all existing tests
- [ ] Add debug info tests
- [ ] Performance benchmarks

### 5.3 Bootstrap
- [ ] Self-host with Cranelift backend
- [ ] Verify fixed-point

## Key Design Decisions

### FFI Boundary
- Runtime exposes C ABI functions
- Codegen calls runtime via function pointers
- Simplifies linking and testing

### Memory Layout
- Keep same ARC header layout for compatibility
- Strings: (ptr, len) struct passed by value
- Collections: pointer to heap allocation

### Error Handling
- Runtime panics on OOM/fatal errors
- Result types for recoverable errors
- Panic hook for better messages

## Dependencies

```toml
[dependencies]
cranelift = "0.110"
cranelift-module = "0.110"
cranelift-object = "0.110"
hashbrown = "0.14"  # Faster HashMap
parking_lot = "0.12" # Better synchronization
threadpool = "1.8"   # Task executor
```

## Migration Strategy

1. **Parallel Development**: Keep C backend working
2. **Feature Flags**: `--cranelift` flag for new backend
3. **Gradual Cutover**: Start with simple programs
4. **Full Replacement**: Remove C backend once stable

## Success Criteria

- [ ] All 40+ examples compile and run
- [ ] All tests pass
- [ ] Performance parity or better
- [ ] Debug info works in GDB/LLDB
- [ ] Self-host achieved

## Timeline

**Total: 4 weeks**
- Week 1: Runtime foundation (strings, ARC)
- Week 2: Collections and concurrency
- Week 3: Cranelift codegen core
- Week 4: Integration and testing

Ready to start?
