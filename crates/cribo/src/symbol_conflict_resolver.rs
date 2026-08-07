//! Symbol conflict detection and resolution for Python bundling
//!
//! This module uses ruff's semantic analysis to detect when multiple modules
//! define symbols with the same name, then generates unique renames to resolve
//! conflicts. It maintains a global symbol registry and per-module semantic
//! information to ensure the bundled output has no name collisions.

use std::path::Path;

use ruff_python_ast::{
    ExceptHandler, Expr, ModModule, Pattern, PySourceType, PythonVersion, Stmt,
    visitor::{self, Visitor},
};
use ruff_python_semantic::{
    BindingFlags, BindingId, BindingKind, Module, ModuleKind, ModuleSource, SemanticModel,
};
use ruff_text_size::{Ranged, TextRange};

use crate::{
    import_alias_tracker::{EnhancedFromImport, ImportAliasTracker},
    python::module_path,
    resolver::ModuleId,
    types::{FxIndexMap, FxIndexSet},
};

/// Analyzes and resolves symbol conflicts across modules during bundling
///
/// This analyzer uses ruff's semantic analysis to:
/// - Detect when multiple modules define symbols with the same name
/// - Generate unique renames to resolve conflicts (e.g., `Logger` → `Logger_1`, `Logger_2`)
/// - Track exported symbols and module-scope bindings
/// - Manage import aliases for resolving symbol sources
#[derive(Debug)]
pub(crate) struct SymbolConflictResolver {
    /// Module-specific semantic models
    module_semantics: FxIndexMap<ModuleId, ModuleSemanticInfo>,
    /// Global symbol registry with full semantic information
    global_symbols: SymbolRegistry,
    /// Import alias tracker for resolving import aliases
    import_alias_tracker: ImportAliasTracker,
    /// Track which `ModuleIds` have already been analyzed to prevent duplicate processing
    analyzed_modules: FxIndexSet<ModuleId>,
}

/// Semantic model builder that properly populates bindings using visitor pattern
struct SemanticModelBuilder<'a> {
    semantic: SemanticModel<'a>,
    /// Tracks enhanced from-import information found during traversal
    from_imports: Vec<EnhancedFromImport>,
}

#[derive(Clone, Copy)]
enum AssignmentBindingKind {
    Assignment,
    Annotation,
    LoopVar,
    WithItemVar,
}

impl<'a> SemanticModelBuilder<'a> {
    /// Create and populate a semantic model for a module
    fn build_semantic_model(
        file_path: &'a Path,
        ast: &'a ModModule,
    ) -> (SemanticModel<'a>, Vec<EnhancedFromImport>) {
        // Step 1: We already have the parsed AST; no need to re-parse the source here.

        // Step 2: Determine module kind
        let kind = if file_path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(module_path::is_init_file_name)
        {
            ModuleKind::Package
        } else {
            ModuleKind::Module
        };

        // Step 3: Create module and semantic model
        let module = Module {
            kind,
            source: ModuleSource::File(file_path),
            python_ast: &ast.body,
            name: None,
        };

        let semantic = SemanticModel::new(
            &[],
            &[],
            PythonVersion::default(),
            PySourceType::Python,
            file_path,
            module,
        );

        // Step 4: Create builder and populate semantic model
        let mut builder = Self {
            semantic,
            from_imports: Vec::new(),
        };
        builder.traverse_and_bind(&ast.body);

        (builder.semantic, builder.from_imports)
    }

    /// Traverse AST and create bindings for module-level definitions
    fn traverse_and_bind(&mut self, statements: &'a [Stmt]) {
        log::trace!("Traversing {} statements", statements.len());

        for stmt in statements {
            self.visit_stmt(stmt);
        }
    }

