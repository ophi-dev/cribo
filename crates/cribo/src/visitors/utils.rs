//! Shared utilities for visitor implementations

use ruff_python_ast::{Expr, ExprList, ExprName, ExprStringLiteral, ExprTuple};

/// Result of extracting exports from an expression
#[derive(Debug)]
pub(crate) struct ExtractedExports {
    /// The list of exported names if successfully extracted
    pub names: Option<Vec<String>>,
    /// Whether the expression contains dynamic elements
    pub is_dynamic: bool,
}

/// Extract a list of string literals from a List or Tuple expression
/// commonly used for parsing __all__ declarations
///
/// Returns:
/// - `ExtractedExports` with names if all elements are string literals
/// - `ExtractedExports` with `is_dynamic=true` if any element is not a string literal
pub(crate) fn extract_string_list_from_expr(expr: &Expr) -> ExtractedExports {
    match expr {
        Expr::List(ExprList { elts, .. }) | Expr::Tuple(ExprTuple { elts, .. }) => {
            extract_strings_from_elements(elts)
        }
        _ => ExtractedExports {
            names: None,
            is_dynamic: true,
        },
    }
}

/// Extract strings from a slice of expressions
fn extract_strings_from_elements(elts: &[Expr]) -> ExtractedExports {
    let maybe_names: Option<Vec<String>> = elts
        .iter()
        .map(|elt| {
            if let Expr::StringLiteral(ExprStringLiteral { value, .. }) = elt {
                Some(value.to_str().to_owned())
            } else {
                None
            }
        })
        .collect();

    maybe_names.map_or(
        ExtractedExports {
            names: None,
            is_dynamic: true,
        },
        |names| ExtractedExports {
            names: Some(names),
            is_dynamic: false,
        },
    )
}

/// Collect all variable names from an assignment target expression.
///
/// This function handles various assignment patterns including:
/// - Simple names: `x = ...`
/// - Tuple unpacking: `a, b, c = ...`
/// - List unpacking: `[a, b, c] = ...`
/// - Nested unpacking: `(a, (b, c)) = ...`
/// - Starred expressions: `a, *rest = ...`
///
/// Returns a vector of unique variable names found in the target.
/// The names are sorted and deduplicated.
pub(crate) fn collect_names_from_assignment_target(expr: &Expr) -> Vec<&str> {
    let mut names = Vec::new();
    collect_names_recursive(expr, &mut names);
    names.sort_unstable();
    names.dedup();
    names
}

/// Recursively collect variable names from nested assignment targets
fn collect_names_recursive<'a>(expr: &'a Expr, out: &mut Vec<&'a str>) {
    match expr {
        Expr::Name(ExprName { id, .. }) => {
            out.push(id.as_str());
        }
        Expr::Tuple(tuple) => {
            for elem in &tuple.elts {
                collect_names_recursive(elem, out);
            }
        }
        Expr::List(list) => {
            for elem in &list.elts {
                collect_names_recursive(elem, out);
            }
        }
        Expr::Starred(starred) => {
            // Handle starred expressions like *rest in (a, *rest) = ...
            collect_names_recursive(&starred.value, out);
        }
        _ => {
            // Other expression types (like Attribute or Subscript) don't bind new names
            // in assignment contexts, they modify existing objects
        }
    }
}

