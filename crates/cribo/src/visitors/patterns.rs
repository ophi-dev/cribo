//! Shared structural pattern traversal helpers.

use ruff_python_ast::{Expr, Identifier, Pattern};

/// Transform runtime expressions in a structural pattern without changing capture bindings.
pub(crate) fn transform_runtime_exprs(
    pattern: &mut Pattern,
    transform_expr: &mut impl FnMut(&mut Expr),
) {
    transform_runtime_exprs_and_bindings(pattern, transform_expr, &mut |_| {});
}

/// Transform runtime expressions and capture bindings in a structural pattern.
pub(crate) fn transform_runtime_exprs_and_bindings(
    pattern: &mut Pattern,
    transform_expr: &mut impl FnMut(&mut Expr),
    transform_binding: &mut impl FnMut(&mut Identifier),
) {
    match pattern {
        Pattern::MatchValue(value) => transform_expr(&mut value.value),
        Pattern::MatchSequence(sequence) => {
            for pattern in &mut sequence.patterns {
                transform_runtime_exprs_and_bindings(pattern, transform_expr, transform_binding);
            }
        }
        Pattern::MatchMapping(mapping) => {
            for key in &mut mapping.keys {
                transform_expr(key);
            }
            for pattern in &mut mapping.patterns {
                transform_runtime_exprs_and_bindings(pattern, transform_expr, transform_binding);
            }
            if let Some(name) = &mut mapping.rest {
                transform_binding(name);
            }
        }
        Pattern::MatchClass(class) => {
            transform_expr(&mut class.cls);
            for pattern in &mut class.arguments.patterns {
                transform_runtime_exprs_and_bindings(pattern, transform_expr, transform_binding);
            }
            for keyword in &mut class.arguments.keywords {
                transform_runtime_exprs_and_bindings(
                    &mut keyword.pattern,
                    transform_expr,
                    transform_binding,
                );
            }
        }
        Pattern::MatchStar(star) => {
            if let Some(name) = &mut star.name {
                transform_binding(name);
            }
        }
        Pattern::MatchAs(as_pattern) => {
            if let Some(pattern) = &mut as_pattern.pattern {
                transform_runtime_exprs_and_bindings(pattern, transform_expr, transform_binding);
            }
            if let Some(name) = &mut as_pattern.name {
                transform_binding(name);
            }
        }
        Pattern::MatchOr(or_pattern) => {
            for pattern in &mut or_pattern.patterns {
                transform_runtime_exprs_and_bindings(pattern, transform_expr, transform_binding);
            }
        }
        Pattern::MatchSingleton(_) => {}
    }
}

/// Visit every name bound by a structural pattern.
pub(crate) fn visit_binding_names(pattern: &Pattern, visit_name: &mut impl FnMut(&str)) {
    match pattern {
        Pattern::MatchSequence(sequence) => {
            for pattern in &sequence.patterns {
                visit_binding_names(pattern, visit_name);
            }
        }
        Pattern::MatchMapping(mapping) => {
            for pattern in &mapping.patterns {
                visit_binding_names(pattern, visit_name);
            }
            if let Some(name) = &mapping.rest {
                visit_name(name);
            }
        }
        Pattern::MatchClass(class) => {
            for pattern in &class.arguments.patterns {
                visit_binding_names(pattern, visit_name);
            }
            for keyword in &class.arguments.keywords {
                visit_binding_names(&keyword.pattern, visit_name);
            }
        }
        Pattern::MatchStar(star) => {
            if let Some(name) = &star.name {
                visit_name(name);
            }
        }
        Pattern::MatchAs(as_pattern) => {
            if let Some(pattern) = &as_pattern.pattern {
                visit_binding_names(pattern, visit_name);
            }
            if let Some(name) = &as_pattern.name {
                visit_name(name);
            }
        }
        Pattern::MatchOr(or_pattern) => {
            for pattern in &or_pattern.patterns {
                visit_binding_names(pattern, visit_name);
            }
        }
        Pattern::MatchValue(_) | Pattern::MatchSingleton(_) => {}
    }
}
