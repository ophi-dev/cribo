use ruff_python_ast::{
    ExceptHandler, StmtAnnAssign, StmtAssert, StmtAugAssign, StmtClassDef, StmtExpr, StmtFor,
    StmtIf, StmtMatch, StmtRaise, StmtReturn, StmtTry, StmtWhile, StmtWith,
};

use crate::code_generator::import_transformer::RecursiveImportTransformer;

pub(crate) struct StatementsHandler;

impl StatementsHandler {
    pub(in crate::code_generator::import_transformer) fn handle_ann_assign(
        t: &mut RecursiveImportTransformer<'_>,
        s: &mut StmtAnnAssign,
    ) {
        // Transform the annotation
        t.transform_expr(&mut s.annotation);

        // Transform the target
        t.transform_expr(&mut s.target);

        // Transform the value if present
        if let Some(value) = &mut s.value {
            t.transform_expr(value);
        }
    }

    pub(in crate::code_generator::import_transformer) fn handle_aug_assign(
        t: &mut RecursiveImportTransformer<'_>,
        s: &mut StmtAugAssign,
    ) {
        t.transform_expr(&mut s.target);
        t.transform_expr(&mut s.value);
    }

    pub(in crate::code_generator::import_transformer) fn handle_expr_stmt(
        t: &mut RecursiveImportTransformer<'_>,
        s: &mut StmtExpr,
    ) {
        t.transform_expr(&mut s.value);
    }

    pub(in crate::code_generator::import_transformer) fn handle_return(
        t: &mut RecursiveImportTransformer<'_>,
        s: &mut StmtReturn,
    ) {
        if let Some(value) = &mut s.value {
            t.transform_expr(value);
        }
    }

    pub(in crate::code_generator::import_transformer) fn handle_raise(
        t: &mut RecursiveImportTransformer<'_>,
        s: &mut StmtRaise,
    ) {
        if let Some(exc) = &mut s.exc {
            t.transform_expr(exc);
        }
        if let Some(cause) = &mut s.cause {
            t.transform_expr(cause);
        }
    }

    pub(in crate::code_generator::import_transformer) fn handle_assert(
        t: &mut RecursiveImportTransformer<'_>,
        s: &mut StmtAssert,
    ) {
        t.transform_expr(&mut s.test);
        if let Some(msg) = &mut s.msg {
            t.transform_expr(msg);
        }
    }

    pub(in crate::code_generator::import_transformer) fn handle_try(
        t: &mut RecursiveImportTransformer<'_>,
        s: &mut StmtTry,
    ) {
        // Alias additions survive the try only when BOTH the body path and
        // every handler path establish them (an exception may jump to a
        // handler before the body's import ran); uncaught exceptions make
        // later statements unreachable, so body+handlers cover all paths
        let pre_aliases = t.state.import_aliases.clone();
        t.transform_statements(&mut s.body);

        // Ensure try body is not empty
        if s.body.is_empty() {
            log::debug!("Adding pass statement to empty try body in import transformer");
            s.body.push(crate::ast_builder::statements::pass());
        }

        // The else suite runs only after the body succeeded: same branch
        t.transform_statements(&mut s.orelse);
        let mut branch_aliases = vec![std::mem::replace(
            &mut t.state.import_aliases,
            pre_aliases.clone(),
        )];

        for handler in &mut s.handlers {
            let ExceptHandler::ExceptHandler(eh) = handler;
            if let Some(exc_type) = &mut eh.type_ {
                t.transform_expr(exc_type);
            }
            if let Some(name) = &eh.name {
                t.state.local_variables.insert(name.as_str().to_owned());
                t.state.shadowed_bindings.insert(name.as_str().to_owned());
                log::debug!("Tracking except alias as local: {}", name.as_str());
            }
            t.transform_statements(&mut eh.body);

            // Ensure exception handler body is not empty
            if eh.body.is_empty() {
                log::debug!("Adding pass statement to empty except handler in import transformer");
                eh.body.push(crate::ast_builder::statements::pass());
            }
            branch_aliases.push(std::mem::replace(
                &mut t.state.import_aliases,
                pre_aliases.clone(),
            ));
        }
        Self::merge_conditional_aliases(t, &pre_aliases, &branch_aliases, true);
        // The finally suite always runs: its additions promote normally
        t.transform_statements(&mut s.finalbody);
    }

