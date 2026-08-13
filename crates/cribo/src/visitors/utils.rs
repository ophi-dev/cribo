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
/// `sys.modules.get(__name__)` — through `sys.modules`, an aliased `sys` import
/// (`import sys as system`), a dotted access ending in `.sys.modules` (the bundled
/// `_cribo.sys.modules` rewrite), or a possibly aliased `from sys import modules`
/// binding (`from sys import modules as loaded`).
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

    use crate::types::FxIndexSet;

    /// Collect every alias bound to `sys` or to `sys.modules`, anywhere in the body
    /// (aliases may be imported after the use site inside functions).
    struct AliasCollector {
        sys_aliases: FxIndexSet<String>,
        modules_aliases: FxIndexSet<String>,
    }

    impl<'ast> Visitor<'ast> for AliasCollector {
        fn visit_stmt(&mut self, stmt: &'ast Stmt) {
            match stmt {
                Stmt::Import(import_stmt) => {
                    for alias in &import_stmt.names {
                        if alias.name.as_str() == "sys" {
                            let bound_name = alias
                                .asname
                                .as_ref()
                                .map_or_else(|| alias.name.as_str(), |name| name.as_str());
                            self.sys_aliases.insert(bound_name.to_owned());
                        }
                    }
                }
                Stmt::ImportFrom(import_from)
                    if import_from.level == 0 && import_from.module.as_deref() == Some("sys") =>
                {
                    for alias in &import_from.names {
                        if alias.name.as_str() == "modules" {
                            let bound_name = alias
                                .asname
                                .as_ref()
                                .map_or_else(|| alias.name.as_str(), |name| name.as_str());
                            self.modules_aliases.insert(bound_name.to_owned());
                        }
                    }
                }
                _ => {}
            }
            walk_stmt(self, stmt);
        }
    }

    struct SelfEntryDetector {
        found: bool,
        sys_aliases: FxIndexSet<String>,
        modules_aliases: FxIndexSet<String>,
    }

    impl SelfEntryDetector {
        fn is_sys_modules(&self, expr: &Expr) -> bool {
            match expr {
                Expr::Attribute(attribute) if attribute.attr.as_str() == "modules" => {
                    match &*attribute.value {
                        Expr::Name(name) => self.sys_aliases.contains(name.id.as_str()),
                        // Dotted access ending in `.sys.modules` (e.g. the bundled
                        // `_cribo.sys.modules` rewrite)
                        Expr::Attribute(inner) => inner.attr.as_str() == "sys",
                        _ => false,
                    }
                }
                Expr::Name(name) => self.modules_aliases.contains(name.id.as_str()),
                _ => false,
            }
        }
    }

    fn is_dunder_name(expr: &Expr) -> bool {
        matches!(expr, Expr::Name(name) if name.id.as_str() == "__name__")
    }

    impl<'ast> Visitor<'ast> for SelfEntryDetector {
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

    // Aliases may be bound after the use site (inside functions), so collect them
    // over the whole body first
    let mut alias_collector = AliasCollector {
        sys_aliases: FxIndexSet::default(),
        modules_aliases: FxIndexSet::default(),
    };
    // `sys` itself is always recognized: the bundled rewrite may reference it
    // without a surviving import statement
    alias_collector.sys_aliases.insert("sys".to_owned());
    for stmt in body {
        alias_collector.visit_stmt(stmt);
    }

    let mut detector = SelfEntryDetector {
        found: false,
        sys_aliases: alias_collector.sys_aliases,
        modules_aliases: alias_collector.modules_aliases,
    };
    for stmt in body {
        use ruff_python_ast::visitor::Visitor as _;
        detector.visit_stmt(stmt);
    }
    detector.found
}

