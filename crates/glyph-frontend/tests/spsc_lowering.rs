use glyph_core::mir::{MirInst, Rvalue};
use glyph_core::types::Type;
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

fn rvalues(output: &glyph_frontend::FrontendOutput) -> impl Iterator<Item = &Rvalue> {
    output
        .mir
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.insts)
        .filter_map(|instruction| match instruction {
            MirInst::Assign { value, .. } => Some(value),
            _ => None,
        })
}

#[test]
fn canonical_channel_and_result_methods_lower_to_specialized_mir() {
    let output = compile(
        r#"
from std/sync/spsc import channel, Sender, Receiver, TrySendResult, TryRecvResult

fn main() -> i32 {
  let mut receiver: Receiver<i32>
  let sender: Sender<i32> = channel(2, &mut receiver)
  let sent: TrySendResult<i32> = sender.try_send(7)
  let received: TryRecvResult<i32> = receiver.try_recv()
  ret 0
}
"#,
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert!(rvalues(&output).any(|value| matches!(value, Rvalue::SpscChannelNew { .. })));
    assert!(rvalues(&output).any(|value| matches!(value, Rvalue::SpscTrySend { .. })));
    assert!(rvalues(&output).any(|value| matches!(value, Rvalue::SpscTryRecv { .. })));
    assert!(
        output
            .mir
            .functions
            .iter()
            .flat_map(|f| &f.locals)
            .any(|local| local.ty.as_ref() == Some(&Type::spsc_sender(Type::I32)))
    );
    assert!(
        output
            .mir
            .functions
            .iter()
            .flat_map(|f| &f.locals)
            .any(|local| local.ty.as_ref() == Some(&Type::spsc_receiver(Type::I32)))
    );
}

#[test]
fn one_argument_channel_supports_the_exact_endpoint_tuple_type() {
    let output = compile(
        r#"
from std/sync/spsc import channel, Sender, Receiver

fn main() -> i32 {
  let pair: (Sender<i32>, Receiver<i32>) = channel(4)
  ret 0
}
"#,
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert!(rvalues(&output).any(|value| matches!(value, Rvalue::SpscChannelNew { .. })));
    assert!(rvalues(&output).any(|value| matches!(value, Rvalue::StructLit { .. })));
}

#[test]
fn channel_payloads_must_be_owned_and_endpoint_clone_is_rejected() {
    let borrowed = compile(
        r#"
from std/sync/spsc import channel, Sender, Receiver

fn main() -> i32 {
  let mut receiver: Receiver<&i32>
  let sender: Sender<&i32> = channel(2, &mut receiver)
  ret 0
}
"#,
    );
    assert!(
        borrowed.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("cannot carry borrowed references")),
        "{:?}",
        borrowed.diagnostics
    );

    let cloned = compile(
        r#"
from std/sync/spsc import channel, Sender, Receiver

fn main() -> i32 {
  let mut receiver: Receiver<i32>
  let sender: Sender<i32> = channel(2, &mut receiver)
  let duplicate = sender.clone()
  ret 0
}
"#,
    );
    assert!(
        cloned
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("unique and cannot be cloned")),
        "{:?}",
        cloned.diagnostics
    );
}

#[test]
fn same_named_user_sender_never_selects_the_intrinsic() {
    let output = compile_source(
        r#"
struct Sender<T> { value: T }

fn main() -> i32 {
  let sender = Sender { value: 1 }
  let result = sender.try_send(2)
  ret 0
}
"#,
        FrontendOptions {
            emit_mir: true,
            include_std: false,
        },
    );
    assert!(!rvalues(&output).any(|value| matches!(value, Rvalue::SpscTrySend { .. })));
    assert!(
        output
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("no method 'try_send'")),
        "{:?}",
        output.diagnostics
    );
}

#[test]
fn endpoints_are_send_but_not_sync_and_require_send_payloads() {
    let accepted = compile(
        r#"
from std/sync/spsc import channel, Sender, Receiver
import spawn from std/thread

fn main() -> i32 {
  let mut receiver: Receiver<i32>
  let sender: Sender<i32> = channel(2, &mut receiver)
  let task: FnOnce<(), ()> = move () -> {
    let sent = sender.try_send(1)
  }
  let outcome = spawn(task)
  ret 0
}
"#,
    );
    assert!(
        accepted.diagnostics.is_empty(),
        "{:?}",
        accepted.diagnostics
    );

    let rejected = compile(
        r#"
from std/sync/spsc import channel, Sender, Receiver
import spawn from std/thread

fn main() -> i32 {
  let mut receiver: Receiver<Shared<i32>>
  let sender: Sender<Shared<i32>> = channel(2, &mut receiver)
  let task: FnOnce<(), ()> = move () -> {
    let sent = sender.try_send(Shared::new(1))
  }
  let outcome = spawn(task)
  ret 0
}
"#,
    );
    assert!(
        rejected
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("requires `T: Send`")),
        "{:?}",
        rejected.diagnostics
    );
}
