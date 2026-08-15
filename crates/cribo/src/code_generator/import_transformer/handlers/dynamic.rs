use ruff_python_ast::{AtomicNodeIndex, Expr, ExprAttribute, ExprCall, ExprContext, ExprName};

use crate::{
    code_generator::bundler::Bundler,
    resolver::ModuleId,
    types::{FxIndexMap, FxIndexSet},
};

/// Handle dynamic import transformations (`importlib.import_module`)
pub(crate) struct DynamicHandler;

impl DynamicHandler {
    /// Check if this is an `importlib.import_module()` call.
    ///
    /// A callee whose base name is rebound by a local binding (function parameter,
    /// assignment, loop variable) is not recognized: the call dispatches to that
    /// binding's own `import_module` at runtime, not to `importlib`.
    pub(in crate::code_generator::import_transformer) fn is_importlib_import_module_call(
        call: &ExprCall,
        import_aliases: &FxIndexMap<String, String>,
        shadowed_bindings: &FxIndexSet<String>,
    ) -> bool {
        match &call.func.as_ref() {
            // Direct call: importlib.import_module()
            Expr::Attribute(attr) if attr.attr.as_str() == "import_module" => {
                match &attr.value.as_ref() {
                    Expr::Name(name) => {
                        let name_str = name.id.as_str();
                        if shadowed_bindings.contains(name_str) {
                            return false;
                        }
                        // Check if it's 'importlib' directly or an alias that maps to 'importlib'
                        name_str == "importlib"
                            || import_aliases.get(name_str) == Some(&"importlib".to_owned())
                    }
                    _ => false,
                }
            }
            // Function call: im() where im is import_module
            Expr::Name(name) => {
                let name_str = name.id.as_str();
                if shadowed_bindings.contains(name_str) {
                    return false;
                }
                // Check if this name is an alias for importlib.import_module
                import_aliases
                    .get(name_str)
                    .is_some_and(|module| module == "importlib.import_module")
            }
            _ => false,
        }
    }

    /// Resolve `importlib.import_module()` target module name, handling relative imports.
    /// Both argument forms are supported: positional (`import_module(".m", "pkg")`) and
    /// keyword (`import_module(name=".m", package="pkg")`).
    ///
    /// Two argument shapes resolve: fully discardable arguments, and the
    /// evaluable-package form (`import_module("pkg", package=touch())`), whose extra
    /// expression the rewrite must still evaluate (see
    /// [`Self::transform_importlib_import_module`]). Anything else stays a runtime
    /// call.
    fn resolve_importlib_target(call: &ExprCall) -> Option<String> {
        if !(crate::python::importlib_call::arguments_safely_discardable(call)
            || crate::python::importlib_call::evaluable_package_argument(call).is_some())
        {
            return None;
        }
        let module_name = crate::python::importlib_call::literal_module_name(call)?;

        // Handle relative imports with package context, exactly like CPython's
        // `_resolve_name`: the literal package string is the verbatim anchor. No
        // path-based module-vs-package adjustment is applied — Python computes the
        // target textually and only then validates it, so a call anchored at a plain
        // module (e.g. `import_module(".sub", "pkg.mod")` → `pkg.mod.sub`) resolves
        // to a name that is not bundled, stays preserved, and raises at runtime like
        // the original.
        let package_argument = crate::python::importlib_call::literal_package_context(call);
        let resolved_name = if module_name.starts_with('.') {
            let package = package_argument?;
            crate::python::importlib_call::resolve_relative_name(module_name, package)?
        } else {
            module_name.to_owned()
        };

        Some(resolved_name)
    }

    /// Wrap a bundled-module access expression with a `sys.modules` consult:
    ///
    /// ```python
    /// _cribo.importlib.import_module("pkg.sub") \
    ///     if "pkg" in _cribo.sys.modules or "pkg.sub" in _cribo.sys.modules \
    ///     else <access>
    /// ```
    ///
    /// CPython's `_find_and_load` returns an existing `sys.modules` entry
    /// before invoking any finder or loader, and resolves DOTTED names through
    /// the parent's `__path__` — so a preloaded replacement of the target OR of
    /// any ancestor package must route through the real machinery rather than
    /// the bundled access. When no component is preloaded, the direct bundled
    /// access is used. The conditional (rather than `or`) also honors falsy
    /// replacement objects.
    fn sys_modules_entry_or(module_name: &str, access: Expr) -> Expr {
        use ruff_python_ast::{BoolOp, ExprContext};

        use crate::ast_builder::expressions;

        let cribo_attribute = |attribute: &str| {
            expressions::attribute(
                expressions::name(crate::ast_builder::CRIBO_PREFIX, ExprContext::Load),
                attribute,
                ExprContext::Load,
            )
        };
        let sys_modules =
            || expressions::attribute(cribo_attribute("sys"), "modules", ExprContext::Load);

        // "pkg" in _cribo.sys.modules or "pkg.mid" in ... or "pkg.mid.sub" in ...
        let mut component_tests = Vec::new();
        let mut boundary = 0_usize;
        loop {
            match module_name[boundary..].find('.') {
                Some(offset) => boundary += offset,
                None => boundary = module_name.len(),
            }
            component_tests.push(expressions::in_op(
                expressions::string_literal(&module_name[..boundary]),
                sys_modules(),
            ));
            if boundary == module_name.len() {
                break;
            }
            boundary += 1;
        }
        let any_component_preloaded = if component_tests.len() == 1 {
            component_tests
                .pop()
                .expect("one component test must exist")
        } else {
            expressions::bool_op(BoolOp::Or, component_tests)
        };

        expressions::if_exp(
            any_component_preloaded,
            expressions::call(
                expressions::attribute(
                    cribo_attribute("importlib"),
                    "import_module",
                    ExprContext::Load,
                ),
                vec![expressions::string_literal(module_name)],
                vec![],
            ),
            access,
        )
    }

