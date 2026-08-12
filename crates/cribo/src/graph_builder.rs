/// Graph builder that creates `DependencyGraph` from Python AST
/// This module bridges the gap between ruff's AST and our dependency graph
use anyhow::Result;
use ruff_python_ast::{
    self as ast, Expr, ExprContext, ModModule, Stmt,
    visitor::{self, Visitor},
};

use crate::{
    dependency_graph::{ItemData, ItemType, ModuleDepGraph, ScopeKind, ScopePath},
    types::{FxIndexMap, FxIndexSet},
    visitors::{ExpressionSideEffectDetector, utils::extract_string_list_from_expr},
};

/// Python scope kinds that affect name resolution during dependency collection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DependencyScopeKind {
    Function,
    Class,
    Comprehension,
}

/// Names bound or redirected within one Python lexical scope.
struct ScopeBindings {
    kind: DependencyScopeKind,
    locals: FxIndexSet<String>,
    globals: FxIndexSet<String>,
    nonlocals: FxIndexSet<String>,
}

impl ScopeBindings {
    /// Collect bindings for a function body, including all parameters.
    fn function(parameters: &ast::Parameters, body: &[Stmt]) -> Self {
        let mut collector = BindingCollector::new(DependencyScopeKind::Function);
        collector.add_parameters(parameters);
        collector.visit_body(body);
        collector.finish()
    }

    /// Collect bindings for a lambda body and its optional parameters.
    fn lambda(parameters: Option<&ast::Parameters>, body: &Expr) -> Self {
        let mut collector = BindingCollector::new(DependencyScopeKind::Function);
        if let Some(parameters) = parameters {
            collector.add_parameters(parameters);
        }
        collector.visit_expr(body);
        collector.finish()
    }

    /// Collect names bound while executing a class body.
    fn class(body: &[Stmt]) -> Self {
        let mut collector = BindingCollector::new(DependencyScopeKind::Class);
        collector.visit_body(body);
        collector.finish()
    }

    /// Collect target names local to a comprehension's implicit scope.
    fn comprehension(generators: &[ast::Comprehension]) -> Self {
        let mut locals = FxIndexSet::default();
        for generator in generators {
            collect_binding_names(&generator.target, &mut locals);
        }
        Self {
            kind: DependencyScopeKind::Comprehension,
            locals,
            globals: FxIndexSet::default(),
            nonlocals: FxIndexSet::default(),
        }
    }
}

/// Collects Python bindings without descending into nested lexical bodies.
struct BindingCollector {
    bindings: ScopeBindings,
}

impl BindingCollector {
    /// Create a binding collector for the requested lexical scope kind.
    fn new(kind: DependencyScopeKind) -> Self {
        Self {
            bindings: ScopeBindings {
                kind,
                locals: FxIndexSet::default(),
                globals: FxIndexSet::default(),
                nonlocals: FxIndexSet::default(),
            },
        }
    }

    /// Add all positional and variadic parameter names as local bindings.
    fn add_parameters(&mut self, parameters: &ast::Parameters) {
        for parameter in parameters.iter_non_variadic_params() {
            self.bindings
                .locals
                .insert(parameter.parameter.name.to_string());
        }
        if let Some(vararg) = &parameters.vararg {
            self.bindings.locals.insert(vararg.name.to_string());
        }
        if let Some(kwarg) = &parameters.kwarg {
            self.bindings.locals.insert(kwarg.name.to_string());
        }
    }

    /// Add the local name introduced by a direct import alias.
    fn add_import(&mut self, alias: &ast::Alias) {
        let local_name = alias.asname.as_ref().map_or_else(
            || {
                alias
                    .name
                    .as_str()
                    .split('.')
                    .next()
                    .expect("import name should have at least one part")
            },
            ruff_python_ast::Identifier::as_str,
        );
        self.bindings.locals.insert(local_name.to_owned());
    }

    /// Record a nested function binding and visit only its definition-time expressions.
    fn add_function_definition(&mut self, function_def: &ast::StmtFunctionDef) {
        self.bindings.locals.insert(function_def.name.to_string());
        self.visit_definition_time_parts(function_def);
    }

    /// Record a nested class binding and visit only its definition-time expressions.
    fn add_class_definition(&mut self, class_def: &ast::StmtClassDef) {
        self.bindings.locals.insert(class_def.name.to_string());
        for decorator in &class_def.decorator_list {
            self.visit_decorator(decorator);
        }
        if let Some(type_params) = &class_def.type_params {
            self.visit_type_params(type_params);
        }
        if let Some(arguments) = &class_def.arguments {
            self.visit_arguments(arguments);
        }
    }

    /// Add all local names introduced by a direct import statement.
    fn add_import_statement(&mut self, import_stmt: &ast::StmtImport) {
        for alias in &import_stmt.names {
            self.add_import(alias);
        }
    }

    /// Add all explicit local names introduced by a from-import statement.
    fn add_from_import_statement(&mut self, import_from: &ast::StmtImportFrom) {
        for alias in &import_from.names {
            if alias.name.as_str() == "*" {
                continue;
            }
            let local_name = alias
                .asname
                .as_ref()
                .map_or(alias.name.as_str(), ruff_python_ast::Identifier::as_str);
            self.bindings.locals.insert(local_name.to_owned());
        }
    }

    /// Visit expressions evaluated when a function object is defined.
    fn visit_definition_time_parts(&mut self, function_def: &ast::StmtFunctionDef) {
        for decorator in &function_def.decorator_list {
            self.visit_decorator(decorator);
        }
        if let Some(type_params) = &function_def.type_params {
            self.visit_type_params(type_params);
        }
        self.visit_parameters(&function_def.parameters);
        if let Some(returns) = &function_def.returns {
            self.visit_annotation(returns);
        }
    }

    /// Remove names redirected by global or nonlocal declarations.
    fn finish(mut self) -> ScopeBindings {
        for name in self
            .bindings
            .globals
            .iter()
            .chain(self.bindings.nonlocals.iter())
        {
            self.bindings.locals.shift_remove(name);
        }
        self.bindings
    }
}

impl<'ast> Visitor<'ast> for BindingCollector {
    fn visit_stmt(&mut self, stmt: &'ast Stmt) {
        match stmt {
            Stmt::FunctionDef(function_def) => self.add_function_definition(function_def),
            Stmt::ClassDef(class_def) => self.add_class_definition(class_def),
            Stmt::Import(import_stmt) => self.add_import_statement(import_stmt),
            Stmt::ImportFrom(import_from) => self.add_from_import_statement(import_from),
            Stmt::Global(global_stmt) => {
                for name in &global_stmt.names {
                    self.bindings.globals.insert(name.to_string());
                }
            }
            Stmt::Nonlocal(nonlocal_stmt) => {
                for name in &nonlocal_stmt.names {
                    self.bindings.nonlocals.insert(name.to_string());
                }
            }
            Stmt::TypeAlias(type_alias) => {
                collect_binding_names(&type_alias.name, &mut self.bindings.locals);
            }
            _ => visitor::walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        match expr {
            Expr::Name(name) if matches!(name.ctx, ExprContext::Store | ExprContext::Del) => {
                self.bindings.locals.insert(name.id.to_string());
            }
            Expr::Lambda(lambda) => {
                if let Some(parameters) = &lambda.parameters {
                    self.visit_parameters(parameters);
                }
            }
            _ => visitor::walk_expr(self, expr),
        }
    }

    fn visit_comprehension(&mut self, comprehension: &'ast ast::Comprehension) {
        self.visit_expr(&comprehension.iter);
        for condition in &comprehension.ifs {
            self.visit_expr(condition);
        }
    }

    fn visit_except_handler(&mut self, handler: &'ast ast::ExceptHandler) {
        let ast::ExceptHandler::ExceptHandler(handler) = handler;
        if let Some(type_expression) = &handler.type_ {
            self.visit_expr(type_expression);
        }
        if let Some(name) = &handler.name {
            self.bindings.locals.insert(name.to_string());
        }
        self.visit_body(&handler.body);
    }

    fn visit_pattern(&mut self, pattern: &'ast ast::Pattern) {
        match pattern {
            ast::Pattern::MatchMapping(mapping) => {
                if let Some(name) = &mapping.rest {
                    self.bindings.locals.insert(name.to_string());
                }
            }
            ast::Pattern::MatchStar(star) => {
                if let Some(name) = &star.name {
                    self.bindings.locals.insert(name.to_string());
                }
            }
            ast::Pattern::MatchAs(as_pattern) => {
                if let Some(name) = &as_pattern.name {
                    self.bindings.locals.insert(name.to_string());
                }
            }
            ast::Pattern::MatchValue(_)
            | ast::Pattern::MatchSingleton(_)
            | ast::Pattern::MatchSequence(_)
            | ast::Pattern::MatchClass(_)
            | ast::Pattern::MatchOr(_) => {}
        }
        visitor::walk_pattern(self, pattern);
    }
}

/// Add names bound by an assignment target, including destructuring targets.
fn collect_binding_names(target: &Expr, names: &mut FxIndexSet<String>) {
    match target {
        Expr::Name(name) => {
            names.insert(name.id.to_string());
        }
        Expr::Tuple(tuple) => {
            for element in &tuple.elts {
                collect_binding_names(element, names);
            }
        }
        Expr::List(list) => {
            for element in &list.elts {
                collect_binding_names(element, names);
            }
        }
        Expr::Starred(starred) => collect_binding_names(&starred.value, names),
        _ => {}
    }
}

/// Collects runtime dependencies while delegating complete AST traversal to Ruff.
struct DependencyCollector<'a> {
    read_vars: &'a mut FxIndexSet<String>,
    write_vars: Option<&'a mut FxIndexSet<String>>,
    attribute_accesses: &'a mut FxIndexMap<String, FxIndexSet<String>>,
    variable_annotations_are_runtime: bool,
    scopes: Vec<ScopeBindings>,
}

