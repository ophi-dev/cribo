//! Post-Processing Phase
//!
//! This phase handles final transformations after all modules are processed:
//! - Namespace attachment for entry module exports
//! - Proxy generation for stdlib access
//! - Package child alias generation

use ruff_python_ast::Stmt;

use crate::{
    code_generator::{bundler::Bundler, context::PostProcessingResult},
    types::{FxIndexMap, FxIndexSet},
};

/// Post-processing phase handler (stateless)
#[derive(Default)]
pub(crate) struct PostProcessingPhase;

impl PostProcessingPhase {
    /// Create a new post-processing phase
    pub(crate) const fn new() -> Self {
        Self
    }

    /// Execute the post-processing phase
    ///
    /// This method:
    /// 1. Attaches entry module exports to namespace (for packages)
    /// 2. Generates proxy statements for stdlib access
    /// 3. Generates package child alias statements
    ///
    /// Returns statements to be inserted at appropriate positions in the final bundle.
    pub(crate) fn execute(
        &self,
        bundler: &mut Bundler<'_>,
        entry_symbols: &FxIndexSet<String>,
        entry_renames: &FxIndexMap<String, String>,
        final_body: &[Stmt],
    ) -> PostProcessingResult {
        // Generate namespace attachments for entry module exports
        let namespace_attachments =
            Self::generate_namespace_attachments(bundler, entry_symbols, entry_renames);

        // Generate proxy statements for stdlib access
        let mut proxy_statements = Self::generate_proxy_statements();

        // Generate the meta-path finder serving bundled modules to REAL runtime
        // imports; it resolves registrations lazily through globals(), so it can
        // ride along right after the proxy prelude (which binds the `_sys` alias)
        proxy_statements.extend(Self::generate_module_finder_statements(bundler, final_body));

        // Generate package child aliases
        let alias_statements = Self::generate_package_child_aliases(bundler, final_body);

        PostProcessingResult {
            proxy_statements,
            alias_statements,
            namespace_attachments,
        }
    }

    /// Generate namespace attachment statements for entry module exports
    fn generate_namespace_attachments(
        bundler: &mut Bundler<'_>,
        entry_symbols: &FxIndexSet<String>,
        entry_renames: &FxIndexMap<String, String>,
    ) -> Vec<Stmt> {
        log::debug!(
            "Checking if entry module needs namespace attachment: \
             entry_is_package_init_or_main={}, entry_module_name='{}'",
            bundler.entry_is_package_init_or_main,
            bundler.entry_module_name
        );

        if !bundler.entry_is_package_init_or_main {
            return Vec::new();
        }

        let entry_pkg = bundler
            .entry_package_name()
            .map(str::to_owned)
            .or_else(|| bundler.infer_entry_root_package())
            .unwrap_or_else(|| bundler.entry_module_name.clone());

        if entry_pkg.is_empty() || entry_pkg == crate::python::constants::MAIN_STEM {
            log::warn!(
                "Skipping namespace attachment: ambiguous entry package for '{}'",
                bundler.entry_module_name
            );
            return Vec::new();
        }

        log::debug!("Using package name '{entry_pkg}' for namespace attachment");

        let mut attachments = Vec::new();
        bundler.emit_entry_namespace_attachments(
            &entry_pkg,
            &mut attachments,
            entry_symbols,
            entry_renames,
        );
        attachments
    }

    /// Generate proxy statements for stdlib access
    fn generate_proxy_statements() -> Vec<Stmt> {
        log::debug!("Generating _cribo proxy for stdlib access");
        crate::ast_builder::proxy_generator::generate_cribo_proxy()
    }

