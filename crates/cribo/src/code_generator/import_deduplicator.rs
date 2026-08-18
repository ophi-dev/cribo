//! Import deduplication and cleanup utilities
//!
//! This module contains functions for finding and removing duplicate or unused imports,
//! and other import-related cleanup tasks during the bundling process.

use std::path::PathBuf;

use ruff_python_ast::{Alias, ModModule, Stmt, StmtImport, StmtImportFrom};

use super::bundler::Bundler;
use crate::{
    dependency_graph::DependencyGraph,
    types::{FxIndexMap, FxIndexSet},
};

/// Check if a statement is a hoisted import
pub(super) fn is_hoisted_import(_bundler: &Bundler<'_>, stmt: &Stmt) -> bool {
    match stmt {
        Stmt::ImportFrom(import_from) => {
            if let Some(ref module) = import_from.module {
                let module_name = module.as_str();
                // Check if this is a __future__ import (always hoisted)
                if module_name == "__future__" {
                    return true;
                }
                // Stdlib imports are no longer hoisted - handled by proxy
            }
            false
        }
        Stmt::Import(_import_stmt) => {
            // Stdlib imports are no longer hoisted - handled by proxy
            false
        }
        _ => false,
    }
}

/// Check if an import from statement is a duplicate
pub(super) fn is_duplicate_import_from(
    bundler: &Bundler<'_>,
    import_from: &StmtImportFrom,
    existing_body: &[Stmt],
    python_version: u8,
) -> bool {
    if let Some(ref module) = import_from.module {
        let module_name = module.as_str();
        // For third-party imports, check if they're already in the body
        // Check if it's a stdlib module
        let root_module = module_name.split('.').next().unwrap_or(module_name);
        let is_stdlib =
            ruff_python_stdlib::sys::is_known_standard_library(python_version, root_module);
        let is_third_party = !is_stdlib && !is_bundled_module_or_package(bundler, module_name);

        if is_third_party {
            return existing_body.iter().any(|existing| {
                if let Stmt::ImportFrom(existing_import) = existing {
                    existing_import
                        .module
                        .as_ref()
                        .map(ruff_python_ast::Identifier::as_str)
                        == Some(module_name)
                        && import_names_match(&import_from.names, &existing_import.names)
                } else {
                    false
                }
            });
        }
    }
    false
}

/// Check if an import statement is a duplicate
pub(super) fn is_duplicate_import(
    _bundler: &Bundler<'_>,
    import_stmt: &StmtImport,
    existing_body: &[Stmt],
) -> bool {
    import_stmt.names.iter().any(|alias| {
        existing_body.iter().any(|existing| {
            if let Stmt::Import(existing_import) = existing {
                existing_import.names.iter().any(|existing_alias| {
                    existing_alias.name == alias.name && existing_alias.asname == alias.asname
                })
            } else {
                false
            }
        })
    })
}

/// Check if two sets of import names match
pub(super) fn import_names_match(names1: &[Alias], names2: &[Alias]) -> bool {
    if names1.len() != names2.len() {
        return false;
    }
    // Check if all names match (order doesn't matter)
    names1.iter().all(|n1| {
        names2
            .iter()
            .any(|n2| n1.name == n2.name && n1.asname == n2.asname)
    })
}

/// Check if a module is bundled or is a package containing bundled modules
pub(super) fn is_bundled_module_or_package(bundler: &Bundler<'_>, module_name: &str) -> bool {
    // Direct check - convert module_name to ModuleId for lookup
    if bundler
        .get_module_id(module_name)
        .is_some_and(|id| bundler.bundled_modules.contains(&id))
    {
        return true;
    }
    // Check if it's a package containing bundled modules
    // e.g., if "greetings.greeting" is bundled, then "greetings" is a package
    let package_prefix = format!("{module_name}.");
    bundler.bundled_modules.iter().any(|bundled_id| {
        bundler
            .resolver
            .get_module_name(*bundled_id)
            .is_some_and(|name| name.starts_with(&package_prefix))
    })
}