    /// Visit a statement and create appropriate bindings
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::ClassDef(class_def) => {
                for decorator in &class_def.decorator_list {
                    self.visit_decorator(decorator);
                }
                if let Some(type_params) = &class_def.type_params {
                    self.visit_type_params(type_params);
                }
                if let Some(arguments) = &class_def.arguments {
                    self.visit_arguments(arguments);
                }
                log::trace!("Processing class definition: {}", class_def.name.id);
                self.add_binding(
                    class_def.name.id.as_str(),
                    class_def.name.range,
                    BindingKind::ClassDefinition(self.semantic.scope_id),
                    BindingFlags::empty(),
                );
            }
            Stmt::FunctionDef(func_def) => {
                for decorator in &func_def.decorator_list {
                    self.visit_decorator(decorator);
                }
                if let Some(type_params) = &func_def.type_params {
                    self.visit_type_params(type_params);
                }
                self.visit_parameters(&func_def.parameters);
                if let Some(returns) = &func_def.returns {
                    self.visit_annotation(returns);
                }
                log::trace!("Processing function definition: {}", func_def.name.id);
                self.add_binding(
                    func_def.name.id.as_str(),
                    func_def.name.range,
                    BindingKind::FunctionDefinition(self.semantic.scope_id),
                    BindingFlags::empty(),
                );
            }
            Stmt::Assign(assign) => {
                self.visit_expr(&assign.value);
                for target in &assign.targets {
                    self.visit_expr(target);
                    self.bind_assignment_target(target, AssignmentBindingKind::Assignment);
                }
            }
            Stmt::AnnAssign(assign) => {
                if let Some(value) = &assign.value {
                    self.visit_expr(value);
                }
                self.visit_annotation(&assign.annotation);
                self.visit_expr(&assign.target);
                let kind = if assign.value.is_some() {
                    AssignmentBindingKind::Assignment
                } else {
                    AssignmentBindingKind::Annotation
                };
                self.bind_assignment_target(&assign.target, kind);
            }
            Stmt::AugAssign(assign) => {
                self.visit_expr(&assign.value);
                self.visit_expr(&assign.target);
                self.bind_assignment_target(&assign.target, AssignmentBindingKind::Assignment);
            }
            Stmt::TypeAlias(type_alias) => {
                if let Some(type_params) = &type_alias.type_params {
                    self.visit_type_params(type_params);
                }
                self.visit_expr(&type_alias.value);
                self.bind_assignment_target(&type_alias.name, AssignmentBindingKind::Assignment);
            }
            // Handle imports to enable qualified name resolution
            Stmt::Import(import) => {
                for alias in &import.names {
                    let module = alias
                        .name
                        .as_str()
                        .split('.')
                        .next()
                        .expect("module name should have at least one part");
                    self.semantic.add_module(module);

                    let name = alias
                        .asname
                        .as_ref()
                        .map_or(module, ruff_python_ast::Identifier::as_str);
                    self.add_binding(
                        name,
                        alias.range,
                        BindingKind::Import(ruff_python_semantic::Import {
                            qualified_name: Box::new(
                                ruff_python_ast::name::QualifiedName::user_defined(
                                    alias.name.as_str(),
                                ),
                            ),
                        }),
                        BindingFlags::EXTERNAL,
                    );
                }
            }
            Stmt::ImportFrom(import_from) => {
                // Get the module name
                let module_name = import_from.module.as_ref().map(ToString::to_string);

                for alias in &import_from.names {
                    let original_name = alias.name.as_str();
                    let local_name = alias
                        .asname
                        .as_ref()
                        .map_or(original_name, ruff_python_ast::Identifier::as_str);

                    if local_name != "*" {
                        // Track the enhanced import information
                        if let Some(ref module) = module_name {
                            let has_alias = alias.asname.is_some();
                            self.from_imports.push(EnhancedFromImport {
                                module: module.clone(),
                                original_name: original_name.to_owned(),
                                local_alias: if has_alias {
                                    Some(local_name.to_owned())
                                } else {
                                    None
                                },
                            });
                        }

                        self.add_binding(
                            local_name,
                            alias.range,
                            BindingKind::FromImport(ruff_python_semantic::FromImport {
                                qualified_name: Box::new(
                                    ruff_python_ast::name::QualifiedName::user_defined(
                                        original_name,
                                    ),
                                ),
                            }),
                            BindingFlags::EXTERNAL,
                        );
                    }
                }
            }
            Stmt::If(if_stmt) => {
                self.visit_expr(&if_stmt.test);
                self.traverse_and_bind(&if_stmt.body);
                for clause in &if_stmt.elif_else_clauses {
                    if let Some(condition) = &clause.test {
                        self.visit_expr(condition);
                    }
                    self.traverse_and_bind(&clause.body);
                }
            }
            Stmt::Try(try_stmt) => {
                self.traverse_and_bind(&try_stmt.body);
                for handler in &try_stmt.handlers {
                    let ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(type_) = &handler.type_ {
                        self.visit_expr(type_);
                    }
                    if let Some(name) = &handler.name {
                        self.add_binding(
                            name.as_str(),
                            name.range,
                            BindingKind::BoundException,
                            BindingFlags::empty(),
                        );
                    }
                    self.traverse_and_bind(&handler.body);
                }
                self.traverse_and_bind(&try_stmt.orelse);
                self.traverse_and_bind(&try_stmt.finalbody);
            }
            Stmt::With(with_stmt) => {
                for item in &with_stmt.items {
                    self.visit_expr(&item.context_expr);
                    if let Some(target) = &item.optional_vars {
                        self.visit_expr(target);
                        self.bind_assignment_target(target, AssignmentBindingKind::WithItemVar);
                    }
                }
                self.traverse_and_bind(&with_stmt.body);
            }
            Stmt::For(for_stmt) => {
                self.visit_expr(&for_stmt.iter);
                self.visit_expr(&for_stmt.target);
                self.bind_assignment_target(&for_stmt.target, AssignmentBindingKind::LoopVar);
                self.traverse_and_bind(&for_stmt.body);
                self.traverse_and_bind(&for_stmt.orelse);
            }
            Stmt::While(while_stmt) => {
                self.visit_expr(&while_stmt.test);
                self.traverse_and_bind(&while_stmt.body);
                self.traverse_and_bind(&while_stmt.orelse);
            }
            Stmt::Match(match_stmt) => {
                self.visit_expr(&match_stmt.subject);
                for case in &match_stmt.cases {
                    self.bind_pattern(&case.pattern);
                    if let Some(guard) = &case.guard {
                        self.visit_expr(guard);
                    }
                    self.traverse_and_bind(&case.body);
                }
            }
            Stmt::Return(return_stmt) => {
                if let Some(value) = &return_stmt.value {
                    self.visit_expr(value);
                }
            }
            Stmt::Delete(delete) => {
                for target in &delete.targets {
                    self.visit_expr(target);
                }
            }
            Stmt::Raise(raise_stmt) => {
                if let Some(exc) = &raise_stmt.exc {
                    self.visit_expr(exc);
                }
                if let Some(cause) = &raise_stmt.cause {
                    self.visit_expr(cause);
                }
            }
            Stmt::Assert(assert_stmt) => {
                self.visit_expr(&assert_stmt.test);
                if let Some(message) = &assert_stmt.msg {
                    self.visit_expr(message);
                }
            }
            Stmt::Expr(expr_stmt) => self.visit_expr(&expr_stmt.value),
            Stmt::Global(_)
            | Stmt::Nonlocal(_)
            | Stmt::Pass(_)
            | Stmt::Break(_)
            | Stmt::Continue(_)
            | Stmt::IpyEscapeCommand(_) => {}
        }
    }

    /// Bind every name introduced by an assignment target without descending into attributes or
    /// subscripts, which mutate existing objects rather than defining module symbols.
    fn bind_assignment_target(&mut self, target: &'a Expr, kind: AssignmentBindingKind) {
        match target {
            Expr::Name(name) => {
                let binding_kind = match kind {
                    AssignmentBindingKind::Assignment => BindingKind::Assignment,
                    AssignmentBindingKind::Annotation => BindingKind::Annotation,
                    AssignmentBindingKind::LoopVar => BindingKind::LoopVar,
                    AssignmentBindingKind::WithItemVar => BindingKind::WithItemVar,
                };
                self.add_binding(
                    name.id.as_str(),
                    name.range(),
                    binding_kind,
                    BindingFlags::empty(),
                );
            }
            Expr::Tuple(tuple) => {
                for element in &tuple.elts {
                    self.bind_assignment_target(element, kind);
                }
            }
            Expr::List(list) => {
                for element in &list.elts {
                    self.bind_assignment_target(element, kind);
                }
            }
            Expr::Starred(starred) => {
                self.bind_assignment_target(&starred.value, kind);
            }
            _ => {}
        }
    }

    /// Bind names captured by a structural pattern at module scope.
    fn bind_pattern(&mut self, pattern: &'a Pattern) {
        match pattern {
            Pattern::MatchSequence(sequence) => {
                for pattern in &sequence.patterns {
                    self.bind_pattern(pattern);
                }
            }
            Pattern::MatchMapping(mapping) => {
                for pattern in &mapping.patterns {
                    self.bind_pattern(pattern);
                }
                if let Some(name) = &mapping.rest {
                    self.add_binding(
                        name.as_str(),
                        name.range,
                        BindingKind::Assignment,
                        BindingFlags::empty(),
                    );
                }
            }
            Pattern::MatchClass(class) => {
                for pattern in &class.arguments.patterns {
                    self.bind_pattern(pattern);
                }
                for keyword in &class.arguments.keywords {
                    self.bind_pattern(&keyword.pattern);
                }
            }
            Pattern::MatchStar(star) => {
                if let Some(name) = &star.name {
                    self.add_binding(
                        name.as_str(),
                        name.range,
                        BindingKind::Assignment,
                        BindingFlags::empty(),
                    );
                }
            }
            Pattern::MatchAs(as_pattern) => {
                if let Some(pattern) = &as_pattern.pattern {
                    self.bind_pattern(pattern);
                }
                if let Some(name) = &as_pattern.name {
                    self.add_binding(
                        name.as_str(),
                        name.range,
                        BindingKind::Assignment,
                        BindingFlags::empty(),
                    );
                }
            }
            Pattern::MatchOr(or_pattern) => {
                for pattern in &or_pattern.patterns {
                    self.bind_pattern(pattern);
                }
            }
            Pattern::MatchValue(_) | Pattern::MatchSingleton(_) => {}
        }
    }

    /// Add a binding to the semantic model
    fn add_binding(
        &mut self,
        name: &'a str,
        range: TextRange,
        kind: BindingKind<'a>,
        flags: BindingFlags,
    ) -> BindingId {
        // Mark private declarations
        let mut binding_flags = flags;
        if name.starts_with('_') && !name.starts_with("__") {
            binding_flags |= BindingFlags::PRIVATE_DECLARATION;
        }

        // Create binding and add to current scope
        let binding_id = self.semantic.push_binding(name, range, kind, binding_flags);
        let scope = self.semantic.current_scope_mut();
        scope.add(name, binding_id);

        log::trace!("Added binding '{name}' with ID {binding_id:?}");
        binding_id
    }

    /// Extract symbols from a populated semantic model
    fn extract_symbols_from_semantic_model(semantic: &SemanticModel<'_>) -> FxIndexSet<String> {
        let mut symbols = FxIndexSet::default();

        // Get the global scope (module scope)
        let global_scope = semantic.global_scope();

        log::trace!(
            "Extracting from global scope with {} bindings",
            global_scope.bindings().count()
        );

        // Iterate through all bindings in global scope
        for (name, binding_id) in global_scope.bindings() {
            let binding = &semantic.bindings[binding_id];

            // Only include symbols that are actual definitions (not imports) and not builtins
            // and are not private (unless they are dunder methods)
            log::trace!("Processing binding '{}' with kind {:?}", name, binding.kind);
            match &binding.kind {
                BindingKind::ClassDefinition(_) => {
                    if !name.starts_with('_') || name.starts_with("__") {
                        log::trace!("Adding class symbol: {name}");
                        symbols.insert(name.to_owned());
                    }
                }
                BindingKind::FunctionDefinition(_) => {
                    if !name.starts_with('_') || name.starts_with("__") {
                        log::trace!("Adding function symbol: {name}");
                        symbols.insert(name.to_owned());
                    }
                }
                BindingKind::Assignment | BindingKind::LoopVar | BindingKind::WithItemVar => {
                    // Include module-level assignments (variables)
                    if !name.starts_with('_') {
                        log::trace!("Adding assignment symbol: {name}");
                        symbols.insert(name.to_owned());
                    }
                }
                BindingKind::FromImport(_) => {
                    // Include FromImport symbols as exports
                    // This is important for __init__.py files that re-export symbols
                    if !name.starts_with('_') || name.starts_with("__") {
                        log::trace!("Adding from-import symbol: {name}");
                        symbols.insert(name.to_owned());
                    }
                }
                // Skip regular imports and builtins
                BindingKind::Builtin | BindingKind::Import(_) => {
                    log::trace!("Skipping import/builtin binding: {name}");
                }
                _ => {
                    log::trace!(
                        "Skipping other binding '{}' of kind {:?}",
                        name,
                        binding.kind
                    );
                }
            }
        }

        log::trace!("Final extracted symbols: {symbols:?}");
        symbols
    }

    /// Extract ALL module-scope symbols that need to be exposed in the module namespace
    /// This includes symbols defined in conditional blocks (if/else, try/except) and imports
    fn extract_all_module_scope_symbols(semantic: &SemanticModel<'_>) -> FxIndexSet<String> {
        let mut symbols = FxIndexSet::default();

        // Get the global scope (module scope)
        let global_scope = semantic.global_scope();

        log::trace!(
            "Extracting ALL module-scope symbols from global scope with {} bindings",
            global_scope.bindings().count()
        );

        // Iterate through all bindings in global scope
        for (name, binding_id) in global_scope.bindings() {
            let binding = &semantic.bindings[binding_id];

            // Include ALL symbols except builtins
            if matches!(&binding.kind, BindingKind::Builtin) {
                log::trace!("Skipping builtin binding: {name}");
            } else {
                // Include all non-builtin symbols: classes, functions, assignments, imports
                log::trace!(
                    "Adding module-scope symbol '{}' of kind {:?}",
                    name,
                    binding.kind
                );
                symbols.insert(name.to_owned());
            }
        }

        log::trace!("All module-scope symbols: {symbols:?}");
        symbols
    }
}