    pub(in crate::code_generator::import_transformer) fn handle_with(
        t: &mut RecursiveImportTransformer<'_>,
        s: &mut StmtWith,
    ) {
        for item in &mut s.items {
            t.transform_expr(&mut item.context_expr);
            if let Some(vars) = &mut item.optional_vars {
                // Track assigned names as locals before transforming
                let mut with_names = crate::types::FxIndexSet::default();
                crate::code_generator::import_transformer::statement::StatementProcessor::collect_assigned_names(
                    vars,
                    &mut with_names,
                );
                for n in with_names {
                    t.state.local_variables.insert(n.clone());
                    t.state.shadowed_bindings.insert(n.clone());
                    log::debug!("Tracking with-as variable as local: {n}");
                }
                t.transform_expr(vars);
            }
        }
        t.transform_statements(&mut s.body);
    }

    pub(in crate::code_generator::import_transformer) fn handle_for(
        t: &mut RecursiveImportTransformer<'_>,
        s: &mut StmtFor,
    ) {
        // Python evaluates the ITERABLE before assigning the loop target, so
        // the iterable expression still sees the pre-loop binding (an imported
        // name shadowed by the target is only rebound afterwards); transform it
        // before installing the target shadows. Function-wide local shadowing
        // is collected separately by the function pre-pass.
        t.transform_expr(&mut s.iter);

        // Track loop variable as local before transforming target and body
        {
            let mut loop_names = crate::types::FxIndexSet::default();
            crate::code_generator::import_transformer::statement::StatementProcessor::collect_assigned_names(
                &s.target,
                &mut loop_names,
            );
            for n in loop_names {
                t.state.local_variables.insert(n.clone());
                t.state.shadowed_bindings.insert(n.clone());
                log::debug!("Tracking for loop variable as local: {n}");
            }
        }

        t.transform_expr(&mut s.target);
        // A loop body may execute zero times: alias additions inside it must
        // not promote past the loop (removals still veto)
        let pre_aliases = t.state.import_aliases.clone();
        t.transform_statements(&mut s.body);
        t.transform_statements(&mut s.orelse);
        let branch = std::mem::replace(&mut t.state.import_aliases, pre_aliases.clone());
        Self::merge_conditional_aliases(t, &pre_aliases, &[branch], false);
    }

    pub(in crate::code_generator::import_transformer) fn handle_while(
        t: &mut RecursiveImportTransformer<'_>,
        s: &mut StmtWhile,
    ) {
        t.transform_expr(&mut s.test);
        // A loop body may execute zero times: alias additions inside it must
        // not promote past the loop (removals still veto)
        let pre_aliases = t.state.import_aliases.clone();
        t.transform_statements(&mut s.body);
        t.transform_statements(&mut s.orelse);
        let branch = std::mem::replace(&mut t.state.import_aliases, pre_aliases.clone());
        Self::merge_conditional_aliases(t, &pre_aliases, &[branch], false);
    }

    pub(in crate::code_generator::import_transformer) fn handle_if(
        t: &mut RecursiveImportTransformer<'_>,
        s: &mut StmtIf,
    ) {
        t.transform_expr(&mut s.test);

        // TYPE_CHECKING suites never execute at runtime, but their imports feed
        // static annotation resolution: keep the legacy promotion for them
        let is_type_checking =
            crate::code_generator::import_transformer::statement::StatementProcessor::is_type_checking_condition(
                &s.test,
            );

        // Imports inside conditional branches must not promote their alias
        // bookkeeping past the branch as though they definitely executed: a
        // later use is rewritten only when EVERY path (including the implicit
        // fall-through) establishes the same alias — otherwise the original
        // program's NameError semantics must survive
        let pre_aliases = t.state.import_aliases.clone();
        t.transform_statements(&mut s.body);

        // Check if this is a TYPE_CHECKING block and ensure it has a body
        if s.body.is_empty() && is_type_checking {
            log::debug!("Adding pass statement to empty TYPE_CHECKING block in import transformer");
            s.body.push(crate::ast_builder::statements::pass());
        }

        let mut branch_aliases = vec![std::mem::replace(
            &mut t.state.import_aliases,
            pre_aliases.clone(),
        )];
        let mut all_paths_covered = false;
        for clause in &mut s.elif_else_clauses {
            if let Some(test_expr) = &mut clause.test {
                t.transform_expr(test_expr);
            } else {
                all_paths_covered = true;
            }
            t.transform_statements(&mut clause.body);

            // Ensure non-empty body for elif/else clauses too
            if clause.body.is_empty() {
                log::debug!(
                    "Adding pass statement to empty elif/else clause in import transformer"
                );
                clause.body.push(crate::ast_builder::statements::pass());
            }
            branch_aliases.push(std::mem::replace(
                &mut t.state.import_aliases,
                pre_aliases.clone(),
            ));
        }
        if is_type_checking {
            // Legacy promotion: adopt the TYPE_CHECKING branch's aliases
            if let Some(first) = branch_aliases.into_iter().next() {
                t.state.import_aliases = first;
            }
        } else {
            Self::merge_conditional_aliases(t, &pre_aliases, &branch_aliases, all_paths_covered);
        }
    }

