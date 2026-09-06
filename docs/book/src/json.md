# JSON

Glyph ships a small JSON type model in `std/json`.

## Types

`JsonValue` is an enum that can represent any JSON value:

```glyph
from std/json import JsonValue
from std/map import Map
from std/vec import Vec

fn main() -> i32 {
  let n = JsonValue::Null()
  let b = JsonValue::Bool(true)
  let x = JsonValue::Number(42.5)
  let s = JsonValue::String(String::from_str("hello"))

  // Arrays and objects use `Vec` and `Map`.
  let arr: Vec<JsonValue> = Vec::new()
  let a = JsonValue::Array(arr)

  let obj: Map<String, JsonValue> = Map::new()
  let o = JsonValue::Object(obj)

  let _keep_alive = (n, b, x, s, a, o)
  ret 0
}
```

Parse errors are represented by `ParseError { message: String, position: usize }`.

## Parsing

The parser lives in `std/json/parser` and returns `ParseResult<T>`:

```glyph
from std/json import JsonValue, ParseResult
from std/json/parser import parse

fn main() -> i32 {
  let r: ParseResult<JsonValue> = parse("{\"k\": 1}")
  ret match r {
    Ok(value) => match value {
      Object(_obj) => 0,
      _ => 1,
    },
    Err(_err) => 2,
  }
}
```

The parser handles objects, arrays, strings and escapes (including `\uXXXX`
and surrogate pairs, decoded to full Unicode), numbers, booleans, `null`,
nested values, trailing-input rejection, and parse errors with source
positions. It is an iterative implementation (no recursive-descent stack
depth limits from Glyph call frames).

## Accessors

`std/json/parser` also exports typed accessors for pulling values out of a
`JsonValue` without writing a `match` by hand:

```glyph
from std/json import JsonValue, ParseResult
from std/enums import Option
from std/json/parser import parse, json_get_string, json_get_number,
  json_get_bool, json_is_null, json_get_array, json_get_object

fn main() -> i32 {
  let r = parse("{\"name\": \"glyph\", \"count\": 3}")
  ret match r {
    Ok(value) => match json_get_object(value) {
      Some(obj) => if obj.has(String::from_str("name")) { 0 } else { 1 },
      None => 2,
    },
    Err(_err) => 3,
  }
}
```

| Function | Signature |
| --- | --- |
| `json_get_string` | `(value: JsonValue) -> Option<String>` |
| `json_get_number` | `(value: JsonValue) -> Option<f64>` |
| `json_get_bool` | `(value: JsonValue) -> Option<bool>` |
| `json_is_null` | `(value: JsonValue) -> bool` |
| `json_get_array` | `(value: JsonValue) -> Option<Vec<JsonValue>>` |
| `json_get_object` | `(value: JsonValue) -> Option<Map<String, JsonValue>>` |

Each accessor returns `None` (or `false` for `json_is_null`) when the value
is a different variant, rather than panicking.

## Serializing

`std/json/parser` also exports `stringify(value: JsonValue) -> String`,
which round-trips through `parse`: strings are escaped, numbers use a
shortest-round-trip representation, and arrays/objects are serialized
recursively via `stringify_array` and `stringify_object`.

```glyph
from std/json import ParseResult
from std/json/parser import parse, stringify

fn main() -> i32 {
  ret match parse("[1, 2, 3]") {
    Ok(value) => {
      let s: str = stringify(value)
      if s.len() > 0 { 0 } else { 1 }
    },
    Err(_err) => 2,
  }
}
```

## Status

`std/json/parser::parse` (an iterative, full-Unicode recursive-descent-style
parser), its accessors, and `stringify` are the shipped, release-gated JSON
API — this is the only parser the compiler embeds. There is no stub fallback:
`crates/glyph-cli/tests/std_json_parser.rs` includes a regression test
(`std_json_parser_embeds_full_parser_not_stub`) asserting the compiler
embeds the full implementation, alongside coverage for nested/trailing
input, string escapes, Unicode, hardening matrices, accessors, and
stringify round-trips.

Map-owned `JsonValue` snapshots (e.g. taking a `JsonValue` by value out of a
`Map::get` on a nested object) are covered and safe: views deep-clone at
ownership-escape points, so there is no dangling or double-free risk. See
`docs/book/src/limitations.md` for value-category and inference rules that
apply generally (not specific to JSON).
