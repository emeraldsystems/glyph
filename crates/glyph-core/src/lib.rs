pub mod ast;
pub mod atomic;
pub mod diag;
pub mod mir;
pub mod span;
pub mod token;
pub mod types;

#[cfg(test)]
mod tests {
    use super::diag::{Diagnostic, Severity};
    use super::span::Span;

    #[test]
    fn spans_join_and_len() {
        let a = Span::new(0, 4);
        let b = Span::new(2, 6);
        let joined = a.join(b);
        assert_eq!(joined.start, 0);
        assert_eq!(joined.end, 6);
        assert_eq!(joined.len(), 6);
    }

    #[test]
    fn diagnostics_store_message_and_severity() {
        let diag = Diagnostic::error("oops", Some(Span::new(1, 2)));
        assert_eq!(diag.message, "oops");
        assert!(matches!(diag.severity, Severity::Error));
        assert_eq!(diag.span.unwrap().start, 1);
    }

    #[test]
    fn diagnostics_support_module_ids() {
        let diag = Diagnostic::error("oops", Some(Span::new(1, 2)));
        assert!(diag.module_id.is_none());

        let tagged = Diagnostic::error("oops", None).with_module_id("main");
        assert_eq!(tagged.module_id.as_deref(), Some("main"));
    }

    #[test]
    fn struct_def_construction() {
        use super::ast::{FieldDef, Ident, StructDef, TypeExpr};

        let field1 = FieldDef {
            name: Ident("x".into()),
            ty: TypeExpr::Path {
                segments: vec!["i32".into()],
                span: Span::new(0, 5),
            },
            span: Span::new(0, 5),
        };
        let field2 = FieldDef {
            name: Ident("y".into()),
            ty: TypeExpr::Path {
                segments: vec!["i32".into()],
                span: Span::new(6, 11),
            },
            span: Span::new(6, 11),
        };

        let struct_def = StructDef {
            name: Ident("Point".into()),
            generic_params: Vec::new(),
            fields: vec![field1, field2],
            interfaces: Vec::new(),
            methods: Vec::new(),
            inline_impls: Vec::new(),
            span: Span::new(0, 20),
        };

        assert_eq!(struct_def.name.0, "Point");
        assert_eq!(struct_def.fields.len(), 2);
        assert_eq!(struct_def.fields[0].name.0, "x");
        match &struct_def.fields[1].ty {
            TypeExpr::Path { segments, .. } => assert_eq!(segments[0], "i32"),
            _ => panic!("expected path type"),
        }
    }

    #[test]
    fn struct_lit_expression() {
        use super::ast::{Expr, Ident, Literal};

        let expr = Expr::StructLit {
            name: Ident("Point".into()),
            fields: vec![
                (
                    Ident("x".into()),
                    Expr::Lit(Literal::Int(10), Span::new(0, 2)),
                ),
                (
                    Ident("y".into()),
                    Expr::Lit(Literal::Int(20), Span::new(4, 6)),
                ),
            ],
            span: Span::new(0, 10),
        };

        if let Expr::StructLit { name, fields, .. } = expr {
            assert_eq!(name.0, "Point");
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].0.0, "x");
        } else {
            panic!("Expected StructLit");
        }
    }

    #[test]
    fn field_access_expression() {
        use super::ast::{Expr, Ident};

        let base = Expr::Ident(Ident("p".into()), Span::new(0, 1));
        let expr = Expr::FieldAccess {
            base: Box::new(base),
            field: Ident("x".into()),
            span: Span::new(0, 3),
        };

        if let Expr::FieldAccess { field, .. } = expr {
            assert_eq!(field.0, "x");
        } else {
            panic!("Expected FieldAccess");
        }
    }

    #[test]
    fn type_is_ref_immutable() {
        use super::types::{Mutability, Type};

        let ref_type = Type::Ref(Box::new(Type::I32), Mutability::Immutable);
        assert!(ref_type.is_ref());
        assert!(!ref_type.is_mut_ref());
    }

    #[test]
    fn type_is_ref_mutable() {
        use super::types::{Mutability, Type};

        let ref_type = Type::Ref(Box::new(Type::I32), Mutability::Mutable);
        assert!(ref_type.is_ref());
        assert!(ref_type.is_mut_ref());
    }

    #[test]
    fn type_inner_type() {
        use super::types::{Mutability, Type};

        let ref_type = Type::Ref(Box::new(Type::I32), Mutability::Immutable);
        assert_eq!(ref_type.inner_type(), Some(&Type::I32));

        let plain_type = Type::I32;
        assert_eq!(plain_type.inner_type(), None);
    }

    #[test]
    fn type_nested_references() {
        use super::types::{Mutability, Type};

        // &mut &i32
        let nested = Type::Ref(
            Box::new(Type::Ref(Box::new(Type::I32), Mutability::Immutable)),
            Mutability::Mutable,
        );

        assert!(nested.is_ref());
        assert!(nested.is_mut_ref());

        let inner = nested.inner_type().unwrap();
        assert!(inner.is_ref());
        assert!(!inner.is_mut_ref());
    }

    #[test]
    fn array_type_construction() {
        use super::types::Type;

        let arr_ty = Type::Array(Box::new(Type::I32), 10);
        assert!(arr_ty.is_array());
        assert_eq!(arr_ty.array_size(), Some(10));

        let elem_ty = arr_ty.array_element_type().unwrap();
        assert_eq!(*elem_ty, Type::I32);
    }

    #[test]
    fn array_helper_methods() {
        use super::types::Type;

        let arr_ty = Type::Array(Box::new(Type::Bool), 5);

        assert!(arr_ty.is_array());
        assert!(!arr_ty.is_int());
        assert!(!arr_ty.is_ref());

        assert_eq!(arr_ty.array_size(), Some(5));
        assert_eq!(arr_ty.array_element_type(), Some(&Type::Bool));

        // Non-array types should return None
        let i32_ty = Type::I32;
        assert!(!i32_ty.is_array());
        assert_eq!(i32_ty.array_size(), None);
        assert_eq!(i32_ty.array_element_type(), None);
    }

    #[test]
    fn nested_array_types() {
        use super::types::Type;

        // [[i32; 3]; 2] - 2D array
        let inner_arr = Type::Array(Box::new(Type::I32), 3);
        let outer_arr = Type::Array(Box::new(inner_arr.clone()), 2);

        assert!(outer_arr.is_array());
        assert_eq!(outer_arr.array_size(), Some(2));

        let inner = outer_arr.array_element_type().unwrap();
        assert!(inner.is_array());
        assert_eq!(inner.array_size(), Some(3));
        assert_eq!(inner.array_element_type(), Some(&Type::I32));
    }
}