    /// Generate the `sys.meta_path` finder serving bundled modules to REAL
    /// runtime imports, plus one registration per wrapper module and per bundled
    /// ancestor package (the machinery imports parents before dotted targets).
    ///
    /// Two consumers rely on it:
    /// - preserved `import_module` calls (opaque arguments) stay verbatim, and
    ///   Python's own machinery evaluates their arguments, initializes parents in
    ///   order, and manages `sys.modules`
    /// - external code importing bundled modules by their ORIGINAL names —
    ///   `pickle`/`multiprocessing` resolve a class through
    ///   `__import__(cls.__module__)`, which must yield the bundled namespace
    ///   holding the very same class object (wrapper inits stamp `__module__`
    ///   with the original module name)
    ///
    /// Registration is lazy (`find_spec` consults it per import), so unlike eager
    /// `sys.modules` registration it cannot shadow installed distributions that
    /// are never imported under a bundled name.
    fn generate_module_finder_statements(bundler: &Bundler<'_>, final_body: &[Stmt]) -> Vec<Stmt> {
        use crate::code_generator::module_registry::sanitize_module_name_for_identifier;

        // Every wrapper module (with an init function) is registered under its
        // original name; preserved import_module targets are wrapper modules too
        let mut names: FxIndexSet<String> = bundler
            .module_init_functions
            .keys()
            .filter_map(|module_id| bundler.resolver.get_module_name(*module_id))
            .collect();
        // INLINED modules that define classes are registered too: the inliner
        // stamps `X.__module__ = "models"` (and `__name__`/`__qualname__` for
        // renamed classes), so external consumers resolving classes by identity
        // (pickle) import that original name — harvest the stamps to expose the
        // same class objects through an on-demand namespace
        let inlined_exports = Self::harvest_inlined_class_exports(bundler, final_body, &names);
        // Plus every bundled ancestor package: the machinery imports parents
        // before dotted targets (inlined ancestors register without an init)
        let mut ancestor_names: FxIndexSet<String> = FxIndexSet::default();
        for name in names.iter().chain(inlined_exports.keys()) {
            let mut boundary = name.len();
            while let Some(dot) = name[..boundary].rfind('.') {
                boundary = dot;
                ancestor_names.insert(name[..boundary].to_owned());
            }
        }
        for ancestor in ancestor_names {
            if !inlined_exports.contains_key(&ancestor) {
                names.insert(ancestor);
            }
        }
        if names.is_empty() && inlined_exports.is_empty() {
            return Vec::new();
        }

        let mut statements =
            crate::ast_builder::preserved_finder::generate_preserved_import_finder();
        for name in &names {
            let Some(module_id) = bundler.get_module_id(name) else {
                log::debug!("Bundled module '{name}' has no module id; skipping registration");
                continue;
            };
            let init_function = bundler.module_init_functions.get(&module_id);
            let namespace_variable = sanitize_module_name_for_identifier(name);
            let is_package = bundler.resolver.is_package_init(module_id)
                || bundler.resolver.is_namespace_package(module_id);
            log::debug!(
                "Registering bundled module '{name}' with the meta-path finder \
                 (init={init_function:?}, namespace='{namespace_variable}', is_package={is_package})"
            );
            statements.push(
                crate::ast_builder::preserved_finder::generate_preserved_target_registration(
                    name,
                    init_function.map(String::as_str),
                    &namespace_variable,
                    is_package,
                ),
            );
        }
        for (name, exports) in &inlined_exports {
            let Some(module_id) = bundler.get_module_id(name) else {
                continue;
            };
            let is_package = bundler.resolver.is_package_init(module_id)
                || bundler.resolver.is_namespace_package(module_id);
            log::debug!(
                "Registering inlined class module '{name}' with the meta-path finder \
                 (exports={exports:?}, is_package={is_package})"
            );
            statements.push(
                crate::ast_builder::preserved_finder::generate_inlined_module_registration(
                    name, exports, is_package,
                ),
            );
        }
        statements
    }