/// Collect the module names whose `sys.modules` entries a CONSUMER module
/// observes: literal keys (`sys.modules["dep"]`) and import-resolved dynamic
/// keys (`sys.modules[dep.__name__]` where `dep` is bound by `import dep`),
/// through subscripts, `in`/`not in` membership, and `.get`/`.setdefault`/
/// `.pop` calls on `sys.modules` (including aliased forms).
///
/// Bundled modules observed this way must register in `sys.modules` when their
/// init runs: static imports call the generated initializer directly rather
/// than the import machinery, so nothing else would populate the entry.
pub(crate) fn sys_modules_observed_module_names(
    body: &[ruff_python_ast::Stmt],
) -> crate::types::FxIndexSet<String> {
    use ruff_python_ast::{
        CmpOp, Stmt,
        visitor::{Visitor, walk_expr, walk_stmt},
    };

    use crate::types::FxIndexSet;

    /// Collect `sys`/`sys.modules` aliases plus import bindings (name -> module).
    struct ConsumerAliasCollector {
        sys_aliases: FxIndexSet<String>,
        modules_aliases: FxIndexSet<String>,
        import_bindings: crate::types::FxIndexMap<String, String>,
    }

    impl<'ast> Visitor<'ast> for ConsumerAliasCollector {
        fn visit_stmt(&mut self, stmt: &'ast Stmt) {
            match stmt {
                Stmt::Import(import_stmt) => {
                    for alias in &import_stmt.names {
                        let module_name = alias.name.as_str();
                        if module_name == "sys" {
                            let bound_name = alias
                                .asname
                                .as_ref()
                                .map_or_else(|| alias.name.as_str(), |name| name.as_str());
                            self.sys_aliases.insert(bound_name.to_owned());
                            continue;
                        }
                        if let Some(asname) = &alias.asname {
                            self.import_bindings
                                .insert(asname.as_str().to_owned(), module_name.to_owned());
                        } else {
                            let top_level = module_name.split('.').next().unwrap_or(module_name);
                            self.import_bindings
                                .insert(top_level.to_owned(), top_level.to_owned());
                        }
                    }
                }
                Stmt::ImportFrom(import_from) if import_from.level == 0 => {
                    if let Some(module) = import_from.module.as_deref() {
                        for alias in &import_from.names {
                            if alias.name.as_str() == "*" {
                                continue;
                            }
                            let bound_name = alias
                                .asname
                                .as_ref()
                                .map_or_else(|| alias.name.as_str(), |name| name.as_str());
                            if module == "sys" && alias.name.as_str() == "modules" {
                                self.modules_aliases.insert(bound_name.to_owned());
                                continue;
                            }
                            // `from pkg import sub` may bind the submodule
                            self.import_bindings.insert(
                                bound_name.to_owned(),
                                format!("{module}.{}", alias.name.as_str()),
                            );
                        }
                    }
                }
                _ => {}
            }
            walk_stmt(self, stmt);
        }
    }

    struct ObservedKeyCollector {
        observed: FxIndexSet<String>,
        sys_aliases: FxIndexSet<String>,
        modules_aliases: FxIndexSet<String>,
        import_bindings: crate::types::FxIndexMap<String, String>,
    }

    impl ObservedKeyCollector {
        fn is_sys_modules(&self, expr: &Expr) -> bool {
            match expr {
                Expr::Attribute(attribute) if attribute.attr.as_str() == "modules" => {
                    match &*attribute.value {
                        Expr::Name(name) => self.sys_aliases.contains(name.id.as_str()),
                        Expr::Attribute(inner) => inner.attr.as_str() == "sys",
                        _ => false,
                    }
                }
                Expr::Name(name) => self.modules_aliases.contains(name.id.as_str()),
                _ => false,
            }
        }

        /// Resolve a key expression to a module name: a string literal, or an
        /// `<import binding>.__name__` chain.
        fn resolve_key(&self, expr: &Expr) -> Option<String> {
            match expr {
                Expr::StringLiteral(literal) => Some(literal.value.to_str().to_owned()),
                Expr::Attribute(attribute) if attribute.attr.as_str() == "__name__" => {
                    let mut segments: Vec<&str> = Vec::new();
                    let mut current = &*attribute.value;
                    loop {
                        match current {
                            Expr::Attribute(inner) => {
                                segments.push(inner.attr.as_str());
                                current = &inner.value;
                            }
                            Expr::Name(name) => {
                                let base = self.import_bindings.get(name.id.as_str())?;
                                let mut path = base.clone();
                                for segment in segments.iter().rev() {
                                    path.push('.');
                                    path.push_str(segment);
                                }
                                return Some(path);
                            }
                            _ => return None,
                        }
                    }
                }
                _ => None,
            }
        }

        fn record_key(&mut self, expr: &Expr) {
            if let Some(name) = self.resolve_key(expr) {
                self.observed.insert(name);
            }
        }
    }

    impl<'ast> Visitor<'ast> for ObservedKeyCollector {
        fn visit_expr(&mut self, expr: &'ast Expr) {
            match expr {
                Expr::Subscript(subscript) if self.is_sys_modules(&subscript.value) => {
                    self.record_key(&subscript.slice);
                }
                Expr::Compare(compare) => {
                    for (op, comparator) in compare.ops.iter().zip(compare.comparators.iter()) {
                        if matches!(op, CmpOp::In | CmpOp::NotIn) && self.is_sys_modules(comparator)
                        {
                            self.record_key(&compare.left);
                        }
                    }
                }
                Expr::Call(call) => {
                    if let Expr::Attribute(method) = &*call.func
                        && matches!(method.attr.as_str(), "get" | "setdefault" | "pop")
                        && self.is_sys_modules(&method.value)
                        && let Some(key) = call.arguments.args.first()
                    {
                        self.record_key(key);
                    }
                }
                _ => {}
            }
            walk_expr(self, expr);
        }
    }

    let mut alias_collector = ConsumerAliasCollector {
        sys_aliases: FxIndexSet::default(),
        modules_aliases: FxIndexSet::default(),
        import_bindings: crate::types::FxIndexMap::default(),
    };
    alias_collector.sys_aliases.insert("sys".to_owned());
    for stmt in body {
        alias_collector.visit_stmt(stmt);
    }

    let mut collector = ObservedKeyCollector {
        observed: FxIndexSet::default(),
        sys_aliases: alias_collector.sys_aliases,
        modules_aliases: alias_collector.modules_aliases,
        import_bindings: alias_collector.import_bindings,
    };
    for stmt in body {
        collector.visit_stmt(stmt);
    }
    collector.observed
}

