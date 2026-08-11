//! Argument helpers for `importlib.import_module(name, package=None)` call sites.
//!
//! Every component inspecting these calls (import discovery, graph building,
//! classification, code generation) must use the same positional-or-keyword rule so
//! the supported call forms cannot drift apart.

use ruff_python_ast::{Expr, ExprCall, ExprStringLiteral};

/// Return a call argument supplied either at a positional index or through a keyword.
///
/// Returns `None` when the argument is bound both positionally and by keyword: such a
/// call raises `TypeError` at runtime, so it must be preserved verbatim rather than
/// treated as a static import.
pub(crate) fn positional_or_keyword_argument<'c>(
    call: &'c ExprCall,
    position: usize,
    keyword_name: &str,
) -> Option<&'c Expr> {
    let positional = call.arguments.args.get(position);
    let keyword = call.arguments.keywords.iter().find_map(|keyword| {
        keyword
            .arg
            .as_ref()
            .is_some_and(|name| name.as_str() == keyword_name)
            .then_some(&keyword.value)
    });
    match (positional, keyword) {
        // Bound twice: an invalid call whose TypeError must survive bundling
        (Some(_), Some(_)) => None,
        (Some(argument), None) => Some(argument),
        (None, keyword_argument) => keyword_argument,
    }
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

/// Return whether replacing an `import_module` call with direct module access
/// discards no observable behavior from its remaining arguments.
///
/// Python evaluates every argument before importing: a non-literal `package`
/// argument (e.g. `package=touch()`) or an unrecognized/unpacked argument may have
/// side effects or raise, so such calls must be preserved verbatim rather than
/// rewritten.
pub(crate) fn arguments_safely_discardable(call: &ExprCall) -> bool {
    // Only the two supported positionals, without *args unpacking
    if call.arguments.args.len() > 2 || call.arguments.args.iter().any(Expr::is_starred_expr) {
        return false;
    }
    // Only name=/package= keywords; **kwargs unpacking (arg == None) is opaque, and
    // a keyword that also has a positional binding raises TypeError at runtime, so
    // the call must be preserved for the conflict to surface
    let positional_count = call.arguments.args.len();
    for keyword in &call.arguments.keywords {
        match keyword.arg.as_deref() {
            Some("name") if positional_count >= 1 => return false,
            Some("package") if positional_count >= 2 => return false,
            Some("name" | "package") => {}
            _ => return false,
        }
    }
    // A present package context must be a side-effect-free literal
    match positional_or_keyword_argument(call, 1, "package") {
        None | Some(Expr::StringLiteral(_) | Expr::NoneLiteral(_)) => true,
        Some(_) => false,
    }
}

/// Return the non-literal `package` argument of an `import_module` call whose
/// import target is nevertheless statically known and WILL be imported at runtime.
///
/// This is the "evaluable extra argument" form, e.g.
/// `import_module("pkg", package=touch())`: the name is an absolute string literal,
/// only the supported arguments are present (no unpacking, no unknown keywords, no
/// double binding), and the package expression — which CPython evaluates but ignores
/// for absolute names — is not a discardable literal. Rewrites must evaluate the
/// returned expression before yielding the bundled module.
pub(crate) fn evaluable_package_argument(call: &ExprCall) -> Option<&Expr> {
    if arguments_safely_discardable(call) || statically_raises_type_error(call) {
        return None;
    }
    // No unpacking: *args could rebind the name, **kwargs could add arguments
    if call.arguments.args.iter().any(Expr::is_starred_expr)
        || call.arguments.keywords.iter().any(|kw| kw.arg.is_none())
    {
        return None;
    }
    // The target must be an absolute literal: relative names actually consume the
    // package context, so a non-literal one makes the target unknowable
    let name = literal_module_name(call)?;
    if name.starts_with('.') {
        return None;
    }
    positional_or_keyword_argument(call, 1, "package")
}

/// Return whether an `import_module` call's arguments are opaque (`*args`/`**kwargs`
/// unpacking): neither its target nor its runtime behavior can be determined
/// statically.
pub(crate) fn has_opaque_arguments(call: &ExprCall) -> bool {
    call.arguments.args.iter().any(Expr::is_starred_expr)
        || call.arguments.keywords.iter().any(|kw| kw.arg.is_none())
}

/// Return whether an `import_module` call is statically known to raise `TypeError`
/// before importing anything: extra positional arguments, unknown keywords, or an
/// argument bound both positionally and by keyword. `*args`/`**kwargs` unpacking is
/// opaque and not reported.
pub(crate) fn statically_raises_type_error(call: &ExprCall) -> bool {
    let positional_count = call.arguments.args.len();
    if positional_count > 2 && !call.arguments.args.iter().any(Expr::is_starred_expr) {
        return true;
    }
    call.arguments
        .keywords
        .iter()
        .any(|keyword| match keyword.arg.as_deref() {
            Some("name") => positional_count >= 1,
            Some("package") => positional_count >= 2,
            Some(_) => true,
            None => false,
        })
}
