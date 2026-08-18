//! Third-party dependency detection (`cribo deps`).
//!
//! Given an entry Python file or a directory, follows all first-party sources and
//! their imports (reusing the bundler's module discovery, dependency graph, import
//! classification, and tree-shaking machinery) and reports the third-party
//! distributions the code requires, in `requirements.txt` or JSON form.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use indexmap::IndexMap;
use log::{debug, info, warn};
use serde::Serialize;

use crate::{
    analyzers::{
        import_analyzer::{ImportAnalyzer, extend_unused_imports_after_tree_shaking},
        types::UnusedImportInfo,
    },
    dependency_graph::{DependencyGraph, ItemType, ModuleDepGraph},
    orchestrator::BundleOrchestrator,
    requirement_resolver::RequirementResolver,
    resolver::{ImportOrigin, ModuleId, ModuleResolver},
    tree_shaking::TreeShaker,
    types::{FxIndexMap, FxIndexSet},
    visitors::{DiscoveredImport, ImportLocation, ScopeElement},
};

/// Options controlling third-party dependency detection.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DepsOptions {
    /// Skip imports that only occur inside `if TYPE_CHECKING:` blocks.
    pub exclude_type_checking: bool,
    /// Skip imports that only occur inside conditional control flow
    /// (`if`/`try`/`while`/`for` blocks outside of `TYPE_CHECKING`).
    pub exclude_conditional: bool,
}

/// One detected third-party (or unresolvable) import, aggregated across all
/// analyzed modules.
#[derive(Debug, Serialize)]
pub(crate) struct DetectedImport {
    /// Imported module name as written in source (e.g. `requests`, `google.cloud.storage`)
    pub module: String,
    /// Resolved PEP 508 requirement, when the import is included and resolvable
    pub requirement: Option<String>,
    /// Every occurrence sits inside an `if TYPE_CHECKING:` block
    pub type_checking_only: bool,
    /// Every occurrence sits inside conditional control flow (`if`/`try`/`while`/`for`)
    pub conditional: bool,
    /// Whether the import contributes to the emitted requirements (false when
    /// excluded by options or dropped by tree-shaking)
    pub included: bool,
    /// First-party modules that import it
    pub imported_by: Vec<String>,
}

/// Result of dependency detection.
#[derive(Debug, Serialize)]
pub(crate) struct DepsReport {
    /// Merged, sorted PEP 508 requirement lines
    pub requirements: Vec<String>,
    /// Per-import detail for every detected third-party import
    pub imports: Vec<DetectedImport>,
}

impl DepsReport {
    /// Render as `requirements.txt` content (one requirement per line).
    pub(crate) fn to_requirements_txt(&self) -> String {
        if self.requirements.is_empty() {
            String::new()
        } else {
            let mut content = self.requirements.join("\n");
            content.push('\n');
            content
        }
    }

    /// Render as pretty-printed JSON.
    pub(crate) fn to_json(&self) -> Result<String> {
        let mut content = serde_json::to_string_pretty(self)
            .context("Failed to serialize dependency report to JSON")?;
        content.push('\n');
        Ok(content)
    }
}

/// Aggregated occurrence data for one imported module name.
struct ImportOccurrences {
    /// All occurrences so far are `TYPE_CHECKING`-only
    all_type_checking: bool,
    /// All occurrences so far are conditional
    all_conditional: bool,
    /// At least one occurrence passed the filters and survived tree-shaking
    any_included: bool,
    /// First-party modules importing this name
    imported_by: FxIndexSet<String>,
    /// Preferred search root used to disambiguate distribution metadata
    search_root: Option<PathBuf>,
}

impl ImportOccurrences {
    fn new() -> Self {
        Self {
            all_type_checking: true,
            all_conditional: true,
            any_included: false,
            imported_by: FxIndexSet::default(),
            search_root: None,
        }
    }

    /// Fold one occurrence into the aggregate.
    fn record(
        &mut self,
        type_checking: bool,
        conditional: bool,
        included: bool,
        importing_module: &str,
        search_root: Option<PathBuf>,
    ) {
        self.all_type_checking &= type_checking;
        self.all_conditional &= conditional;
        self.any_included |= included;
        self.imported_by.insert(importing_module.to_owned());
        if self.search_root.is_none() {
            self.search_root = search_root;
        }
    }
}

/// Directories never descended into while scanning for Python sources.
const SKIPPED_SCAN_DIRECTORIES: &[&str] = &["__pycache__", "site-packages", "node_modules"];

