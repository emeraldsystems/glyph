# Standard Library

The standard library keeps a tight surface area:

- `std/io` for I/O
- `std/vec` for vectors
- `std/map` for maps
- `std/string` for strings
- `std/enums` for `Option<T>` and `Result<T, E>`
- `std/json` for JSON types (and `std/json/parser`)
- `std/process` for launching external processes
- `std/time` for timestamps and UTC formatting
- `std/sys` for low-level system access

String utilities live on `String` and include `clone`, `len`, `concat`, `slice`, `trim`,
`split`, `starts_with`, and `ends_with`.