impl<'a> DependencyCollector<'a> {
    const fn expression(
        read_vars: &'a mut FxIndexSet<String>,
        attribute_accesses: &'a mut FxIndexMap<String, FxIndexSet<String>>,
    ) -> Self {
        Self {
            read_vars,
            write_vars: None,
            attribute_accesses,
            variable_annotations_are_runtime: true,
            scopes: Vec::new(),
        }
    }

    /// Create a collector for one function body with its bindings pre-indexed.
    fn function_body(
        parameters: &ast::Parameters,
        body: &[Stmt],
        read_vars: &'a mut FxIndexSet<String>,
        write_vars: &'a mut FxIndexSet<String>,
        attribute_accesses: &'a mut FxIndexMap<String, FxIndexSet<String>>,
    ) -> Self {
        Self {
            read_vars,
            write_vars: Some(write_vars),
            attribute_accesses,
            variable_annotations_are_runtime: false,
            scopes: vec![ScopeBindings::function(parameters, body)],
        }
    }

    /// Record a read only when Python resolves the name outside local scopes.
    fn record_read(&mut self, name: &str) {
        if self.name_resolves_to_module(name) {
            self.read_vars.insert(name.to_owned());
        }
    }

    /// Record writes explicitly redirected to the module scope.
    fn record_write(&mut self, name: &str) {
        let writes_module = self
            .scopes
            .last()
            .is_none_or(|scope| scope.globals.contains(name));
        if writes_module && let Some(write_vars) = &mut self.write_vars {
            write_vars.insert(name.to_owned());
        }
    }

    /// Record a named-expression write in its nearest non-comprehension scope.
    fn record_named_expression_write(&mut self, name: &str) {
        let writes_module = self
            .scopes
            .iter()
            .rev()
            .find(|scope| scope.kind != DependencyScopeKind::Comprehension)
            .is_none_or(|scope| scope.globals.contains(name));
        if writes_module && let Some(write_vars) = &mut self.write_vars {
            write_vars.insert(name.to_owned());
        }
    }

    /// Return whether a name load resolves beyond all active local scopes.
    fn name_resolves_to_module(&self, name: &str) -> bool {
        for (index, scope) in self.scopes.iter().enumerate().rev() {
            if scope.globals.contains(name) {
                return true;
            }
            if scope.nonlocals.contains(name) {
                continue;
            }
            let is_innermost = index + 1 == self.scopes.len();
            if (is_innermost || scope.kind != DependencyScopeKind::Class)
                && scope.locals.contains(name)
            {
                return false;
            }
        }
        true
    }

    /// Visit an assignment target, recording writes and reads from complex targets.
    fn visit_assignment_target(&mut self, target: &Expr) {
        match target {
            Expr::Name(name) => self.record_write(&name.id),
            Expr::Tuple(tuple) => {
                for element in &tuple.elts {
                    self.visit_assignment_target(element);
                }
            }
            Expr::List(list) => {
                for element in &list.elts {
                    self.visit_assignment_target(element);
                }
            }
            Expr::Starred(starred) => self.visit_assignment_target(&starred.value),
            Expr::Subscript(subscript) => {
                self.visit_expr(&subscript.value);
                self.visit_expr(&subscript.slice);
            }
            Expr::Attribute(attribute) => self.visit_expr(&attribute.value),
            _ => self.visit_expr(target),
        }
    }

    /// Visit a read-before-write augmented assignment target.
    fn visit_augmented_assignment_target(&mut self, target: &Expr) {
        match target {
            Expr::Name(name) => {
                self.record_read(&name.id);
                self.record_write(&name.id);
            }
            Expr::Attribute(attribute) => {
                self.track_attribute_access(attribute);
                self.visit_expr(&attribute.value);
            }
            Expr::Subscript(subscript) => {
                self.visit_expr(&subscript.value);
                self.visit_expr(&subscript.slice);
            }
            _ => self.visit_assignment_target(target),
        }
    }

    /// Visit a deletion target, which reads the binding or containing object.
    fn visit_delete_target(&mut self, target: &Expr) {
        match target {
            Expr::Name(name) => self.record_read(&name.id),
            Expr::Tuple(tuple) => {
                for element in &tuple.elts {
                    self.visit_delete_target(element);
                }
            }
            Expr::List(list) => {
                for element in &list.elts {
                    self.visit_delete_target(element);
                }
            }
            Expr::Starred(starred) => self.visit_delete_target(&starred.value),
            Expr::Attribute(attribute) => {
                self.track_attribute_access(attribute);
                self.visit_expr(&attribute.value);
            }
            Expr::Subscript(subscript) => {
                self.visit_expr(&subscript.value);
                self.visit_expr(&subscript.slice);
            }
            _ => self.visit_expr(target),
        }
    }

    /// Record a module-resolved dotted attribute access.
    fn track_attribute_access(&mut self, attribute: &ast::ExprAttribute) {
        let Some(full_name) = Self::build_full_dotted_name(&attribute.value) else {
            return;
        };
        let root = full_name
            .split('.')
            .next()
            .expect("full_name should have at least one part");
        if !self.name_resolves_to_module(root) {
            return;
        }

        if let Expr::Name(base_name) = attribute.value.as_ref() {
            let base = base_name.id.to_string();
            self.read_vars.insert(base.clone());
            self.attribute_accesses
                .entry(base)
                .or_default()
                .insert(attribute.attr.to_string());
        } else if let Expr::Attribute(_) = attribute.value.as_ref()
            && let Some(base_path) = Self::build_full_dotted_name(&attribute.value)
        {
            log::debug!(
                "Nested attribute access: base_path='{}', attr='{}'",
                base_path,
                attribute.attr
            );
            self.attribute_accesses
                .entry(base_path.clone())
                .or_default()
                .insert(attribute.attr.to_string());
            self.read_vars.insert(base_path);
        }

        self.read_vars.insert(full_name.clone());
        if full_name.contains('.') {
            self.read_vars.insert(root.to_owned());
        }
    }

    /// Build a dotted name from a name-or-attribute expression chain.
    fn build_full_dotted_name(expr: &Expr) -> Option<String> {
        match expr {
            Expr::Name(name) => Some(name.id.to_string()),
            Expr::Attribute(attribute) => Self::build_full_dotted_name(&attribute.value)
                .map(|base| format!("{}.{}", base, attribute.attr)),
            _ => None,
        }
    }

    /// Visit function definition-time expressions and then its deferred body scope.
    fn visit_function_def(&mut self, function_def: &ast::StmtFunctionDef) {
        for decorator in &function_def.decorator_list {
            self.visit_decorator(decorator);
        }
        if let Some(type_params) = &function_def.type_params {
            self.visit_type_params(type_params);
        }
        self.visit_parameters(&function_def.parameters);
        if let Some(returns) = &function_def.returns {
            self.visit_annotation(returns);
        }

        let scope = ScopeBindings::function(&function_def.parameters, &function_def.body);
        let enclosing_scope = self.variable_annotations_are_runtime;
        self.scopes.push(scope);
        self.variable_annotations_are_runtime = false;
        self.visit_body(&function_def.body);
        self.variable_annotations_are_runtime = enclosing_scope;
        self.scopes
            .pop()
            .expect("function dependency scope should be present");
    }

    /// Visit class definition-time expressions and then its executing body scope.
    fn visit_class_def(&mut self, class_def: &ast::StmtClassDef) {
        for decorator in &class_def.decorator_list {
            self.visit_decorator(decorator);
        }
        if let Some(type_params) = &class_def.type_params {
            self.visit_type_params(type_params);
        }
        if let Some(arguments) = &class_def.arguments {
            self.visit_arguments(arguments);
        }

        let scope = ScopeBindings::class(&class_def.body);
        let enclosing_scope = self.variable_annotations_are_runtime;
        self.scopes.push(scope);
        self.variable_annotations_are_runtime = true;
        self.visit_body(&class_def.body);
        self.variable_annotations_are_runtime = enclosing_scope;
        self.scopes
            .pop()
            .expect("class dependency scope should be present");
    }

    /// Visit a comprehension in Python's evaluation order and implicit scope.
    fn visit_comprehension_parts(
        &mut self,
        generators: &[ast::Comprehension],
        result_expressions: &[&Expr],
    ) {
        let Some((first, remaining)) = generators.split_first() else {
            for expression in result_expressions {
                self.visit_expr(expression);
            }
            return;
        };

        self.visit_expr(&first.iter);
        self.scopes.push(ScopeBindings::comprehension(generators));
        self.visit_assignment_target(&first.target);
        for condition in &first.ifs {
            self.visit_expr(condition);
        }
        for generator in remaining {
            self.visit_expr(&generator.iter);
            self.visit_assignment_target(&generator.target);
            for condition in &generator.ifs {
                self.visit_expr(condition);
            }
        }
        for expression in result_expressions {
            self.visit_expr(expression);
        }
        self.scopes
            .pop()
            .expect("comprehension dependency scope should be present");
    }
}