    /// Merge importlib-alias bookkeeping after conditional branches: a PRE
    /// entry survives only when every branch kept it unchanged (any branch
    /// removing or rebinding it vetoes later rewrites), and a branch ADDITION
    /// survives only when all paths are covered and every branch establishes
    /// the identical alias.
    fn merge_conditional_aliases(
        t: &mut RecursiveImportTransformer<'_>,
        pre_aliases: &crate::types::FxIndexMap<String, String>,
        branch_aliases: &[crate::types::FxIndexMap<String, String>],
        all_paths_covered: bool,
    ) {
        let mut merged = pre_aliases.clone();
        merged.retain(|name, path| {
            branch_aliases
                .iter()
                .all(|branch| branch.get(name).is_some_and(|entry| entry == path))
        });
        if all_paths_covered && let Some(first) = branch_aliases.first() {
            for (name, path) in first {
                if pre_aliases.get(name) != Some(path)
                    && branch_aliases
                        .iter()
                        .all(|branch| branch.get(name).is_some_and(|entry| entry == path))
                {
                    merged.insert(name.clone(), path.clone());
                }
            }
        }
        t.state.import_aliases = merged;
    }

    pub(in crate::code_generator::import_transformer) fn handle_match(
        t: &mut RecursiveImportTransformer<'_>,
        s: &mut StmtMatch,
    ) {
        for case in &s.cases {
            crate::visitors::patterns::visit_binding_names(&case.pattern, &mut |name| {
                t.state.local_variables.insert(name.to_owned());
                t.state.shadowed_bindings.insert(name.to_owned());
                log::debug!("Tracking match case variable as local: {name}");
            });
        }

        t.transform_expr(&mut s.subject);
        // Cases are mutually exclusive branches; alias additions promote past
        // the match only if every case establishes them, and no wildcard
        // analysis is attempted (additions are conservatively dropped)
        let pre_aliases = t.state.import_aliases.clone();
        let mut branch_aliases = Vec::new();
        for case in &mut s.cases {
            crate::visitors::patterns::transform_runtime_exprs(&mut case.pattern, &mut |expr| {
                t.transform_expr(expr);
            });
            if let Some(guard) = &mut case.guard {
                t.transform_expr(guard);
            }
            t.transform_statements(&mut case.body);
            if case.body.is_empty() {
                case.body.push(crate::ast_builder::statements::pass());
            }
            branch_aliases.push(std::mem::replace(
                &mut t.state.import_aliases,
                pre_aliases.clone(),
            ));
        }
        Self::merge_conditional_aliases(t, &pre_aliases, &branch_aliases, false);
    }

    pub(in crate::code_generator::import_transformer) fn handle_class_def(
        t: &mut RecursiveImportTransformer<'_>,
        s: &mut StmtClassDef,
    ) {
        // Transform decorators
        for decorator in &mut s.decorator_list {
            t.transform_expr(&mut decorator.expression);
        }

        // Transform base classes
        t.transform_class_bases(s);

        // Class-body bindings shadow names only for expressions evaluated IN
        // the class body: save the enclosing scope's state and restore it
        // after, so a class attribute `importlib = custom` (or a method named
        // `importlib`) does not kill the MODULE-level import alias for
        // subsequent module-level statements
        let saved_locals = t.state.local_variables.clone();
        let saved_shadowed_bindings = t.state.shadowed_bindings.clone();

        // Transform class body
        t.transform_statements(&mut s.body);

        t.state.local_variables = saved_locals;
        t.state.shadowed_bindings = saved_shadowed_bindings;

        // The definition's NAME rebinds in the enclosing scope from here on: a
        // later `class importlib: ...` kills an earlier import alias
        t.state.shadowed_bindings.insert(s.name.to_string());
    }