    /// Harvest inlined class stamps from the final bundle body: top-level
    /// `X.__module__ = "models"` assignments name the original module, and a
    /// following `X.__name__ = "Item"` records the original export name for
    /// renamed classes. Returns module name -> [(export name, bundle binding)]
    /// for bundled modules that are not wrapper-registered.
    fn harvest_inlined_class_exports(
        bundler: &Bundler<'_>,
        final_body: &[Stmt],
        wrapper_names: &FxIndexSet<String>,
    ) -> FxIndexMap<String, Vec<(String, String)>> {
        use ruff_python_ast::Expr;

        // binding -> original module name (from __module__ stamps)
        let mut binding_modules: FxIndexMap<String, String> = FxIndexMap::default();
        // binding -> original export name (from __name__ stamps on renamed classes)
        let mut binding_exports: FxIndexMap<String, String> = FxIndexMap::default();
        for stmt in final_body {
            let Stmt::Assign(assign) = stmt else {
                continue;
            };
            let [Expr::Attribute(attribute)] = assign.targets.as_slice() else {
                continue;
            };
            let Expr::Name(binding) = &*attribute.value else {
                continue;
            };
            let Expr::StringLiteral(value) = &*assign.value else {
                continue;
            };
            match attribute.attr.as_str() {
                "__module__" => {
                    binding_modules.insert(binding.id.to_string(), value.value.to_str().to_owned());
                }
                "__name__" => {
                    binding_exports.insert(binding.id.to_string(), value.value.to_str().to_owned());
                }
                _ => {}
            }
        }

        let mut exports_by_module: FxIndexMap<String, Vec<(String, String)>> =
            FxIndexMap::default();
        for (binding, module_name) in binding_modules {
            if wrapper_names.contains(&module_name) || bundler.get_module_id(&module_name).is_none()
            {
                continue;
            }
            let export = binding_exports
                .get(&binding)
                .cloned()
                .unwrap_or_else(|| binding.clone());
            exports_by_module
                .entry(module_name)
                .or_default()
                .push((export, binding));
        }
        exports_by_module
    }

    /// Generate package child alias statements
    fn generate_package_child_aliases(bundler: &Bundler<'_>, final_body: &[Stmt]) -> Vec<Stmt> {
        use ruff_python_ast::ExprContext;

        use crate::{
            ast_builder::{expressions, statements},
            python::constants::INIT_STEM,
        };

        let mut alias_statements = Vec::new();

        let entry_pkg = bundler
            .infer_entry_root_package()
            .unwrap_or_else(|| bundler.entry_module_name.clone());

        if entry_pkg.is_empty() || entry_pkg == INIT_STEM {
            return alias_statements;
        }

        // Collect simple names already defined
        let existing_variables: FxIndexSet<String> = final_body
            .iter()
            .filter_map(|stmt| {
                if let Stmt::Assign(assign) = stmt
                    && let [ruff_python_ast::Expr::Name(name)] = assign.targets.as_slice()
                {
                    Some(name.id.to_string())
                } else {
                    None
                }
            })
            .collect();

        // Add aliases for all direct child modules
        let mut seen: FxIndexSet<String> = FxIndexSet::default();
        let mut added = 0_usize;

        for child in bundler
            .bundled_modules
            .iter()
            .filter_map(|id| bundler.resolver.get_module_name(*id))
        {
            if let Some(rest) = child.strip_prefix(&format!("{entry_pkg}.")) {
                let first = rest.split('.').next().unwrap_or("");
                if first.is_empty() || first.starts_with('_') {
                    continue;
                }
                if !seen.insert(first.to_owned()) {
                    continue;
                }
                if existing_variables.contains(first) {
                    log::debug!(
                        "Post-pass: skipping alias for {child} as '{first}' (would overwrite)"
                    );
                    continue;
                }

                log::debug!("Post-pass: adding alias '{first} = {entry_pkg}.{first}'");
                alias_statements.push(statements::simple_assign(
                    first,
                    expressions::attribute(
                        expressions::name(&entry_pkg, ExprContext::Load),
                        first,
                        ExprContext::Load,
                    ),
                ));
                added += 1;
            }
        }

        log::debug!("Post-pass: added {added} module-level aliases for package '{entry_pkg}'");
        alias_statements
    }