/// Return whether a statement body accesses its OWN `sys.modules` entry — e.g.
/// `sys.modules[__name__]`, `__name__ in sys.modules`, or
/// `sys.modules.get(__name__)` — through `sys.modules`, a dotted access ending in
/// `.sys.modules` (the bundled `_cribo.sys.modules` rewrite), or a `from sys import
/// modules` binding.
///
/// Such modules rely on being registered with the import machinery under their
/// original name, so the bundler wraps them and their init registers the wrapper
/// namespace in `sys.modules`. Registration is scoped to exactly this self-access
/// pattern: registering modules that merely look up OTHER names in `sys.modules`
/// (or registering every bundled module) would shadow installed distributions whose
/// native extensions re-import their package while the bundled copy is still
/// initializing.
pub(crate) fn accesses_own_sys_modules_entry(body: &[ruff_python_ast::Stmt]) -> bool {
    use ruff_python_ast::{
        CmpOp, Stmt,
        visitor::{Visitor, walk_expr, walk_stmt},
    };

    struct SelfEntryDetector {
        found: bool,
        /// Whether `from sys import modules` binds a bare `modules` name
        modules_from_import: bool,
    }

    impl SelfEntryDetector {
        fn is_sys_modules(&self, expr: &Expr) -> bool {
            match expr {
                Expr::Attribute(attribute) if attribute.attr.as_str() == "modules" => {
                    match &*attribute.value {
                        Expr::Name(name) => name.id.as_str() == "sys",
                        Expr::Attribute(inner) => inner.attr.as_str() == "sys",
                        _ => false,
                    }
                }
                Expr::Name(name) => self.modules_from_import && name.id.as_str() == "modules",
                _ => false,
            }
        }
    }

    fn is_dunder_name(expr: &Expr) -> bool {
        matches!(expr, Expr::Name(name) if name.id.as_str() == "__name__")
    }

    impl<'ast> Visitor<'ast> for SelfEntryDetector {
        fn visit_stmt(&mut self, stmt: &'ast Stmt) {
            if self.found {
                return;
            }
            if let Stmt::ImportFrom(import_from) = stmt
                && import_from.level == 0
                && import_from.module.as_deref() == Some("sys")
                && import_from
                    .names
                    .iter()
                    .any(|alias| alias.name.as_str() == "modules" && alias.asname.is_none())
            {
                self.modules_from_import = true;
            }
            walk_stmt(self, stmt);
        }

        fn visit_expr(&mut self, expr: &'ast Expr) {
            if self.found {
                return;
            }
            match expr {
                // sys.modules[__name__]
                Expr::Subscript(subscript)
                    if self.is_sys_modules(&subscript.value)
                        && is_dunder_name(&subscript.slice) =>
                {
                    self.found = true;
                    return;
                }
                // __name__ in sys.modules / __name__ not in sys.modules
                Expr::Compare(compare)
                    if is_dunder_name(&compare.left)
                        && compare.ops.iter().zip(compare.comparators.iter()).any(
                            |(op, comparator)| {
                                matches!(op, CmpOp::In | CmpOp::NotIn)
                                    && self.is_sys_modules(comparator)
                            },
                        ) =>
                {
                    self.found = true;
                    return;
                }
                // sys.modules.get(__name__), .setdefault(__name__, ...), .pop(__name__)
                Expr::Call(call) => {
                    if let Expr::Attribute(method) = &*call.func
                        && matches!(method.attr.as_str(), "get" | "setdefault" | "pop")
                        && self.is_sys_modules(&method.value)
                        && call.arguments.args.first().is_some_and(is_dunder_name)
                    {
                        self.found = true;
                        return;
                    }
                }
                _ => {}
            }
            walk_expr(self, expr);
        }
    }

    // `from sys import modules` may appear after the use site inside functions;
    // collect it first
    let mut detector = SelfEntryDetector {
        found: false,
        modules_from_import: false,
    };
    for stmt in body {
        detector.visit_stmt(stmt);
    }
    if detector.found {
        return true;
    }
    // Second pass with complete from-import knowledge
    if detector.modules_from_import {
        detector.found = false;
        for stmt in body {
            detector.visit_stmt(stmt);
        }
        return detector.found;
    }
    false
}

#[cfg(test)]
mod tests {
    use ruff_python_parser::parse_module;

    use super::*;

    #[test]
    fn test_extract_string_list_from_list() {
        let code = r#"["foo", "bar", "baz"]"#;
        let parsed = parse_module(code).expect("Failed to parse");
        let module = parsed.into_syntax();

        if let Some(ruff_python_ast::Stmt::Expr(expr_stmt)) = module.body.first() {
            let result = extract_string_list_from_expr(&expr_stmt.value);
            assert!(!result.is_dynamic);
            assert_eq!(
                result.names,
                Some(vec!["foo".to_owned(), "bar".to_owned(), "baz".to_owned()])
            );
        }
    }

    #[test]
    fn test_extract_string_list_with_non_literal() {
        let code = r#"["foo", some_var, "baz"]"#;
        let parsed = parse_module(code).expect("Failed to parse");
        let module = parsed.into_syntax();

        if let Some(ruff_python_ast::Stmt::Expr(expr_stmt)) = module.body.first() {
            let result = extract_string_list_from_expr(&expr_stmt.value);
            assert!(result.is_dynamic);
            assert_eq!(result.names, None);
        }
    }
}
