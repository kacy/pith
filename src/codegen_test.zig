// codegen tests — moved from codegen.zig for readability
//
// exercises C emission helpers (type mapping, quote stripping) and
// a full pipeline smoke test. run with: zig build test

const std = @import("std");
const types = @import("types.zig");
const checker_mod = @import("checker.zig");
const Checker = checker_mod.Checker;
const Scope = checker_mod.Scope;
const MethodEntry = checker_mod.MethodEntry;
const GenericDecl = checker_mod.GenericDecl;
const codegen = @import("codegen.zig");
const CEmitter = codegen.CEmitter;

const lexer_mod = @import("lexer.zig");
const Lexer = lexer_mod.Lexer;
const Parser = @import("parser.zig").Parser;

const TypeId = types.TypeId;
const TypeTable = types.TypeTable;

test "mapType maps forge primitives to C types" {
    try std.testing.expectEqualStrings("int64_t", CEmitter.mapType("Int"));
    try std.testing.expectEqualStrings("uint64_t", CEmitter.mapType("UInt"));
    try std.testing.expectEqualStrings("double", CEmitter.mapType("Float"));
    try std.testing.expectEqualStrings("bool", CEmitter.mapType("Bool"));
    try std.testing.expectEqualStrings("forge_string_t", CEmitter.mapType("String"));
    try std.testing.expectEqualStrings("void", CEmitter.mapType("Void"));
    try std.testing.expectEqualStrings("int8_t", CEmitter.mapType("Int8"));
    try std.testing.expectEqualStrings("uint64_t", CEmitter.mapType("UInt64"));
}

test "mapType passes through user-defined types" {
    try std.testing.expectEqualStrings("Point", CEmitter.mapType("Point"));
    try std.testing.expectEqualStrings("MyStruct", CEmitter.mapType("MyStruct"));
}

test "mapTypeId maps builtin type ids" {
    try std.testing.expectEqualStrings("int64_t", CEmitter.mapTypeId(.int));
    try std.testing.expectEqualStrings("double", CEmitter.mapTypeId(.float));
    try std.testing.expectEqualStrings("bool", CEmitter.mapTypeId(.bool));
    try std.testing.expectEqualStrings("forge_string_t", CEmitter.mapTypeId(.string));
    try std.testing.expectEqualStrings("void", CEmitter.mapTypeId(.void));
}

test "mapTypeId returns empty for user-defined types" {
    const user_id = TypeId.fromIndex(TypeId.first_user);
    try std.testing.expectEqualStrings("", CEmitter.mapTypeId(user_id));
}

test "emitter init and deinit" {
    var table = try TypeTable.init(std.testing.allocator);
    defer table.deinit();

    var scope = Scope.init(std.testing.allocator, null);
    defer scope.deinit();

    var methods = std.StringHashMap(MethodEntry).init(std.testing.allocator);
    defer methods.deinit();

    var generics = std.StringHashMap(GenericDecl).init(std.testing.allocator);
    defer generics.deinit();

    var emitter = CEmitter.init(std.testing.allocator, &table, &scope, &methods, &generics);
    defer emitter.deinit();

    try std.testing.expectEqual(@as(usize, 0), emitter.getOutput().len);
}

test "emitPreamble writes includes" {
    var table = try TypeTable.init(std.testing.allocator);
    defer table.deinit();

    var scope = Scope.init(std.testing.allocator, null);
    defer scope.deinit();

    var methods = std.StringHashMap(MethodEntry).init(std.testing.allocator);
    defer methods.deinit();

    var generics = std.StringHashMap(GenericDecl).init(std.testing.allocator);
    defer generics.deinit();

    var emitter = CEmitter.init(std.testing.allocator, &table, &scope, &methods, &generics);
    defer emitter.deinit();

    try emitter.emitPreamble();
    const output = emitter.getOutput();
    try std.testing.expect(std.mem.indexOf(u8, output, "forge_runtime.h") != null);
}

test "stripQuotes strips surrounding double quotes" {
    try std.testing.expectEqualStrings("hello", CEmitter.stripQuotes("\"hello\""));
    try std.testing.expectEqualStrings("", CEmitter.stripQuotes("\"\""));
    try std.testing.expectEqualStrings("no quotes", CEmitter.stripQuotes("no quotes"));
    try std.testing.expectEqualStrings("a", CEmitter.stripQuotes("a"));
}

test "type alias emits typedef" {
    const allocator = std.testing.allocator;

    const source =
        \\type Meters = Int
        \\type Name = String
        \\fn main():
        \\    d: Meters := 42
        \\    print(d.to_string())
        \\
    ;

    var lexer = try Lexer.init(source, allocator);
    defer lexer.deinit();
    const tokens = try lexer.tokenize();
    defer allocator.free(tokens);

    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    var parser = Parser.init(tokens, source, arena.allocator());
    defer parser.deinit();
    const module = try parser.parseModule();

    var checker = try Checker.init(allocator, source);
    defer checker.deinit();
    checker.check(&module);
    try std.testing.expect(!checker.diagnostics.hasErrors());

    var emitter = CEmitter.init(allocator, &checker.type_table, &checker.module_scope, &checker.method_types, &checker.generic_decls);
    defer emitter.deinit();
    try emitter.emitModule(&module);

    const output = emitter.getOutput();

    try std.testing.expect(std.mem.indexOf(u8, output, "typedef int64_t Meters;") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "typedef forge_string_t Name;") != null);
}

test "full pipeline emits valid C for simple program" {
    const allocator = std.testing.allocator;

    const source =
        \\fn main():
        \\    x := 42
        \\    print("hello")
        \\
    ;

    // lex
    var lexer = try Lexer.init(source, allocator);
    defer lexer.deinit();
    const tokens = try lexer.tokenize();
    defer allocator.free(tokens);

    // parse
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    var parser = Parser.init(tokens, source, arena.allocator());
    defer parser.deinit();
    const module = try parser.parseModule();

    // check
    var checker = try Checker.init(allocator, source);
    defer checker.deinit();
    checker.check(&module);
    try std.testing.expect(!checker.diagnostics.hasErrors());

    // emit
    var emitter = CEmitter.init(allocator, &checker.type_table, &checker.module_scope, &checker.method_types, &checker.generic_decls);
    defer emitter.deinit();
    try emitter.emitModule(&module);

    const output = emitter.getOutput();

    // verify key elements in the output
    try std.testing.expect(std.mem.indexOf(u8, output, "int main(int __argc, char **__argv)") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "int64_t x = 42") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "forge_print(FORGE_STRING_LIT(\"hello\"))") != null);
    try std.testing.expect(std.mem.indexOf(u8, output, "return 0;") != null);
}