/// Trim unused imports from modules using dependency graph analysis
pub(super) fn trim_unused_imports_from_modules(
    modules: &FxIndexMap<crate::resolver::ModuleId, (ModModule, PathBuf, String)>,
    graph: &DependencyGraph,
    tree_shaker: Option<&crate::tree_shaking::TreeShaker<'_>>,
    python_version: u8,
    circular_modules: &FxIndexSet<crate::resolver::ModuleId>,
) -> FxIndexMap<crate::resolver::ModuleId, (ModModule, PathBuf, String)> {
    let mut trimmed_modules = FxIndexMap::default();

    for (module_id, (ast, module_path, content_hash)) in modules {
        log::debug!("Trimming unused imports from module: {module_id:?}");
        let mut ast = ast.clone(); // Clone here to allow mutation

        // Check if this is an __init__.py file
        let is_init_py = module_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(crate::python::module_path::is_init_file_name);

        // Get unused imports from the graph
        if let Some(module_dep_graph) = graph.get_module(*module_id) {
            // Check if this module has side effects (will become a wrapper module)
            let has_side_effects = !module_dep_graph.side_effect_items.is_empty();

            if has_side_effects {
                log::debug!(
                    "Module {module_id:?} has side effects - skipping stdlib import removal"
                );
            }

            let mut unused_imports =
                crate::analyzers::import_analyzer::ImportAnalyzer::find_unused_imports_in_module(
                    module_dep_graph,
                    is_init_py,
                );

            // Skip tree-shaking based import removal for circular modules
            // Circular modules become init functions that include ALL their original code,
            // even the parts that would be tree-shaken, so we need to keep all imports
            let is_circular_module = circular_modules.contains(module_id);
            log::debug!(
                "Module {module_id:?} - checking if circular: {is_circular_module}, \
                 circular_modules: {circular_modules:?}"
            );
            if is_circular_module {
                log::debug!(
                    "Module {module_id:?} is circular - skipping tree-shaking based import removal"
                );
                // For circular modules, also preserve imports that are module-level symbols
                // These can be accessed from other modules as module attributes even if not
                // directly used within this module A module-level symbol is one
                // that is either:
                // 1. Imported at module level (not inside a function/class)
                // 2. In __all__ export list
                // 3. Explicitly re-exported
                let original_count = unused_imports.len();
                unused_imports.retain(|import_info| {
                    // Check if this symbol is in __all__ or explicitly re-exported
                    let is_in_all = module_dep_graph.is_in_all_export(&import_info.name);

                    // Check if any import item has this as a reexported name
                    let is_reexported = module_dep_graph
                        .items
                        .values()
                        .any(|item| item.reexported_names.contains(&import_info.name));

                    // Check if this import is at module level (any import item that imports this
                    // name) In circular modules, all imports at module level
                    // become module attributes in the init function
                    let is_module_level_import = module_dep_graph
                        .items
                        .values()
                        .any(|item| item.imported_names.contains(&import_info.name));

                    let should_preserve = is_in_all || is_reexported || is_module_level_import;

                    if should_preserve {
                        log::debug!(
                            "Preserving import '{}' in circular module - module-level import \
                             (in_all: {}, reexported: {}, module_level: {})",
                            import_info.name,
                            is_in_all,
                            is_reexported,
                            is_module_level_import
                        );
                    }

                    !should_preserve // Keep in unused list only if NOT to be preserved
                });

                if original_count != unused_imports.len() {
                    log::debug!(
                        "Filtered {} module-level imports from unused list in circular module",
                        original_count - unused_imports.len()
                    );
                }
            }

            // If tree shaking is enabled, also check if imported symbols were removed.
            // The shared analyzer collects module-level imports that are only used by
            // tree-shaken code; scoped imports are left to local unused-import analysis.
            if let Some(shaker) = tree_shaker
                && !is_circular_module
            {
                crate::analyzers::import_analyzer::extend_unused_imports_after_tree_shaking(
                    shaker,
                    *module_id,
                    module_dep_graph,
                    &mut unused_imports,
                );
            }

            if !unused_imports.is_empty() {
                // If this is a wrapper module (has side effects), filter out stdlib imports
                // from the unused list since they should be preserved as part of the module's API
                if has_side_effects {
                    let original_count = unused_imports.len();
                    unused_imports.retain(|import_info| {
                        // Check if this is a stdlib import
                        let root_module = import_info
                            .module
                            .split('.')
                            .next()
                            .unwrap_or(&import_info.module);
                        let is_stdlib = ruff_python_stdlib::sys::is_known_standard_library(
                            python_version,
                            root_module,
                        );

                        if is_stdlib {
                            log::debug!(
                                "Preserving stdlib import '{}' from '{}' in wrapper module",
                                import_info.name,
                                import_info.module
                            );
                            false // Remove from unused list (preserve the import)
                        } else {
                            true // Keep in unused list (will be removed)
                        }
                    });

                    if original_count != unused_imports.len() {
                        log::debug!(
                            "Filtered {} stdlib imports from unused list for wrapper module '{}'",
                            original_count - unused_imports.len(),
                            module_dep_graph.module_name
                        );
                    }
                }

                if !unused_imports.is_empty() {
                    log::debug!(
                        "Found {} unused imports in {}",
                        unused_imports.len(),
                        module_dep_graph.module_name
                    );
                    // Log unused imports details
                    log_unused_imports_details(&unused_imports);

                    // Filter out unused imports from the AST
                    ast.body
                        .retain(|stmt| !should_remove_import_stmt(stmt, &unused_imports));
                }
            }
        }

        trimmed_modules.insert(*module_id, (ast, module_path.clone(), content_hash.clone()));
    }

    log::debug!(
        "Successfully trimmed unused imports from {} modules",
        trimmed_modules.len()
    );
    trimmed_modules
}

