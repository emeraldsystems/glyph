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

The parser handles objects, arrays, strings and escapes, numbers, booleans,
`null`, nested values, trailing-input rejection, and parse errors with source
positions.

## Status

`std/json` provides the JSON types, and `std/json/parser::parse` is the shipped
parser. Current ownership semantics around map-owned `JsonValue` snapshots are
still being tightened: avoid APIs that keep by-value snapshots from `Map::get`;
move the owning value, borrow where possible, or clone/deep-copy when that API is
available.
