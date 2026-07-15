//! Compatibility wrapper around Ruff's Python code generator.
//!
//! Ruff's generator currently omits structural class patterns. This module delegates ordinary
//! statements and expressions to Ruff while replacing only affected match-case patterns.

use std::cell::RefCell;

use ruff_python_ast::{
    AtomicNodeIndex, Identifier, Pattern, PatternMatchAs, PatternMatchClass, PatternMatchMapping,
    Singleton, Stmt,
    visitor::{
        Visitor,
        transformer::{Transformer, walk_match_case},
        walk_pattern,
    },
};
use ruff_python_codegen::{Generator, Stylist};
use ruff_text_size::Ranged;

#[derive(Debug)]
struct PatternReplacement {
    marker: String,
    source: String,
}

struct ClassPatternDetector {
    found: bool,
}

impl<'a> Visitor<'a> for ClassPatternDetector {
    fn visit_pattern(&mut self, pattern: &'a Pattern) {
        if matches!(pattern, Pattern::MatchClass(_)) {
            self.found = true;
        } else if !self.found {
            walk_pattern(self, pattern);
        }
    }
}

struct ClassPatternRewriter<'a> {
    stylist: &'a Stylist<'a>,
    replacements: RefCell<Vec<PatternReplacement>>,
}

impl Transformer for ClassPatternRewriter<'_> {
    fn visit_match_case(&self, match_case: &mut ruff_python_ast::MatchCase) {
        if pattern_contains_class(&match_case.pattern) {
            let source = generate_pattern(&match_case.pattern, self.stylist);
            let marker = format!(
                "$__cribo_class_pattern_{}",
                self.replacements.borrow().len()
            );
            let range = match_case.pattern.range();

            self.replacements.borrow_mut().push(PatternReplacement {
                marker: marker.clone(),
                source,
            });
            match_case.pattern = Pattern::MatchAs(PatternMatchAs {
                node_index: AtomicNodeIndex::NONE,
                range,
                pattern: None,
                name: Some(Identifier::new(marker, range)),
            });
        }

        walk_match_case(self, match_case);
    }
}

/// Generate one statement, filling the class-pattern gap in Ruff's generator.
pub(crate) fn generate_statement(stmt: &Stmt, stylist: &Stylist<'_>) -> String {
    if !statement_contains_class_pattern(stmt) {
        return Generator::from(stylist).stmt(stmt);
    }

    let mut compatible_stmt = stmt.clone();
    let rewriter = ClassPatternRewriter {
        stylist,
        replacements: RefCell::new(Vec::new()),
    };
    rewriter.visit_stmt(&mut compatible_stmt);

    let mut generated = Generator::from(stylist).stmt(&compatible_stmt);
    for replacement in rewriter.replacements.into_inner() {
        replace_case_marker(&mut generated, &replacement);
    }
    generated
}

/// Return whether a statement contains a structural class pattern.
fn statement_contains_class_pattern(stmt: &Stmt) -> bool {
    let mut detector = ClassPatternDetector { found: false };
    detector.visit_stmt(stmt);
    detector.found
}

/// Return whether a pattern or any nested pattern is a structural class pattern.
fn pattern_contains_class(pattern: &Pattern) -> bool {
    match pattern {
        Pattern::MatchClass(_) => true,
        Pattern::MatchSequence(sequence) => sequence.patterns.iter().any(pattern_contains_class),
        Pattern::MatchMapping(mapping) => mapping.patterns.iter().any(pattern_contains_class),
        Pattern::MatchAs(as_pattern) => as_pattern
            .pattern
            .as_deref()
            .is_some_and(pattern_contains_class),
        Pattern::MatchOr(or_pattern) => or_pattern.patterns.iter().any(pattern_contains_class),
        Pattern::MatchValue(_) | Pattern::MatchSingleton(_) | Pattern::MatchStar(_) => false,
    }
}

/// Render a complete pattern, delegating embedded expressions to Ruff.
fn generate_pattern(pattern: &Pattern, stylist: &Stylist<'_>) -> String {
    let mut output = String::new();
    write_pattern(pattern, stylist, &mut output);
    output
}