impl<'ast> Visitor<'ast> for DependencyCollector<'_> {
    fn visit_stmt(&mut self, stmt: &'ast Stmt) {
        match stmt {
            Stmt::Delete(delete) => {
                for target in &delete.targets {
                    self.visit_delete_target(target);
                }
            }
            Stmt::Assign(assign) => {
                self.visit_expr(&assign.value);
                for target in &assign.targets {
                    self.visit_assignment_target(target);
                }
            }
            Stmt::AugAssign(aug_assign) => {
                self.visit_augmented_assignment_target(&aug_assign.target);
                self.visit_expr(&aug_assign.value);
            }
            Stmt::AnnAssign(ann_assign) => {
                if let Some(value) = &ann_assign.value {
                    self.visit_expr(value);
                }
                if self.variable_annotations_are_runtime {
                    self.visit_annotation(&ann_assign.annotation);
                }
                self.visit_assignment_target(&ann_assign.target);
            }
            Stmt::ClassDef(class_def) => self.visit_class_def(class_def),
            Stmt::FunctionDef(function_def) => self.visit_function_def(function_def),
            Stmt::For(for_stmt) => {
                self.visit_expr(&for_stmt.iter);
                self.visit_assignment_target(&for_stmt.target);
                self.visit_body(&for_stmt.body);
                self.visit_body(&for_stmt.orelse);
            }
            Stmt::With(with_stmt) => {
                for item in &with_stmt.items {
                    self.visit_expr(&item.context_expr);
                    if let Some(optional_vars) = &item.optional_vars {
                        self.visit_assignment_target(optional_vars);
                    }
                }
                self.visit_body(&with_stmt.body);
            }
            Stmt::Import(import_stmt) => {
                for alias in &import_stmt.names {
                    let local_name = alias.asname.as_ref().map_or_else(
                        || {
                            alias
                                .name
                                .as_str()
                                .split('.')
                                .next()
                                .expect("import name should have at least one part")
                        },
                        ruff_python_ast::Identifier::as_str,
                    );
                    self.record_read(local_name);
                }
            }
            Stmt::ImportFrom(import_from) => {
                for alias in &import_from.names {
                    if alias.name.as_str() != "*" {
                        let local_name = alias
                            .asname
                            .as_ref()
                            .map_or(alias.name.as_str(), ruff_python_ast::Identifier::as_str);
                        self.record_read(local_name);
                    }
                }
            }
            Stmt::Global(global_stmt) => {
                for name in &global_stmt.names {
                    self.record_read(name);
                    self.record_write(name);
                }
            }
            Stmt::Nonlocal(_) => {
                // Nonlocal names resolve to an enclosing function, not the module graph.
            }
            _ => visitor::walk_stmt(self, stmt),
        }
    }

    fn visit_except_handler(&mut self, handler: &'ast ast::ExceptHandler) {
        let ast::ExceptHandler::ExceptHandler(handler) = handler;
        if let Some(type_expression) = &handler.type_ {
            self.visit_expr(type_expression);
        }
        if let Some(name) = &handler.name {
            self.record_write(name);
        }
        self.visit_body(&handler.body);
    }

    fn visit_pattern(&mut self, pattern: &'ast ast::Pattern) {
        match pattern {
            ast::Pattern::MatchMapping(mapping) => {
                if let Some(name) = &mapping.rest {
                    self.record_write(name);
                }
            }
            ast::Pattern::MatchStar(star) => {
                if let Some(name) = &star.name {
                    self.record_write(name);
                }
            }
            ast::Pattern::MatchAs(as_pattern) => {
                if let Some(name) = &as_pattern.name {
                    self.record_write(name);
                }
            }
            ast::Pattern::MatchValue(_)
            | ast::Pattern::MatchSingleton(_)
            | ast::Pattern::MatchSequence(_)
            | ast::Pattern::MatchClass(_)
            | ast::Pattern::MatchOr(_) => {}
        }
        visitor::walk_pattern(self, pattern);
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        match expr {
            Expr::Name(name) if matches!(name.ctx, ExprContext::Load) => {
                self.record_read(&name.id);
            }
            Expr::Attribute(attribute) => {
                if matches!(attribute.ctx, ExprContext::Load) {
                    self.track_attribute_access(attribute);
                }
                visitor::walk_expr(self, expr);
            }
            Expr::Named(named) => {
                self.visit_expr(&named.value);
                if let Expr::Name(name) = named.target.as_ref() {
                    self.record_named_expression_write(&name.id);
                } else {
                    self.visit_assignment_target(&named.target);
                }
            }
            Expr::Lambda(lambda) => {
                if let Some(parameters) = &lambda.parameters {
                    self.visit_parameters(parameters);
                }
                self.scopes.push(ScopeBindings::lambda(
                    lambda.parameters.as_deref(),
                    &lambda.body,
                ));
                self.visit_expr(&lambda.body);
                self.scopes
                    .pop()
                    .expect("lambda dependency scope should be present");
            }
            Expr::ListComp(comprehension) => {
                self.visit_comprehension_parts(&comprehension.generators, &[&comprehension.elt]);
            }
            Expr::SetComp(comprehension) => {
                self.visit_comprehension_parts(&comprehension.generators, &[&comprehension.elt]);
            }
            Expr::DictComp(comprehension) => {
                let mut result_expressions = Vec::with_capacity(2);
                if let Some(key) = &comprehension.key {
                    result_expressions.push(key.as_ref());
                }
                result_expressions.push(comprehension.value.as_ref());
                self.visit_comprehension_parts(&comprehension.generators, &result_expressions);
            }
            Expr::Generator(comprehension) => {
                self.visit_comprehension_parts(&comprehension.generators, &[&comprehension.elt]);
            }
            _ => visitor::walk_expr(self, expr),
        }
    }
}

/// Builds a `ModuleDepGraph` from a Python AST
pub(crate) struct GraphBuilder<'a> {
    graph: &'a mut ModuleDepGraph,
    /// Track import aliases for importlib detection
    /// Maps local name -> module path (e.g., "il" -> "importlib", "im" ->
    /// "`importlib.import_module`")
    import_aliases: FxIndexMap<String, String>,
    /// Names rebound by non-import local bindings in the enclosing function scopes
    /// (parameters, assignments, loop/with targets); such names must not be treated
    /// as `importlib` or one of its aliases
    shadowed_bindings: FxIndexSet<String>,
    python_version: u8,
    /// Qualified identity of the function or class currently being traversed.
    scope_path: Option<ScopePath>,
    /// Next module-local scope identifier.
    next_scope_id: u32,
}

impl<'a> GraphBuilder<'a> {
    pub(crate) fn new(graph: &'a mut ModuleDepGraph, python_version: u8) -> Self {
        Self {
            graph,
            import_aliases: FxIndexMap::default(),
            shadowed_bindings: FxIndexSet::default(),
            python_version,
            scope_path: None,
            next_scope_id: 0,
        }
    }

    /// Allocate a unique qualified path beneath the current lexical scope.
    fn allocate_scope(&mut self, kind: ScopeKind) -> ScopePath {
        let scope_id = self.next_scope_id;
        self.next_scope_id += 1;
        self.scope_path.as_ref().map_or_else(
            || ScopePath::root(scope_id, kind),
            |parent| parent.child(scope_id, kind),
        )
    }

    /// Build the graph from an AST
    pub(crate) fn build_from_ast(&mut self, ast: &ModModule) -> Result<()> {
        // Process all statements in the module
        log::trace!("Building graph from AST with {} statements", ast.body.len());
        for stmt in &ast.body {
            self.process_statement(stmt)?;
        }
        Ok(())
    }

    /// Process a statement and add it to the graph
    fn process_statement(&mut self, stmt: &Stmt) -> Result<()> {
        log::trace!(
            "process_statement: Processing statement type: {:?}",
            std::mem::discriminant(stmt)
        );
        // Inside functions, process imports, functions, and classes normally
        // Skip other statements as they're tracked via eventual_read_vars
        if self
            .scope_path
            .as_ref()
            .is_some_and(|scope| scope.kind() == ScopeKind::Function)
        {
            match stmt {
                Stmt::Import(import_stmt) => {
                    self.process_import(import_stmt);
                    return Ok(());
                }
                Stmt::ImportFrom(import_from) => {
                    self.process_import_from(import_from);
                    return Ok(());
                }
                Stmt::FunctionDef(func_def) => return self.process_function_def(func_def),
                Stmt::ClassDef(class_def) => return self.process_class_def(class_def),
                // Recurse into control flow blocks that may contain imports
                Stmt::If(_)
                | Stmt::For(_)
                | Stmt::While(_)
                | Stmt::With(_)
                | Stmt::Try(_)
                | Stmt::Match(_) => {
                    // Fall through to regular processing to handle nested imports
                }
                _ => return Ok(()),
            }
        }

        match stmt {
            Stmt::Import(import_stmt) => {
                log::debug!("Processing import statement");
                self.process_import(import_stmt);
                Ok(())
            }
            Stmt::ImportFrom(import_from) => {
                self.process_import_from(import_from);
                Ok(())
            }
            Stmt::FunctionDef(func_def) => self.process_function_def(func_def),
            Stmt::ClassDef(class_def) => self.process_class_def(class_def),
            Stmt::Assign(assign) => {
                self.process_assign(assign);
                Ok(())
            }
            Stmt::AnnAssign(ann_assign) => {
                self.process_ann_assign(ann_assign);
                Ok(())
            }
            Stmt::TypeAlias(type_alias) => {
                self.process_type_alias(type_alias);
                Ok(())
            }
            Stmt::Expr(expr_stmt) => {
                self.process_expr_stmt(&expr_stmt.value);
                Ok(())
            }
            Stmt::Assert(assert_stmt) => {
                self.process_assert_stmt(assert_stmt);
                Ok(())
            }
            Stmt::If(if_stmt) => self.process_if_stmt(if_stmt),
            Stmt::For(for_stmt) => self.process_for_stmt(for_stmt),
            Stmt::While(while_stmt) => self.process_while_stmt(while_stmt),
            Stmt::With(with_stmt) => self.process_with_stmt(with_stmt),
            Stmt::Try(try_stmt) => self.process_try_stmt(try_stmt),
            Stmt::Match(match_stmt) => self.process_match_stmt(match_stmt),
            Stmt::Raise(raise_stmt) => {
                self.process_raise_stmt(raise_stmt);
                Ok(())
            }
            _ => Ok(()), // Other statements
        }
    }

