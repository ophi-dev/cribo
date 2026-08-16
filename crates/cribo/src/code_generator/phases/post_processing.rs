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
    #[expect(clippy::too_many_arguments)] // Post-processing consumes the full bundle context
    pub(crate) fn execute(
        &self,
        bundler: &mut Bundler<'_>,
        entry_symbols: &FxIndexSet<String>,
        entry_renames: &FxIndexMap<String, String>,
        symbol_renames: &FxIndexMap<crate::resolver::ModuleId, FxIndexMap<String, String>>,
        final_body: &[Stmt],
        module_section_end: usize,
    ) -> PostProcessingResult {
        // Generate namespace attachments for entry module exports
        let namespace_attachments =
            Self::generate_namespace_attachments(bundler, entry_symbols, entry_renames);

        // Generate proxy statements for stdlib access
        let mut proxy_statements = Self::generate_proxy_statements();

        // Generate the meta-path finder serving bundled modules to REAL runtime
        // imports; it resolves registrations lazily through globals(), so it can
        // ride along right after the proxy prelude (which binds the `_sys` alias)
        let (finder_statements, capture_statements) = Self::generate_module_finder_statements(
            bundler,
            symbol_renames,
            final_body,
            module_section_end,
        );
        proxy_statements.extend(finder_statements);

        // Generate package child aliases
        let alias_statements = Self::generate_package_child_aliases(bundler, final_body);

        PostProcessingResult {
            proxy_statements,
            alias_statements,
            namespace_attachments,
            capture_statements,
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
    fn generate_module_finder_statements(
        bundler: &Bundler<'_>,
        symbol_renames: &FxIndexMap<crate::resolver::ModuleId, FxIndexMap<String, String>>,
        final_body: &[Stmt],
        module_section_end: usize,
    ) -> (Vec<Stmt>, Vec<Stmt>) {
        use crate::code_generator::module_registry::sanitize_module_name_for_identifier;

        let external_submodule_roots = Self::collect_external_submodule_roots(bundler, final_body);

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
        // The harvested stamps cover classes and functions, but a REAL module
        // namespace exposes every module-scope binding: merge in all retained
        // exports (constants like VERSION included), resolving renamed bindings
        // and skipping symbols dropped by tree-shaking (their bundle globals do
        // not exist)
        let inlined_exports = Self::extend_with_retained_exports(
            bundler,
            symbol_renames,
            // Bindings must exist in the MODULE-DEFINITION section: entry
            // statements run after the boundary captures, so their globals
            // cannot back a registration (and may collide by name)
            &final_body[..module_section_end.min(final_body.len())],
            inlined_exports,
        );
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
        if names.is_empty() && inlined_exports.is_empty() && bundler.created_namespaces.is_empty() {
            return (Vec::new(), Vec::new());
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
            let local_precedence =
                !name.contains('.') && !external_submodule_roots.contains(name.as_str());
            log::debug!(
                "Registering bundled module '{name}' with the meta-path finder \
                 (init={init_function:?}, namespace='{namespace_variable}', \
                 is_package={is_package}, local_precedence={local_precedence})"
            );
            statements.push(
                crate::ast_builder::preserved_finder::generate_preserved_target_registration(
                    name,
                    init_function.map(String::as_str),
                    &namespace_variable,
                    is_package,
                    local_precedence,
                ),
            );
        }
        for (name, exports) in &inlined_exports {
            let Some(module_id) = bundler.get_module_id(name) else {
                continue;
            };
            let is_package = bundler.resolver.is_package_init(module_id)
                || bundler.resolver.is_namespace_package(module_id);
            let local_precedence =
                !name.contains('.') && !external_submodule_roots.contains(name.as_str());
            log::debug!(
                "Registering inlined class module '{name}' with the meta-path finder \
                 (exports={exports:?}, is_package={is_package}, local_precedence={local_precedence})"
            );
            statements.push(
                crate::ast_builder::preserved_finder::generate_inlined_module_registration(
                    name,
                    exports,
                    is_package,
                    local_precedence,
                ),
            );
        }

        // Capture the export VALUES at the module/entry boundary: entry code
        // may legally delete or rebind the bundle globals these registrations
        // refer to, while a real module's namespace would retain the objects
        let mut capture_statements = Vec::new();
        let mut captured_bindings: FxIndexSet<&str> = FxIndexSet::default();
        for exports in inlined_exports.values() {
            for (_, binding) in exports {
                if captured_bindings.insert(binding.as_str()) {
                    capture_statements.push(
                        crate::ast_builder::preserved_finder::generate_export_capture(binding),
                    );
                }
            }
        }

        (statements, capture_statements)
    }

    /// Merge every retained module-scope export into the registration maps of
    /// inlined modules: a runtime import must observe the COMPLETE namespace
    /// (constants alongside classes), not only the definitions that carry
    /// identity stamps. Renamed bindings resolve through the module's rename
    /// map, and symbols whose bundle globals were tree-shaken are skipped.
    fn extend_with_retained_exports(
        bundler: &Bundler<'_>,
        symbol_renames: &FxIndexMap<crate::resolver::ModuleId, FxIndexMap<String, String>>,
        final_body: &[Stmt],
        mut inlined_exports: FxIndexMap<String, Vec<(String, String)>>,
    ) -> FxIndexMap<String, Vec<(String, String)>> {
        use ruff_python_ast::Expr;

        // Bundle globals actually defined in the emitted body (tree-shaken
        // symbols never appear)
        let mut defined_globals: FxIndexSet<&str> = FxIndexSet::default();
        fn collect_target_names<'stmt>(target: &'stmt Expr, into: &mut FxIndexSet<&'stmt str>) {
            match target {
                Expr::Name(name) => {
                    into.insert(name.id.as_str());
                }
                Expr::Tuple(tuple) => {
                    for element in &tuple.elts {
                        collect_target_names(element, into);
                    }
                }
                Expr::List(list) => {
                    for element in &list.elts {
                        collect_target_names(element, into);
                    }
                }
                Expr::Starred(starred) => collect_target_names(&starred.value, into),
                _ => {}
            }
        }
        for stmt in final_body {
            match stmt {
                Stmt::Assign(assign) => {
                    // Destructuring binds exports too: `LEFT, RIGHT = values`
                    for target in &assign.targets {
                        collect_target_names(target, &mut defined_globals);
                    }
                }
                Stmt::AnnAssign(ann_assign) => {
                    collect_target_names(&ann_assign.target, &mut defined_globals);
                }
                Stmt::FunctionDef(function_def) => {
                    defined_globals.insert(function_def.name.as_str());
                }
                Stmt::ClassDef(class_def) => {
                    defined_globals.insert(class_def.name.as_str());
                }
                _ => {}
            }
        }

        for (module_name, exports) in &mut inlined_exports {
            let Some(module_id) = bundler.get_module_id(module_name) else {
                continue;
            };
            let Some(semantic_exports) = bundler.semantic_exports.get(&module_id) else {
                continue;
            };
            let renames = symbol_renames.get(&module_id);
            for export in semantic_exports {
                if exports.iter().any(|(existing, _)| existing == export) {
                    continue;
                }
                let binding = renames
                    .and_then(|renames| renames.get(export))
                    .cloned()
                    .unwrap_or_else(|| export.clone());
                if defined_globals.contains(binding.as_str()) {
                    exports.push((export.clone(), binding));
                }
            }
        }
        inlined_exports
    }

    /// Collect top-level roots of dotted import targets that remain EXTERNAL
    /// in the emitted bundle (e.g. `from yaml._yaml import ...` for an
    /// optional C extension kept as a real import). A bundled first-party
    /// package with such a submodule must NOT register in front of `PathFinder`:
    /// the external import resolves through the INSTALLED distribution's
    /// `__path__`, which the bundled namespace cannot provide, so the
    /// installed package must stay reachable under the original name.
    fn collect_external_submodule_roots(
        bundler: &Bundler<'_>,
        final_body: &[Stmt],
    ) -> FxIndexSet<String> {
        use ruff_python_ast::visitor::source_order::{SourceOrderVisitor, walk_stmt};

        struct Collector<'a> {
            bundler: &'a Bundler<'a>,
            roots: FxIndexSet<String>,
            /// Names bound to the real `importlib` module in the emitted
            /// bundle (`import importlib [as il]`, `importlib =
            /// _cribo.importlib`)
            importlib_names: FxIndexSet<String>,
            /// Names bound to the real `importlib.import_module` function
            /// (`from importlib import import_module [as im]`, assignments
            /// off an importlib binding)
            import_module_names: FxIndexSet<String>,
        }

        impl Collector<'_> {
            fn record(&mut self, name: &str) {
                let Some((root, _)) = name.split_once('.') else {
                    return;
                };
                if self.bundler.get_module_id(name).is_none() {
                    self.roots.insert(root.to_owned());
                }
            }

            /// Track which names dispatch to the REAL importlib. A user-defined
            /// method that merely shares the `import_module` spelling
            /// (`loader.import_module("pkg.missing")`) is not an import and must
            /// not demote a bundled root behind `PathFinder`.
            ///
            /// Bindings are collected recursively — a function-local
            /// `import importlib as il` dispatches just as well as a top-level
            /// one. Tracking is sticky and name-based: over-inclusion only
            /// keeps an installed root reachable.
            fn track_bindings_in_suite(&mut self, suite: &[Stmt]) {
                for stmt in suite {
                    self.track_bindings(stmt);
                    match stmt {
                        Stmt::FunctionDef(function_def) => {
                            self.track_bindings_in_suite(&function_def.body);
                        }
                        Stmt::ClassDef(class_def) => {
                            self.track_bindings_in_suite(&class_def.body);
                        }
                        Stmt::If(if_stmt) => {
                            self.track_bindings_in_suite(&if_stmt.body);
                            for clause in &if_stmt.elif_else_clauses {
                                self.track_bindings_in_suite(&clause.body);
                            }
                        }
                        Stmt::For(for_stmt) => {
                            self.track_bindings_in_suite(&for_stmt.body);
                            self.track_bindings_in_suite(&for_stmt.orelse);
                        }
                        Stmt::While(while_stmt) => {
                            self.track_bindings_in_suite(&while_stmt.body);
                            self.track_bindings_in_suite(&while_stmt.orelse);
                        }
                        Stmt::With(with_stmt) => {
                            self.track_bindings_in_suite(&with_stmt.body);
                        }
                        Stmt::Try(try_stmt) => {
                            self.track_bindings_in_suite(&try_stmt.body);
                            for handler in &try_stmt.handlers {
                                let ruff_python_ast::ExceptHandler::ExceptHandler(handler) =
                                    handler;
                                self.track_bindings_in_suite(&handler.body);
                            }
                            self.track_bindings_in_suite(&try_stmt.orelse);
                            self.track_bindings_in_suite(&try_stmt.finalbody);
                        }
                        Stmt::Match(match_stmt) => {
                            for case in &match_stmt.cases {
                                self.track_bindings_in_suite(&case.body);
                            }
                        }
                        _ => {}
                    }
                }
            }

            fn track_bindings(&mut self, stmt: &Stmt) {
                use ruff_python_ast::Expr;
                match stmt {
                    Stmt::Import(import) => {
                        for alias in &import.names {
                            let module = alias.name.as_str();
                            if module == "importlib" || module.starts_with("importlib.") {
                                let bound = alias.asname.as_ref().map_or_else(
                                    || module.split('.').next().unwrap_or(module),
                                    ruff_python_ast::Identifier::as_str,
                                );
                                self.importlib_names.insert(bound.to_owned());
                            }
                        }
                    }
                    Stmt::ImportFrom(import_from)
                        if import_from.level == 0
                            && import_from.module.as_deref() == Some("importlib") =>
                    {
                        for alias in &import_from.names {
                            if alias.name.as_str() == "import_module" {
                                let bound = alias.asname.as_ref().unwrap_or(&alias.name);
                                self.import_module_names.insert(bound.to_string());
                            }
                        }
                    }
                    Stmt::Assign(assign) => {
                        let [Expr::Name(target)] = assign.targets.as_slice() else {
                            return;
                        };
                        let binds_importlib = match &*assign.value {
                            // The import transformer spells a stdlib binding as
                            // `importlib = _cribo.importlib`
                            Expr::Attribute(attribute)
                                if attribute.attr.as_str() == "importlib" =>
                            {
                                matches!(&*attribute.value, Expr::Name(base)
                                    if base.id.as_str() == crate::ast_builder::CRIBO_PREFIX)
                            }
                            Expr::Name(source) => self.importlib_names.contains(source.id.as_str()),
                            _ => false,
                        };
                        let binds_import_module = match &*assign.value {
                            Expr::Attribute(attribute)
                                if attribute.attr.as_str() == "import_module" =>
                            {
                                match &*attribute.value {
                                    Expr::Name(base) => {
                                        self.importlib_names.contains(base.id.as_str())
                                    }
                                    Expr::Attribute(inner)
                                        if inner.attr.as_str() == "importlib" =>
                                    {
                                        matches!(&*inner.value, Expr::Name(base)
                                            if base.id.as_str() == crate::ast_builder::CRIBO_PREFIX)
                                    }
                                    _ => false,
                                }
                            }
                            Expr::Name(source) => {
                                self.import_module_names.contains(source.id.as_str())
                            }
                            _ => false,
                        };
                        // Sticky: once a name has carried the real importlib,
                        // calls through it anywhere in the body may dispatch
                        // there (function bodies run after any later rebind
                        // too) — over-inclusion only keeps an installed root
                        // reachable, while a name NEVER bound to importlib
                        // (a user loader object) records nothing
                        if binds_importlib {
                            self.importlib_names.insert(target.id.to_string());
                        }
                        if binds_import_module {
                            self.import_module_names.insert(target.id.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }

        impl<'a> SourceOrderVisitor<'a> for Collector<'a> {
            fn visit_stmt(&mut self, stmt: &'a Stmt) {
                match stmt {
                    Stmt::Import(import) => {
                        for alias in &import.names {
                            self.record(alias.name.as_str());
                        }
                    }
                    Stmt::ImportFrom(import_from) if import_from.level == 0 => {
                        if let Some(module) = &import_from.module {
                            self.record(module.as_str());
                            // A surviving from-import also names its children
                            // through the installed package's __path__:
                            // `from mixed_package import _native` resolves the
                            // dotted submodule `mixed_package._native`, so each
                            // imported name is a potential external child that
                            // must keep the installed root reachable
                            for alias in &import_from.names {
                                if alias.name.as_str() == "*" {
                                    continue;
                                }
                                self.record(&format!(
                                    "{}.{}",
                                    module.as_str(),
                                    alias.name.as_str()
                                ));
                            }
                        }
                    }
                    _ => {}
                }
                walk_stmt(self, stmt);
            }

            fn visit_expr(&mut self, expr: &'a ruff_python_ast::Expr) {
                use ruff_python_ast::Expr;
                // Preserved DYNAMIC imports with a statically known dotted
                // target (`importlib.import_module("mixed_package._native",
                // **{})`) resolve through the real machinery too: the parent
                // must stay reachable through PathFinder for its installed
                // __path__ to serve the external child. Only calls dispatching
                // through a tracked importlib binding count — a user method
                // spelled `import_module` is not an import
                if let Expr::Call(call) = expr {
                    let callee_is_import_module = match &*call.func {
                        Expr::Attribute(attribute)
                            if attribute.attr.as_str() == "import_module" =>
                        {
                            matches!(&*attribute.value, Expr::Name(base)
                                if self.importlib_names.contains(base.id.as_str()))
                        }
                        Expr::Name(name) => self.import_module_names.contains(name.id.as_str()),
                        _ => false,
                    };
                    if callee_is_import_module
                        && let Some(module_name) =
                            crate::python::importlib_call::literal_module_name(call)
                        && !module_name.starts_with('.')
                    {
                        self.record(module_name);
                    }
                }
                ruff_python_ast::visitor::source_order::walk_expr(self, expr);
            }
        }

        let mut collector = Collector {
            bundler,
            roots: FxIndexSet::default(),
            importlib_names: FxIndexSet::default(),
            import_module_names: FxIndexSet::default(),
        };
        // Bindings are collected recursively in source order FIRST: calls in
        // function bodies execute after the whole module-level program has
        // run, so any binding anywhere in the bundle may serve them
        collector.track_bindings_in_suite(final_body);
        for stmt in final_body {
            collector.visit_stmt(stmt);
        }
        collector.roots
    }

    /// Harvest inlined definition stamps (classes AND functions) from the
    /// final bundle body: top-level `X.__module__ = "models"` assignments name
    /// the original module, and a following `X.__name__ = "Item"` records the
    /// original export name for renamed definitions. Returns module name ->
    /// [(export name, bundle binding)] for bundled modules that are not
    /// wrapper-registered.
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
        let record_stamp = |binding_modules: &mut FxIndexMap<String, String>,
                            binding_exports: &mut FxIndexMap<String, String>,
                            stmt: &Stmt| {
            let Stmt::Assign(assign) = stmt else {
                return;
            };
            let [Expr::Attribute(attribute)] = assign.targets.as_slice() else {
                return;
            };
            let Expr::Name(binding) = &*attribute.value else {
                return;
            };
            let Expr::StringLiteral(value) = &*assign.value else {
                return;
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
        };
        // Stamps live wherever the inlined statements were emitted — at the
        // top level, inside stamp-guard try/except blocks (decorated
        // definitions), and inside ordinary compound suites the module code
        // carried (`if True: class Entry: ...` emits its stamps in the same
        // suite): walk them all recursively
        fn harvest_suite(
            suite: &[Stmt],
            binding_modules: &mut FxIndexMap<String, String>,
            binding_exports: &mut FxIndexMap<String, String>,
            record_stamp: &impl Fn(
                &mut FxIndexMap<String, String>,
                &mut FxIndexMap<String, String>,
                &Stmt,
            ),
        ) {
            for stmt in suite {
                match stmt {
                    Stmt::Try(try_stmt) => {
                        // Decorated definitions carry their stamps inside a
                        // try/except guard (decorators may return objects
                        // rejecting attribute writes), wrapped in an identity
                        // check
                        for guarded in &try_stmt.body {
                            if let Stmt::If(if_stmt) = guarded {
                                for conditional in &if_stmt.body {
                                    record_stamp(binding_modules, binding_exports, conditional);
                                }
                                continue;
                            }
                            record_stamp(binding_modules, binding_exports, guarded);
                        }
                        for handler in &try_stmt.handlers {
                            let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
                            harvest_suite(
                                &handler.body,
                                binding_modules,
                                binding_exports,
                                record_stamp,
                            );
                        }
                        harvest_suite(
                            &try_stmt.orelse,
                            binding_modules,
                            binding_exports,
                            record_stamp,
                        );
                        harvest_suite(
                            &try_stmt.finalbody,
                            binding_modules,
                            binding_exports,
                            record_stamp,
                        );
                    }
                    Stmt::If(if_stmt) => {
                        harvest_suite(
                            &if_stmt.body,
                            binding_modules,
                            binding_exports,
                            record_stamp,
                        );
                        for clause in &if_stmt.elif_else_clauses {
                            harvest_suite(
                                &clause.body,
                                binding_modules,
                                binding_exports,
                                record_stamp,
                            );
                        }
                    }
                    Stmt::For(for_stmt) => {
                        harvest_suite(
                            &for_stmt.body,
                            binding_modules,
                            binding_exports,
                            record_stamp,
                        );
                        harvest_suite(
                            &for_stmt.orelse,
                            binding_modules,
                            binding_exports,
                            record_stamp,
                        );
                    }
                    Stmt::While(while_stmt) => {
                        harvest_suite(
                            &while_stmt.body,
                            binding_modules,
                            binding_exports,
                            record_stamp,
                        );
                        harvest_suite(
                            &while_stmt.orelse,
                            binding_modules,
                            binding_exports,
                            record_stamp,
                        );
                    }
                    Stmt::With(with_stmt) => {
                        harvest_suite(
                            &with_stmt.body,
                            binding_modules,
                            binding_exports,
                            record_stamp,
                        );
                    }
                    Stmt::Match(match_stmt) => {
                        for case in &match_stmt.cases {
                            harvest_suite(
                                &case.body,
                                binding_modules,
                                binding_exports,
                                record_stamp,
                            );
                        }
                    }
                    _ => record_stamp(binding_modules, binding_exports, stmt),
                }
            }
        }
        harvest_suite(
            final_body,
            &mut binding_modules,
            &mut binding_exports,
            &record_stamp,
        );

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
            capture_statements: vec![],
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
            capture_statements: vec![],
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