impl<'a> Visitor<'a> for SemanticModelBuilder<'a> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Named(named) => {
                self.visit_expr(&named.value);
                self.bind_assignment_target(&named.target, AssignmentBindingKind::Assignment);
            }
            Expr::Lambda(lambda) => {
                if let Some(parameters) = &lambda.parameters {
                    self.visit_parameters(parameters);
                }
            }
            _ => visitor::walk_expr(self, expr),
        }
    }
}

/// Module semantic analyzer that provides static methods for symbol extraction
/// Semantic information for a single module
#[derive(Debug)]
pub(crate) struct ModuleSemanticInfo {
    /// Symbols exported by this module (from semantic analysis)
    pub exported_symbols: FxIndexSet<String>,
    /// Symbol conflicts detected in this module
    pub conflicts: Vec<String>,
    /// All module-scope symbols that need to be exposed in the module namespace
    /// This includes symbols defined in conditional blocks (if/else, try/except)
    pub module_scope_symbols: FxIndexSet<String>,
}

/// Global symbol registry across all modules with semantic information
#[derive(Debug)]
pub(crate) struct SymbolRegistry {
    /// Symbol name -> list of modules that define it
    pub symbols: FxIndexMap<String, Vec<ModuleId>>,
    /// Renames: (`ModuleId`, `OriginalName`) -> `NewName`
    pub renames: FxIndexMap<(ModuleId, String), String>,
}