    /// Process an import statement
    fn process_import(&mut self, import_stmt: &ast::StmtImport) {
        for alias in &import_stmt.names {
            let module_name = alias.name.as_str();
            let local_name = alias
                .asname
                .as_ref()
                .map_or(module_name, ruff_python_ast::Identifier::as_str);

            log::trace!("Processing import: {module_name} as {local_name}");

            // The import statement makes its binding live from here on: lift any
            // body-wide shadow recorded for the bound name (a dotted import
            // without alias binds only the root)
            let bound_name = if alias.asname.is_some() {
                local_name
            } else {
                module_name.split('.').next().unwrap_or(module_name)
            };
            self.shadowed_bindings.shift_remove(bound_name);

            // Track importlib aliases for later detection
            if module_name == "importlib" {
                self.import_aliases
                    .insert(local_name.to_owned(), "importlib".to_owned());
            }

            let mut imported_names = FxIndexSet::default();
            let mut var_decls = FxIndexSet::default();

            // For imports like `import xml.etree.ElementTree`:
            // - The imported name is the full module path "xml.etree.ElementTree"
            // - The declared variable is determined by the alias or the module path
            if alias.asname.is_some() {
                // import xml.etree.ElementTree as ET
                // Imported: xml.etree.ElementTree, Declared: ET
                imported_names.insert(local_name.to_owned());
                var_decls.insert(local_name.to_owned());
            } else if module_name.contains('.') {
                // import xml.etree.ElementTree
                // Imported: xml.etree.ElementTree, Declared: xml (root variable)
                imported_names.insert(module_name.to_owned());
                let root_module = module_name
                    .split('.')
                    .next()
                    .expect("module name should have at least one part");
                var_decls.insert(root_module.to_owned());
            } else {
                // import os
                // Imported: os, Declared: os
                imported_names.insert(local_name.to_owned());
                var_decls.insert(local_name.to_owned());
            }

            let item_data = ItemData {
                item_type: ItemType::Import {
                    module: module_name.to_owned(),
                    alias: alias.asname.as_ref().map(ToString::to_string),
                },
                var_decls,
                read_vars: FxIndexSet::default(),
                eventual_read_vars: FxIndexSet::default(),
                write_vars: FxIndexSet::default(),
                eventual_write_vars: FxIndexSet::default(),
                has_side_effects: crate::side_effects::import_has_side_effects(
                    module_name,
                    self.python_version,
                ),
                imported_names,
                reexported_names: FxIndexSet::default(),
                defined_symbols: FxIndexSet::default(),
                symbol_dependencies: FxIndexMap::default(),
                attribute_accesses: FxIndexMap::default(),
                containing_scope: self.scope_path.clone(),
            };

            self.graph.add_item(item_data);
        }
    }

    /// Process a from-import statement
    fn process_import_from(&mut self, import_from: &ast::StmtImportFrom) {
        let module_name = import_from
            .module
            .as_ref()
            .map_or("", ruff_python_ast::Identifier::as_str);

        // Skip __future__ imports as they're handled separately
        if module_name == "__future__" {
            return;
        }

        // For relative imports, we should not store the raw module name
        // It should be resolved to the full module path or marked as relative
        let effective_module = if import_from.level > 0 {
            // This is a relative import - mark it with dots
            let dots = ".".repeat(import_from.level as usize);
            if module_name.is_empty() {
                dots
            } else {
                format!("{dots}{module_name}")
            }
        } else {
            module_name.to_owned()
        };

        let is_star = import_from.names.len() == 1 && import_from.names[0].name.as_str() == "*";

        let mut imported_names = FxIndexSet::default();
        let mut names = Vec::new();
        let mut reexported_names = FxIndexSet::default();

        if is_star {
            imported_names.insert("*".to_owned());
        } else {
            for alias in &import_from.names {
                let imported_name = alias.name.as_str();
                let local_name = alias
                    .asname
                    .as_ref()
                    .map_or(imported_name, ruff_python_ast::Identifier::as_str);

                imported_names.insert(local_name.to_owned());
                // The import statement makes its binding live from here on: lift
                // any body-wide shadow recorded for the bound name
                self.shadowed_bindings.shift_remove(local_name);
                names.push((
                    imported_name.to_owned(),
                    alias.asname.as_ref().map(ToString::to_string),
                ));

                // Check for explicit re-export pattern: from foo import Bar as Bar
                if alias
                    .asname
                    .as_ref()
                    .map(ruff_python_ast::Identifier::as_str)
                    == Some(imported_name)
                {
                    reexported_names.insert(local_name.to_owned());
                }

                // Track import_module from importlib
                if module_name == "importlib" && imported_name == "import_module" {
                    self.import_aliases
                        .insert(local_name.to_owned(), "importlib.import_module".to_owned());
                }
            }
        }

        let item_data = ItemData {
            item_type: ItemType::FromImport {
                module: effective_module,
                names,
                level: import_from.level,
                is_star,
            },
            var_decls: if is_star {
                FxIndexSet::default() // star-import declares nothing explicitly
            } else {
                imported_names.clone() // FromImport declares the imported names as variables
            },
            read_vars: FxIndexSet::default(),
            eventual_read_vars: FxIndexSet::default(),
            write_vars: FxIndexSet::default(),
            eventual_write_vars: FxIndexSet::default(),
            has_side_effects: crate::side_effects::from_import_has_side_effects(
                import_from,
                self.python_version,
            ),
            imported_names,
            reexported_names,
            defined_symbols: FxIndexSet::default(),
            symbol_dependencies: FxIndexMap::default(),
            attribute_accesses: FxIndexMap::default(),
            containing_scope: self.scope_path.clone(),
        };

        self.graph.add_item(item_data);
    }

    /// Process a function definition
    fn process_function_def(&mut self, func_def: &ast::StmtFunctionDef) -> Result<()> {
        let func_name = func_def.name.to_string();
        let function_scope = self.allocate_scope(ScopeKind::Function);

        // Collect variables from decorators and type annotations
        let mut read_vars = FxIndexSet::default();

        // Process decorators
        for decorator in &func_def.decorator_list {
            self.collect_vars_in_expr(&decorator.expression, &mut read_vars);
        }

        // Process parameter type annotations and defaults
        self.collect_function_parameter_vars(&func_def.parameters, &mut read_vars);

        // Process return type annotation
        if let Some(returns) = &func_def.returns {
            self.collect_vars_in_expr(returns, &mut read_vars);
        }

        // Collect variables that will be read within the function
        let mut eventual_read_vars = FxIndexSet::default();
        let mut eventual_write_vars = FxIndexSet::default();
        let mut eventual_attribute_accesses = FxIndexMap::default();
        self.collect_vars_in_function_body(
            &func_def.parameters,
            &func_def.body,
            &mut eventual_read_vars,
            &mut eventual_write_vars,
            &mut eventual_attribute_accesses,
        );

        // Build symbol dependencies - the function depends on all variables it reads
        let mut symbol_dependencies = FxIndexMap::default();
        let mut all_deps = FxIndexSet::default();
        all_deps.extend(read_vars.clone());
        all_deps.extend(eventual_read_vars.clone());
        symbol_dependencies.insert(func_name.clone(), all_deps);

        log::debug!(
            "Function {func_name} has eventual_read_vars: {eventual_read_vars:?}, \
             eventual_write_vars: {eventual_write_vars:?}"
        );

        let item_data = ItemData {
            item_type: ItemType::FunctionDef {
                name: func_name.clone(),
                scope: function_scope.clone(),
            },
            var_decls: std::iter::once(func_name.clone()).collect(),
            read_vars,
            eventual_read_vars,
            write_vars: FxIndexSet::default(),
            eventual_write_vars,
            has_side_effects: false,
            imported_names: FxIndexSet::default(),
            reexported_names: FxIndexSet::default(),
            defined_symbols: std::iter::once(func_name).collect(),
            symbol_dependencies,
            attribute_accesses: eventual_attribute_accesses,
            containing_scope: self.scope_path.clone(),
        };

        self.graph.add_item(item_data);

        // Python scoping makes parameters and any name assigned in the function local
        // for the WHOLE body: precompute them so calls through shadowed names (e.g. a
        // parameter named `importlib`) are not recorded as real imports
        let saved_shadowed_bindings = self.shadowed_bindings.clone();
        for param in func_def
            .parameters
            .posonlyargs
            .iter()
            .chain(&func_def.parameters.args)
            .chain(&func_def.parameters.kwonlyargs)
        {
            self.shadowed_bindings
                .insert(param.parameter.name.as_str().to_owned());
        }
        if let Some(vararg) = &func_def.parameters.vararg {
            self.shadowed_bindings
                .insert(vararg.name.as_str().to_owned());
        }
        if let Some(kwarg) = &func_def.parameters.kwarg {
            self.shadowed_bindings
                .insert(kwarg.name.as_str().to_owned());
        }
        {
            // Import bindings shadow the whole body too (UnboundLocalError before
            // the import statement); `process_import`/`process_from_import` lift
            // the shadow when the statement is reached. `global`-declared names
            // rebind the module scope instead of shadowing.
            let mut body_bindings = FxIndexSet::default();
            let scope_globals = crate::visitors::collect_scope_global_declarations(&func_def.body);
            crate::visitors::LocalVarCollector::new(&mut body_bindings, &scope_globals)
                .collect_from_stmts(&func_def.body);
            self.shadowed_bindings.extend(body_bindings);
        }

        // Process the function body in function scope
        let old_scope_path = self.scope_path.replace(function_scope);
        for stmt in &func_def.body {
            self.process_statement(stmt)?;
        }
        self.scope_path = old_scope_path;
        self.shadowed_bindings = saved_shadowed_bindings;

        // The definition's NAME rebinds in the enclosing scope from here on: a
        // later `def importlib(): ...` kills an earlier import alias
        self.shadowed_bindings.insert(func_def.name.to_string());

        Ok(())
    }