    pub(in crate::code_generator::import_transformer) fn handle_function_def(
        t: &mut RecursiveImportTransformer<'_>,
        s: &mut ruff_python_ast::StmtFunctionDef,
    ) {
        log::debug!(
            "RecursiveImportTransformer: Entering function '{}'",
            s.name.as_str()
        );

        // Transform decorators
        for decorator in &mut s.decorator_list {
            t.transform_expr(&mut decorator.expression);
        }

        // Transform parameter annotations and default values
        for param in &mut s.parameters.posonlyargs {
            if let Some(annotation) = &mut param.parameter.annotation {
                t.transform_expr(annotation);
            }
            if let Some(default) = &mut param.default {
                t.transform_expr(default);
            }
        }
        for param in &mut s.parameters.args {
            if let Some(annotation) = &mut param.parameter.annotation {
                t.transform_expr(annotation);
            }
            if let Some(default) = &mut param.default {
                t.transform_expr(default);
            }
        }
        if let Some(vararg) = &mut s.parameters.vararg
            && let Some(annotation) = &mut vararg.annotation
        {
            t.transform_expr(annotation);
        }
        for param in &mut s.parameters.kwonlyargs {
            if let Some(annotation) = &mut param.parameter.annotation {
                t.transform_expr(annotation);
            }
            if let Some(default) = &mut param.default {
                t.transform_expr(default);
            }
        }
        if let Some(kwarg) = &mut s.parameters.kwarg
            && let Some(annotation) = &mut kwarg.annotation
        {
            t.transform_expr(annotation);
        }

        // Transform return type annotation
        if let Some(returns) = &mut s.returns {
            t.transform_expr(returns);
        }

        // Save current local variables and create a new scope for the function
        let saved_locals = t.state.local_variables.clone();
        let saved_shadowed_bindings = t.state.shadowed_bindings.clone();

        // Save the wrapper module imports - these should be scoped to each function
        // to prevent imports from one function affecting another
        let saved_wrapper_imports = t.state.wrapper_module_imports.clone();

        // Track function parameters as local variables before transforming the body
        // This prevents incorrect transformation of parameter names that shadow
        // stdlib modules

        // Track positional-only parameters
        for param in &s.parameters.posonlyargs {
            t.state
                .local_variables
                .insert(param.parameter.name.as_str().to_owned());
            t.state
                .shadowed_bindings
                .insert(param.parameter.name.as_str().to_owned());
            log::debug!(
                "Tracking function parameter as local (posonly): {}",
                param.parameter.name.as_str()
            );
        }

        // Track regular parameters
        for param in &s.parameters.args {
            t.state
                .local_variables
                .insert(param.parameter.name.as_str().to_owned());
            t.state
                .shadowed_bindings
                .insert(param.parameter.name.as_str().to_owned());
            log::debug!(
                "Tracking function parameter as local: {}",
                param.parameter.name.as_str()
            );
        }

        // Track *args if present
        if let Some(vararg) = &s.parameters.vararg {
            t.state
                .local_variables
                .insert(vararg.name.as_str().to_owned());
            t.state
                .shadowed_bindings
                .insert(vararg.name.as_str().to_owned());
            log::debug!(
                "Tracking function parameter as local (vararg): {}",
                vararg.name.as_str()
            );
        }

        // Track keyword-only parameters
        for param in &s.parameters.kwonlyargs {
            t.state
                .local_variables
                .insert(param.parameter.name.as_str().to_owned());
            t.state
                .shadowed_bindings
                .insert(param.parameter.name.as_str().to_owned());
            log::debug!(
                "Tracking function parameter as local (kwonly): {}",
                param.parameter.name.as_str()
            );
        }

        // Track **kwargs if present
        if let Some(kwarg) = &s.parameters.kwarg {
            t.state
                .local_variables
                .insert(kwarg.name.as_str().to_owned());
            t.state
                .shadowed_bindings
                .insert(kwarg.name.as_str().to_owned());
            log::debug!(
                "Tracking function parameter as local (kwarg): {}",
                kwarg.name.as_str()
            );
        }

        // Python scoping makes any name assigned in the function local for the WHOLE
        // body: collect all bindings up front so a call placed before the assignment
        // is not treated as referring to a module-level import alias (executing it
        // raises UnboundLocalError, which bundling must preserve). Import bindings
        // shadow the body too; the shadow is lifted when the import statement itself
        // is transformed. `global`-declared names rebind the module scope instead.
        {
            let mut body_bindings = crate::types::FxIndexSet::default();
            let scope_globals = crate::visitors::collect_scope_global_declarations(&s.body);
            crate::visitors::LocalVarCollector::new(&mut body_bindings, &scope_globals)
                .collect_from_stmts(&s.body);
            for name in body_bindings {
                log::debug!("Tracking function-body binding as shadowing: {name}");
                t.state.shadowed_bindings.insert(name);
            }
        }

        // Save the current scope level and mark that we're entering a local scope
        let saved_at_module_level = t.state.at_module_level;
        t.state.at_module_level = false;

        // Save current function context and compute symbol analysis once
        let saved_function_body = t.state.current_function_body.take();
        let saved_used_symbols = t.state.current_function_used_symbols.take();

        // Compute used symbols once from the original body (before transformation)
        t.state.current_function_used_symbols = Some(
            crate::visitors::SymbolUsageVisitor::collect_used_symbols(&s.body),
        );

        // Set function body for compatibility with existing APIs
        t.state.current_function_body = Some(s.body.to_vec());

        // Transform the function body
        t.transform_statements(&mut s.body);

        // After all transformations, hoist and deduplicate any inserted
        // `global` statements to the start of the function body (after a
        // docstring if present) to ensure correct Python semantics.
        crate::code_generator::import_transformer::statement::StatementProcessor::hoist_function_globals(
            s,
        );

        // Restore the previous scope level
        t.state.at_module_level = saved_at_module_level;

        // Restore the previous function context
        t.state.current_function_body = saved_function_body;
        t.state.current_function_used_symbols = saved_used_symbols;

        // Restore the wrapper module imports to prevent function-level imports from
        // affecting other functions
        t.state.wrapper_module_imports = saved_wrapper_imports;

        // Restore the previous scope's local variables
        t.state.local_variables = saved_locals;
        t.state.shadowed_bindings = saved_shadowed_bindings;

        // The definition's NAME rebinds in the enclosing scope from here on: a
        // later `def importlib(): ...` kills an earlier import alias
        t.state.shadowed_bindings.insert(s.name.to_string());
    }