impl Default for SymbolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolRegistry {
    /// Create a new symbol registry
    pub(crate) fn new() -> Self {
        Self {
            symbols: FxIndexMap::default(),
            renames: FxIndexMap::default(),
        }
    }

    /// Register a symbol from a module (legacy interface)
    pub(crate) fn register_symbol(&mut self, symbol: String, module_id: ModuleId) {
        self.symbols.entry(symbol).or_default().push(module_id);
    }

    /// Detect conflicts across all modules
    pub(crate) fn detect_conflicts(&self) -> Vec<SymbolConflict> {
        let mut conflicts = Vec::new();

        for (symbol, modules) in &self.symbols {
            if modules.len() > 1 {
                conflicts.push(SymbolConflict {
                    symbol: symbol.clone(),
                    modules: modules.clone(),
                });
            }
        }

        conflicts
    }

    /// Generate rename for conflicting symbol
    pub(crate) fn generate_rename(
        &mut self,
        module_id: ModuleId,
        original: &str,
        suffix: usize,
    ) -> String {
        let new_name = format!("{original}_{suffix}");
        self.renames
            .insert((module_id, original.to_owned()), new_name.clone());
        new_name
    }

    /// Get rename for a symbol if it exists
    pub(crate) fn get_rename(&self, module_id: ModuleId, original: &str) -> Option<&str> {
        self.renames
            .get(&(module_id, original.to_owned()))
            .map(String::as_str)
    }
}