    /// Process a class definition
    fn process_class_def(&mut self, class_def: &ast::StmtClassDef) -> Result<()> {
        let class_name = class_def.name.to_string();
        let class_scope = self.allocate_scope(ScopeKind::Class);

        // Collect variables from decorators
        let mut read_vars = FxIndexSet::default();
        for decorator in &class_def.decorator_list {
            self.collect_vars_in_expr(&decorator.expression, &mut read_vars);
        }

        // Collect variables from base classes
        if let Some(_arguments) = &class_def.type_params {
            // Handle type parameters if present
            // Note: This is for generic classes
        }

        let mut attribute_accesses = FxIndexMap::default();
        if let Some(arguments) = &class_def.arguments {
            log::debug!(
                "Class {} has {} base classes",
                class_name,
                arguments.args.len()
            );
            for arg in &arguments.args {
                self.collect_vars_in_expr_with_attrs(arg, &mut read_vars, &mut attribute_accesses);
            }
            log::debug!("Class {class_name} base class read_vars: {read_vars:?}");
            // Collect variables from keyword arguments (e.g., metaclass=ABCMeta)
            for kw in &arguments.keywords {
                self.collect_vars_in_expr_with_attrs(
                    &kw.value,
                    &mut read_vars,
                    &mut attribute_accesses,
                );
            }
        }

        // Build symbol dependencies - the class depends on its base classes and decorators
        let mut symbol_dependencies = FxIndexMap::default();
        symbol_dependencies.insert(class_name.clone(), read_vars.clone());

        // Collect all variables used in methods to add as eventual dependencies
        let mut method_read_vars = FxIndexSet::default();
        let mut method_write_vars = FxIndexSet::default();
        let mut method_attribute_accesses = FxIndexMap::default();
        for stmt in &class_def.body {
            match stmt {
                Stmt::FunctionDef(method_def) => {
                    // Collect variables from method decorators
                    // While decorators execute at class definition time, they don't affect
                    // the ordering of the class itself - only the methods
                    for decorator in &method_def.decorator_list {
                        self.collect_vars_in_expr(&decorator.expression, &mut method_read_vars);
                    }

                    // Collect variables from method parameter annotations and defaults
                    // These don't affect class definition ordering
                    self.collect_function_parameter_vars(
                        &method_def.parameters,
                        &mut method_read_vars,
                    );

                    // Collect variables from return type annotation
                    if let Some(returns) = &method_def.returns {
                        self.collect_vars_in_expr(returns, &mut method_read_vars);
                    }

                    // Collect variables used in the method body (these are eventual dependencies)
                    self.collect_vars_in_function_body(
                        &method_def.parameters,
                        &method_def.body,
                        &mut method_read_vars,
                        &mut method_write_vars,
                        &mut method_attribute_accesses,
                    );
                }
                Stmt::Assign(assign) => {
                    // Class-level assignments like `yaml_loader = [Loader, FullLoader,
                    // UnsafeLoader]` These execute at class definition time, so
                    // they're immediate dependencies
                    self.collect_vars_in_expr(&assign.value, &mut read_vars);
                }
                Stmt::AnnAssign(ann_assign) => {
                    // Annotated class-level assignments (immediate deps)
                    // Read the annotation
                    self.collect_vars_in_expr_with_attrs(
                        &ann_assign.annotation,
                        &mut read_vars,
                        &mut method_attribute_accesses,
                    );
                    // Read the value if present
                    if let Some(value) = &ann_assign.value {
                        self.collect_vars_in_expr_with_attrs(
                            value,
                            &mut read_vars,
                            &mut method_attribute_accesses,
                        );
                    }
                    // Reads from attribute/subscript targets (e.g., cfg['x']: T = v)
                    self.collect_reads_from_assignment_target(&ann_assign.target, &mut read_vars);
                }
                _ => {
                    // Other statements in class body (e.g., docstrings)
                }
            }
        }

        // Merge attribute accesses from base classes and methods
        for (key, values) in attribute_accesses {
            method_attribute_accesses
                .entry(key)
                .or_default()
                .extend(values);
        }

        let item_data = ItemData {
            item_type: ItemType::ClassDef {
                name: class_name.clone(),
                scope: class_scope.clone(),
            },
            var_decls: std::iter::once(class_name.clone()).collect(),
            read_vars,
            eventual_read_vars: method_read_vars, // Methods may use these variables
            write_vars: FxIndexSet::default(),
            eventual_write_vars: FxIndexSet::default(),
            has_side_effects: false,
            imported_names: FxIndexSet::default(),
            reexported_names: FxIndexSet::default(),
            defined_symbols: std::iter::once(class_name).collect(),
            symbol_dependencies,
            attribute_accesses: method_attribute_accesses,
            containing_scope: self.scope_path.clone(),
        };

        self.graph.add_item(item_data);

        // Process the class body in class scope
        let old_scope_path = self.scope_path.replace(class_scope);
        for stmt in &class_def.body {
            self.process_statement(stmt)?;
        }
        self.scope_path = old_scope_path;

        // The definition's NAME rebinds in the enclosing scope from here on: a
        // later `class importlib: ...` kills an earlier import alias
        self.shadowed_bindings.insert(class_def.name.to_string());

        Ok(())
    }

    /// Process an assignment statement
    fn process_assign(&mut self, assign: &ast::StmtAssign) {
        let mut targets = Vec::new();
        let mut var_decls = FxIndexSet::default();

        for target in &assign.targets {
            if let Some(names) = self.extract_assignment_targets(target) {
                targets.extend(names.iter().cloned());
                var_decls.extend(names);
            }
        }

        // Collect variables read in the value expression
        let mut read_vars = FxIndexSet::default();
        let mut attribute_accesses = FxIndexMap::default();
        self.collect_vars_in_expr_with_attrs(
            &assign.value,
            &mut read_vars,
            &mut attribute_accesses,
        );

        if !attribute_accesses.is_empty() {
            log::debug!("Assignment collected attribute_accesses: {attribute_accesses:?}");
        }

        // Also collect reads from assignment targets (for subscript/attribute mutations)
        for target in &assign.targets {
            self.collect_reads_from_assignment_target(target, &mut read_vars);
        }

        // Check if this is an importlib.import_module() assignment
        if let Some(module_name) = self.is_static_importlib_call(&assign.value) {
            // This is an importlib.import_module() assignment
            // Track it as an import for tree-shaking purposes
            log::debug!(
                "Found importlib.import_module('{module_name}') assignment to: {targets:?}"
            );

            // Create an Import item for each target variable
            for target in &targets {
                let mut imported_names = FxIndexSet::default();
                imported_names.insert(module_name.clone());

                let item_data = ItemData {
                    item_type: ItemType::Import {
                        module: module_name.clone(),
                        alias: Some(target.clone()),
                    },
                    var_decls: std::iter::once(target.clone()).collect(),
                    read_vars: read_vars.clone(),
                    eventual_read_vars: FxIndexSet::default(),
                    write_vars: FxIndexSet::default(),
                    eventual_write_vars: FxIndexSet::default(),
                    has_side_effects: true, // Import always has side effects
                    imported_names,
                    reexported_names: FxIndexSet::default(),
                    defined_symbols: std::iter::once(target.clone()).collect(),
                    symbol_dependencies: FxIndexMap::default(),
                    attribute_accesses: FxIndexMap::default(),
                    containing_scope: self.scope_path.clone(),
                };

                self.graph.add_item(item_data);
            }
        } else {
            // Regular assignment
            // Check if this is an __all__ assignment
            let is_all_assignment = targets.contains(&"__all__".to_owned());
            let mut reexported_names = FxIndexSet::default();

            if is_all_assignment {
                // Extract names from __all__ value
                let extracted = extract_string_list_from_expr(&assign.value);
                if let Some(names) = extracted.names {
                    reexported_names.extend(names);
                }
            }

            // With the proxy approach, stdlib modules are handled dynamically,
            // so we treat all attribute accesses conservatively for side effects

            let item_data = ItemData {
                item_type: ItemType::Assignment {
                    targets: targets.clone(),
                },
                var_decls: var_decls.clone(),
                read_vars,
                eventual_read_vars: reexported_names.clone(), /* Names in __all__ are "eventually
                                                               * read" */
                write_vars: FxIndexSet::default(),
                eventual_write_vars: FxIndexSet::default(),
                has_side_effects: Self::expression_has_side_effects(&assign.value),
                imported_names: FxIndexSet::default(),
                reexported_names,
                defined_symbols: var_decls,
                symbol_dependencies: FxIndexMap::default(),
                attribute_accesses,
                containing_scope: self.scope_path.clone(),
            };

            self.graph.add_item(item_data);
        }
    }