/// Log details about unused imports for debugging
fn log_unused_imports_details(unused_imports: &[crate::analyzers::types::UnusedImportInfo]) {
    if log::log_enabled!(log::Level::Debug) {
        for unused in unused_imports {
            log::debug!("  - {} from {}", unused.name, unused.module);
        }
    }
}

/// Check if an import statement should be removed based on unused imports
fn should_remove_import_stmt(
    stmt: &Stmt,
    unused_imports: &[crate::analyzers::types::UnusedImportInfo],
) -> bool {
    match stmt {
        Stmt::Import(import_stmt) => {
            // Check if all names in this import are unused
            let should_remove = import_stmt.names.iter().all(|alias| {
                let local_name = alias
                    .asname
                    .as_ref()
                    .map_or(alias.name.as_str(), ruff_python_ast::Identifier::as_str);

                unused_imports.iter().any(|unused| {
                    log::trace!(
                        "Checking if import '{}' matches unused '{}' from '{}'",
                        local_name,
                        unused.name,
                        unused.module
                    );
                    // For regular imports, match by name only
                    unused.name == local_name
                })
            });

            if should_remove {
                log::debug!(
                    "Removing import statement: {:?}",
                    import_stmt
                        .names
                        .iter()
                        .map(|a| a.name.as_str())
                        .collect::<Vec<_>>()
                );
            }
            should_remove
        }
        Stmt::ImportFrom(import_from_stmt) => {
            // For from imports, we need to check if all imported names are unused
            let should_remove = import_from_stmt.names.iter().all(|alias| {
                let local_name = alias
                    .asname
                    .as_ref()
                    .map_or(alias.name.as_str(), ruff_python_ast::Identifier::as_str);

                // For relative imports (level > 0), we can't directly compare module names
                // since UnusedImportInfo has resolved names but import_from_stmt has raw syntax.
                // For absolute imports, we can compare the module names directly.
                if import_from_stmt.level > 0 {
                    // Relative import - just match by name since we can't easily resolve the module
                    // here This is safe because the UnusedImportInfo was
                    // created from the same module context
                    unused_imports
                        .iter()
                        .any(|unused| unused.name == local_name)
                } else {
                    // Absolute import - match by both name and module
                    let from_module = import_from_stmt
                        .module
                        .as_ref()
                        .map_or("", ruff_python_ast::Identifier::as_str);
                    unused_imports
                        .iter()
                        .any(|unused| unused.name == local_name && unused.module == from_module)
                }
            });

            if should_remove {
                log::debug!(
                    "Removing from import: from {} import {:?}",
                    import_from_stmt
                        .module
                        .as_ref()
                        .map_or("<None>", ruff_python_ast::Identifier::as_str),
                    import_from_stmt
                        .names
                        .iter()
                        .map(|a| a.name.as_str())
                        .collect::<Vec<_>>()
                );
            }
            should_remove
        }
        _ => false,
    }
}