/// Append a pattern's Python source to the output buffer.
fn write_pattern(pattern: &Pattern, stylist: &Stylist<'_>, output: &mut String) {
    match pattern {
        Pattern::MatchValue(value) => {
            output.push_str(&Generator::from(stylist).expr(&value.value));
        }
        Pattern::MatchSingleton(singleton) => output.push_str(match singleton.value {
            Singleton::None => "None",
            Singleton::True => "True",
            Singleton::False => "False",
        }),
        Pattern::MatchSequence(sequence) => {
            output.push('[');
            write_patterns(&sequence.patterns, stylist, output);
            output.push(']');
        }
        Pattern::MatchMapping(mapping) => write_mapping_pattern(mapping, stylist, output),
        Pattern::MatchClass(class) => write_class_pattern(class, stylist, output),
        Pattern::MatchStar(star) => {
            output.push('*');
            output.push_str(star.name.as_deref().unwrap_or("_"));
        }
        Pattern::MatchAs(as_pattern) => {
            if let Some(pattern) = &as_pattern.pattern {
                write_pattern(pattern, stylist, output);
                output.push_str(" as ");
            }
            output.push_str(as_pattern.name.as_deref().unwrap_or("_"));
        }
        Pattern::MatchOr(or_pattern) => write_or_pattern(&or_pattern.patterns, stylist, output),
    }
}

/// Append an OR pattern, preserving grouping around `as` alternatives.
fn write_or_pattern(patterns: &[Pattern], stylist: &Stylist<'_>, output: &mut String) {
    let mut needs_separator = false;
    for pattern in patterns {
        if needs_separator {
            output.push_str(" | ");
        }
        let needs_parentheses =
            matches!(pattern, Pattern::MatchAs(as_pattern) if as_pattern.pattern.is_some());
        if needs_parentheses {
            output.push('(');
        }
        write_pattern(pattern, stylist, output);
        if needs_parentheses {
            output.push(')');
        }
        needs_separator = true;
    }
}

/// Append a mapping pattern and its optional rest capture.
fn write_mapping_pattern(
    mapping: &PatternMatchMapping,
    stylist: &Stylist<'_>,
    output: &mut String,
) {
    output.push('{');
    let mut needs_separator = false;
    for (key, value) in mapping.keys.iter().zip(&mapping.patterns) {
        push_separator(output, &mut needs_separator);
        output.push_str(&Generator::from(stylist).expr(key));
        output.push_str(": ");
        write_pattern(value, stylist, output);
    }
    if let Some(rest) = &mapping.rest {
        push_separator(output, &mut needs_separator);
        output.push_str("**");
        output.push_str(rest);
    }
    output.push('}');
}

/// Append a class pattern with positional and keyword subpatterns.
fn write_class_pattern(class: &PatternMatchClass, stylist: &Stylist<'_>, output: &mut String) {
    output.push_str(&Generator::from(stylist).expr(&class.cls));
    output.push('(');
    let mut needs_separator = false;
    for argument in &class.arguments.patterns {
        push_separator(output, &mut needs_separator);
        write_pattern(argument, stylist, output);
    }
    for keyword in &class.arguments.keywords {
        push_separator(output, &mut needs_separator);
        output.push_str(&keyword.attr);
        output.push('=');
        write_pattern(&keyword.pattern, stylist, output);
    }
    output.push(')');
}

/// Append comma-separated patterns to the output buffer.
fn write_patterns(patterns: &[Pattern], stylist: &Stylist<'_>, output: &mut String) {
    let mut needs_separator = false;
    for pattern in patterns {
        push_separator(output, &mut needs_separator);
        write_pattern(pattern, stylist, output);
    }
}

/// Append a comma separator after the first emitted item.
fn push_separator(output: &mut String, needs_separator: &mut bool) {
    if *needs_separator {
        output.push_str(", ");
    }
    *needs_separator = true;
}

/// Replace a generated match-case marker with its rendered pattern source.
fn replace_case_marker(generated: &mut String, replacement: &PatternReplacement) {
    let target = format!("case {}", replacement.marker);
    let mut offset = 0;

    for line in generated.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if let Some(suffix) = trimmed.strip_prefix(&target)
            && (suffix.starts_with(':') || suffix.starts_with(" if "))
        {
            let indentation = line.len() - trimmed.len();
            let start = offset + indentation + "case ".len();
            let end = start + replacement.marker.len();
            generated.replace_range(start..end, &replacement.source);
            return;
        }
        offset += line.len();
    }

    unreachable!("generated class-pattern marker was not found");
}