    /// Process an annotated assignment statement
    fn process_ann_assign(&mut self, ann_assign: &ast::StmtAnnAssign) {
        let mut var_decls = FxIndexSet::default();
        let mut read_vars = FxIndexSet::default();

        // Extract target variable name
        if let Some(names) = self.extract_assignment_targets(&ann_assign.target) {
            var_decls.extend(names);
        }

        // Collect variables from the type annotation
        self.collect_vars_in_expr(&ann_assign.annotation, &mut read_vars);

        // Collect variables from the value expression if present
        if let Some(value) = &ann_assign.value {
            self.collect_vars_in_expr(value, &mut read_vars);
        }

        let item_data = ItemData {
            item_type: ItemType::Assignment {
                targets: var_decls.iter().cloned().collect(),
            },
            var_decls: var_decls.clone(),
            read_vars,
            eventual_read_vars: FxIndexSet::default(),
            write_vars: FxIndexSet::default(),
            eventual_write_vars: FxIndexSet::default(),
            has_side_effects: ann_assign
                .value
                .as_ref()
                .is_some_and(|v| Self::expression_has_side_effects(v)),
            imported_names: FxIndexSet::default(),
            reexported_names: FxIndexSet::default(),
            defined_symbols: var_decls,
            symbol_dependencies: FxIndexMap::default(),
            attribute_accesses: FxIndexMap::default(),
            containing_scope: self.scope_path.clone(),
        };

        self.graph.add_item(item_data);
    }

    /// Process a lazily evaluated type alias definition.
    fn process_type_alias(&mut self, type_alias: &ast::StmtTypeAlias) {
        let Some(targets) = self.extract_assignment_targets(&type_alias.name) else {
            return;
        };
        let var_decls: FxIndexSet<String> = targets.iter().cloned().collect();
        let mut eventual_read_vars = FxIndexSet::default();
        let mut attribute_accesses = FxIndexMap::default();
        self.collect_vars_in_expr_with_attrs(
            &type_alias.value,
            &mut eventual_read_vars,
            &mut attribute_accesses,
        );

        if let Some(type_params) = &type_alias.type_params {
            let mut type_param_names = FxIndexSet::default();
            for type_param in type_params.iter() {
                match type_param {
                    ast::TypeParam::TypeVar(type_var) => {
                        type_param_names.insert(type_var.name.to_string());
                        for dependency in [type_var.bound.as_deref(), type_var.default.as_deref()]
                            .into_iter()
                            .flatten()
                        {
                            self.collect_vars_in_expr_with_attrs(
                                dependency,
                                &mut eventual_read_vars,
                                &mut attribute_accesses,
                            );
                        }
                    }
                    ast::TypeParam::TypeVarTuple(type_var_tuple) => {
                        type_param_names.insert(type_var_tuple.name.to_string());
                        if let Some(default) = &type_var_tuple.default {
                            self.collect_vars_in_expr_with_attrs(
                                default,
                                &mut eventual_read_vars,
                                &mut attribute_accesses,
                            );
                        }
                    }
                    ast::TypeParam::ParamSpec(param_spec) => {
                        type_param_names.insert(param_spec.name.to_string());
                        if let Some(default) = &param_spec.default {
                            self.collect_vars_in_expr_with_attrs(
                                default,
                                &mut eventual_read_vars,
                                &mut attribute_accesses,
                            );
                        }
                    }
                }
            }
            for name in type_param_names {
                eventual_read_vars.shift_remove(&name);
                attribute_accesses.shift_remove(&name);
            }
        }

        self.graph.add_item(ItemData {
            item_type: ItemType::Assignment { targets },
            var_decls: var_decls.clone(),
            read_vars: FxIndexSet::default(),
            eventual_read_vars,
            write_vars: FxIndexSet::default(),
            eventual_write_vars: FxIndexSet::default(),
            has_side_effects: false,
            imported_names: FxIndexSet::default(),
            reexported_names: FxIndexSet::default(),
            defined_symbols: var_decls,
            symbol_dependencies: FxIndexMap::default(),
            attribute_accesses,
            containing_scope: self.scope_path.clone(),
        });
    }

    /// Process an expression statement
    fn process_expr_stmt(&mut self, expr: &Expr) {
        let mut read_vars = FxIndexSet::default();
        let mut attribute_accesses = FxIndexMap::default();
        self.collect_vars_in_expr_with_attrs(expr, &mut read_vars, &mut attribute_accesses);

        log::debug!(
            "Processing expression statement, read_vars: {read_vars:?}, attribute_accesses: \
             {attribute_accesses:?}"
        );

        // Check if this is a docstring or other constant expression
        let has_side_effects = match expr {
            // Docstrings and constant expressions don't have side effects
            Expr::StringLiteral(_)
            | Expr::NumberLiteral(_)
            | Expr::BooleanLiteral(_)
            | Expr::NoneLiteral(_)
            | Expr::BytesLiteral(_)
            | Expr::EllipsisLiteral(_) => false,
            // For other expressions, check using the side effect detector
            _ => Self::expression_has_side_effects(expr),
        };

        let item_data = ItemData {
            item_type: ItemType::Expression,
            var_decls: FxIndexSet::default(),
            read_vars,
            eventual_read_vars: FxIndexSet::default(),
            write_vars: FxIndexSet::default(),
            eventual_write_vars: FxIndexSet::default(),
            has_side_effects,
            imported_names: FxIndexSet::default(),
            reexported_names: FxIndexSet::default(),
            defined_symbols: FxIndexSet::default(),
            symbol_dependencies: FxIndexMap::default(),
            attribute_accesses,
            containing_scope: self.scope_path.clone(),
        };

        self.graph.add_item(item_data);
    }

    /// Process assert statement
    fn process_assert_stmt(&mut self, assert_stmt: &ast::StmtAssert) {
        let mut read_vars = FxIndexSet::default();
        let mut attribute_accesses = FxIndexMap::default();

        // Collect variables from the test expression
        self.collect_vars_in_expr_with_attrs(
            &assert_stmt.test,
            &mut read_vars,
            &mut attribute_accesses,
        );

        // Also collect from the message expression if present
        if let Some(msg) = &assert_stmt.msg {
            self.collect_vars_in_expr_with_attrs(msg, &mut read_vars, &mut attribute_accesses);
        }

        log::debug!(
            "Processing assert statement, read_vars: {read_vars:?}, attribute_accesses: \
             {attribute_accesses:?}"
        );

        let item_data = ItemData {
            item_type: ItemType::Expression, // Assert is treated as an expression with side effects
            var_decls: FxIndexSet::default(),
            read_vars,
            eventual_read_vars: FxIndexSet::default(),
            write_vars: FxIndexSet::default(),
            eventual_write_vars: FxIndexSet::default(),
            has_side_effects: true, /* Assert statements have side effects (can raise
                                     * AssertionError) */
            imported_names: FxIndexSet::default(),
            reexported_names: FxIndexSet::default(),
            defined_symbols: FxIndexSet::default(),
            symbol_dependencies: FxIndexMap::default(),
            attribute_accesses,
            containing_scope: self.scope_path.clone(),
        };

        self.graph.add_item(item_data);
    }

    /// Process if statement
    fn process_if_stmt(&mut self, if_stmt: &ast::StmtIf) -> Result<()> {
        // Process condition
        let mut read_vars = FxIndexSet::default();
        self.collect_vars_in_expr(&if_stmt.test, &mut read_vars);

        let item_data = ItemData {
            item_type: ItemType::If {
                condition: String::new(), // Could extract condition text if needed
            },
            var_decls: FxIndexSet::default(),
            read_vars,
            eventual_read_vars: FxIndexSet::default(),
            write_vars: FxIndexSet::default(),
            eventual_write_vars: FxIndexSet::default(),
            has_side_effects: true,
            imported_names: FxIndexSet::default(),
            reexported_names: FxIndexSet::default(),
            defined_symbols: FxIndexSet::default(),
            symbol_dependencies: FxIndexMap::default(),
            attribute_accesses: FxIndexMap::default(),
            containing_scope: self.scope_path.clone(),
        };

        self.graph.add_item(item_data);

        // Process body
        for stmt in &if_stmt.body {
            self.process_statement(stmt)?;
        }

        // Process elif/else branches
        for clause in &if_stmt.elif_else_clauses {
            if let Some(condition) = &clause.test {
                let mut read_vars = FxIndexSet::default();
                self.collect_vars_in_expr(condition, &mut read_vars);
                // Could add as separate If item
            }
            for stmt in &clause.body {
                self.process_statement(stmt)?;
            }
        }

        Ok(())
    }

