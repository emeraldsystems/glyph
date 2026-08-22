use glyph_core::ast::Expr;
use glyph_core::mir::{LocalId, MirInst, MirValue, Rvalue};
use glyph_core::span::Span;
use glyph_core::types::{Mutability, Type};

use crate::resolver::ResolvedSymbol;

use super::super::context::{LocalState, LowerCtx};
use super::super::expr::lower_value;
use super::super::value::infer_value_type;

pub(crate) fn is_canonical_mutex_new(ctx: &LowerCtx<'_>, name: &str) -> bool {
    name == "Mutex::new"
        && matches!(
            ctx.resolver.resolve_symbol("Mutex"),
            Some(ResolvedSymbol::Struct(module, symbol))
                if module == "std/sync" && symbol == "Mutex"
        )
}

pub(crate) fn lower_mutex_new<'a>(
    ctx: &mut LowerCtx<'a>,
    args: &'a [Expr],
    span: Span,
) -> Option<Rvalue> {
    if args.len() != 1 {
        ctx.error("Mutex::new expects exactly one argument", Some(span));
        return Some(Rvalue::ConstInt(0));
    }
    if matches!(&args[0], Expr::Ref { .. }) {
        ctx.error("Mutex::new cannot own a borrowed reference", Some(span));
        return Some(Rvalue::ConstInt(0));
    }
    let value = lower_value(ctx, &args[0])?;
    let elem_type = infer_value_type(&value, ctx)?;
    if matches!(elem_type, Type::Ref(_, _)) {
        ctx.error("Mutex::new cannot own a borrowed reference", Some(span));
        return Some(Rvalue::ConstInt(0));
    }
    let result = ctx.fresh_local(None);
    ctx.locals[result.0 as usize].ty = Some(Type::mutex(elem_type.clone()));
    ctx.push_inst(MirInst::Assign {
        local: result,
        value: Rvalue::MutexNew { value, elem_type },
    });
    Some(Rvalue::Move(result))
}

fn receiver(ctx: &mut LowerCtx<'_>, expr: &Expr) -> Option<(LocalId, Type)> {
    let Expr::Ident(name, span) = expr else {
        return None;
    };
    let local = ctx.bindings.get(name.0.as_str()).copied()?;
    if !ctx.check_local_available(local, Some(*span)) {
        return None;
    }
    let ty = ctx.local_ty(local)?.clone();
    let inner = ty.mutex_inner_type().cloned().or_else(|| match &ty {
        Type::Ref(inner, _) => inner.mutex_inner_type().cloned(),
        _ => None,
    })?;
    Some((local, inner))
}

fn guard_receiver(ctx: &mut LowerCtx<'_>, expr: &Expr) -> Option<(LocalId, Type, Span)> {
    let Expr::Ident(name, span) = expr else {
        return None;
    };
    let guard = ctx.bindings.get(name.0.as_str()).copied()?;
    if !ctx.check_local_available(guard, Some(*span)) {
        return None;
    }
    let inner = ctx.local_ty(guard)?.mutex_guard_inner_type()?.clone();
    Some((guard, inner, *span))
}

fn require_no_args(ctx: &mut LowerCtx<'_>, method: &str, args: &[Expr], span: Span) -> bool {
    if args.is_empty() {
        true
    } else {
        ctx.error(
            format!("Mutex.{method}() does not take arguments"),
            Some(span),
        );
        false
    }
}

fn lower_try_lock(ctx: &mut LowerCtx<'_>, owner: LocalId, elem_type: Type, span: Span) -> Rvalue {
    let guard_type = Type::mutex_guard(elem_type.clone());
    let guard = ctx.fresh_local(None);
    ctx.locals[guard.0 as usize].ty = Some(guard_type.clone());
    ctx.push_inst(MirInst::Assign {
        local: guard,
        value: Rvalue::MutexTryLock {
            base: owner,
            elem_type,
        },
    });
    let acquired = ctx.fresh_local(None);
    ctx.locals[acquired.0 as usize].ty = Some(Type::Bool);
    ctx.push_inst(MirInst::Assign {
        local: acquired,
        value: Rvalue::MutexGuardIsAcquired {
            guard,
            elem_type: guard_type
                .mutex_guard_inner_type()
                .expect("constructed guard")
                .clone(),
        },
    });

    let some_block = ctx.new_block();
    let none_block = ctx.new_block();
    let join_block = ctx.new_block();
    let option_type = Type::App {
        base: "Option".into(),
        args: vec![guard_type],
    };
    let result = ctx.fresh_local(None);
    ctx.locals[result.0 as usize].ty = Some(option_type);
    ctx.mark_mutex_try_option(result);
    ctx.push_inst(MirInst::If {
        cond: MirValue::Local(acquired),
        then_bb: some_block,
        else_bb: none_block,
    });

    ctx.switch_to(some_block);
    ctx.push_inst(MirInst::Assign {
        local: result,
        value: Rvalue::EnumConstruct {
            enum_name: "Option".into(),
            variant_index: 1,
            payload: Some(MirValue::Local(guard)),
        },
    });
    ctx.push_inst(MirInst::Goto(join_block));

    ctx.local_states[result.0 as usize] = LocalState::Uninitialized;
    ctx.switch_to(none_block);
    if let Some(state) = ctx.local_states.get_mut(guard.0 as usize) {
        *state = LocalState::Moved;
    }
    ctx.push_inst(MirInst::Assign {
        local: result,
        value: Rvalue::EnumConstruct {
            enum_name: "Option".into(),
            variant_index: 0,
            payload: None,
        },
    });
    ctx.push_inst(MirInst::Goto(join_block));
    ctx.local_states[result.0 as usize] = LocalState::Initialized;
    ctx.switch_to(join_block);
    ctx.register_mutex_guard(result, owner, span);
    Rvalue::Move(result)
}

pub(crate) fn lower_mutex_method<'a>(
    ctx: &mut LowerCtx<'a>,
    receiver_expr: &'a Expr,
    method: &str,
    args: &'a [Expr],
    span: Span,
) -> Option<Option<Rvalue>> {
    if matches!(method, "borrow" | "borrow_mut") {
        let (guard, elem_type, origin) = guard_receiver(ctx, receiver_expr)?;
        if !require_no_args(ctx, method, args, span) {
            return Some(None);
        }
        let result = ctx.fresh_local(None);
        ctx.locals[result.0 as usize].ty =
            Some(Type::Ref(Box::new(elem_type.clone()), Mutability::Mutable));
        ctx.push_inst(MirInst::Assign {
            local: result,
            value: Rvalue::MutexGuardBorrow { guard, elem_type },
        });
        ctx.register_mutex_guard_borrow(result, guard, origin);
        return Some(Some(Rvalue::Move(result)));
    }

    if !matches!(method, "lock" | "try_lock") {
        return None;
    }
    let (owner, elem_type) = receiver(ctx, receiver_expr)?;
    if !require_no_args(ctx, method, args, span) {
        return Some(None);
    }
    if method == "try_lock" {
        return Some(Some(lower_try_lock(ctx, owner, elem_type, span)));
    }

    let guard = ctx.fresh_local(None);
    ctx.locals[guard.0 as usize].ty = Some(Type::mutex_guard(elem_type.clone()));
    ctx.push_inst(MirInst::Assign {
        local: guard,
        value: Rvalue::MutexLock {
            base: owner,
            elem_type,
        },
    });
    ctx.register_mutex_guard(guard, owner, span);
    Some(Some(Rvalue::Move(guard)))
}