    /// Handle assignment statement. Returns whether the caller should advance `i` normally
    /// (true) or perform `i += 1; continue;` (false). Mirrors current control flow which
    /// advances and continues within the arm.
    pub(in crate::code_generator::import_transformer) fn handle_assign(
        t: &mut RecursiveImportTransformer<'_>,
        s: &mut ruff_python_ast::StmtAssign,
    ) -> bool {
        // Track assignment LHS names to prevent collapsing RHS to self
        let mut lhs_names = crate::types::FxIndexSet::<String>::default();
        for target in &s.targets {
            crate::code_generator::import_transformer::statement::StatementProcessor::collect_assigned_names(
                target,
                &mut lhs_names,
            );
        }

        let saved_targets = t.state.current_assignment_targets.clone();
        t.state.current_assignment_targets = if lhs_names.is_empty() {
            None
        } else {
            Some(lhs_names)
        };

        // Handle importlib.import_module() assignment tracking
        if let ruff_python_ast::Expr::Call(call) = &s.value.as_ref()
            && crate::code_generator::import_transformer::handlers::dynamic::DynamicHandler::is_importlib_import_module_call(
                call,
                &t.state.import_aliases,
                &t.state.shadowed_bindings,
            )
        {
            // Get assigned names to pass to the handler
            let mut assigned_names = crate::types::FxIndexSet::default();
            for target in &s.targets {
                crate::code_generator::import_transformer::statement::StatementProcessor::collect_assigned_names(
                    target,
                    &mut assigned_names,
                );
            }

            // Track all assigned names (including tuple/list destructuring) as locals
            for n in &assigned_names {
                t.state.local_variables.insert(n.clone());
            }

            crate::code_generator::import_transformer::handlers::dynamic::DynamicHandler::handle_importlib_assignment(
                &assigned_names,
                call,
                t.state.bundler,
                &mut t.state.importlib_inlined_modules,
            );
        } else {
            // For non-importlib assignments, still track all assigned names as locals
            let mut assigned_names = crate::types::FxIndexSet::default();
            for target in &s.targets {
                crate::code_generator::import_transformer::statement::StatementProcessor::collect_assigned_names(
                    target,
                    &mut assigned_names,
                );
            }

            for n in &assigned_names {
                t.state.local_variables.insert(n.clone());
            }
        }

        // Transform the targets
        for target in &mut s.targets {
            t.transform_expr(target);
        }

        // Transform the RHS
        t.transform_expr(&mut s.value);

        // Names rebound by this assignment shadow any same-named import alias for
        // subsequent statements (the RHS above still saw the pre-assignment binding)
        if let Some(targets) = &t.state.current_assignment_targets {
            for name in targets {
                t.state.shadowed_bindings.insert(name.clone());
            }
        }

        // Restore previous context
        t.state.current_assignment_targets = saved_targets;

        // Original code performs `i += 1; continue;` in the caller.
        false
    }
}