    /// Process for loop
    fn process_for_stmt(&mut self, for_stmt: &ast::StmtFor) -> Result<()> {
        let mut read_vars = FxIndexSet::default();
        self.collect_vars_in_expr(&for_stmt.iter, &mut read_vars);

        // Extract loop variables
        let mut write_vars = FxIndexSet::default();
        if let Some(names) = self.extract_assignment_targets(&for_stmt.target) {
            write_vars.extend(names);
        }

        let item_data = ItemData {
            item_type: ItemType::Other,
            var_decls: FxIndexSet::default(),
            read_vars,
            eventual_read_vars: FxIndexSet::default(),
            write_vars,
            eventual_write_vars: FxIndexSet::default(),
            has_side_effects: true,
            imported_names: FxIndexSet::default(),
            reexported_names: FxIndexSet::default(),
            defined_symbols: FxIndexSet::default(),
            symbol_dependencies: FxIndexMap::default(),
            attribute_accesses: FxIndexMap::default(),
            containing_scope: self.scope_path.clone(),
        };

        self.graph.add_item(item_data);

        // Process body
        for stmt in &for_stmt.body {
            self.process_statement(stmt)?;
        }

        // Process else clause
        for stmt in &for_stmt.orelse {
            self.process_statement(stmt)?;
        }

        Ok(())
    }

    /// Process while loop
    fn process_while_stmt(&mut self, while_stmt: &ast::StmtWhile) -> Result<()> {
        let mut read_vars = FxIndexSet::default();
        self.collect_vars_in_expr(&while_stmt.test, &mut read_vars);

        self.add_control_flow_item(ItemType::Other, read_vars, FxIndexMap::default(), true);

        // Process body
        for stmt in &while_stmt.body {
            self.process_statement(stmt)?;
        }

        // Process else clause
        for stmt in &while_stmt.orelse {
            self.process_statement(stmt)?;
        }

        Ok(())
    }

    /// Process with statement
    fn process_with_stmt(&mut self, with_stmt: &ast::StmtWith) -> Result<()> {
        let mut read_vars = FxIndexSet::default();

        for item in &with_stmt.items {
            self.collect_vars_in_expr(&item.context_expr, &mut read_vars);
        }

        self.add_control_flow_item(ItemType::Other, read_vars, FxIndexMap::default(), true);

        // Process body
        for stmt in &with_stmt.body {
            self.process_statement(stmt)?;
        }

        Ok(())
    }

    /// Process raise statement
    fn process_raise_stmt(&mut self, raise_stmt: &ast::StmtRaise) {
        log::debug!("Processing raise statement");

        let mut read_vars = FxIndexSet::default();
        let mut attribute_accesses = FxIndexMap::default();

        // Collect variables from the exception expression
        if let Some(exc) = &raise_stmt.exc {
            self.collect_vars_in_expr_with_attrs(exc, &mut read_vars, &mut attribute_accesses);
        }

        // Also collect from the cause expression if present
        if let Some(cause) = &raise_stmt.cause {
            self.collect_vars_in_expr_with_attrs(cause, &mut read_vars, &mut attribute_accesses);
        }

        log::debug!(
            "Processing raise statement, read_vars: {read_vars:?}, attribute_accesses: \
             {attribute_accesses:?}"
        );

        self.add_control_flow_item(ItemType::Other, read_vars, attribute_accesses, true);
    }

    /// Process try statement
    fn process_try_stmt(&mut self, try_stmt: &ast::StmtTry) -> Result<()> {
        log::debug!(
            "Processing try statement with {} statements in body",
            try_stmt.body.len()
        );

        self.add_control_flow_item(
            ItemType::Try,
            FxIndexSet::default(),
            FxIndexMap::default(),
            true,
        );

        // Process try body
        for stmt in &try_stmt.body {
            self.process_statement(stmt)?;
        }

        // Process except handlers
        for handler in &try_stmt.handlers {
            let ast::ExceptHandler::ExceptHandler(handler) = handler;

            // Track exception type if specified
            if let Some(type_expr) = &handler.type_ {
                let mut read_vars = FxIndexSet::default();
                let mut attribute_accesses = FxIndexMap::default();
                self.collect_vars_in_expr_with_attrs(
                    type_expr,
                    &mut read_vars,
                    &mut attribute_accesses,
                );

                self.add_control_flow_item(ItemType::Other, read_vars, attribute_accesses, false);
            }

            for stmt in &handler.body {
                self.process_statement(stmt)?;
            }
        }

        // Process else clause
        for stmt in &try_stmt.orelse {
            self.process_statement(stmt)?;
        }

        // Process finally clause
        for stmt in &try_stmt.finalbody {
            self.process_statement(stmt)?;
        }

        Ok(())
    }

    /// Process a structural pattern matching statement.
    fn process_match_stmt(&mut self, match_stmt: &ast::StmtMatch) -> Result<()> {
        let mut read_vars = FxIndexSet::default();
        let mut attribute_accesses = FxIndexMap::default();

        self.collect_vars_in_expr_with_attrs(
            &match_stmt.subject,
            &mut read_vars,
            &mut attribute_accesses,
        );
        for case in &match_stmt.cases {
            self.collect_vars_in_pattern(&case.pattern, &mut read_vars, &mut attribute_accesses);
            if let Some(guard) = &case.guard {
                self.collect_vars_in_expr_with_attrs(
                    guard,
                    &mut read_vars,
                    &mut attribute_accesses,
                );
            }
        }

        self.add_control_flow_item(ItemType::Other, read_vars, attribute_accesses, true);

        for case in &match_stmt.cases {
            for stmt in &case.body {
                self.process_statement(stmt)?;
            }
        }

        Ok(())
    }

    /// Add a dependency item for control flow that does not declare module symbols.
    fn add_control_flow_item(
        &mut self,
        item_type: ItemType,
        read_vars: FxIndexSet<String>,
        attribute_accesses: FxIndexMap<String, FxIndexSet<String>>,
        has_side_effects: bool,
    ) {
        self.graph.add_item(ItemData {
            item_type,
            var_decls: FxIndexSet::default(),
            read_vars,
            eventual_read_vars: FxIndexSet::default(),
            write_vars: FxIndexSet::default(),
            eventual_write_vars: FxIndexSet::default(),
            has_side_effects,
            imported_names: FxIndexSet::default(),
            reexported_names: FxIndexSet::default(),
            defined_symbols: FxIndexSet::default(),
            symbol_dependencies: FxIndexMap::default(),
            attribute_accesses,
            containing_scope: self.scope_path.clone(),
        });
    }

    /// Extract assignment target names
    fn extract_assignment_targets(&self, expr: &Expr) -> Option<Vec<String>> {
        let mut names = Vec::new();
        let mut stack = vec![expr];

        while let Some(current_expr) = stack.pop() {
            match current_expr {
                Expr::Name(name) => {
                    names.push(name.id.to_string());
                }
                Expr::Tuple(tuple) => {
                    stack.extend(tuple.elts.iter());
                }
                Expr::List(list) => {
                    stack.extend(list.elts.iter());
                }
                Expr::Subscript(_) | Expr::Attribute(_) => {
                    // For subscript (e.g., result["key"]) and attribute (e.g., obj.attr)
                    // assignments, we don't add them to write_vars as they
                    // don't create new variables However, we need to track that
                    // they're being mutated - handled separately
                }
                _ => return None, // Unsupported target type
            }
        }

        if names.is_empty() { None } else { Some(names) }
    }

    /// Collect variables used in an expression and track attribute accesses
    fn collect_vars_in_expr_with_attrs(
        &self,
        expr: &Expr,
        vars: &mut FxIndexSet<String>,
        attribute_accesses: &mut FxIndexMap<String, FxIndexSet<String>>,
    ) {
        DependencyCollector::expression(vars, attribute_accesses).visit_expr(expr);
    }

    /// Collect variables used in an expression
    fn collect_vars_in_expr(&self, expr: &Expr, vars: &mut FxIndexSet<String>) {
        // Use the new method but ignore attribute accesses for backward compatibility
        let mut dummy_attrs = FxIndexMap::default();
        self.collect_vars_in_expr_with_attrs(expr, vars, &mut dummy_attrs);
    }

    /// Collect variables in a statement body
    fn collect_vars_in_function_body(
        &self,
        parameters: &ast::Parameters,
        body: &[Stmt],
        read_vars: &mut FxIndexSet<String>,
        write_vars: &mut FxIndexSet<String>,
        attribute_accesses: &mut FxIndexMap<String, FxIndexSet<String>>,
    ) {
        DependencyCollector::function_body(
            parameters,
            body,
            read_vars,
            write_vars,
            attribute_accesses,
        )
        .visit_body(body);
    }

    /// Collect runtime variable reads from a structural pattern.
    fn collect_vars_in_pattern(
        &self,
        pattern: &ast::Pattern,
        read_vars: &mut FxIndexSet<String>,
        attribute_accesses: &mut FxIndexMap<String, FxIndexSet<String>>,
    ) {
        match pattern {
            ast::Pattern::MatchValue(pattern) => {
                self.collect_vars_in_expr_with_attrs(&pattern.value, read_vars, attribute_accesses);
            }
            ast::Pattern::MatchSequence(pattern) => {
                for pattern in &pattern.patterns {
                    self.collect_vars_in_pattern(pattern, read_vars, attribute_accesses);
                }
            }
            ast::Pattern::MatchMapping(pattern) => {
                for key in &pattern.keys {
                    self.collect_vars_in_expr_with_attrs(key, read_vars, attribute_accesses);
                }
                for pattern in &pattern.patterns {
                    self.collect_vars_in_pattern(pattern, read_vars, attribute_accesses);
                }
            }
            ast::Pattern::MatchClass(pattern) => {
                self.collect_vars_in_expr_with_attrs(&pattern.cls, read_vars, attribute_accesses);
                for pattern in &pattern.arguments.patterns {
                    self.collect_vars_in_pattern(pattern, read_vars, attribute_accesses);
                }
                for keyword in &pattern.arguments.keywords {
                    self.collect_vars_in_pattern(&keyword.pattern, read_vars, attribute_accesses);
                }
            }
            ast::Pattern::MatchAs(pattern) => {
                if let Some(pattern) = &pattern.pattern {
                    self.collect_vars_in_pattern(pattern, read_vars, attribute_accesses);
                }
            }
            ast::Pattern::MatchOr(pattern) => {
                for pattern in &pattern.patterns {
                    self.collect_vars_in_pattern(pattern, read_vars, attribute_accesses);
                }
            }
            ast::Pattern::MatchSingleton(_) | ast::Pattern::MatchStar(_) => {}
        }
    }