/// Return whether a directory should be skipped while scanning for Python sources.
fn is_skipped_scan_directory(dir: &Path) -> bool {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.starts_with('.')
        || SKIPPED_SCAN_DIRECTORIES.contains(&name)
        // A directory with pyvenv.cfg is a virtual environment
        || dir.join("pyvenv.cfg").is_file()
}

/// Return directory entries sorted by path for deterministic traversal.
fn sorted_dir_entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("Failed to read directory: {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    Ok(entries)
}

fn is_python_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext == "py" || ext == "pyw")
}

/// Collect analysis roots for a scanned directory: top-level Python files become
/// individual roots, while package directories (containing `__init__.py`) become
/// single package roots handled by the regular entry machinery.
///
/// `visited` holds canonical paths of already walked directories so symlink
/// cycles cannot recurse forever.
fn scan_directory_roots(
    dir: &Path,
    roots: &mut Vec<PathBuf>,
    visited: &mut FxIndexSet<PathBuf>,
) -> Result<()> {
    if !mark_directory_visited(dir, visited) {
        return Ok(());
    }
    for path in sorted_dir_entries(dir)? {
        if path.is_dir() {
            if is_skipped_scan_directory(&path) {
                continue;
            }
            if path.join(crate::python::constants::INIT_FILE).is_file() {
                // Package root: bundle_core follows the package as one entry
                roots.push(path);
            } else {
                scan_directory_roots(&path, roots, visited)?;
            }
        } else if is_python_file(&path) {
            roots.push(path);
        }
    }
    Ok(())
}

/// Collect every Python file under `dir` (descending into packages) so a final
/// sweep can pick up files not reachable from any package or script root.
///
/// `visited` holds canonical paths of already walked directories so symlink
/// cycles cannot recurse forever.
fn collect_all_python_files(
    dir: &Path,
    files: &mut Vec<PathBuf>,
    visited: &mut FxIndexSet<PathBuf>,
) -> Result<()> {
    if !mark_directory_visited(dir, visited) {
        return Ok(());
    }
    for path in sorted_dir_entries(dir)? {
        if path.is_dir() {
            if !is_skipped_scan_directory(&path) {
                collect_all_python_files(&path, files, visited)?;
            }
        } else if is_python_file(&path) {
            files.push(path);
        }
    }
    Ok(())
}

/// Record a directory as visited by its canonical path; returns false when it
/// was walked before (e.g. reached again through a symlink cycle).
fn mark_directory_visited(dir: &Path, visited: &mut FxIndexSet<PathBuf>) -> bool {
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let first_visit = visited.insert(canonical);
    if !first_visit {
        debug!(
            "Skipping already visited directory {} (symlink cycle?)",
            dir.display()
        );
    }
    first_visit
}

/// Return whether an import statement location is conditional control flow.
///
/// `TYPE_CHECKING` blocks are conditional too, but they are tracked through the
/// dedicated `is_type_checking_only` flag, so callers keep the two categories
/// disjoint.
fn is_conditional_location(location: &ImportLocation) -> bool {
    match location {
        ImportLocation::Conditional { .. } => true,
        ImportLocation::Nested(elements) => elements.iter().any(|element| {
            matches!(
                element,
                ScopeElement::If | ScopeElement::Try | ScopeElement::While | ScopeElement::For
            )
        }),
        ImportLocation::Module
        | ImportLocation::Function(_)
        | ImportLocation::Class(_)
        | ImportLocation::Method { .. } => false,
    }
}

/// Compute the set of imported module names whose module-level import statements
/// survive tree-shaking in one module.
///
/// Scoped imports (inside functions/classes) are conservatively retained, matching
/// the shared unused-import analysis, which defers them to local analysis.
fn retained_import_modules(
    module_dep_graph: &ModuleDepGraph,
    unused_imports: &[UnusedImportInfo],
) -> FxIndexSet<String> {
    let mut retained = FxIndexSet::default();
    for (_, item) in module_dep_graph.get_all_import_items() {
        match &item.item_type {
            ItemType::Import { module, .. } => {
                let binding = item.var_decls.iter().next().map_or_else(
                    || module.split('.').next().unwrap_or(module),
                    String::as_str,
                );
                let dropped = item.containing_scope.is_none()
                    && unused_imports.iter().any(|unused| unused.name == binding);
                if !dropped {
                    retained.insert(module.clone());
                }
            }
            ItemType::FromImport {
                module,
                names,
                level,
                ..
            } => {
                // Relative imports are first-party by construction; only absolute
                // imports can reach requirements
                if *level > 0 {
                    continue;
                }
                let dropped = item.containing_scope.is_none()
                    && names.iter().all(|(name, alias)| {
                        let local_name = alias.as_ref().unwrap_or(name);
                        unused_imports
                            .iter()
                            .any(|unused| unused.name == *local_name && unused.module == *module)
                    });
                if !dropped {
                    retained.insert(module.clone());
                }
            }
            _ => {}
        }
    }
    retained
}

