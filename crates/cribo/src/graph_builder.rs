/// Graph builder that creates `DependencyGraph` from Python AST
/// This module bridges the gap between ruff's AST and our dependency graph
use anyhow::Result;
use ruff_python_ast::{
    self as ast, Expr, ExprContext, ModModule, Stmt,
    visitor::{self, Visitor},
};

use crate::{
    dependency_graph::{ItemData, ItemType, ModuleDepGraph},
    types::{FxIndexMap, FxIndexSet},
    visitors::{ExpressionSideEffectDetector, utils::extract_string_list_from_expr},
};

/// Collects runtime dependencies while delegating complete AST traversal to Ruff.
struct DependencyCollector<'a> {
    read_vars: &'a mut FxIndexSet<String>,
    write_vars: Option<&'a mut FxIndexSet<String>>,
    attribute_accesses: &'a mut FxIndexMap<String, FxIndexSet<String>>,
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
        }
    }

    const fn body(
        read_vars: &'a mut FxIndexSet<String>,
        write_vars: &'a mut FxIndexSet<String>,
        attribute_accesses: &'a mut FxIndexMap<String, FxIndexSet<String>>,
    ) -> Self {
        Self {
            read_vars,
            write_vars: Some(write_vars),
            attribute_accesses,
        }
    }

    fn record_read(&mut self, name: &str) {
        self.read_vars.insert(name.to_owned());
    }

    fn record_write(&mut self, name: &str) {
        if let Some(write_vars) = &mut self.write_vars {
            write_vars.insert(name.to_owned());
        }
    }

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

    fn track_attribute_access(&mut self, attribute: &ast::ExprAttribute) {
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

        if let Some(full_name) = Self::extract_dotted_name(attribute) {
            self.read_vars.insert(full_name.clone());
            if full_name.contains('.') {
                let root = full_name
                    .split('.')
                    .next()
                    .expect("full_name should have at least one part");
                self.read_vars.insert(root.to_owned());
            }
        }
    }

    fn build_full_dotted_name(expr: &Expr) -> Option<String> {
        match expr {
            Expr::Name(name) => Some(name.id.to_string()),
            Expr::Attribute(attribute) => Self::build_full_dotted_name(&attribute.value)
                .map(|base| format!("{}.{}", base, attribute.attr)),
            _ => None,
        }
    }

    fn extract_dotted_name(attribute: &ast::ExprAttribute) -> Option<String> {
        fn build_dotted_name(expr: &Expr, parts: &mut Vec<String>) -> bool {
            match expr {
                Expr::Name(name) => {
                    parts.push(name.id.to_string());
                    true
                }
                Expr::Attribute(attribute) if build_dotted_name(&attribute.value, parts) => {
                    parts.push(attribute.attr.to_string());
                    true
                }
                _ => false,
            }
        }

        let mut parts = Vec::new();
        if build_dotted_name(&attribute.value, &mut parts) {
            parts.reverse();
            Some(parts.join("."))
        } else {
            None
        }
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
                self.visit_annotation(&ann_assign.annotation);
                self.visit_assignment_target(&ann_assign.target);
            }
            Stmt::For(for_stmt) => {
                self.visit_expr(&for_stmt.iter);
                self.visit_assignment_target(&for_stmt.target);
                self.visit_body(&for_stmt.body);
                self.visit_body(&for_stmt.orelse);
            }
            Stmt::Import(import_stmt) => {
                for alias in &import_stmt.names {
                    let local_name = alias
                        .asname
                        .as_ref()
                        .map_or(alias.name.as_str(), ruff_python_ast::Identifier::as_str);
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
            _ => visitor::walk_expr(self, expr),
        }
    }
}

/// Builds a `ModuleDepGraph` from a Python AST
pub(crate) struct GraphBuilder<'a> {
    graph: &'a mut ModuleDepGraph,
    current_scope: ScopeType,
    /// Track import aliases for importlib detection
    /// Maps local name -> module path (e.g., "il" -> "importlib", "im" ->
    /// "`importlib.import_module`")
    import_aliases: FxIndexMap<String, String>,
    python_version: u8,
    /// When inside a function or class, track the scope name
    scope_name: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum ScopeType {
    Module,
    Function,
    Class,
}

impl<'a> GraphBuilder<'a> {
    pub(crate) fn new(graph: &'a mut ModuleDepGraph, python_version: u8) -> Self {
        Self {
            graph,
            current_scope: ScopeType::Module,
            import_aliases: FxIndexMap::default(),
            python_version,
            scope_name: None,
        }
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
        if matches!(self.current_scope, ScopeType::Function) {
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
                containing_scope: self.scope_name.clone(),
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
            containing_scope: self.scope_name.clone(),
        };

        self.graph.add_item(item_data);
    }

