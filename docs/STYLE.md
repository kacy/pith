# Forge Style Guide

This guide establishes best practices for writing readable, maintainable Forge code. The goal is code that humans can understand quickly and reason about confidently.

## Core Philosophy

> **Code is read much more often than it is written.**

- Clarity over brevity
- Explicit over clever
- Consistency over novelty

These principles are adapted from Go's readability philosophy, tailored for Forge's specific features and constraints.

---

## Naming Conventions

### Variables

Use descriptive names. Avoid single-letter variables except in these specific cases:

**OK to use single letters:**
- Loop indices: `i`, `j`, `k` for nested loops
- Receivers in short methods: `p` for a `Point` receiver in a 3-line function
- Mathematical conventions: `x`, `y` for coordinates in geometry code

**Always use full words:**
```forge
# GOOD
mut character_index := 0
mut current_line := ""
mut total_count := 0
mut buffer_capacity := 1024

# BAD - cryptic abbreviations
mut pos := 0
mut cur := ""
mut cnt := 0
mut cap := 1024
```

### Loop Variables

Use the full name of what you're iterating over:

```forge
# GOOD
for item in items:
    process(item)

for character in input_line:
    if is_whitespace(character):
        continue

# BAD - cryptic
for n in items:
    process(n)

for c in line:
    if is_ws(c):
        continue
```

For index-based iteration, use meaningful names:

```forge
# GOOD
mut line_index := 0
while line_index < lines.len():
    current_line := lines[line_index]
    process_line(current_line)
    line_index = line_index + 1

# OK for simple cases
mut i := 0
while i < items.len():
    process(items[i])
    i = i + 1
```

### Function Parameters

Parameters should be descriptive, especially when the function is public or long:

```forge
# GOOD - public API
pub fn read_file_contents(file_path: String) -> String:
    return read_file(file_path)

# GOOD - short helper with clear context
fn is_whitespace(char: String) -> Bool:
    return char == " " or char == "\t" or char == "\n"

# BAD - cryptic
fn read(p: String) -> String:
    return read_file(p)

fn is_ws(c: String) -> Bool:
    return c == " " or c == "\t"
```

### Function Names

All functions use `snake_case`. Names should describe what the function does:

```forge
# GOOD
fn calculate_checksum(data: String) -> Int:
fn parse_http_request(raw_request: String) -> Request:
fn validate_email_address(address: String) -> Bool:

# BAD - vague
fn calc(s: String) -> Int:
fn parse(r: String) -> Request:
fn check(a: String) -> Bool:
```

**Predicate functions** (returning Bool) should start with `is_`, `has_`, or `can_`:

```forge
fn is_valid_email(address: String) -> Bool:
fn has_permission(user: User, resource: String) -> Bool:
fn can_execute(command: String) -> Bool:
```

### Type Names

All types use `PascalCase`:

```forge
struct HttpRequest:
    pub method: String
    pub path: String
    pub headers: List[Header]

struct Header:
    pub name: String
    pub value: String

type UserId = Int
type EmailAddress = String
```

---

## Code Organization

### Group Related Declarations

Separate logical sections with blank lines:

```forge
# GOOD
mut input_buffer := ""
mut output_buffer := ""

mut current_position := 0
mut total_lines := 0

# BAD - jumbled
mut buf1 := ""
mut pos := 0
mut buf2 := ""
mut lines := 0
```

### Keep Functions Small

A function should do one thing. If you can't describe it in a single sentence, it might be too big.

```forge
# GOOD - clear purpose
pub fn read_configuration_file(path: String) -> Configuration:
    raw_contents := read_file(path)
    return parse_configuration(raw_contents)

# GOOD - helper for single task
fn parse_configuration(contents: String) -> Configuration:
    mut config := Configuration{}
    mut current_section := ""
    # ... parsing logic ...
    return config

# BAD - doing too much
pub fn load_and_parse_and_validate_and_return_config(p: String) -> Configuration:
    # 50 lines of mixed concerns
```

### Early Returns

Prefer early returns over deep nesting:

```forge
# GOOD - flat, readable
fn find_user_by_id(users: List[User], target_id: Int) -> Option[User]:
    for user in users:
        if user.id != target_id:
            continue
        if not user.is_active:
            continue
        return Some(user)
    return None

# BAD - deeply nested
fn find_user_by_id(users: List[User], target_id: Int) -> Option[User]:
    for user in users:
        if user.id == target_id:
            if user.is_active:
                return Some(user)
    return None
```

---

## Comments and Documentation

### Public API Documentation

Every public function must have a doc comment. The comment should explain:
1. What the function does
2. What parameters it accepts
3. What it returns
4. Any error conditions

```forge
# Reads the entire contents of a file at the given path.
# Returns the file contents as a string.
# Returns an empty string if the file doesn't exist.
# Errors if the file cannot be read (permissions, etc.).
pub fn read_file(path: String) -> String:
    return read_file_internal(path)
```

### Internal Comments

Use comments to explain *why*, not *what*:

```forge
# GOOD - explains the reasoning
# We use a simple linear scan because the list is always small (< 100 items).
mut current_index := 0
while current_index < items.len():
    # ...

# BAD - states the obvious
# Increment the index by 1
current_index = current_index + 1
```

### Section Comments

Use section comments to group related functionality:

```forge
# ============================================================
# File Operations
# ============================================================

pub fn read_file(path: String) -> String:
    # ...

pub fn write_file(path: String, contents: String):
    # ...

# ============================================================
# Directory Operations  
# ============================================================

pub fn list_directory(path: String) -> List[String]:
    # ...
```

---

## Error Handling

### Use the Error Operator for Simple Cases

```forge
# GOOD - simple error propagation
pub fn load_user_data(user_id: Int) -> UserData!:
    raw_data := fetch_from_database(user_id)!  # propagate errors
    return parse_user_data(raw_data)
```

### Handle Errors Explicitly for Complex Cases

```forge
# GOOD - explicit error handling with context
pub fn save_configuration(config: Configuration, path: String) -> Result[Unit, String]:
    json_string := serialize_to_json(config)
    
    write_result := write_file_safe(path, json_string)
    if write_result.is_err():
        error_message := "Failed to save config to " + path + ": " + write_result.error()
        return Err(error_message)
    
    return Ok(Unit)
```

---

## Common Patterns

### String Building

```forge
# GOOD - descriptive variable names
mut result_lines := ""
for line in input_lines:
    trimmed_line := trim_whitespace(line)
    if trimmed_line.len() == 0:
        continue
    result_lines = result_lines + trimmed_line + "\n"
```

### Working with Collections

```forge
# GOOD - use descriptive names
mut active_users := [] as List[User]
for user in all_users:
    if not user.is_active:
        continue
    if user.last_login_days > 30:
        continue
    active_users.push(user)
```

### Mathematical Operations

```forge
# OK - mathematical conventions apply
fn calculate_distance(x1: Float, y1: Float, x2: Float, y2: Float) -> Float:
    dx := x2 - x1
    dy := y2 - y1
    return square_root(dx * dx + dy * dy)
```

---

## Anti-Patterns to Avoid

### 1. Cryptic Abbreviations

```forge
# BAD
g_ooff  # What is this? Object offset? Outgoing offer?
buf
tmp
cur

# GOOD
global_object_offset
buffer
temporary_file
current_user
```

### 2. Mixing Abbreviation Styles

```forge
# BAD - inconsistent
current_position
pos
idx
current_index
current_pos

# GOOD - consistent
current_position
next_position
start_position
end_position
```

### 3. Deep Nesting

```forge
# BAD
if condition_a:
    if condition_b:
        if condition_c:
            do_something()

# GOOD
if not condition_a:
    return
if not condition_b:
    return
if not condition_c:
    return
do_something()
```

### 4. Long Chains of Single Letters

```forge
# BAD - cryptographic code becomes unreadable
mut a := h0
mut b := h1
mut c := h2
# ...

# BETTER - use descriptive names or add comments
mut hash_state_a := initial_hash_value_a
mut hash_state_b := initial_hash_value_b
# ...
```

---

## Standard Abbreviations

These abbreviations are acceptable because they are industry-standard:

| Abbreviation | Full Form | Usage |
|--------------|-----------|-------|
| `ctx` | context | For request/execution context objects |
| `cfg` | configuration | Only when it conflicts with `config` |
| `err` | error | When used as a local error variable |
| `fn` | function | In documentation, not code |
| `id` | identifier | For IDs (database, user, etc.) |
| `num` | number | In mathematical contexts |
| `req` | request | HTTP/request context |
| `resp` | response | HTTP/response context |
| `str` | string | Only in low-level string manipulation |
| `val` | value | Only in generic/map contexts |

When in doubt, spell it out.

---

## File Structure

### Header Comments

Every file should start with a brief description:

```forge
# http_client.fg - HTTP client implementation with connection pooling
#
# This module provides functions for making HTTP requests with support
# for keep-alive connections, retries, and custom headers.

from std.net.tcp import connect, read, write, close
```

### Import Organization

Group imports by source:

```forge
# Standard library imports
from std.fs import read_file, write_file
from std.json import parse, encode

# Third-party imports (when Forge supports them)
# from external.lib import something

# Local/module imports
from .types import Request, Response
from .utils import format_headers
```

---

## Review Checklist

Before submitting code, verify:

- [ ] All public functions have doc comments
- [ ] Variable names are descriptive (not single letters unless loop indices)
- [ ] Function names describe what they do
- [ ] No cryptic abbreviations
- [ ] Early returns preferred over deep nesting
- [ ] Related declarations are grouped
- [ ] Functions are small and focused
- [ ] Comments explain "why", not "what"

---

## Examples

### Complete Example: Configuration Parser

```forge
# config_parser.fg - Parse configuration files in key=value format
#
# Supports comments (#), empty lines, and basic validation.

from std.fs import read_file
from std.string import trim, split

struct Configuration:
    pub settings: Map[String, String]
    pub errors: List[String]

# Parse a configuration file from the given path.
# Returns a Configuration with settings and any parsing errors.
# Empty or missing files return an empty configuration (not an error).
pub fn parse_configuration_file(file_path: String) -> Configuration:
    file_contents := read_file(file_path)
    if file_contents.len() == 0:
        return Configuration{settings: {}, errors: []}
    
    return parse_configuration_contents(file_contents)

fn parse_configuration_contents(contents: String) -> Configuration:
    mut configuration := Configuration{settings: {}, errors: []}
    mut line_number := 1
    
    raw_lines := split(contents, "\n")
    for raw_line in raw_lines:
        current_line := trim(raw_line)
        
        if should_skip_line(current_line):
            line_number = line_number + 1
            continue
        
        parse_result := parse_setting_line(current_line, line_number)
        if parse_result.is_error:
            configuration.errors.push(parse_result.error_message)
        else:
            configuration.settings[parse_result.key] = parse_result.value
        
        line_number = line_number + 1
    
    return configuration

fn should_skip_line(line: String) -> Bool:
    if line.len() == 0:
        return true
    if line[0] == "#":
        return true
    return false

struct ParseResult:
    pub is_error: Bool
    pub key: String
    pub value: String
    pub error_message: String

fn parse_setting_line(line: String, line_number: Int) -> ParseResult:
    equals_position := find_character(line, "=")
    if equals_position < 0:
        return ParseResult{
            is_error: true,
            error_message: "Line " + int_to_string(line_number) + ": Missing '=' in setting"
        }
    
    setting_key := trim(slice(line, 0, equals_position))
    setting_value := trim(slice(line, equals_position + 1, line.len()))
    
    return ParseResult{
        is_error: false,
        key: setting_key,
        value: setting_value
    }
```

This example demonstrates:
- Descriptive variable names (`configuration`, `line_number`, not `cfg`, `num`)
- Clear function names (`should_skip_line`, `parse_setting_line`)
- Early returns to avoid nesting
- Grouped related declarations
- Doc comments on public functions
- Small, focused functions

---

## Contributing

When contributing to the Forge codebase:

1. Follow this style guide
2. Update this guide if you introduce new patterns
3. Prefer readability over cleverness
4. Ask yourself: "Will someone understand this in 6 months?"

---

*Last updated: March 2026*