/// Mutable state accumulated across analysis roots.
#[derive(Default)]
struct DepsAggregation {
    /// Aggregated occurrences per imported module name
    imports: FxIndexMap<String, ImportOccurrences>,
    /// Canonical paths of every analyzed module file
    covered_files: FxIndexSet<PathBuf>,
    /// Distribution-metadata search directories contributed by every resolver
    metadata_directories: FxIndexSet<PathBuf>,
}

impl BundleOrchestrator {
    /// Detect third-party dependencies for an entry file or directory.
    ///
    /// Reuses the bundler pipeline up to (but excluding) code generation: module
    /// discovery, dependency-graph construction, import classification against the
    /// active environment (entry directory, `PYTHONPATH`, configured `src`,
    /// virtualenv site-packages), and optionally tree-shaking.
    pub(crate) fn analyze_deps(
        &mut self,
        entry_path: &Path,
        options: DepsOptions,
    ) -> Result<DepsReport> {
        // Dependency detection never inlines third-party sources: discovery must
        // keep site-packages imports external regardless of bundling configuration
        self.set_bundle_third_party(false);

        let (roots, directory_scan_root) = collect_analysis_roots(entry_path)?;

        let mut aggregation = DepsAggregation::default();
        // Failures are fatal for a single explicit entry, but a directory scan is
        // best-effort per root: one broken or orphaned file must not hide the
        // dependencies of every other file
        let fail_fast = directory_scan_root.is_none();

        for root in &roots {
            self.analyze_deps_root(root, options, fail_fast, &mut aggregation)?;
        }

        // Sweep: files inside scanned packages that no root reaches (e.g. package
        // submodules never imported by their package) still contribute dependencies
        if let Some(scan_root) = &directory_scan_root {
            let mut all_files = Vec::new();
            let mut visited = FxIndexSet::default();
            collect_all_python_files(scan_root, &mut all_files, &mut visited)?;
            for file in all_files {
                let canonical = file.canonicalize().unwrap_or_else(|_| file.clone());
                if aggregation.covered_files.contains(&canonical) {
                    continue;
                }
                self.analyze_deps_root(&file, options, false, &mut aggregation)?;
            }
        }

        Self::resolve_deps_report(&self.config().requirements, aggregation)
    }

    /// Analyze one root (file, or package/`__main__` directory) and fold its
    /// third-party imports into the aggregate.
    fn analyze_deps_root(
        &mut self,
        root: &Path,
        options: DepsOptions,
        fail_fast: bool,
        aggregation: &mut DepsAggregation,
    ) -> Result<()> {
        if root.is_file() {
            let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
            if aggregation.covered_files.contains(&canonical) {
                debug!(
                    "Skipping root {} - already analyzed through another root",
                    root.display()
                );
                return Ok(());
            }
        }
        info!("Analyzing dependencies of {}", root.display());

        let mut graph = DependencyGraph::new();
        let mut resolver_opt: Option<ModuleResolver> = None;
        let bundle_result = self.bundle_core(root, &mut graph, &mut resolver_opt);
        let circular_dep_analysis = match bundle_result {
            Ok((_, _, circular_dep_analysis)) => circular_dep_analysis,
            Err(error) => {
                if fail_fast {
                    return Err(error);
                }
                warn!("Skipping {}: analysis failed: {error:#}", root.display());
                return Ok(());
            }
        };
        let resolver = resolver_opt.expect("Resolver should be initialized by bundle_core");

        let sorted_module_ids =
            self.get_sorted_modules_from_graph(&graph, circular_dep_analysis.as_ref())?;

        // Obey tree-shaking configuration: when enabled, imports referenced only by
        // shaken code are dropped exactly like in the emitted bundle
        let tree_shaker = if self.config().tree_shake {
            let mut shaker = TreeShaker::from_graph(&graph, &resolver);
            let entry_name = resolver
                .get_module_name(ModuleId::ENTRY)
                .unwrap_or_else(|| "__main__".to_owned());
            shaker.analyze(&entry_name);
            Some(shaker)
        } else {
            None
        };

        // Circular modules keep all their imports, mirroring code generation
        let circular_modules: FxIndexSet<ModuleId> = circular_dep_analysis
            .as_ref()
            .map(|analysis| {
                analysis
                    .resolvable_cycles
                    .iter()
                    .flat_map(|cycle| cycle.modules.iter().copied())
                    .collect()
            })
            .unwrap_or_default();

        for module_id in &sorted_module_ids {
            self.collect_module_imports(CollectModuleImportsParams {
                module_id: *module_id,
                graph: &graph,
                resolver: &resolver,
                tree_shaker: tree_shaker.as_ref(),
                circular_modules: &circular_modules,
                options,
                aggregation,
            });
        }

        // Static importlib.import_module targets recorded during discovery never
        // appear as import statements; they execute unconditionally at runtime
        for target in self.importlib_requirement_targets() {
            let classification = resolver.classify_import(&target);
            if matches!(
                classification.origin,
                ImportOrigin::ThirdParty | ImportOrigin::Unknown
            ) {
                let search_root = resolver.get_import_search_root(&target);
                aggregation
                    .imports
                    .entry(target)
                    .or_insert_with(ImportOccurrences::new)
                    .record(false, false, true, "<importlib>", search_root);
            }
        }

        // Each root's resolver may contribute different search directories (its
        // own entry directory and environment paths): merge them so imports from
        // every root can be mapped to installed distribution metadata
        aggregation
            .metadata_directories
            .extend(resolver.get_distribution_metadata_search_directories());
        Ok(())
    }

