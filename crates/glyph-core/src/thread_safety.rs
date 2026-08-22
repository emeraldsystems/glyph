//! Structural `Send` and `Sync` predicates for thread-escaping values.
//!
//! Glyph deliberately does not expose user-implementable marker interfaces in
//! the first concurrency release. These checks are therefore compiler policy,
//! centralized here so closure and thread lowering cannot disagree.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::types::{EnumType, StructType, Type};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThreadSafetyRequirement {
    Send,
    Sync,
}

impl fmt::Display for ThreadSafetyRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Send => f.write_str("Send"),
            Self::Sync => f.write_str("Sync"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSafetyError {
    pub requirement: ThreadSafetyRequirement,
    /// Source-oriented path such as `task.state.shared` or `result::Err[0]`.
    pub path: String,
    pub reason: String,
}

impl fmt::Display for ThreadSafetyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not {}: {}",
            self.path, self.requirement, self.reason
        )
    }
}

impl std::error::Error for ThreadSafetyError {}

/// Read-only registry used to evaluate structural thread-safety rules.
pub struct ThreadSafetyRegistry<'a> {
    structs: &'a HashMap<String, StructType>,
    enums: &'a HashMap<String, EnumType>,
}

impl<'a> ThreadSafetyRegistry<'a> {
    pub fn new(
        structs: &'a HashMap<String, StructType>,
        enums: &'a HashMap<String, EnumType>,
    ) -> Self {
        Self { structs, enums }
    }

    pub fn check_send(&self, root: &str, ty: &Type) -> Result<(), ThreadSafetyError> {
        self.check(ty, ThreadSafetyRequirement::Send, root, &mut HashSet::new())
    }

    pub fn check_sync(&self, root: &str, ty: &Type) -> Result<(), ThreadSafetyError> {
        self.check(ty, ThreadSafetyRequirement::Sync, root, &mut HashSet::new())
    }

    /// Check the concrete environment of an owned callable.
    ///
    /// `Type::Function` does not carry capture types because all callable
    /// values share an erased ABI. Closure conversion must therefore use this
    /// entry point while it still has capture metadata.
    pub fn check_callable_send<'b, I>(&self, captures: I) -> Result<(), ThreadSafetyError>
    where
        I: IntoIterator<Item = (&'b str, &'b Type)>,
    {
        for (name, ty) in captures {
            self.check_send(&format!("capture `{name}`"), ty)?;
        }
        Ok(())
    }

    fn check(
        &self,
        ty: &Type,
        requirement: ThreadSafetyRequirement,
        path: &str,
        visiting: &mut HashSet<(String, ThreadSafetyRequirement)>,
    ) -> Result<(), ThreadSafetyError> {
        match ty {
            Type::I8
            | Type::I32
            | Type::I64
            | Type::U8
            | Type::U32
            | Type::U64
            | Type::Usize
            | Type::F32
            | Type::F64
            | Type::Bool
            | Type::Char
            | Type::String
            | Type::Void
            | Type::Atomic(_) => Ok(()),

            Type::Str => self.reject(
                requirement,
                path,
                "borrowed `str` may not escape to a thread",
            ),
            Type::Ref(..) => self.reject(
                requirement,
                path,
                "borrowed references may not escape to a thread",
            ),
            Type::RawPtr(..) => self.reject(
                requirement,
                path,
                "raw pointers have no compiler-verifiable ownership protocol",
            ),
            Type::Shared(..) => self.reject(
                requirement,
                path,
                "`Shared<T>` is not synchronized and cannot cross threads",
            ),

            Type::Own(inner) => self.check(inner, requirement, path, visiting),
            Type::Array(inner, _) => self.check(inner, requirement, &format!("{path}[]"), visiting),
            Type::Tuple(elements) => {
                for (index, element) in elements.iter().enumerate() {
                    self.check(element, requirement, &format!("{path}[{index}]"), visiting)?;
                }
                Ok(())
            }
            Type::Named(name) => self.check_named(name, requirement, path, visiting),
            Type::Enum(name) => self.check_enum(name, requirement, path, visiting),
            Type::Param(name) => self.reject(
                requirement,
                path,
                &format!(
                    "generic parameter `{name}` was not resolved before thread-safety checking"
                ),
            ),
            Type::Function { .. } => self.reject(
                requirement,
                path,
                if requirement == ThreadSafetyRequirement::Send {
                    "callable `Send` depends on its concrete capture environment"
                } else {
                    "owned `FnOnce` values are not `Sync`"
                },
            ),
            Type::App { base, args } => {
                self.check_application(base, args, requirement, path, visiting)
            }
        }
    }

    fn check_named(
        &self,
        name: &str,
        requirement: ThreadSafetyRequirement,
        path: &str,
        visiting: &mut HashSet<(String, ThreadSafetyRequirement)>,
    ) -> Result<(), ThreadSafetyError> {
        if self.structs.contains_key(name) {
            return self.check_struct(name, requirement, path, visiting);
        }
        if self.enums.contains_key(name) {
            return self.check_enum(name, requirement, path, visiting);
        }
        self.reject(
            requirement,
            path,
            &format!("type `{name}` has no structural thread-safety definition"),
        )
    }

