//! Shared structural pattern traversal helpers.

use ruff_python_ast::{Expr, Pattern};

/// Transform runtime expressions in a structural pattern without changing capture bindings.
pub(crate) fn transform_runtime_exprs(
    pattern: &mut Pattern,
    transform_expr: &mut impl FnMut(&mut Expr),
) {
    match pattern {
        Pattern::MatchValue(value) => transform_expr(&mut value.value),
        Pattern::MatchSequence(sequence) => {
            for pattern in &mut sequence.patterns {
                transform_runtime_exprs(pattern, transform_expr);
            }
        }
        Pattern::MatchMapping(mapping) => {
            for key in &mut mapping.keys {
                transform_expr(key);
            }
            for pattern in &mut mapping.patterns {
                transform_runtime_exprs(pattern, transform_expr);
            }
        }
        Pattern::MatchClass(class) => {
            transform_expr(&mut class.cls);
            for pattern in &mut class.arguments.patterns {
                transform_runtime_exprs(pattern, transform_expr);
            }
            for keyword in &mut class.arguments.keywords {
                transform_runtime_exprs(&mut keyword.pattern, transform_expr);
            }
        }
        Pattern::MatchAs(as_pattern) => {
            if let Some(pattern) = &mut as_pattern.pattern {
                transform_runtime_exprs(pattern, transform_expr);
            }
        }
        Pattern::MatchOr(or_pattern) => {
            for pattern in &mut or_pattern.patterns {
                transform_runtime_exprs(pattern, transform_expr);
            }
        }
        Pattern::MatchSingleton(_) | Pattern::MatchStar(_) => {}
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