/// Collect the module names of IMPORTED modules whose filesystem/import-spec
/// globals a consumer reads: `provider.__file__`, `provider.__spec__.origin`,
/// `provider.__loader__`, `provider.__cached__`, `provider.__path__`.
///
/// A bundled provider's generated namespace carries no faithful values for
/// these, so observed targets must keep their installed module identity.
pub(crate) fn imported_module_dunder_read_targets(
    body: &[ruff_python_ast::Stmt],
) -> crate::types::FxIndexSet<String> {
    use ruff_python_ast::{
        Stmt,
        visitor::{Visitor, walk_expr, walk_stmt},
    };

    use crate::types::{FxIndexMap, FxIndexSet};

    /// Collect import bindings (name -> module) over the whole body.
    struct ImportBindingCollector {
        bindings: FxIndexMap<String, String>,
    }

    impl<'ast> Visitor<'ast> for ImportBindingCollector {
        fn visit_stmt(&mut self, stmt: &'ast Stmt) {
            match stmt {
                Stmt::Import(import_stmt) => {
                    for alias in &import_stmt.names {
                        let module_name = alias.name.as_str();
                        if let Some(asname) = &alias.asname {
                            self.bindings
                                .insert(asname.as_str().to_owned(), module_name.to_owned());
                        } else {
                            let top_level = module_name.split('.').next().unwrap_or(module_name);
                            self.bindings
                                .insert(top_level.to_owned(), top_level.to_owned());
                        }
                    }
                }
                Stmt::ImportFrom(import_from) if import_from.level == 0 => {
                    if let Some(module) = import_from.module.as_deref() {
                        for alias in &import_from.names {
                            if alias.name.as_str() == "*" {
                                continue;
                            }
                            let bound_name = alias
                                .asname
                                .as_ref()
                                .map_or_else(|| alias.name.as_str(), |name| name.as_str());
                            // `from pkg import sub` may bind the submodule
                            self.bindings.insert(
                                bound_name.to_owned(),
                                format!("{module}.{}", alias.name.as_str()),
                            );
                        }
                    }
                }
                _ => {}
            }
            walk_stmt(self, stmt);
        }
    }

    struct DunderReadCollector {
        observed: FxIndexSet<String>,
        bindings: FxIndexMap<String, String>,
    }

    impl<'ast> Visitor<'ast> for DunderReadCollector {
        fn visit_expr(&mut self, expr: &'ast Expr) {
            if let Expr::Attribute(attribute) = expr
                && matches!(
                    attribute.attr.as_str(),
                    "__file__" | "__spec__" | "__loader__" | "__cached__" | "__path__"
                )
                && let Expr::Name(base) = &*attribute.value
                && let Some(module_name) = self.bindings.get(base.id.as_str())
            {
                self.observed.insert(module_name.clone());
            }
            walk_expr(self, expr);
        }
    }

    let mut binding_collector = ImportBindingCollector {
        bindings: FxIndexMap::default(),
    };
    for stmt in body {
        binding_collector.visit_stmt(stmt);
    }
    let mut collector = DunderReadCollector {
        observed: FxIndexSet::default(),
        bindings: binding_collector.bindings,
    };
    for stmt in body {
        collector.visit_stmt(stmt);
    }
    collector.observed
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