    fn check_struct(
        &self,
        name: &str,
        requirement: ThreadSafetyRequirement,
        path: &str,
        visiting: &mut HashSet<(String, ThreadSafetyRequirement)>,
    ) -> Result<(), ThreadSafetyError> {
        let key = (format!("struct:{name}"), requirement);
        if !visiting.insert(key.clone()) {
            // Recursive ownership graphs are accepted provisionally; every
            // non-recursive edge is still visited before this cycle closes.
            return Ok(());
        }
        let result = (|| {
            let definition = self.structs.get(name).expect("checked above");
            for (field, ty) in &definition.fields {
                self.check(ty, requirement, &format!("{path}.{field}"), visiting)?;
            }
            Ok(())
        })();
        visiting.remove(&key);
        result
    }

    fn check_enum(
        &self,
        name: &str,
        requirement: ThreadSafetyRequirement,
        path: &str,
        visiting: &mut HashSet<(String, ThreadSafetyRequirement)>,
    ) -> Result<(), ThreadSafetyError> {
        let key = (format!("enum:{name}"), requirement);
        if !visiting.insert(key.clone()) {
            return Ok(());
        }
        let result = (|| match self.enums.get(name) {
            Some(definition) => {
                for variant in &definition.variants {
                    if let Some(payload) = &variant.payload {
                        self.check(
                            payload,
                            requirement,
                            &format!("{path}::{}", variant.name),
                            visiting,
                        )?;
                    }
                }
                Ok(())
            }
            None => self.reject(
                requirement,
                path,
                &format!("enum `{name}` has no structural thread-safety definition"),
            ),
        })();
        visiting.remove(&key);
        result
    }

    fn check_application(
        &self,
        base: &str,
        args: &[Type],
        requirement: ThreadSafetyRequirement,
        path: &str,
        visiting: &mut HashSet<(String, ThreadSafetyRequirement)>,
    ) -> Result<(), ThreadSafetyError> {
        use ThreadSafetyRequirement::{Send, Sync};

        let require_arity = |expected: usize| {
            if args.len() == expected {
                Ok(())
            } else {
                self.reject(
                    requirement,
                    path,
                    &format!(
                        "`{base}` expected {expected} type arguments, found {}",
                        args.len()
                    ),
                )
            }
        };

        match base {
            "Vec" | "Option" => {
                require_arity(1)?;
                self.check(&args[0], requirement, &format!("{path}.value"), visiting)
            }
            "Map" => {
                require_arity(2)?;
                self.check(&args[0], requirement, &format!("{path}.key"), visiting)?;
                self.check(&args[1], requirement, &format!("{path}.value"), visiting)
            }
            "Result" => {
                require_arity(2)?;
                self.check(&args[0], requirement, &format!("{path}::Ok"), visiting)?;
                self.check(&args[1], requirement, &format!("{path}::Err"), visiting)
            }
            "Arc" => {
                require_arity(1)?;
                // Arc is Send and Sync only when the pointee satisfies both.
                self.check(&args[0], Send, &format!("{path}.value"), visiting)?;
                self.check(&args[0], Sync, &format!("{path}.value"), visiting)
            }
            "Mutex" => {
                require_arity(1)?;
                // A mutex supplies synchronized shared access; moving the
                // protected value between lock holders still requires Send.
                self.check(&args[0], Send, &format!("{path}.value"), visiting)
            }
            "JoinHandle" => {
                require_arity(1)?;
                if requirement == Sync {
                    return self.reject(requirement, path, "join handles have one exclusive owner");
                }
                self.check(&args[0], Send, &format!("{path}.result"), visiting)
            }
            "Sender" | "Receiver" => {
                require_arity(1)?;
                if requirement == Sync {
                    return self.reject(
                        requirement,
                        path,
                        "SPSC endpoints require one exclusive owner",
                    );
                }
                self.check(&args[0], Send, &format!("{path}.item"), visiting)
            }
            "MutexGuard" => self.reject(
                requirement,
                path,
                "mutex guards are noescape and cannot cross threads",
            ),
            _ => self.reject(
                requirement,
                path,
                &format!("generic type `{base}` has no structural thread-safety rule"),
            ),
        }
    }