/// Represents a symbol conflict across modules
pub(crate) struct SymbolConflict {
    pub symbol: String,
    pub modules: Vec<ModuleId>,
}

/// Information about module-level global usage
#[derive(Debug, Clone, Default)]
pub(crate) struct ModuleGlobalInfo {
    /// Variables that exist at module level
    pub module_level_vars: FxIndexSet<String>,

    /// Variables that are eligible for lifting when referenced via `global` statements.
    /// Includes module-level assignments/classes plus names introduced by imports and
    /// function definitions at module scope.
    pub liftable_vars: FxIndexSet<String>,

    /// Variables declared with 'global' keyword in functions
    pub global_declarations: FxIndexMap<String, Vec<TextRange>>,

    /// Functions that use global statements
    pub functions_using_globals: FxIndexSet<String>,

    /// Module name for generating unique prefixes
    pub module_name: String,
}

impl Default for SymbolConflictResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolConflictResolver {
    /// Create a new symbol conflict resolver
    pub(crate) fn new() -> Self {
        Self {
            module_semantics: FxIndexMap::default(),
            global_symbols: SymbolRegistry::new(),
            import_alias_tracker: ImportAliasTracker::new(),
            analyzed_modules: FxIndexSet::default(),
        }
    }

    /// Analyze a module using full semantic model approach
    pub(crate) fn analyze_module(&mut self, module_id: ModuleId, ast: &ModModule, path: &Path) {
        // Check if this ModuleId has already been analyzed
        // This prevents duplicate processing when multiple module names map to the same file
        if !self.analyzed_modules.insert(module_id) {
            log::debug!(
                "Module {} already analyzed, skipping duplicate processing",
                module_id.as_u32()
            );
            return;
        }

        log::debug!(
            "Starting semantic analysis for module {}",
            module_id.as_u32()
        );

        // Build semantic model and extract information
        let (semantic_model, from_imports) = SemanticModelBuilder::build_semantic_model(path, ast);

        // Extract exported symbols (public API)
        let exported_symbols =
            SemanticModelBuilder::extract_symbols_from_semantic_model(&semantic_model);
        log::debug!(
            "Module {} has exported symbols: {:?}",
            module_id.as_u32(),
            exported_symbols
        );

        // Extract ALL module-scope symbols (including conditional imports)
        let module_scope_symbols =
            SemanticModelBuilder::extract_all_module_scope_symbols(&semantic_model);
        log::debug!(
            "Module {} has all module-scope symbols: {:?}",
            module_id.as_u32(),
            module_scope_symbols
        );

        // Register from imports in the alias tracker
        for import in from_imports {
            self.import_alias_tracker.register_from_import(
                module_id,
                import.module,
                import.original_name,
                import.local_alias,
            );
        }

        // Register symbols in global registry, but only those that are defined locally
        // Skip FromImport symbols to avoid incorrect conflict resolution

        // Build a lookup map for O(1) access to binding information
        let binding_lookup: FxIndexMap<&str, BindingId> =
            semantic_model.global_scope().bindings().collect();

        for symbol in &exported_symbols {
            // Check if this symbol is a FromImport by looking at the semantic model
            let is_from_import = binding_lookup.get(symbol.as_str()).is_some_and(|&id| {
                matches!(semantic_model.bindings[id].kind, BindingKind::FromImport(_))
            });

            if is_from_import {
                log::debug!(
                    "Skipping registration of FromImport symbol '{}' from module {} for conflict \
                     resolution",
                    symbol,
                    module_id.as_u32()
                );
            } else {
                self.global_symbols
                    .register_symbol(symbol.clone(), module_id);
            }
        }

        // Store module semantic info
        self.module_semantics.insert(
            module_id,
            ModuleSemanticInfo {
                exported_symbols,
                conflicts: Vec::new(), // Will be populated later
                module_scope_symbols,
            },
        );
    }