    /// Fold one graph module's third-party imports into the aggregate.
    fn collect_module_imports(&self, params: CollectModuleImportsParams<'_, '_>) {
        let CollectModuleImportsParams {
            module_id,
            graph,
            resolver,
            tree_shaker,
            circular_modules,
            options,
            aggregation,
        } = params;
        let Some(module_path) = resolver.get_module_path(module_id) else {
            return;
        };
        aggregation.covered_files.insert(module_path.clone());

        let Some(module_dep_graph) = graph.get_module(module_id) else {
            return;
        };
        let module_name = module_dep_graph.module_name.clone();

        // Namespace packages carry no code and produce no facts
        let Some(facts) = self.cached_module_facts(&module_path) else {
            return;
        };

        // Under tree-shaking, compute which module-level import statements survive
        let retained = tree_shaker
            .filter(|_| !circular_modules.contains(&module_id))
            .map(|shaker| {
                let is_init_py = module_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(crate::python::module_path::is_init_file_name);
                let mut unused_imports =
                    ImportAnalyzer::find_unused_imports_in_module(module_dep_graph, is_init_py);
                extend_unused_imports_after_tree_shaking(
                    shaker,
                    module_id,
                    module_dep_graph,
                    &mut unused_imports,
                );
                retained_import_modules(module_dep_graph, &unused_imports)
            });

        for import in &facts.discovered_imports {
            record_discovered_import(RecordImportParams {
                import,
                module_name: &module_name,
                resolver,
                retained: retained.as_ref(),
                options,
                aggregated: &mut aggregation.imports,
            });
        }
    }

    /// Resolve aggregated imports to requirements and assemble the final report.
    fn resolve_deps_report(
        requirements_config: &crate::config::RequirementsConfig,
        aggregation: DepsAggregation,
    ) -> Result<DepsReport> {
        let DepsAggregation {
            imports: aggregated,
            metadata_directories,
            ..
        } = aggregation;
        let mut requirement_imports: IndexMap<String, Option<PathBuf>> = IndexMap::new();
        for (name, occurrences) in &aggregated {
            if occurrences.any_included {
                requirement_imports.insert(name.clone(), occurrences.search_root.clone());
            }
        }

        let resolutions = if requirement_imports.is_empty() {
            IndexMap::new()
        } else {
            RequirementResolver::new(
                requirements_config,
                metadata_directories.into_iter().collect(),
            )
            .resolve_detailed(&requirement_imports)?
        };

        let requirements =
            Self::merge_requirement_entries(resolutions.values().flatten().cloned().collect());

        let mut imports: Vec<DetectedImport> = aggregated
            .into_iter()
            .map(|(module, occurrences)| DetectedImport {
                requirement: resolutions.get(&module).cloned().flatten(),
                type_checking_only: occurrences.all_type_checking,
                conditional: occurrences.all_conditional,
                included: occurrences.any_included,
                imported_by: occurrences.imported_by.into_iter().collect(),
                module,
            })
            .collect();
        imports.sort_by(|a, b| a.module.cmp(&b.module));

        Ok(DepsReport {
            requirements,
            imports,
        })
    }
}

/// Parameters for folding one module's imports into the aggregate.
struct CollectModuleImportsParams<'a, 'shaker> {
    module_id: ModuleId,
    graph: &'a DependencyGraph,
    resolver: &'a ModuleResolver,
    tree_shaker: Option<&'a TreeShaker<'shaker>>,
    circular_modules: &'a FxIndexSet<ModuleId>,
    options: DepsOptions,
    aggregation: &'a mut DepsAggregation,
}