    fn reject<T>(
        &self,
        requirement: ThreadSafetyRequirement,
        path: &str,
        reason: &str,
    ) -> Result<T, ThreadSafetyError> {
        Err(ThreadSafetyError {
            requirement,
            path: path.to_string(),
            reason: reason.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atomic::AtomicScalar;
    use crate::types::EnumVariant;

    fn registry<'a>(
        structs: &'a HashMap<String, StructType>,
        enums: &'a HashMap<String, EnumType>,
    ) -> ThreadSafetyRegistry<'a> {
        ThreadSafetyRegistry::new(structs, enums)
    }

    #[test]
    fn scalars_strings_and_atomics_are_send_and_sync() {
        let structs = HashMap::new();
        let enums = HashMap::new();
        let registry = registry(&structs, &enums);
        for ty in [Type::I32, Type::String, Type::Atomic(AtomicScalar::Usize)] {
            assert!(registry.check_send("value", &ty).is_ok());
            assert!(registry.check_sync("value", &ty).is_ok());
        }
    }

    #[test]
    fn nested_struct_reports_the_first_exact_field_path() {
        let structs = HashMap::from([
            (
                "Inner".into(),
                StructType {
                    name: "Inner".into(),
                    fields: vec![("raw".into(), Type::RawPtr(Box::new(Type::I32)))],
                },
            ),
            (
                "Outer".into(),
                StructType {
                    name: "Outer".into(),
                    fields: vec![("inner".into(), Type::Named("Inner".into()))],
                },
            ),
        ]);
        let enums = HashMap::new();
        let error = registry(&structs, &enums)
            .check_send("capture `state`", &Type::Named("Outer".into()))
            .unwrap_err();
        assert_eq!(error.path, "capture `state`.inner.raw");
        assert_eq!(error.requirement, ThreadSafetyRequirement::Send);
    }

    #[test]
    fn enum_variant_and_tuple_indices_are_in_the_path() {
        let structs = HashMap::new();
        let enums = HashMap::from([(
            "Message".into(),
            EnumType {
                name: "Message".into(),
                variants: vec![EnumVariant {
                    name: "Data".into(),
                    payload: Some(Type::Tuple(vec![
                        Type::I32,
                        Type::Shared(Box::new(Type::I32)),
                    ])),
                }],
            },
        )]);
        let error = registry(&structs, &enums)
            .check_send("message", &Type::Enum("Message".into()))
            .unwrap_err();
        assert_eq!(error.path, "message::Data[1]");
    }

    #[test]
    fn arc_cannot_launder_a_non_sync_pointee() {
        let structs = HashMap::new();
        let enums = HashMap::new();
        let ty = Type::App {
            base: "Arc".into(),
            args: vec![Type::Shared(Box::new(Type::I32))],
        };
        let error = registry(&structs, &enums)
            .check_send("arc", &ty)
            .unwrap_err();
        assert_eq!(error.path, "arc.value");

        let send_but_not_sync = Type::App {
            base: "Arc".into(),
            args: vec![Type::App {
                base: "JoinHandle".into(),
                args: vec![Type::I32],
            }],
        };
        let error = registry(&structs, &enums)
            .check_send("arc", &send_but_not_sync)
            .unwrap_err();
        assert_eq!(error.path, "arc.value");
        assert_eq!(error.requirement, ThreadSafetyRequirement::Sync);
    }

    #[test]
    fn mutex_requires_send_but_supplies_sync() {
        let structs = HashMap::new();
        let enums = HashMap::new();
        let good = Type::App {
            base: "Mutex".into(),
            args: vec![Type::String],
        };
        let registry = registry(&structs, &enums);
        assert!(registry.check_send("mutex", &good).is_ok());
        assert!(registry.check_sync("mutex", &good).is_ok());

        let bad = Type::App {
            base: "Mutex".into(),
            args: vec![Type::RawPtr(Box::new(Type::I32))],
        };
        assert_eq!(
            registry.check_sync("mutex", &bad).unwrap_err().path,
            "mutex.value"
        );
    }

    #[test]
    fn endpoints_and_join_handles_are_send_but_not_sync() {
        let structs = HashMap::new();
        let enums = HashMap::new();
        let registry = registry(&structs, &enums);
        for base in ["JoinHandle", "Sender", "Receiver"] {
            let ty = Type::App {
                base: base.into(),
                args: vec![Type::I32],
            };
            assert!(registry.check_send("handle", &ty).is_ok());
            assert!(registry.check_sync("handle", &ty).is_err());
        }
    }

    #[test]
    fn callable_capture_check_names_the_rejected_capture() {
        let structs = HashMap::new();
        let enums = HashMap::new();
        let captures = [("counter", &Type::I32), ("view", &Type::Str)];
        let error = registry(&structs, &enums)
            .check_callable_send(captures)
            .unwrap_err();
        assert_eq!(error.path, "capture `view`");
    }

    #[test]
    fn recursive_owned_types_terminate_without_hiding_bad_edges() {
        let structs = HashMap::from([
            (
                "Node".into(),
                StructType {
                    name: "Node".into(),
                    fields: vec![(
                        "next".into(),
                        Type::Own(Box::new(Type::Named("Node".into()))),
                    )],
                },
            ),
            (
                "BadNode".into(),
                StructType {
                    name: "BadNode".into(),
                    fields: vec![
                        (
                            "next".into(),
                            Type::Own(Box::new(Type::Named("BadNode".into()))),
                        ),
                        ("shared".into(), Type::Shared(Box::new(Type::I32))),
                    ],
                },
            ),
        ]);
        let enums = HashMap::new();
        let registry = registry(&structs, &enums);
        assert!(registry
            .check_send("node", &Type::Named("Node".into()))
            .is_ok());
        assert_eq!(
            registry
                .check_send("node", &Type::Named("BadNode".into()))
                .unwrap_err()
                .path,
            "node.shared"
        );
    }
}