    /// Check if an expression has side effects
    fn expression_has_side_effects(expr: &Expr) -> bool {
        // Delegates to visitor-based detector
        ExpressionSideEffectDetector::check(expr)
    }

    /// Collect variables that are read when assigning to subscripts or attributes
    fn collect_reads_from_assignment_target(
        &self,
        target: &Expr,
        read_vars: &mut FxIndexSet<String>,
    ) {
        match target {
            Expr::Subscript(subscript) => {
                // For result["key"] = value, we're reading 'result' to mutate it
                log::debug!("Found subscript assignment target, collecting reads from base object");
                self.collect_vars_in_expr(&subscript.value, read_vars);
            }
            Expr::Attribute(attr) => {
                // For obj.attr = value, we're reading 'obj' to mutate it
                self.collect_vars_in_expr(&attr.value, read_vars);
            }
            Expr::Tuple(tuple) => {
                // Handle tuple unpacking which might contain subscripts/attributes
                for elt in &tuple.elts {
                    self.collect_reads_from_assignment_target(elt, read_vars);
                }
            }
            Expr::List(list) => {
                // Handle list unpacking which might contain subscripts/attributes
                for elt in &list.elts {
                    self.collect_reads_from_assignment_target(elt, read_vars);
                }
            }
            _ => {
                // Simple names don't need special handling here
            }
        }
    }

    /// Check if an expression is an `importlib.import_module()` call with a static
    /// string argument.
    ///
    /// A callee whose base name is rebound by a local binding (function parameter,
    /// assignment) dispatches to that binding at runtime, not to `importlib`, and
    /// calls whose remaining arguments cannot be safely discarded stay runtime calls;
    /// neither is recorded as a real import.
    fn is_static_importlib_call(&self, expr: &Expr) -> Option<String> {
        if let Expr::Call(call) = expr {
            // Check if this is importlib.import_module() or an alias
            let is_import_module = match &*call.func {
                // Direct call: importlib.import_module() or alias.import_module()
                Expr::Attribute(attr) if attr.attr.as_str() == "import_module" => {
                    if let Expr::Name(name) = &*attr.value {
                        let name_str = name.id.as_str();
                        // Check if it's importlib directly or an alias
                        !self.shadowed_bindings.contains(name_str)
                            && (name_str == "importlib"
                                || self
                                    .import_aliases
                                    .get(name_str)
                                    .is_some_and(|v| v == "importlib"))
                    } else {
                        false
                    }
                }
                // Direct function call: import_module() or im()
                Expr::Name(name) => {
                    let name_str = name.id.as_str();
                    // Check if this is import_module or an alias for it
                    !self.shadowed_bindings.contains(name_str)
                        && (name_str == "import_module"
                            || self
                                .import_aliases
                                .get(name_str)
                                .is_some_and(|v| v == "importlib.import_module"))
                }
                _ => false,
            };

            if is_import_module
                && (crate::python::importlib_call::arguments_safely_discardable(call)
                    || crate::python::importlib_call::evaluable_package_argument(call).is_some())
            {
                // Extract the module name if it's a static string, from either the
                // first positional argument or the `name=` keyword argument
                return crate::python::importlib_call::literal_module_name(call)
                    .map(ToOwned::to_owned);
            }
        }
        None
    }

    /// Collect variables from function parameters (annotations and defaults)
    /// This helper reduces duplication between function, method, and nested function processing
    fn collect_function_parameter_vars(
        &self,
        parameters: &ast::Parameters,
        vars: &mut FxIndexSet<String>,
    ) {
        // Process parameter type annotations and defaults
        for param in parameters
            .posonlyargs
            .iter()
            .chain(parameters.args.iter())
            .chain(parameters.kwonlyargs.iter())
        {
            if let Some(annotation) = &param.parameter.annotation {
                self.collect_vars_in_expr(annotation, vars);
            }
            if let Some(default) = &param.default {
                self.collect_vars_in_expr(default, vars);
            }
        }

        // Process vararg annotation
        if let Some(vararg) = &parameters.vararg
            && let Some(annotation) = &vararg.annotation
        {
            self.collect_vars_in_expr(annotation, vars);
        }

        // Process kwarg annotation
        if let Some(kwarg) = &parameters.kwarg
            && let Some(annotation) = &kwarg.annotation
        {
            self.collect_vars_in_expr(annotation, vars);
        }
    }
}

#[cfg(test)]
mod tests {
    use ruff_python_parser::parse_module;

    use super::*;
    use crate::resolver::ModuleId;

    #[test]
    fn type_parameter_attributes_do_not_create_module_dependencies() {
        let ast = parse_module("type Alias[T] = T.member")
            .expect("Type alias should parse")
            .into_syntax();
        let mut graph = ModuleDepGraph::new(ModuleId::ENTRY, "module".to_owned());
        GraphBuilder::new(&mut graph, 13)
            .build_from_ast(&ast)
            .expect("Dependency graph should build");

        let alias = graph
            .items
            .values()
            .find(|item| item.var_decls.contains("Alias"))
            .expect("Type alias item should exist");
        assert!(!alias.eventual_read_vars.contains("T"));
        assert!(!alias.attribute_accesses.contains_key("T"));
    }

    #[test]
    fn function_local_bindings_do_not_create_module_dependencies() {
        let ast = parse_module(
            r"
def used(parameter):
    global module_state
    local_assignment = object()
    local_assignment.member()
    values = [comprehension_target for comprehension_target in source]
    try:
        raise ErrorType()
    except ErrorType as exception_target:
        print(exception_target)
    match subject:
        case {'value': pattern_capture}:
            print(pattern_capture)
    for loop_target in values:
        print(loop_target)

    def nested():
        nested_local = object()
        return nested_local, nested_global

    module_state += 1
    return parameter, local_assignment, nested()
",
        )
        .expect("Function-local binding fixture should parse")
        .into_syntax();
        let mut graph = ModuleDepGraph::new(ModuleId::ENTRY, "module".to_owned());
        GraphBuilder::new(&mut graph, 13)
            .build_from_ast(&ast)
            .expect("Dependency graph should build");

        let function = graph
            .items
            .values()
            .find(|item| item.var_decls.contains("used"))
            .expect("Function item should exist");

        for local_name in [
            "parameter",
            "local_assignment",
            "comprehension_target",
            "exception_target",
            "pattern_capture",
            "loop_target",
            "nested_local",
        ] {
            assert!(!function.eventual_read_vars.contains(local_name));
            assert!(!function.eventual_write_vars.contains(local_name));
        }
        assert!(!function.attribute_accesses.contains_key("local_assignment"));
        assert!(function.eventual_read_vars.contains("nested_global"));
        assert!(function.eventual_read_vars.contains("module_state"));
        let expected_writes: FxIndexSet<String> =
            std::iter::once("module_state".to_owned()).collect();
        assert_eq!(function.eventual_write_vars, expected_writes);
    }

    #[test]
    fn class_locals_resolve_only_inside_the_current_class_body() {
        let ast = parse_module(
            r"
def outer():
    class Container:
        class_local = object()
        alias = class_local

        def method(self):
            return method_global

    return Container
",
        )
        .expect("Class-local binding fixture should parse")
        .into_syntax();
        let mut graph = ModuleDepGraph::new(ModuleId::ENTRY, "module".to_owned());
        GraphBuilder::new(&mut graph, 13)
            .build_from_ast(&ast)
            .expect("Dependency graph should build");

        let function = graph
            .items
            .values()
            .find(|item| item.var_decls.contains("outer"))
            .expect("Outer function item should exist");

        assert!(!function.eventual_read_vars.contains("class_local"));
        assert!(function.eventual_read_vars.contains("method_global"));
    }

    #[test]
    fn same_named_nested_functions_have_distinct_scope_paths() {
        let ast = parse_module(
            r"
def first():
    def nested():
        import first_dependency
    return nested

def second():
    def nested():
        import second_dependency
    return nested
",
        )
        .expect("Nested-scope fixture should parse")
        .into_syntax();
        let mut graph = ModuleDepGraph::new(ModuleId::ENTRY, "module".to_owned());
        GraphBuilder::new(&mut graph, 13)
            .build_from_ast(&ast)
            .expect("Dependency graph should build");

        let nested_scopes: FxIndexSet<ScopePath> = graph
            .items
            .values()
            .filter_map(|item| match &item.item_type {
                ItemType::FunctionDef { name, scope } if name == "nested" => Some(scope.clone()),
                _ => None,
            })
            .collect();

        assert_eq!(nested_scopes.len(), 2);
    }
}
