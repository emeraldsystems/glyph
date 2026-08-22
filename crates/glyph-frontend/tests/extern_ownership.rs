use glyph_frontend::{FrontendOptions, compile_source};

fn compile(source: &str) -> glyph_frontend::FrontendOutput {
    compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    )
}

#[test]
fn extern_parameters_reject_owned_droppable_values_by_value() {
    let output = compile(
        r#"
struct OwnedRecord { text: String }

extern "C" fn take_string(value: String) -> i32;
extern "C" fn take_own(value: Own<i32>) -> i32;
extern "C" fn take_shared(value: Shared<i32>) -> i32;
extern "C" fn take_vec(value: Vec<String>) -> i32;
extern "C" fn take_map(value: Map<String, String>) -> i32;
extern "C" fn take_record(value: OwnedRecord) -> i32;

fn main() -> i32 { ret 0 }
"#,
    );

    let messages = output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    for (function, ty) in [
        ("take_string", "String"),
        ("take_own", "Own<i32>"),
        ("take_shared", "Shared<i32>"),
        ("take_vec", "Vec<String>"),
        ("take_map", "Map<String, String>"),
        ("take_record", "OwnedRecord"),
    ] {
        assert!(
            messages.iter().any(|message| {
                message.contains(function)
                    && message.contains(ty)
                    && message.contains("cannot take ownership")
            }),
            "missing extern ownership diagnostic for {function}({ty}); diagnostics: {:?}",
            output.diagnostics
        );
    }
}

#[test]
fn extern_parameters_allow_scalars_raw_pointers_and_references() {
    let output = compile(
        r#"
extern "C" fn observe(count: i32, view: str, raw: RawPtr<i32>, text: &String, values: &Vec<String>) -> i32;

fn main() -> i32 {
  let text = String::from_str("still owned")
  let values: Vec<String> = Vec::new()
  let allocation = Own::new(7)
  let raw = allocation.into_raw()
  let result = observe(1, "borrowed", raw, &text, &values)
  ret text.len()
}
"#,
    );

    assert!(
        output.diagnostics.is_empty(),
        "unexpected diagnostics for non-owning extern parameters: {:?}",
        output.diagnostics
    );
}
