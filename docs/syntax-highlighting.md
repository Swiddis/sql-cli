# Syntax Highlighting for Calcite Plans

## Overview

When using the `--explain` flag, the CLI automatically colorizes Calcite query plans for better readability.

## Example

```bash
$ echo "source=accounts | stats count() by gender" | ppl --explain
```

Output shows:
- **Operators** (LogicalProject, EnumerableCalc) in bright cyan bold
- **Functions** (COUNT, SUM) in cyan
- **Variables** ($0, $t1) in yellow
- **Attributes** (field=) in magenta
- **Numbers** in blue
- **Strings** in green

## How It Works

The highlighter uses a context-aware parser that:
1. Tokenizes the plan into structural elements
2. Determines semantic meaning from context
3. Applies appropriate colors based on role

For example, it distinguishes:
- `LogicalProject(...)` ← operator (start of line + capitalized)
- `COUNT(...)` ← function (mid-line or nested)
- `field=value` ← attribute (identifier before `=`)
- `=(a, b)` ← function (not a comment!)
- `= Calcite Plan =` ← comment (line starting with `=`)

## Implementation

See:
- `src/explain/parser.rs` - Lexer and highlighter
- `src/explain/mod.rs` - Public API
- `HIGHLIGHTING.md` - Technical details
- `SUMMARY.md` - Design decisions

## Testing

```bash
# Run tests
cargo test

# See colored output
cargo test test_highlighting_visual -- --nocapture
```