    /// Insert proxy statements after __future__ imports
    pub(crate) fn insert_proxy_statements(proxy_statements: Vec<Stmt>, final_body: &mut Vec<Stmt>) {
        log::debug!("Inserting _cribo proxy after __future__ imports");

        // Find position after optional module docstring and __future__ imports
        // Skip leading module docstring
        let mut insert_position = if let Some(Stmt::Expr(expr)) = final_body.first()
            && matches!(expr.value.as_ref(), ruff_python_ast::Expr::StringLiteral(_))
        {
            1
        } else {
            0
        };

        // Skip contiguous __future__ imports after docstring
        for (i, stmt) in final_body.iter().enumerate().skip(insert_position) {
            if let Stmt::ImportFrom(import_from) = stmt
                && let Some(module) = &import_from.module
                && module.as_str() == "__future__"
            {
                insert_position = i + 1;
                continue;
            }
            break;
        }

        // Insert proxy statements
        for (i, stmt) in proxy_statements.into_iter().enumerate() {
            final_body.insert(insert_position + i, stmt);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_post_processing_output_construction() {
        let output = PostProcessingResult {
            proxy_statements: vec![],
            alias_statements: vec![],
            namespace_attachments: vec![],
        };

        assert!(output.proxy_statements.is_empty());
        assert!(output.alias_statements.is_empty());
        assert!(output.namespace_attachments.is_empty());
    }

    #[test]
    fn test_post_processing_with_statements() {
        use ruff_python_ast::{AtomicNodeIndex, StmtExpr};
        use ruff_text_size::TextRange;

        let stmt = Stmt::Expr(StmtExpr {
            node_index: AtomicNodeIndex::NONE,
            range: TextRange::default(),
            value: Box::new(ruff_python_ast::Expr::NumberLiteral(
                ruff_python_ast::ExprNumberLiteral {
                    node_index: AtomicNodeIndex::NONE,
                    range: TextRange::default(),
                    value: ruff_python_ast::Number::Int(ruff_python_ast::Int::ZERO),
                },
            )),
        });

        let output = PostProcessingResult {
            proxy_statements: vec![stmt.clone()],
            alias_statements: vec![stmt.clone()],
            namespace_attachments: vec![stmt],
        };

        assert_eq!(output.proxy_statements.len(), 1);
        assert_eq!(output.alias_statements.len(), 1);
        assert_eq!(output.namespace_attachments.len(), 1);
    }

    #[test]
    fn test_insert_proxy_statements_empty() {
        let proxy_statements = vec![];
        let mut final_body = vec![];

        PostProcessingPhase::insert_proxy_statements(proxy_statements, &mut final_body);

        assert!(final_body.is_empty());
    }

    #[test]
    fn test_insert_proxy_statements_at_beginning() {
        use ruff_python_ast::{AtomicNodeIndex, StmtExpr};
        use ruff_text_size::TextRange;

        let proxy_stmt = Stmt::Expr(StmtExpr {
            node_index: AtomicNodeIndex::NONE,
            range: TextRange::default(),
            value: Box::new(ruff_python_ast::Expr::NumberLiteral(
                ruff_python_ast::ExprNumberLiteral {
                    node_index: AtomicNodeIndex::NONE,
                    range: TextRange::default(),
                    value: ruff_python_ast::Number::Int(ruff_python_ast::Int::ONE),
                },
            )),
        });

        let original_stmt = Stmt::Expr(StmtExpr {
            node_index: AtomicNodeIndex::NONE,
            range: TextRange::default(),
            value: Box::new(ruff_python_ast::Expr::NumberLiteral(
                ruff_python_ast::ExprNumberLiteral {
                    node_index: AtomicNodeIndex::NONE,
                    range: TextRange::default(),
                    value: ruff_python_ast::Number::Int(ruff_python_ast::Int::ZERO),
                },
            )),
        });

        let mut final_body = vec![original_stmt];
        let proxy_statements = vec![proxy_stmt];

        PostProcessingPhase::insert_proxy_statements(proxy_statements, &mut final_body);

        // Proxy should be inserted at position 0 (no __future__ imports)
        assert_eq!(final_body.len(), 2);
    }
}