    /// Detect and resolve symbol conflicts across all modules
    pub(crate) fn detect_and_resolve_conflicts(&mut self) -> Vec<SymbolConflict> {
        let conflicts = self.global_symbols.detect_conflicts();

        // Generate renames for conflicting symbols
        for conflict in &conflicts {
            for (i, module_id) in conflict.modules.iter().enumerate() {
                // Generate renames for all modules in conflict (including first)
                // Generate and register a rename (return value not needed)
                self.global_symbols.generate_rename(
                    *module_id,
                    &conflict.symbol,
                    i + 1, // Start numbering from 1 instead of 0
                );

                // Update conflicts in module info
                if let Some(module_info) = self.module_semantics.get_mut(module_id) {
                    module_info.conflicts.push(conflict.symbol.clone());
                }
            }
        }

        conflicts
    }

    /// Get module semantic info
    pub(crate) fn get_module_info(&self, module_id: ModuleId) -> Option<&ModuleSemanticInfo> {
        self.module_semantics.get(&module_id)
    }

    /// Get symbol registry
    pub(crate) const fn symbol_registry(&self) -> &SymbolRegistry {
        &self.global_symbols
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use ruff_python_parser::parse_module;

    use super::{SemanticModelBuilder, SymbolConflictResolver};
    use crate::resolver::ModuleId;

    /// Verifies that all supported module-level binding forms enter the semantic model.
    #[test]
    fn semantic_model_collects_all_module_scope_bindings() {
        let parsed = parse_module(
            r#"
import package.submodule

if (if_named := True):
    conditional_value = 1

    def conditional_function():
        function_local = 1

    class ConditionalClass:
        class_local = 1

try:
    try_value = 1
except Exception as error:
    except_value = 1
else:
    else_value = 1
finally:
    finally_value = 1

with (context_named := manager()) as (with_value, *with_rest):
    with_body_value = 1

with manager() as with_slots[(with_target_named := 0)]:
    pass

for loop_value, *loop_rest in (iter_named := ()):
    loop_body_value = 1
else:
    loop_else_value = 1

for loop_slots[(for_target_named := 0)] in (1,):
    pass

while (while_named := False):
    while_value = 1
else:
    while_else_value = 1

annotated: int = 1
annotation_only: int
augmented += 1
unpacked_left, *unpacked_middle, unpacked_right = ()
type TypeAlias = int

match (subject_named := {"value": 1}):
    case {"value": captured, **captured_rest} if (guard_named := True):
        match_body_value = 1

@(decorator_named := decorator)
def header_bindings(default=(default_named := 1)):
    return default

comprehension = [item for item in (1,) if (comprehension_named := item)]
lambda_value = lambda: (lambda_local := 1)
"#,
        )
        .expect("test source should parse");
        let module = parsed.into_syntax();
        let (semantic, _) =
            SemanticModelBuilder::build_semantic_model(Path::new("module.py"), &module);

        let exported = SemanticModelBuilder::extract_symbols_from_semantic_model(&semantic);
        for expected in [
            "conditional_value",
            "if_named",
            "conditional_function",
            "ConditionalClass",
            "try_value",
            "except_value",
            "else_value",
            "finally_value",
            "with_value",
            "with_rest",
            "with_body_value",
            "context_named",
            "with_target_named",
            "loop_value",
            "loop_rest",
            "loop_body_value",
            "loop_else_value",
            "iter_named",
            "for_target_named",
            "while_value",
            "while_else_value",
            "while_named",
            "annotated",
            "augmented",
            "unpacked_left",
            "unpacked_middle",
            "unpacked_right",
            "TypeAlias",
            "captured",
            "captured_rest",
            "match_body_value",
            "subject_named",
            "guard_named",
            "header_bindings",
            "decorator_named",
            "default_named",
            "comprehension",
            "comprehension_named",
            "lambda_value",
        ] {
            assert!(exported.contains(expected), "missing export {expected}");
        }
        assert!(!exported.contains("annotation_only"));
        assert!(!exported.contains("function_local"));
        assert!(!exported.contains("class_local"));
        assert!(!exported.contains("lambda_local"));
        assert!(!exported.contains("package"));

        let module_scope = SemanticModelBuilder::extract_all_module_scope_symbols(&semantic);
        assert!(module_scope.contains("annotation_only"));
        assert!(module_scope.contains("error"));
        assert!(module_scope.contains("package"));
        assert!(!module_scope.contains("function_local"));
        assert!(!module_scope.contains("class_local"));
    }

    /// Verifies that conditional bindings are included in cross-module conflict resolution.
    #[test]
    fn conditional_definitions_participate_in_conflict_detection() {
        let first = parse_module(
            r#"
if True:
    shared_value = "first"

    def shared_function():
        return shared_value
"#,
        )
        .expect("first module should parse")
        .into_syntax();
        let second = parse_module(
            r#"
if True:
    shared_value = "second"

    def shared_function():
        return shared_value
"#,
        )
        .expect("second module should parse")
        .into_syntax();

        let mut resolver = SymbolConflictResolver::new();
        resolver.analyze_module(ModuleId::new(1), &first, Path::new("first.py"));
        resolver.analyze_module(ModuleId::new(2), &second, Path::new("second.py"));

        let conflicts = resolver.detect_and_resolve_conflicts();
        for expected in ["shared_value", "shared_function"] {
            let conflict = conflicts
                .iter()
                .find(|conflict| conflict.symbol == expected)
                .unwrap_or_else(|| panic!("missing conflict for {expected}"));
            assert_eq!(conflict.modules, [ModuleId::new(1), ModuleId::new(2)]);
        }
    }
}