    /// Process a function definition
    fn process_function_def(&mut self, func_def: &ast::StmtFunctionDef) -> Result<()> {
        let func_name = func_def.name.to_string();

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
        self.collect_vars_in_body(
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
            },
            var_decls: std::iter::once(func_name.clone()).collect(),
            read_vars,
            eventual_read_vars,
            write_vars: FxIndexSet::default(),
            eventual_write_vars,
            has_side_effects: false,
            imported_names: FxIndexSet::default(),
            reexported_names: FxIndexSet::default(),
            defined_symbols: std::iter::once(func_name.clone()).collect(),
            symbol_dependencies,
            attribute_accesses: eventual_attribute_accesses,
            containing_scope: self.scope_name.clone(),
        };

        self.graph.add_item(item_data);

        // Process the function body in function scope
        let old_scope = self.current_scope;
        let old_scope_name = self.scope_name.clone();
        self.current_scope = ScopeType::Function;
        self.scope_name = Some(func_name);
        for stmt in &func_def.body {
            self.process_statement(stmt)?;
        }
        self.current_scope = old_scope;
        self.scope_name = old_scope_name;

        Ok(())
    }

    /// Process a class definition
    fn process_class_def(&mut self, class_def: &ast::StmtClassDef) -> Result<()> {
        let class_name = class_def.name.to_string();

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
                    self.collect_vars_in_body(
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
            },
            var_decls: std::iter::once(class_name.clone()).collect(),
            read_vars,
            eventual_read_vars: method_read_vars, // Methods may use these variables
            write_vars: FxIndexSet::default(),
            eventual_write_vars: FxIndexSet::default(),
            has_side_effects: false,
            imported_names: FxIndexSet::default(),
            reexported_names: FxIndexSet::default(),
            defined_symbols: std::iter::once(class_name.clone()).collect(),
            symbol_dependencies,
            attribute_accesses: method_attribute_accesses,
            containing_scope: self.scope_name.clone(),
        };

        self.graph.add_item(item_data);

        // Process the class body in class scope
        let old_scope = self.current_scope;
        let old_scope_name = self.scope_name.clone();
        self.current_scope = ScopeType::Class;
        self.scope_name = Some(class_name);
        for stmt in &class_def.body {
            self.process_statement(stmt)?;
        }
        self.current_scope = old_scope;
        self.scope_name = old_scope_name;

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
                    containing_scope: self.scope_name.clone(),
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
                containing_scope: self.scope_name.clone(),
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
            containing_scope: self.scope_name.clone(),
        };

        self.graph.add_item(item_data);
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
            containing_scope: self.scope_name.clone(),
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
            containing_scope: self.scope_name.clone(),
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
            containing_scope: self.scope_name.clone(),
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
            containing_scope: self.scope_name.clone(),
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
            containing_scope: self.scope_name.clone(),
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
    fn collect_vars_in_body(
        &self,
        body: &[Stmt],
        read_vars: &mut FxIndexSet<String>,
        write_vars: &mut FxIndexSet<String>,
        attribute_accesses: &mut FxIndexMap<String, FxIndexSet<String>>,
    ) {
        DependencyCollector::body(read_vars, write_vars, attribute_accesses).visit_body(body);
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

    /// Check if an expression is an `importlib.import_module()` call with a static string argument
    fn is_static_importlib_call(&self, expr: &Expr) -> Option<String> {
        if let Expr::Call(call) = expr {
            // Check if this is importlib.import_module() or an alias
            let is_import_module = match &*call.func {
                // Direct call: importlib.import_module() or alias.import_module()
                Expr::Attribute(attr) if attr.attr.as_str() == "import_module" => {
                    if let Expr::Name(name) = &*attr.value {
                        let name_str = name.id.as_str();
                        // Check if it's importlib directly or an alias
                        name_str == "importlib"
                            || self
                                .import_aliases
                                .get(name_str)
                                .is_some_and(|v| v == "importlib")
                    } else {
                        false
                    }
                }
                // Direct function call: import_module() or im()
                Expr::Name(name) => {
                    let name_str = name.id.as_str();
                    // Check if this is import_module or an alias for it
                    name_str == "import_module"
                        || self
                            .import_aliases
                            .get(name_str)
                            .is_some_and(|v| v == "importlib.import_module")
                }
                _ => false,
            };

            if is_import_module {
                // Extract the module name if it's a static string
                if let Some(arg) = call.arguments.args.first()
                    && let Expr::StringLiteral(string_lit) = arg
                {
                    return Some(string_lit.value.to_string());
                }
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
