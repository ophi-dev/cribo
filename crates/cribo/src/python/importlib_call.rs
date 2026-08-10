//! Argument helpers for `importlib.import_module(name, package=None)` call sites.
//!
//! Every component inspecting these calls (import discovery, graph building,
//! classification, code generation) must use the same positional-or-keyword rule so
//! the supported call forms cannot drift apart.

use ruff_python_ast::{Expr, ExprCall, ExprStringLiteral};

/// Return a call argument supplied either at a positional index or through a keyword.
pub(crate) fn positional_or_keyword_argument<'c>(
    call: &'c ExprCall,
    position: usize,
    keyword_name: &str,
) -> Option<&'c Expr> {
    call.arguments.args.get(position).or_else(|| {
        call.arguments.keywords.iter().find_map(|keyword| {
            keyword
                .arg
                .as_ref()
                .is_some_and(|name| name.as_str() == keyword_name)
                .then_some(&keyword.value)
        })
    })
}

/// Return the string-literal value of a positional-or-keyword call argument, if any.
pub(crate) fn string_argument<'c>(
    call: &'c ExprCall,
    position: usize,
    keyword_name: &str,
) -> Option<&'c str> {
    match positional_or_keyword_argument(call, position, keyword_name)? {
        Expr::StringLiteral(ExprStringLiteral { value, .. }) => Some(value.to_str()),
        _ => None,
    }
}

/// Return the module-name argument of an `import_module` call (first positional or
/// `name=` keyword).
pub(crate) fn module_name_argument(call: &ExprCall) -> Option<&Expr> {
    positional_or_keyword_argument(call, 0, "name")
}

/// Return the literal module name of an `import_module` call, if statically known.
pub(crate) fn literal_module_name(call: &ExprCall) -> Option<&str> {
    string_argument(call, 0, "name")
}

/// Return the literal package context of an `import_module` call (second positional
/// or `package=` keyword), if statically known.
pub(crate) fn literal_package_context(call: &ExprCall) -> Option<&str> {
    string_argument(call, 1, "package")
}
