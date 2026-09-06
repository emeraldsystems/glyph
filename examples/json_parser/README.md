# JSON Parser Example

Demonstrates using Glyph's standard-library JSON parser: `std/json/parser::parse`.

## What this example does

- Calls `parse(...)` on a handful of JSON inputs.
- Uses pattern matching to verify the parsed value has the expected shape (including nested arrays/objects).
- Exits non-zero if parsing fails.

## Build and run

From this directory:

```bash
glyph run
```

## API surface

```glyph
from std/json import JsonValue, ParseResult
from std/json/parser import parse

fn parse(input: &str) -> ParseResult<JsonValue>
```

## Parser notes

`std/json/parser::parse` is the full, release-shipping implementation (an
iterative recursive-descent parser with full Unicode escape support). This
example's nested-array/object and trailing-input checks exercise real parse
results, not a placeholder.