/// Parameters for recording one discovered import occurrence.
struct RecordImportParams<'a> {
    import: &'a DiscoveredImport,
    module_name: &'a str,
    resolver: &'a ModuleResolver,
    retained: Option<&'a FxIndexSet<String>>,
    options: DepsOptions,
    aggregated: &'a mut FxIndexMap<String, ImportOccurrences>,
}

/// Record one discovered import occurrence into the aggregate when it targets a
/// third-party (or unresolvable) module.
fn record_discovered_import(params: RecordImportParams<'_>) {
    let RecordImportParams {
        import,
        module_name,
        resolver,
        retained,
        options,
        aggregated,
    } = params;

    // Relative imports are always first-party
    if import.level > 0 {
        return;
    }
    let Some(imported_module) = &import.module_name else {
        return;
    };
    if imported_module.starts_with('.') {
        return;
    }

    let classification = resolver.classify_import(imported_module);
    if !matches!(
        classification.origin,
        ImportOrigin::ThirdParty | ImportOrigin::Unknown
    ) {
        return;
    }

    let type_checking = import.is_type_checking_only;
    let conditional = !type_checking && is_conditional_location(&import.location);
    // Inclusion policy: type-checking-only and conditional imports are never
    // "used" by surviving runtime code by their very nature, so they are governed
    // exclusively by their dedicated exclusion options. Tree-shaking (when
    // enabled) governs the remaining, unconditional imports
    let included = if type_checking {
        !options.exclude_type_checking
    } else if conditional {
        !options.exclude_conditional
    } else {
        retained.is_none_or(|retained| retained.contains(imported_module.as_str()))
    };

    debug!(
        "Detected third-party import '{imported_module}' in '{module_name}' (type_checking: \
         {type_checking}, conditional: {conditional}, included: {included})"
    );

    let search_root = resolver.get_import_search_root(imported_module);
    aggregated
        .entry(imported_module.clone())
        .or_insert_with(ImportOccurrences::new)
        .record(
            type_checking,
            conditional,
            included,
            module_name,
            search_root,
        );
}

/// Determine the analysis roots for the given entry path.
///
/// Returns the roots plus the scanned directory when the entry is a plain source
/// directory (not a package or `__main__` directory), which enables the
/// best-effort per-root behavior and the final coverage sweep.
fn collect_analysis_roots(entry_path: &Path) -> Result<(Vec<PathBuf>, Option<PathBuf>)> {
    if entry_path.is_file() {
        return Ok((vec![entry_path.to_path_buf()], None));
    }
    if !entry_path.is_dir() {
        return Err(anyhow!(
            "Entry path {} does not exist or is not a file or directory",
            entry_path.display()
        ));
    }

    // A package or runnable directory is one root handled by the entry machinery
    let init_file = entry_path.join(crate::python::constants::INIT_FILE);
    let main_file = entry_path.join(crate::python::constants::MAIN_FILE);
    if init_file.is_file() || main_file.is_file() {
        return Ok((vec![entry_path.to_path_buf()], None));
    }

    let mut roots = Vec::new();
    let mut visited = FxIndexSet::default();
    scan_directory_roots(entry_path, &mut roots, &mut visited)?;
    if roots.is_empty() {
        return Err(anyhow!(
            "No Python files found in directory {}",
            entry_path.display()
        ));
    }
    Ok((roots, Some(entry_path.to_path_buf())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_conditional_location_detection() {
        assert!(is_conditional_location(&ImportLocation::Conditional {
            depth: 1
        }));
        assert!(is_conditional_location(&ImportLocation::Nested(vec![
            ScopeElement::Function("handler".to_owned()),
            ScopeElement::Try,
        ])));
        assert!(is_conditional_location(&ImportLocation::Nested(vec![
            ScopeElement::If
        ])));
        assert!(!is_conditional_location(&ImportLocation::Module));
        assert!(!is_conditional_location(&ImportLocation::Function(
            "handler".to_owned()
        )));
        assert!(!is_conditional_location(&ImportLocation::Nested(vec![
            ScopeElement::Function("handler".to_owned()),
            ScopeElement::With,
        ])));
    }

    #[test]
    fn test_report_rendering() {
        let report = DepsReport {
            requirements: vec!["requests".to_owned(), "rich>=13".to_owned()],
            imports: vec![],
        };
        assert_eq!(report.to_requirements_txt(), "requests\nrich>=13\n");

        let empty = DepsReport {
            requirements: vec![],
            imports: vec![],
        };
        assert_eq!(empty.to_requirements_txt(), "");
    }
}