    /// Transform importlib.import_module("module-name") to direct module reference.
    ///
    /// For the evaluable-package form (`import_module("pkg", package=touch())`), the
    /// package expression is evaluated but ignored by CPython for absolute names, so
    /// the rewrite preserves its evaluation (and any exception or side effect) with
    /// `(touch(), <module access>)[1]`.
    pub(in crate::code_generator::import_transformer) fn transform_importlib_import_module(
        call: &ExprCall,
        bundler: &Bundler<'_>,
        created_namespace_objects: &mut bool,
        create_module_access_expr: impl Fn(&str) -> Expr,
    ) -> Option<Expr> {
        // Get the module name and resolve relative imports
        if let Some(resolved_name) = Self::resolve_importlib_target(call) {
            // Check if this module is part of the bundle (wrapper or inlined)
            if bundler.get_module_id(&resolved_name).is_some_and(|id| {
                bundler.bundled_modules.contains(&id) || bundler.inlined_modules.contains(&id)
            }) {
                log::debug!(
                    "Transforming importlib.import_module call to module access '{resolved_name}'"
                );

                // Check if this creates a namespace object
                if bundler
                    .get_module_id(&resolved_name)
                    .is_some_and(|id| bundler.inlined_modules.contains(&id))
                {
                    *created_namespace_objects = true;
                }

                // Use common logic for module access, but honor a PRELOADED
                // sys.modules entry first: Python consults sys.modules before
                // any loading, so `sys.modules["provider"] = replacement`
                // installed ahead of the call must yield the replacement
                // rather than (re)initializing the bundled module
                let access = Self::sys_modules_entry_or(
                    &resolved_name,
                    create_module_access_expr(&resolved_name),
                );
                // Preserve the evaluation of a non-literal package expression:
                // Python evaluates it before importing, so its side effects and
                // exceptions must survive the rewrite
                if let Some(package_expr) =
                    crate::python::importlib_call::evaluable_package_argument(call)
                {
                    use ruff_python_ast::ExprContext;

                    use crate::ast_builder::expressions;
                    return Some(expressions::subscript(
                        expressions::tuple(vec![package_expr.clone(), access]),
                        expressions::integer_literal(1),
                        ExprContext::Load,
                    ));
                }
                return Some(access);
            }
        }
        // Preserved calls with opaque arguments stay fully verbatim: the bundle's
        // sys.meta_path finder (emitted in post-processing) maps bundled target
        // names to their init functions, so Python's own import machinery
        // evaluates and validates the arguments, initializes parent packages in
        // order, and manages sys.modules — with exact runtime semantics and
        // without eager initialization at bundle load.
        None
    }

    /// For importlib-imported module variables, rewrite `base.attr` to the inlined symbol
    pub(in crate::code_generator::import_transformer) fn rewrite_attr_for_importlib_var(
        attr_expr: &ExprAttribute,
        base: &str,
        module_name: &str,
        bundler: &Bundler<'_>,
        symbol_renames: &FxIndexMap<ModuleId, FxIndexMap<String, String>>,
    ) -> Expr {
        // Only rewrite attribute reads; preserve writes to module attributes.
        if !matches!(attr_expr.ctx, ExprContext::Load) {
            return Expr::Attribute(attr_expr.clone());
        }
        let attr_name = attr_expr.attr.as_str();

        if let Some(module_id) = bundler.get_module_id(module_name)
            && let Some(module_renames) = symbol_renames.get(&module_id)
            && let Some(renamed) = module_renames.get(attr_name)
        {
            let renamed_str = renamed.clone();
            log::debug!(
                "Rewrote {base}.{attr_name} to {renamed_str} (renamed symbol from importlib \
                 inlined module)"
            );
            return Expr::Name(ExprName {
                node_index: AtomicNodeIndex::NONE,
                id: renamed_str.into(),
                ctx: attr_expr.ctx,
                range: attr_expr.range,
            });
        }
        // no rename: fallthrough below
        log::debug!(
            "Rewrote {base}.{attr_name} to {attr_name} (symbol from importlib inlined module)"
        );
        Expr::Name(ExprName {
            node_index: AtomicNodeIndex::NONE,
            id: attr_name.into(),
            ctx: attr_expr.ctx,
            range: attr_expr.range,
        })
    }

    /// Handle assignment from `importlib.import_module` call, tracking inlined modules
    pub(in crate::code_generator::import_transformer) fn handle_importlib_assignment(
        assigned_names: &FxIndexSet<String>,
        call: &ExprCall,
        bundler: &Bundler<'_>,
        importlib_inlined_modules: &mut FxIndexMap<String, String>,
    ) {
        // Get the module name and resolve relative imports
        if let Some(resolved_name) = Self::resolve_importlib_target(call)
            && bundler
                .get_module_id(&resolved_name)
                .is_some_and(|id| bundler.inlined_modules.contains(&id))
        {
            // Track all assigned names as importing this module
            for name in assigned_names {
                log::debug!(
                    "Tracking variable '{name}' as assigned from \
                     importlib.import_module('{resolved_name}')"
                );
                importlib_inlined_modules.insert(name.clone(), resolved_name.clone());
            }
        }
    }
}
