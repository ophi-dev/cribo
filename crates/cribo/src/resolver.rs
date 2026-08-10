use std::{
    cell::RefCell,
    ffi::OsStr,
    fmt,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context, Result, anyhow};
use cow_utils::CowUtils;
use indexmap::{IndexMap, IndexSet};
use log::{debug, info, warn};
use ruff_python_stdlib::sys;

use crate::{
    config::Config,
    types::{FxIndexMap, FxIndexSet},
};

pub(crate) const AUTO_DETECTED_VIRTUALENV_NAMES: [&str; 5] =
    [".venv", "venv", "env", ".virtualenv", "virtualenv"];

/// Unique identifier for a module in the dependency graph
/// The entry module ALWAYS has ID 0 - this is a fundamental invariant
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleId(pub u32);

impl ModuleId {
    /// The entry point - always ID 0
    /// This is where bundling starts, the origin of our module universe
    pub const ENTRY: Self = Self(0);

    #[inline]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Check if this is the entry module
    /// No more complex path detection or boolean flags!
    #[inline]
    pub const fn is_entry(self) -> bool {
        self.0 == 0
    }

    /// Format this `ModuleId` with the resolver to show the module name and path
    /// This is useful for debugging and error messages
    pub fn format_with_resolver(self, resolver: &ModuleResolver) -> String {
        resolver.get_module_name(self).map_or_else(
            || format!("ModuleId({})", self.0),
            |name| {
                resolver.get_module_path(self).map_or_else(
                    || format!("ModuleId({})='{}'", self.0, name),
                    |path| format!("ModuleId({})='{}' at '{}'", self.0, name, path.display()),
                )
            },
        )
    }
}

impl fmt::Display for ModuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "module#{}", self.0)
    }
}

impl From<u32> for ModuleId {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<ModuleId> for u32 {
    fn from(value: ModuleId) -> Self {
        value.0
    }
}

/// Module metadata tracked by resolver
#[derive(Debug, Clone)]
pub struct ModuleMetadata {
    pub id: ModuleId,
    pub name: String,
    pub canonical_path: PathBuf,
    pub is_package: bool,
    pub kind: crate::python::module_path::ModuleKind,
}

/// Internal module registry for ID allocation
#[derive(Debug)]
struct ModuleRegistry {
    next_id: u32,
    by_id: FxIndexMap<ModuleId, ModuleMetadata>,
    by_name: FxIndexMap<String, ModuleId>,
    by_path: FxIndexMap<PathBuf, ModuleId>,
}

impl ModuleRegistry {
    fn new() -> Self {
        Self {
            next_id: 0, // Start at 0 - entry point gets this
            by_id: FxIndexMap::default(),
            by_name: FxIndexMap::default(),
            by_path: FxIndexMap::default(),
        }
    }

    fn register(&mut self, name: String, path: &Path) -> Result<ModuleId> {
        // `path` is expected to be canonicalized by the caller
        let canonical_path = path.to_owned();

        if let Some(&id) = self.by_name.get(&name) {
            let existing = self
                .by_id
                .get(&id)
                .expect("Module name must reference registered metadata");
            if existing.canonical_path == canonical_path {
                return Ok(id);
            }
            return Err(anyhow!(
                "Import name '{}' resolves to conflicting files: '{}' and '{}'",
                name,
                existing.canonical_path.display(),
                canonical_path.display()
            ));
        }

        if let Some(&id) = self.by_path.get(&canonical_path) {
            self.by_name.insert(name, id);
            return Ok(id);
        }

        // Allocate ID - entry gets 0, others get sequential IDs
        let id = ModuleId::new(self.next_id);
        self.next_id += 1;

        // The beauty: first registered module (entry) automatically gets ID 0!
        debug_assert!(
            id != ModuleId::ENTRY || self.by_id.is_empty(),
            "Entry module must be registered first"
        );

        // Determine whether this path represents a package and its kind,
        // including support for PEP 420 namespace packages.
        let (is_package, kind) = if path.is_dir() {
            // A directory without __init__.py is a namespace package
            debug_assert!(
                !crate::python::module_path::is_package_dir_with_init(path),
                "register_module(name, path) should receive the __init__.py file for regular \
                 packages; got a directory: {}",
                path.display()
            );
            (
                true,
                crate::python::module_path::ModuleKind::NamespacePackageDir,
            )
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(crate::python::module_path::is_init_file_name)
        {
            // A file named __init__.py (or equivalent) is a regular package init
            (true, crate::python::module_path::ModuleKind::PackageInit)
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == crate::python::constants::MAIN_FILE)
        {
            // A file named __main__.py
            (false, crate::python::module_path::ModuleKind::Main)
        } else {
            // Any other file is a regular module
            (false, crate::python::module_path::ModuleKind::RegularModule)
        };

        let metadata = ModuleMetadata {
            id,
            name: name.clone(),
            canonical_path: canonical_path.clone(),
            is_package,
            kind,
        };

        self.by_id.insert(id, metadata);
        self.by_name.insert(name, id);
        self.by_path.insert(canonical_path, id);

        Ok(id)
    }

    fn get_metadata(&self, id: ModuleId) -> Option<&ModuleMetadata> {
        self.by_id.get(&id)
    }

    fn get_id_by_name(&self, name: &str) -> Option<&ModuleId> {
        self.by_name.get(name)
    }
}

/// Resolve a relative import based on module name (standalone utility)
///
/// This is a pure function that resolves relative imports based on module names alone,
/// without requiring a resolver instance. Used by both `ModuleResolver` and `ImportAnalyzer`.
///
/// # Arguments
/// * `level` - The number of leading dots in the relative import
/// * `name` - The module name after the dots (if any)
/// * `current_module_name` - The name of the module performing the import
///
/// # Returns
/// The resolved absolute module name
pub(crate) fn resolve_relative_import_from_name(
    level: u32,
    name: Option<&str>,
    current_module_name: &str,
) -> String {
    let mut package_parts: Vec<&str> = current_module_name.split('.').collect();

    // For modules (not packages), we need to remove the module itself first
    // then go up additional levels
    // Check if this is likely a package (__init__) or a regular module
    let is_likely_package = package_parts
        .last()
        .is_some_and(|part| *part == crate::python::constants::INIT_STEM);

    if !is_likely_package && package_parts.len() > 1 {
        // Remove the module name itself for regular modules
        package_parts.pop();
    }

    // Go up additional levels based on the import level
    // Level 1 means current package, level 2 means parent, etc.
    for _ in 1..level {
        if package_parts.is_empty() {
            break; // Can't go up any further
        }
        package_parts.pop();
    }

    // Append the name part if provided
    if let Some(name_part) = name
        && !name_part.is_empty()
    {
        package_parts.push(name_part);
    }

    package_parts.join(".")
}

/// Check if a module is part of the Python standard library using `ruff_python_stdlib`
pub(crate) fn is_stdlib_module(module_name: &str, python_version: u8) -> bool {
    // Check direct match using ruff_python_stdlib
    if sys::is_known_standard_library(python_version, module_name) {
        return true;
    }

    // Check if it's a submodule of a stdlib module
    module_name
        .split('.')
        .next()
        .is_some_and(|top_level| sys::is_known_standard_library(python_version, top_level))
}

/// Where an imported module originates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportOrigin {
    FirstParty,
    ThirdParty,
    StandardLibrary,
    Unknown,
}

/// The artifact available for an imported module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSource {
    Python,
    NamespacePackage,
    NativeExtension,
    Unresolved,
}

/// Whether the current bundling policy includes the module's source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleDisposition {
    Include,
    External,
}

/// Independent import facts used by discovery, code generation, and requirements output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportClassification {
    pub origin: ImportOrigin,
    pub source: ImportSource,
    pub bundle: BundleDisposition,
}

impl ImportClassification {
    /// Construct an import classification from its independent resolution facts.
    const fn new(origin: ImportOrigin, source: ImportSource, bundle: BundleDisposition) -> Self {
        Self {
            origin,
            source,
            bundle,
        }
    }

    /// Whether the bundler should include the module's Python source.
    pub const fn should_bundle(&self) -> bool {
        matches!(self.bundle, BundleDisposition::Include)
    }

    /// Whether module resolution found an importable source artifact.
    pub const fn is_resolved(&self) -> bool {
        !matches!(self.source, ImportSource::Unresolved)
    }

    /// Whether the module is a native extension that must remain an external import.
    pub const fn is_external_native_module(&self) -> bool {
        matches!(self.source, ImportSource::NativeExtension)
            && matches!(self.bundle, BundleDisposition::External)
    }
}

#[derive(Debug, Clone)]
struct ResolvedModule {
    path: PathBuf,
    source: ImportSource,
}

impl ResolvedModule {
    fn bundle_path(&self) -> Option<PathBuf> {
        matches!(
            self.source,
            ImportSource::Python | ImportSource::NamespacePackage
        )
        .then(|| self.path.clone())
    }
}

/// Module descriptor for import resolution
#[derive(Debug)]
struct ImportModuleDescriptor {
    /// Number of leading dots for relative imports
    leading_dots: usize,
    /// Module name parts (e.g., `["foo", "bar"]` for `"foo.bar"`)
    name_parts: Vec<String>,
}

impl ImportModuleDescriptor {
    fn from_module_name(name: &str) -> Self {
        let leading_dots = name.chars().take_while(|c| *c == '.').count();
        let name_parts = name
            .chars()
            .skip_while(|c| *c == '.')
            .collect::<String>()
            .split('.')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();

        Self {
            leading_dots,
            name_parts,
        }
    }
}

/// AST visitor detecting patterns that make a package unsafe to inline into a bundle:
///
/// - imports of runtime package-data and metadata APIs that require an installed
///   distribution on disk (`importlib.metadata`, `importlib_metadata`, `pkg_resources`,
///   `importlib.resources`, `importlib_resources`)
/// - dynamic imports with non-literal module names (including through aliases such as
///   `from importlib import import_module as load`), whose loaded modules cannot be
///   discovered statically
#[derive(Default)]
struct UnbundlablePatternDetector {
    found: bool,
    /// Local aliases bound to `importlib.import_module` via `from importlib import ...`
    dynamic_import_aliases: IndexSet<String>,
}

impl UnbundlablePatternDetector {
    const INSTALLED_PACKAGE_APIS: [&'static str; 5] = [
        "importlib.metadata",
        "importlib_metadata",
        "pkg_resources",
        "importlib.resources",
        "importlib_resources",
    ];

    /// Return whether an imported module name is (or is inside) an API module that
    /// requires an installed distribution.
    fn module_is_metadata_api(module_name: &str) -> bool {
        Self::INSTALLED_PACKAGE_APIS.iter().any(|api| {
            module_name == *api
                || module_name
                    .strip_prefix(api)
                    .is_some_and(|rest| rest.starts_with('.'))
        })
    }

    /// Return how a call expression relates to dynamic importing, if at all.
    fn dynamic_import_kind(&self, callee: &ruff_python_ast::Expr) -> Option<DynamicImportKind> {
        use ruff_python_ast::Expr;
        match callee {
            Expr::Attribute(attribute) if attribute.attr.as_str() == "import_module" => {
                Some(DynamicImportKind::ImportModule)
            }
            Expr::Name(name) if name.id.as_str() == "__import__" => {
                Some(DynamicImportKind::DunderImport)
            }
            Expr::Name(name)
                if name.id.as_str() == "import_module"
                    || self.dynamic_import_aliases.contains(name.id.as_str()) =>
            {
                Some(DynamicImportKind::ImportModule)
            }
            _ => None,
        }
    }

    /// Record every alias bound to `importlib.import_module` anywhere in the module
    /// (including inside `try`/`if` blocks and function bodies) before the detection
    /// pass, so calls in functions defined above the alias binding are still
    /// recognized. Both import aliases (`from importlib import import_module as X`)
    /// and assignment aliases (`X = importlib.import_module`) are collected.
    /// Over-collection is safe: it can only classify more packages as external.
    fn collect_dynamic_import_aliases(&mut self, body: &[ruff_python_ast::Stmt]) {
        struct AliasCollector<'aliases> {
            aliases: &'aliases mut IndexSet<String>,
        }

        impl<'a> ruff_python_ast::visitor::Visitor<'a> for AliasCollector<'_> {
            fn visit_stmt(&mut self, stmt: &'a ruff_python_ast::Stmt) {
                use ruff_python_ast::Stmt;
                match stmt {
                    Stmt::ImportFrom(import_from)
                        if import_from.level == 0
                            && import_from.module.as_deref() == Some("importlib") =>
                    {
                        record_import_module_aliases(import_from, self.aliases);
                    }
                    Stmt::Assign(assign) => {
                        record_assigned_import_module_aliases(assign, self.aliases);
                    }
                    _ => {}
                }
                ruff_python_ast::visitor::walk_stmt(self, stmt);
            }
        }

        use ruff_python_ast::visitor::Visitor as _;
        let mut collector = AliasCollector {
            aliases: &mut self.dynamic_import_aliases,
        };
        for stmt in body {
            collector.visit_stmt(stmt);
        }
    }
}

/// Track local names bound to `importlib.import_module` by a `from importlib
/// import ...` statement.
fn record_import_module_aliases(
    import_from: &ruff_python_ast::StmtImportFrom,
    aliases: &mut IndexSet<String>,
) {
    for alias in &import_from.names {
        if alias.name.as_str() == "import_module" {
            let bound_name = alias
                .asname
                .as_ref()
                .map_or_else(|| alias.name.as_str(), |name| name.as_str());
            aliases.insert(bound_name.to_owned());
        }
    }
}

/// Track callable aliases created by assignments such as
/// `load = importlib.import_module` or `load = import_module`.
fn record_assigned_import_module_aliases(
    assign: &ruff_python_ast::StmtAssign,
    aliases: &mut IndexSet<String>,
) {
    use ruff_python_ast::Expr;
    let value_is_import_module = match assign.value.as_ref() {
        Expr::Attribute(attribute) => attribute.attr.as_str() == "import_module",
        Expr::Name(name) => {
            name.id.as_str() == "import_module" || aliases.contains(name.id.as_str())
        }
        _ => false,
    };
    if !value_is_import_module {
        return;
    }
    for target in &assign.targets {
        if let Expr::Name(name) = target {
            aliases.insert(name.id.to_string());
        }
    }
}

/// The dynamic-import mechanism used by a call expression.
enum DynamicImportKind {
    /// `importlib.import_module(...)` (possibly aliased); literal arguments are
    /// statically discovered, so only non-literal ones block bundling
    ImportModule,
    /// `__import__(...)`; import discovery never resolves these, so any call blocks
    /// bundling
    DunderImport,
}

impl<'a> ruff_python_ast::visitor::Visitor<'a> for UnbundlablePatternDetector {
    fn visit_stmt(&mut self, stmt: &'a ruff_python_ast::Stmt) {
        use ruff_python_ast::Stmt;
        if self.found {
            return;
        }
        match stmt {
            Stmt::Import(import_stmt) => {
                if import_stmt
                    .names
                    .iter()
                    .any(|alias| Self::module_is_metadata_api(alias.name.as_str()))
                {
                    self.found = true;
                    return;
                }
            }
            Stmt::ImportFrom(import_from) if import_from.level == 0 => {
                if let Some(module) = import_from.module.as_deref() {
                    if Self::module_is_metadata_api(module)
                        || (module == "importlib"
                            && import_from.names.iter().any(|alias| {
                                matches!(alias.name.as_str(), "metadata" | "resources")
                            }))
                    {
                        self.found = true;
                        return;
                    }
                    // Track aliases of importlib.import_module so aliased dynamic
                    // calls (e.g. `load(f".{name}")`) are recognized below
                    if module == "importlib" {
                        record_import_module_aliases(import_from, &mut self.dynamic_import_aliases);
                    }
                }
            }
            _ => {}
        }
        ruff_python_ast::visitor::walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'a ruff_python_ast::Expr) {
        use ruff_python_ast::Expr;
        if self.found {
            return;
        }
        // Dynamic imports whose targets are not statically discovered would be missing
        // from the bundle: non-literal (or missing) `import_module` names cannot be
        // resolved, and `__import__` calls are never handled by import discovery at all
        if let Expr::Call(call) = expr {
            match self.dynamic_import_kind(&call.func) {
                Some(DynamicImportKind::DunderImport) => {
                    self.found = true;
                    return;
                }
                Some(DynamicImportKind::ImportModule)
                    if !crate::python::importlib_call::module_name_argument(call)
                        .is_some_and(|argument| matches!(argument, Expr::StringLiteral(_))) =>
                {
                    self.found = true;
                    return;
                }
                _ => {}
            }
        }
        ruff_python_ast::visitor::walk_expr(self, expr);
    }
}

/// Ownership and policy facts for one installed distribution (a `.dist-info` entry).
#[derive(Debug, Default)]
struct DistributionInfo {
    /// Distribution name from `METADATA` (e.g. "pure-helper")
    name: String,
    /// Raw `Requires-Python` specifier, when declared
    requires_python: Option<String>,
    /// Raw `Requires-Dist` entries, when declared
    requires_dist: Vec<String>,
    /// Whether the distribution installs native artifacts (`.so`/`.pyd`) per `RECORD`
    ships_native_artifacts: bool,
    /// Import prefixes declared via `Import-Name`/`Import-Namespace`
    declared_prefixes: IndexSet<String>,
    /// Import names implied by installed `RECORD` files
    record_imports: IndexSet<String>,
}

impl DistributionInfo {
    /// Return whether this distribution's metadata or installed files claim an import.
    fn owns_import(&self, import_name: &str) -> bool {
        self.record_imports.contains(import_name)
            || self.declared_prefixes.iter().any(|prefix| {
                import_name == prefix
                    || import_name
                        .strip_prefix(prefix.as_str())
                        .is_some_and(|suffix| suffix.starts_with('.'))
            })
    }
}

/// All installed distributions discovered under one search root.
#[derive(Debug, Default)]
struct DistributionOwnershipIndex {
    distributions: Vec<DistributionInfo>,
}

impl DistributionOwnershipIndex {
    /// Return the distributions claiming an import.
    ///
    /// Exact `RECORD`/installed-files ownership takes precedence over declared
    /// namespace prefixes: in a namespace package split across distributions, only the
    /// distribution actually installing the child module owns it. When no exact owner
    /// exists, all prefix claimants are returned so policy checks can aggregate
    /// conservatively instead of depending on directory iteration order.
    fn owning_distributions(&self, import_name: &str) -> Vec<&DistributionInfo> {
        let record_owners: Vec<&DistributionInfo> = self
            .distributions
            .iter()
            .filter(|distribution| distribution.record_imports.contains(import_name))
            .collect();
        if !record_owners.is_empty() {
            return record_owners;
        }
        self.distributions
            .iter()
            .filter(|distribution| distribution.owns_import(import_name))
            .collect()
    }

    /// Return whether indexed metadata or installed files claim an import.
    fn owns_import(&self, import_name: &str) -> bool {
        !self.owning_distributions(import_name).is_empty()
    }

    /// Return whether any distribution claiming an import ships native artifacts
    /// anywhere in its installed files (e.g. a sibling `_backend.so` module).
    fn native_distribution_owns_import(&self, import_name: &str) -> bool {
        self.owning_distributions(import_name)
            .iter()
            .any(|distribution| distribution.ships_native_artifacts)
    }
}

#[derive(Debug)]
pub struct ModuleResolver {
    config: Config,
    /// Module registry for ID allocation - the single source of truth for module identity
    registry: Mutex<ModuleRegistry>,
    /// Cache of resolved module paths
    module_cache: RefCell<IndexMap<String, Option<PathBuf>>>,
    /// Cache of module classifications
    classification_cache: RefCell<IndexMap<String, ImportClassification>>,
    /// Cache of virtual environment packages to avoid repeated filesystem scans
    virtualenv_packages_cache: RefCell<Option<IndexSet<String>>>,
    /// Cache of "must this site-packages package stay external?" scan results
    external_packages_cache: RefCell<IndexMap<PathBuf, bool>>,
    /// Cache of resolved environment site-packages roots (hot path for resolution)
    site_packages_dirs_cache: RefCell<Option<Vec<PathBuf>>>,
    /// Distribution ownership indexed once for each searched filesystem root
    distribution_ownership_cache: RefCell<IndexMap<PathBuf, DistributionOwnershipIndex>>,
    /// Entry file's directory (first in search path)
    entry_dir: Option<PathBuf>,
    /// Python version for stdlib classification
    python_version: u8,
    /// PYTHONPATH override for testing
    pythonpath_override: Option<String>,
    /// `VIRTUAL_ENV` override for testing
    virtualenv_override: Option<String>,
}

impl ModuleResolver {
    /// Canonicalize a path, handling errors gracefully
    fn canonicalize_path(&self, path: PathBuf) -> PathBuf {
        match path.canonicalize() {
            Ok(canonical) => canonical,
            Err(e) => {
                // Log warning but don't fail - return the original path
                warn!("Failed to canonicalize path {}: {}", path.display(), e);
                path
            }
        }
    }

    pub fn new(config: Config) -> Result<Self> {
        Self::new_with_overrides(config, None, None)
    }

    /// Create a new `ModuleResolver` with optional PYTHONPATH and `VIRTUAL_ENV` overrides for
    /// testing
    pub fn new_with_overrides(
        config: Config,
        pythonpath_override: Option<&str>,
        virtualenv_override: Option<&str>,
    ) -> Result<Self> {
        let python_version = config
            .python_version()
            .context("ModuleResolver requires a validated target Python version")?;

        Ok(Self {
            config,
            registry: Mutex::new(ModuleRegistry::new()),
            module_cache: RefCell::new(IndexMap::new()),
            classification_cache: RefCell::new(IndexMap::new()),
            virtualenv_packages_cache: RefCell::new(None),
            external_packages_cache: RefCell::new(IndexMap::new()),
            site_packages_dirs_cache: RefCell::new(None),
            distribution_ownership_cache: RefCell::new(IndexMap::new()),
            entry_dir: None,
            python_version,
            pythonpath_override: pythonpath_override.map(str::to_owned),
            virtualenv_override: virtualenv_override.map(str::to_owned),
        })
    }

    /// Set the entry file for the resolver
    /// This establishes the first search path directory
    pub fn set_entry_file(&mut self, entry_path: &Path, original_entry_path: &Path) {
        // The entry directory participates in fallback virtualenv discovery, so any
        // environment-derived caches computed before this point are stale
        self.site_packages_dirs_cache.borrow_mut().take();
        self.virtualenv_packages_cache.borrow_mut().take();

        debug!(
            "set_entry_file: entry_path={}, original_entry_path={}, is_dir={}",
            entry_path.display(),
            original_entry_path.display(),
            original_entry_path.is_dir()
        );

        // Check if the entry is a special entry file (__init__.py or __main__.py)
        // Use shared helper to keep behavior in sync with orchestrator
        let is_package_file = entry_path
            .file_name()
            .and_then(|f| f.to_str())
            .is_some_and(crate::python::module_path::is_special_entry_file_name);

        if is_package_file {
            // For __init__.py or __main__.py, use the parent's parent as search root
            // e.g., for path/to/pkg/__init__.py, use path/to/ as search root
            if let Some(pkg_dir) = entry_path.parent() {
                if let Some(parent_of_pkg) = pkg_dir.parent() {
                    self.entry_dir = Some(parent_of_pkg.to_path_buf());
                    debug!(
                        "Set entry directory to parent of package: {}",
                        parent_of_pkg.display()
                    );
                } else {
                    // Package is at root, use root
                    self.entry_dir = Some(PathBuf::from("."));
                    debug!("Set entry directory to current directory (package at root)");
                }
            }
        } else if let Some(parent) = entry_path.parent() {
            // For regular module files, use the parent directory
            self.entry_dir = Some(parent.to_path_buf());
            debug!("Set entry directory to: {}", parent.display());
        }
    }

    /// Get module name by ID
    pub fn get_module_name(&self, id: ModuleId) -> Option<String> {
        let registry = self.registry.lock().expect("Module registry lock poisoned");
        registry.get_metadata(id).map(|m| m.name.clone())
    }

    /// Get module kind by ID (post-registration truth source)
    pub fn get_module_kind(&self, id: ModuleId) -> Option<crate::python::module_path::ModuleKind> {
        let registry = self.registry.lock().expect("Module registry lock poisoned");
        registry.get_metadata(id).map(|m| m.kind)
    }

    /// Returns true if the module is a package initializer (__init__.py)
    pub fn is_package_init(&self, id: ModuleId) -> bool {
        matches!(
            self.get_module_kind(id),
            Some(crate::python::module_path::ModuleKind::PackageInit)
        )
    }

    /// Returns true if the module is a namespace package (directory without __init__.py)
    pub fn is_namespace_package(&self, id: ModuleId) -> bool {
        matches!(
            self.get_module_kind(id),
            Some(crate::python::module_path::ModuleKind::NamespacePackageDir)
        )
    }

    /// Get module path by ID
    pub fn get_module_path(&self, id: ModuleId) -> Option<PathBuf> {
        let registry = self.registry.lock().expect("Module registry lock poisoned");
        registry.get_metadata(id).map(|m| m.canonical_path.clone())
    }

    /// Check if the entry module is a package
    pub fn is_entry_package(&self) -> bool {
        let registry = self.registry.lock().expect("Module registry lock poisoned");
        registry.get_metadata(ModuleId::ENTRY).is_some_and(|m| {
            matches!(
                m.kind,
                crate::python::module_path::ModuleKind::PackageInit
                    | crate::python::module_path::ModuleKind::NamespacePackageDir
            )
        })
    }

    /// Get module metadata by ID
    pub fn get_module(&self, id: ModuleId) -> Option<ModuleMetadata> {
        let registry = self.registry.lock().expect("Module registry lock poisoned");
        registry.get_metadata(id).cloned()
    }

    /// Get module ID by name (reverse lookup)
    pub fn get_module_id_by_name(&self, name: &str) -> Option<ModuleId> {
        let registry = self.registry.lock().expect("Module registry lock poisoned");
        registry.get_id_by_name(name).copied()
    }

    /// Get registered module IDs whose names start with `prefix`.
    pub(crate) fn get_module_ids_by_name_prefix(&self, prefix: &str) -> FxIndexSet<ModuleId> {
        let registry = self.registry.lock().expect("Module registry lock poisoned");
        registry
            .by_name
            .iter()
            .filter_map(|(name, &id)| name.starts_with(prefix).then_some(id))
            .collect()
    }

    /// Get module ID by path (reverse lookup)
    pub fn get_module_id_by_path(&self, path: &Path) -> Option<ModuleId> {
        let canonical_path = self.canonicalize_path(path.to_path_buf());
        let registry = self.registry.lock().expect("Module registry lock poisoned");
        registry.by_path.get(&canonical_path).copied()
    }

    /// Get all directories to search for modules
    /// Per docs/resolution.md: Entry file's directory is always first
    pub fn get_search_directories(&self) -> Vec<PathBuf> {
        let pythonpath = self.pythonpath_override.as_deref();
        let virtualenv = self.virtualenv_override.as_deref();
        self.get_search_directories_with_overrides(pythonpath, virtualenv)
    }

    /// Get import roots and the selected virtual environments' distribution metadata roots.
    pub(crate) fn get_distribution_metadata_search_directories(&self) -> Vec<PathBuf> {
        let mut unique_dirs: IndexSet<PathBuf> =
            self.get_search_directories().into_iter().collect();
        unique_dirs.extend(self.get_virtualenv_site_packages_search_directories(None));
        unique_dirs.into_iter().collect()
    }

    /// Return the search root that supplied a concrete importable module.
    ///
    /// Namespace packages intentionally have no preferred root because multiple roots can
    /// contribute providers to the same namespace.
    pub(crate) fn get_import_search_root(&self, module_name: &str) -> Option<PathBuf> {
        let classification = self.classify_import(module_name);
        if !classification.is_resolved()
            || matches!(classification.source, ImportSource::NamespacePackage)
        {
            return None;
        }

        let search_dirs = self.get_search_directories();
        if let Some((search_root, resolved)) = self.locate_in_directories(module_name, &search_dirs)
        {
            return (!matches!(resolved.source, ImportSource::NamespacePackage))
                .then_some(search_root);
        }

        let virtualenv_dirs = self.get_virtualenv_site_packages_search_directories(None);
        self.locate_in_directories(module_name, &virtualenv_dirs)
            .and_then(|(search_root, resolved)| {
                (!matches!(resolved.source, ImportSource::NamespacePackage)).then_some(search_root)
            })
    }

    /// Get all directories to search for modules with optional PYTHONPATH override
    /// Returns deduplicated, canonicalized paths
    fn get_search_directories_with_overrides(
        &self,
        pythonpath_override: Option<&str>,
        _virtualenv_override: Option<&str>,
    ) -> Vec<PathBuf> {
        let mut unique_dirs = IndexSet::new();

        // 1. Entry file's directory is ALWAYS first (per docs/resolution.md)
        if let Some(entry_dir) = &self.entry_dir {
            if let Ok(canonical) = entry_dir.canonicalize() {
                unique_dirs.insert(canonical);
            } else {
                unique_dirs.insert(entry_dir.clone());
            }
        }

        // 2. Add PYTHONPATH directories
        let pythonpath = pythonpath_override
            .map(ToOwned::to_owned)
            .or_else(|| std::env::var("PYTHONPATH").ok());

        if let Some(pythonpath) = pythonpath {
            let separator = if cfg!(windows) { ';' } else { ':' };
            for path_str in pythonpath.split(separator) {
                self.add_pythonpath_directory(&mut unique_dirs, path_str);
            }
        }

        // 3. Add configured src directories
        for dir in &self.config.src {
            if let Ok(canonical) = dir.canonicalize() {
                unique_dirs.insert(canonical);
            } else {
                unique_dirs.insert(dir.clone());
            }
        }

        unique_dirs.into_iter().collect()
    }

    /// Helper method to add a PYTHONPATH directory to the unique set
    fn add_pythonpath_directory(&self, unique_dirs: &mut IndexSet<PathBuf>, path_str: &str) {
        if path_str.is_empty() {
            return;
        }

        let path = PathBuf::from(path_str);
        if !path.exists() || !path.is_dir() {
            return;
        }

        if let Ok(canonical) = path.canonicalize() {
            unique_dirs.insert(canonical);
        } else {
            unique_dirs.insert(path);
        }
    }

    /// Resolve a module to its file path using Python's resolution rules
    /// Per docs/resolution.md:
    /// 1. Check for package initializer (foo/__init__.py or native equivalent)
    /// 2. Check for file module (foo.py or native equivalent)
    /// 3. Check for namespace package (foo/ directory without a package initializer)
    pub fn resolve_module_path(&self, module_name: &str) -> Result<Option<PathBuf>> {
        // For absolute imports, delegate to the context-aware version
        if !module_name.starts_with('.') {
            return self.resolve_module_path_with_context(module_name, None);
        }

        // Relative imports without context cannot be resolved
        // Don't cache this result since it might be resolvable with context
        warn!("Cannot resolve relative import '{module_name}' without module context");
        Ok(None)
    }

    /// Resolve a module with optional current module context for relative imports
    pub fn resolve_module_path_with_context(
        &self,
        module_name: &str,
        current_module_path: Option<&Path>,
    ) -> Result<Option<PathBuf>> {
        // Check cache first
        if let Some(cached_path) = self.module_cache.borrow().get(module_name) {
            return Ok(cached_path.clone());
        }

        let descriptor = ImportModuleDescriptor::from_module_name(module_name);

        // Handle relative imports
        if descriptor.leading_dots > 0 {
            if let Some(current_path) = current_module_path {
                let resolved = self.resolve_relative_import(&descriptor, current_path)?;
                // Don't cache relative imports as they depend on context
                // Different modules might resolve the same relative import differently
                return Ok(resolved);
            }
            // No context for relative import - don't cache this negative result
            warn!("Cannot resolve relative import '{module_name}' without module context");
            return Ok(None);
        }

        // Try each search directory in order
        let search_dirs = self.get_search_directories();
        for search_dir in &search_dirs {
            if let Some(resolved) = self.resolve_in_directory(search_dir, &descriptor) {
                let bundle_path = resolved.bundle_path();
                self.module_cache
                    .borrow_mut()
                    .insert(module_name.to_owned(), bundle_path.clone());
                return Ok(bundle_path);
            }
        }

        // Opt-in third-party bundling: fall back to virtualenv site-packages for modules
        // the current policy selects for bundling
        if let Some(bundle_path) = self.resolve_in_site_packages_for_bundling(module_name) {
            self.module_cache
                .borrow_mut()
                .insert(module_name.to_owned(), Some(bundle_path.clone()));
            return Ok(Some(bundle_path));
        }

        // Not found - cache the negative result
        self.module_cache
            .borrow_mut()
            .insert(module_name.to_owned(), None);
        Ok(None)
    }

    /// Resolve a relative import given the current module's path
    fn resolve_relative_import(
        &self,
        descriptor: &ImportModuleDescriptor,
        current_module_path: &Path,
    ) -> Result<Option<PathBuf>> {
        // First resolve to absolute module name
        let name_string = if descriptor.name_parts.is_empty() {
            None
        } else {
            Some(descriptor.name_parts.join("."))
        };
        let name = name_string.as_deref();

        let level = u32::try_from(descriptor.leading_dots).map_err(|_| {
            anyhow!(
                "Relative import level {} is too large (max: {})",
                descriptor.leading_dots,
                u32::MAX
            )
        })?;

        let absolute_module_name = self
            .resolve_relative_to_absolute_module_name(level, name, current_module_path)
            .ok_or_else(|| anyhow!("Failed to resolve relative import"))?;

        // Now resolve the absolute module name to a path
        // Create a new descriptor for the absolute import
        let absolute_descriptor = ImportModuleDescriptor::from_module_name(&absolute_module_name);

        // Use the existing resolution logic for absolute imports
        let search_dirs = self.get_search_directories();
        for search_dir in &search_dirs {
            if let Some(resolved) = self.resolve_in_directory(search_dir, &absolute_descriptor) {
                return Ok(resolved.bundle_path());
            }
        }

        // Opt-in third-party bundling: relative imports inside bundled site-packages
        // modules resolve against site-packages as well
        Ok(self.resolve_in_site_packages_for_bundling(&absolute_module_name))
    }

    /// Resolve a module in virtualenv site-packages when third-party bundling is enabled.
    ///
    /// Returns a path only when the current classification policy actually bundles the
    /// module, so external modules (native-extension packages, `known_third_party`)
    /// never leak bundle paths into the module cache.
    fn resolve_in_site_packages_for_bundling(&self, module_name: &str) -> Option<PathBuf> {
        if !self.config.bundle_third_party() || module_name.starts_with('.') {
            return None;
        }
        if !self.classify_import(module_name).should_bundle() {
            return None;
        }
        let virtualenv_dirs = self.get_virtualenv_site_packages_search_directories(None);
        self.locate_in_directories(module_name, &virtualenv_dirs)
            .and_then(|(_, resolved)| resolved.bundle_path())
    }

    /// Resolve an `ImportlibStatic` import that may have invalid Python identifiers
    /// This handles cases like importlib.import_module("data-processor")
    /// Resolve `ImportlibStatic` imports with optional package context for relative imports
    /// Returns a tuple of (`resolved_module_name`, path)
    pub fn resolve_importlib_static_with_context(
        &self,
        module_name: &str,
        package_context: Option<&str>,
    ) -> Option<(String, PathBuf)> {
        // Handle relative imports with package context
        let resolved_name = package_context.map_or_else(
            || module_name.to_owned(),
            |package| {
                if module_name.starts_with('.') {
                    // Count the number of leading dots
                    let level = module_name.chars().take_while(|&c| c == '.').count() as u32;
                    let name_part = module_name.trim_start_matches('.');

                    // Use the centralized helper for relative import resolution
                    self.resolve_relative_import_from_package_name(
                        level,
                        if name_part.is_empty() {
                            None
                        } else {
                            Some(name_part)
                        },
                        package,
                    )
                } else {
                    // Absolute import, use as-is
                    module_name.to_owned()
                }
            },
        );

        debug!(
            "Resolving ImportlibStatic: '{}' with package '{}' -> '{}'",
            module_name,
            package_context.unwrap_or("None"),
            resolved_name
        );

        // For ImportlibStatic imports, we look for files with the exact name
        // (including hyphens and other invalid Python identifier characters)
        let search_dirs = self.get_search_directories();

        for search_dir in &search_dirs {
            // Convert module name to file path (replace dots with slashes)
            let path_components: Vec<&str> = resolved_name.split('.').collect();

            if path_components.len() == 1 {
                // Single component - try as direct file
                let file_path = search_dir.join(format!("{resolved_name}.py"));
                if file_path.is_file() {
                    debug!("Found ImportlibStatic module at: {}", file_path.display());
                    let canonical = self.canonicalize_path(file_path);
                    return Some((resolved_name.clone(), canonical));
                }
            }

            // Try as a nested module path
            let mut module_path = search_dir.clone();
            for (i, component) in path_components.iter().enumerate() {
                if i == path_components.len() - 1 {
                    // Last component - try as file
                    let file_path = module_path.join(format!("{component}.py"));
                    if file_path.is_file() {
                        debug!("Found ImportlibStatic module at: {}", file_path.display());
                        let canonical = self.canonicalize_path(file_path);
                        return Some((resolved_name.clone(), canonical));
                    }
                }
                module_path = module_path.join(component);
            }

            // Try as a package directory with __init__.py
            let init_path = module_path.join(crate::python::constants::INIT_FILE);
            if init_path.is_file() {
                debug!("Found ImportlibStatic package at: {}", init_path.display());
                let canonical = self.canonicalize_path(init_path);
                return Some((resolved_name.clone(), canonical));
            }
        }

        // Opt-in third-party bundling: static importlib imports inside bundled
        // dependencies resolve against site-packages as well
        if let Some(bundle_path) = self.resolve_in_site_packages_for_bundling(&resolved_name) {
            debug!(
                "Found ImportlibStatic module in site-packages at: {}",
                bundle_path.display()
            );
            return Some((resolved_name, bundle_path));
        }

        // Not found
        None
    }

    /// Resolve a module within a specific directory
    /// Implements the resolution algorithm from docs/resolution.md
    fn resolve_in_directory(
        &self,
        root: &Path,
        descriptor: &ImportModuleDescriptor,
    ) -> Option<ResolvedModule> {
        if descriptor.name_parts.is_empty() {
            // Edge case: empty import (shouldn't happen in practice)
            return None;
        }

        let mut current_path = root.to_path_buf();

        // Process all parts except the last one
        for (i, part) in descriptor.name_parts.iter().enumerate() {
            let is_last = i == descriptor.name_parts.len() - 1;
            let package_dir = current_path.join(part);
            let package_init = package_dir.join(crate::python::constants::INIT_FILE);

            if is_last {
                // For the last part, check in order:
                // 1. Package (foo/__init__.py)
                // 2. Native extension package (foo/__init__.so, foo/__init__.pyd, etc.)
                // 3. Module file (foo.py)
                // 4. Native extension module (foo.so, foo.pyd, etc.)
                // 5. Namespace package (foo/ directory)

                // Check for package first
                if package_init.is_file() {
                    debug!("Found package at: {}", package_init.display());
                    let canonical = self.canonicalize_path(package_init);
                    return Some(ResolvedModule {
                        path: canonical,
                        source: ImportSource::Python,
                    });
                }

                if let Some(extension_path) = self
                    .find_native_extension_module(&package_dir, crate::python::constants::INIT_STEM)
                {
                    debug!(
                        "Found native extension package at: {}",
                        extension_path.display()
                    );
                    return Some(ResolvedModule {
                        path: extension_path,
                        source: ImportSource::NativeExtension,
                    });
                }

                // Check for module file
                let module_file = current_path.join(format!("{part}.py"));
                if module_file.is_file() {
                    debug!("Found module file at: {}", module_file.display());
                    let canonical = self.canonicalize_path(module_file);
                    return Some(ResolvedModule {
                        path: canonical,
                        source: ImportSource::Python,
                    });
                }

                if let Some(extension_path) = self.find_native_extension_module(&current_path, part)
                {
                    debug!(
                        "Found native extension module at: {}",
                        extension_path.display()
                    );
                    return Some(ResolvedModule {
                        path: extension_path,
                        source: ImportSource::NativeExtension,
                    });
                }

                // Check for namespace package (directory without __init__.py)
                if crate::python::module_path::is_namespace_package_dir(&package_dir) {
                    debug!("Found namespace package at: {}", package_dir.display());
                    // Return the directory path to indicate this is a namespace package
                    let canonical = self.canonicalize_path(package_dir);
                    return Some(ResolvedModule {
                        path: canonical,
                        source: ImportSource::NamespacePackage,
                    });
                }
            } else {
                // For intermediate parts, they must be packages
                if package_init.is_file() {
                    current_path = package_dir;
                } else if let Some(extension_path) = self
                    .find_native_extension_module(&package_dir, crate::python::constants::INIT_STEM)
                {
                    debug!(
                        "Found intermediate native extension package at: {}",
                        extension_path.display()
                    );
                    current_path = package_dir;
                } else if crate::python::module_path::is_namespace_package_dir(&package_dir) {
                    // Namespace package - continue but don't add to resolved paths
                    current_path = package_dir;
                } else {
                    // Not found
                    return None;
                }
            }
        }

        None
    }

    fn find_native_extension_module(&self, directory: &Path, module_name: &str) -> Option<PathBuf> {
        let entries = std::fs::read_dir(directory).ok()?;
        let module_prefix = format!("{module_name}.");
        let mut candidates = Vec::new();

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(OsStr::to_str) else {
                continue;
            };
            let is_module_name = file_name.starts_with(&module_prefix);
            let is_native_extension = path
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| matches!(extension, "so" | "pyd"));
            if is_module_name && is_native_extension {
                candidates.push(path);
            }
        }

        candidates
            .into_iter()
            .min()
            .map(|path| self.canonicalize_path(path))
    }

    fn locate_in_directories(
        &self,
        module_name: &str,
        directories: &[PathBuf],
    ) -> Option<(PathBuf, ResolvedModule)> {
        let descriptor = ImportModuleDescriptor::from_module_name(module_name);
        directories.iter().find_map(|directory| {
            self.resolve_in_directory(directory, &descriptor)
                .map(|resolved| (directory.clone(), resolved))
        })
    }

    fn classify_resolved_import(
        &self,
        module_name: &str,
        search_root: &Path,
        resolved: &ResolvedModule,
        default_origin: ImportOrigin,
        allow_bundle: bool,
    ) -> ImportClassification {
        let explicit_first_party = self.config.known_first_party.contains(module_name);
        let explicit_third_party = self.is_explicit_third_party(module_name);
        let origin = if explicit_first_party {
            ImportOrigin::FirstParty
        } else if explicit_third_party
            || default_origin == ImportOrigin::ThirdParty
            || self.distribution_owns_import(search_root, module_name)
        {
            ImportOrigin::ThirdParty
        } else {
            default_origin
        };
        let source_can_bundle = matches!(
            resolved.source,
            ImportSource::Python | ImportSource::NamespacePackage
        );
        let bundle =
            if source_can_bundle && !explicit_third_party && (allow_bundle || explicit_first_party)
            {
                BundleDisposition::Include
            } else {
                BundleDisposition::External
            };

        ImportClassification::new(origin, resolved.source, bundle)
    }

    /// Return whether a module or any of its parent packages is listed in
    /// `known_third_party`, so configured package roots also cover their submodules.
    fn is_explicit_third_party(&self, module_name: &str) -> bool {
        if self.config.known_third_party.contains(module_name) {
            return true;
        }
        let mut prefix_end = module_name.len();
        while let Some(separator_index) = module_name[..prefix_end].rfind('.') {
            if self
                .config
                .known_third_party
                .contains(&module_name[..separator_index])
            {
                return true;
            }
            prefix_end = separator_index;
        }
        false
    }

    /// Classify an import without conflating its origin, source kind, bundle policy, and
    /// distribution requirement.
    pub fn classify_import(&self, module_name: &str) -> ImportClassification {
        if let Some(cached_classification) = self.classification_cache.borrow().get(module_name) {
            return cached_classification.clone();
        }

        let classification = self.classify_import_uncached(module_name);
        self.classification_cache
            .borrow_mut()
            .insert(module_name.to_owned(), classification.clone());
        classification
    }

    fn classify_import_uncached(&self, module_name: &str) -> ImportClassification {
        if module_name.starts_with('.') {
            return ImportClassification::new(
                ImportOrigin::FirstParty,
                ImportSource::Unresolved,
                BundleDisposition::Include,
            );
        }

        let explicit_first_party = self.config.known_first_party.contains(module_name);
        let explicit_third_party = self.is_explicit_third_party(module_name);
        if !explicit_first_party
            && !explicit_third_party
            && is_stdlib_module(module_name, self.python_version)
        {
            return ImportClassification::new(
                ImportOrigin::StandardLibrary,
                ImportSource::Unresolved,
                BundleDisposition::External,
            );
        }

        let search_dirs = self.get_search_directories();
        if let Some((search_root, resolved)) = self.locate_in_directories(module_name, &search_dirs)
        {
            return self.classify_resolved_import(
                module_name,
                &search_root,
                &resolved,
                ImportOrigin::FirstParty,
                true,
            );
        }

        if module_name.contains('.') {
            let parent_module = module_name.split('.').next().unwrap_or(module_name);
            let parent_classification = self.classify_import(parent_module);
            if parent_classification.should_bundle()
                && let Some((_, parent)) = self.locate_in_directories(parent_module, &search_dirs)
            {
                let parent_is_package = parent.source == ImportSource::NamespacePackage
                    || parent
                        .path
                        .file_name()
                        .and_then(OsStr::to_str)
                        .is_some_and(crate::python::module_path::is_init_file_name);
                if !parent_is_package {
                    debug!(
                        "Module '{module_name}' cannot exist - parent '{parent_module}' is a \
                         module file, not a package (shadowing behavior)"
                    );
                    return ImportClassification::new(
                        parent_classification.origin,
                        ImportSource::Unresolved,
                        BundleDisposition::Include,
                    );
                }
            }
        }

        let virtualenv_dirs = self.get_virtualenv_site_packages_search_directories(None);
        if let Some((search_root, resolved)) =
            self.locate_in_directories(module_name, &virtualenv_dirs)
        {
            // Opt-in third-party bundling: pure-Python distributions are bundled, but any
            // package shipping native extensions (.so/.pyd) or reading its own installed
            // distribution metadata at runtime stays external as a whole.
            let allow_bundle = self.config.bundle_third_party()
                && !self.package_must_stay_external(&search_root, module_name);
            return self.classify_resolved_import(
                module_name,
                &search_root,
                &resolved,
                ImportOrigin::ThirdParty,
                allow_bundle,
            );
        }

        if explicit_first_party {
            return ImportClassification::new(
                ImportOrigin::FirstParty,
                ImportSource::Unresolved,
                BundleDisposition::Include,
            );
        }

        if explicit_third_party || self.is_virtualenv_package(module_name) {
            return ImportClassification::new(
                ImportOrigin::ThirdParty,
                ImportSource::Unresolved,
                BundleDisposition::External,
            );
        }

        ImportClassification::new(
            ImportOrigin::Unknown,
            ImportSource::Unresolved,
            BundleDisposition::External,
        )
    }

    /// Return whether the top-level package owning `module_name` under `search_root`
    /// must stay external under the third-party bundling policy.
    ///
    /// A package stays external as a whole when it ships native extension (`.so`/`.pyd`)
    /// artifacts anywhere inside its top-level directory (so compiled submodules keep
    /// importing correctly at runtime), or when its Python source reads its own installed
    /// distribution metadata at runtime (`importlib.metadata`, `pkg_resources`), which is
    /// unavailable once the source is inlined into a bundle.
    fn package_must_stay_external(&self, search_root: &Path, module_name: &str) -> bool {
        // A distribution may install native artifacts outside the import's own package
        // directory (e.g. a pure `frontend` package plus a sibling `_backend.so`);
        // consult its RECORD before scanning the package directory itself
        if self.native_distribution_owns_import(search_root, module_name) {
            debug!(
                "Import '{module_name}' is owned by a distribution shipping native artifacts; \
                 keeping it external"
            );
            return true;
        }

        // A distribution targeting a newer Python than the bundle's target version may
        // use unsupported syntax or APIs; leave it external so installers can reject it
        if self.distribution_requires_incompatible_python(search_root, module_name) {
            debug!(
                "Import '{module_name}' is owned by a distribution whose Requires-Python is \
                 incompatible with the target version; keeping it external"
            );
            return true;
        }

        let top_level = module_name.split('.').next().unwrap_or(module_name);
        let package_dir = search_root.join(top_level);
        if !package_dir.is_dir() {
            // Single-file module: check the file itself for metadata usage; native
            // extension files are already classified as `ImportSource::NativeExtension`.
            if self
                .find_native_extension_module(search_root, top_level)
                .is_some()
            {
                return true;
            }
            let module_file = search_root.join(format!("{top_level}.py"));
            return module_file.is_file() && Self::python_file_blocks_bundling(&module_file);
        }

        let package_dir = self.canonicalize_path(package_dir);
        if let Some(&stays_external) = self.external_packages_cache.borrow().get(&package_dir) {
            return stays_external;
        }

        let stays_external = Self::scan_package_for_external_markers(&package_dir);
        debug!(
            "Package directory '{}' must stay external: {stays_external}",
            package_dir.display()
        );
        self.external_packages_cache
            .borrow_mut()
            .insert(package_dir, stays_external);
        stays_external
    }

    /// Scan a package directory tree for markers that force the package to stay
    /// external: native extension artifacts or runtime distribution-metadata access.
    ///
    /// The scan is conservative: unreadable entries keep the package external, and
    /// symlinked directories are never traversed (a symlink cycle or a link to a large
    /// external tree must not stall bundling) — a directory symlink instead keeps the
    /// package external because its contents cannot be inspected safely.
    fn scan_package_for_external_markers(root: &Path) -> bool {
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                // Conservative: a package that cannot be fully inspected stays external
                return true;
            };
            for entry in entries.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    return true;
                };
                let path = entry.path();
                if file_type.is_symlink() {
                    // Do not follow directory symlinks; treat them as uninspectable
                    match std::fs::metadata(&path) {
                        Ok(metadata) if metadata.is_dir() => return true,
                        Ok(_) => {
                            if Self::is_external_marker_file(&path) {
                                return true;
                            }
                        }
                        Err(_) => return true,
                    }
                } else if file_type.is_dir() {
                    pending.push(path);
                } else if Self::is_external_marker_file(&path) {
                    return true;
                }
            }
        }
        false
    }

    /// Return whether a file extension denotes a native binary artifact: Python
    /// extension modules (`.so`/`.pyd`) or shared libraries loaded at runtime through
    /// ctypes/CFFI (`.dll`/`.dylib`). Distinct from importable-extension matching in
    /// [`Self::find_native_extension_module`], which covers only Python extensions.
    fn is_native_artifact_extension(extension: &str) -> bool {
        matches!(extension, "so" | "pyd" | "dll" | "dylib")
    }

    /// Return whether a single file forces its package to stay external: a native
    /// artifact or a Python source containing a bundling-blocking pattern.
    fn is_external_marker_file(path: &Path) -> bool {
        match path.extension().and_then(OsStr::to_str) {
            Some(extension) if Self::is_native_artifact_extension(extension) => true,
            Some("py") => Self::python_file_blocks_bundling(path),
            _ => false,
        }
    }

    /// Return whether a Python source file contains a pattern that makes it unsafe to
    /// inline: imports of installed-distribution APIs (`importlib.metadata`,
    /// `pkg_resources`, `importlib.resources`, and their backports) or dynamic imports
    /// with non-literal module names.
    fn python_file_blocks_bundling(path: &Path) -> bool {
        let Ok(source) = std::fs::read_to_string(path) else {
            // Conservative: an unreadable source file keeps the package external
            return true;
        };
        Self::python_source_blocks_bundling(&source)
    }

    /// Return whether Python source contains a bundling-blocking pattern (see
    /// [`UnbundlablePatternDetector`]).
    ///
    /// Detection is AST-based so any valid formatting (aliases, parenthesized or
    /// multiline import lists) is recognized, while comments and docstrings that merely
    /// mention the APIs are not.
    fn python_source_blocks_bundling(source: &str) -> bool {
        // Cheap pre-filter: skip parsing sources that cannot reference the APIs
        if !source.contains("importlib")
            && !source.contains("pkg_resources")
            && !source.contains("__import__")
        {
            return false;
        }
        let Ok(parsed) = ruff_python_parser::parse_module(source) else {
            // Conservative: unparsable source keeps the package external
            return true;
        };
        use ruff_python_ast::visitor::Visitor as _;
        let mut detector = UnbundlablePatternDetector::default();
        // Collect importlib.import_module aliases up front: an alias imported after a
        // function definition still applies to calls inside that function's body
        detector.collect_dynamic_import_aliases(&parsed.syntax().body);
        for stmt in &parsed.syntax().body {
            detector.visit_stmt(stmt);
        }
        detector.found
    }

    fn get_virtualenv_site_packages_search_directories(
        &self,
        virtualenv_override: Option<&str>,
    ) -> Vec<PathBuf> {
        // The environment does not change during a bundling run; cache the resolved
        // roots because this sits on the per-import resolution hot path
        if virtualenv_override.is_none()
            && let Ok(cache) = self.site_packages_dirs_cache.try_borrow()
            && let Some(cached_dirs) = cache.as_ref()
        {
            return cached_dirs.clone();
        }

        let mut site_packages_dirs = IndexSet::new();
        for virtualenv_path in self.resolve_virtualenv_paths(virtualenv_override) {
            for directory in self.get_virtualenv_site_packages_directories(&virtualenv_path) {
                site_packages_dirs.insert(self.canonicalize_path(directory));
            }
        }
        let directories: Vec<PathBuf> = site_packages_dirs.into_iter().collect();

        if virtualenv_override.is_none()
            && let Ok(mut cache) = self.site_packages_dirs_cache.try_borrow_mut()
        {
            *cache = Some(directories.clone());
        }
        directories
    }

    fn resolve_virtualenv_paths(&self, virtualenv_override: Option<&str>) -> Vec<PathBuf> {
        if let Some(path) = virtualenv_override {
            return Self::explicit_virtualenv_paths(path);
        }
        if let Some(path) = self.virtualenv_override.as_deref() {
            return Self::explicit_virtualenv_paths(path);
        }
        if let Ok(path) = std::env::var("VIRTUAL_ENV") {
            return Self::explicit_virtualenv_paths(&path);
        }
        // Conda environments use the same site-packages layout; RequirementResolver
        // already honors CONDA_PREFIX, keep module resolution consistent with it
        if let Ok(path) = std::env::var("CONDA_PREFIX") {
            return Self::explicit_virtualenv_paths(&path);
        }
        self.detect_fallback_virtualenv_paths()
    }

    fn explicit_virtualenv_paths(virtualenv_path: &str) -> Vec<PathBuf> {
        if virtualenv_path.is_empty() {
            Vec::new()
        } else {
            vec![PathBuf::from(virtualenv_path)]
        }
    }

    /// Get the set of third-party packages installed in the virtual environment
    fn get_virtualenv_packages(&self, virtualenv_override: Option<&str>) -> IndexSet<String> {
        let override_to_use = virtualenv_override.or(self.virtualenv_override.as_deref());

        // If we have a cached result and the same override (or lack thereof), return it
        if override_to_use == self.virtualenv_override.as_deref()
            && let Ok(cache_ref) = self.virtualenv_packages_cache.try_borrow()
            && let Some(cached_packages) = cache_ref.as_ref()
        {
            return cached_packages.clone();
        }

        // Compute the packages
        self.compute_virtualenv_packages(override_to_use)
    }

    /// Compute virtualenv packages by scanning the filesystem
    fn compute_virtualenv_packages(&self, virtualenv_override: Option<&str>) -> IndexSet<String> {
        let mut packages = IndexSet::new();

        for site_packages_dir in
            self.get_virtualenv_site_packages_search_directories(virtualenv_override)
        {
            self.scan_site_packages_directory(&site_packages_dir, &mut packages);
        }

        // Cache the result if it matches our stored override
        if virtualenv_override == self.virtualenv_override.as_deref()
            && let Ok(mut cache_ref) = self.virtualenv_packages_cache.try_borrow_mut()
        {
            *cache_ref = Some(packages.clone());
        }

        packages
    }

    /// Detect common virtual environment directory names beside the working
    /// directory and beside the entry file's project directory
    fn detect_fallback_virtualenv_paths(&self) -> Vec<PathBuf> {
        let mut candidate_roots: IndexSet<PathBuf> = IndexSet::new();
        if let Ok(current_dir) = std::env::current_dir() {
            candidate_roots.insert(current_dir);
        }
        // A project's virtualenv commonly lives next to its entry point; include it so
        // invoking Cribo from outside the project (e.g. a monorepo root) still works
        if let Some(entry_dir) = &self.entry_dir {
            candidate_roots.insert(self.canonicalize_path(entry_dir.clone()));
        }

        let mut venv_paths = Vec::new();
        for candidate_root in candidate_roots {
            for venv_name in AUTO_DETECTED_VIRTUALENV_NAMES {
                let venv_path = candidate_root.join(venv_name);
                if venv_path.is_dir() {
                    // Check if it looks like a virtual environment
                    let has_bin =
                        venv_path.join("bin").is_dir() || venv_path.join("Scripts").is_dir();
                    let has_lib = venv_path.join("lib").is_dir();

                    if has_bin || has_lib {
                        venv_paths.push(venv_path);
                    }
                }
            }
        }

        venv_paths
    }

    /// Get site-packages directories for a virtual environment
    fn get_virtualenv_site_packages_directories(&self, venv_path: &Path) -> Vec<PathBuf> {
        let mut site_packages_dirs = Vec::new();

        // Unix-style virtual environment
        let lib_dir = venv_path.join("lib");
        if lib_dir.is_dir()
            && let Ok(entries) = std::fs::read_dir(&lib_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let site_packages = path.join("site-packages");
                    if site_packages.is_dir() {
                        site_packages_dirs.push(site_packages);
                    }
                }
            }
        }

        // Windows-style virtual environment
        let lib_site_packages = venv_path.join("Lib").join("site-packages");
        if lib_site_packages.is_dir() {
            site_packages_dirs.push(lib_site_packages);
        }

        site_packages_dirs
    }

    /// Scan a site-packages directory and add found packages to the set
    fn scan_site_packages_directory(
        &self,
        site_packages_dir: &Path,
        packages: &mut IndexSet<String>,
    ) {
        let Ok(entries) = std::fs::read_dir(site_packages_dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(OsStr::to_str) else {
                continue;
            };

            // Skip common non-package entries
            if name.starts_with('_') || name.contains("-info") || name.contains(".dist-info") {
                continue;
            }

            // For directories, use the directory name as package name
            if path.is_dir() {
                packages.insert(name.to_owned());
            }
            // For .py files, use the filename without extension
            else if let Some(package_name) = name.strip_suffix(".py") {
                packages.insert(package_name.to_owned());
            }
        }
    }

    /// Check if a module name exists in the virtual environment packages
    fn is_virtualenv_package(&self, module_name: &str) -> bool {
        let virtualenv_packages = self.get_virtualenv_packages(None);

        // Check for exact match
        if virtualenv_packages.contains(module_name) {
            return true;
        }

        // Check if this is a submodule of a virtual environment package
        if let Some(root_module) = module_name.split('.').next()
            && virtualenv_packages.contains(root_module)
        {
            return true;
        }

        false
    }

    /// Return whether installed distribution metadata claims an import.
    fn distribution_owns_import(&self, site_packages_dir: &Path, import_name: &str) -> bool {
        self.with_distribution_ownership_index(site_packages_dir, |index| {
            index.owns_import(import_name)
        })
    }

    /// Return whether the distribution claiming an import ships native artifacts
    /// anywhere among its installed files (per its `RECORD`), even outside the
    /// import's own package directory (e.g. a sibling `_backend.so` module).
    fn native_distribution_owns_import(&self, site_packages_dir: &Path, import_name: &str) -> bool {
        self.with_distribution_ownership_index(site_packages_dir, |index| {
            index.native_distribution_owns_import(import_name)
        })
    }

    /// Return whether any distribution claiming an import declares a `Requires-Python`
    /// specifier that the configured target version does not satisfy.
    fn distribution_requires_incompatible_python(
        &self,
        site_packages_dir: &Path,
        import_name: &str,
    ) -> bool {
        self.with_distribution_ownership_index(site_packages_dir, |index| {
            index
                .owning_distributions(import_name)
                .iter()
                .filter_map(|distribution| distribution.requires_python.as_deref())
                .any(|specifiers| !self.target_python_satisfies(specifiers))
        })
    }

    /// Return whether the configured target Python version satisfies a PEP 440
    /// specifier set. Unparsable specifiers conservatively report as unsatisfied.
    fn target_python_satisfies(&self, specifiers: &str) -> bool {
        use std::str::FromStr;
        let Ok(specifiers) = pep508_rs::pep440_rs::VersionSpecifiers::from_str(specifiers) else {
            warn!("Ignoring unparsable Requires-Python specifier: {specifiers}");
            return false;
        };
        let target = pep508_rs::pep440_rs::Version::new([3, u64::from(self.python_version)]);
        specifiers.contains(&target)
    }

    /// Collect the PEP 508 `Requires-Dist` entries of the distributions owning the
    /// given bundled imports, excluding requirements that are themselves bundled and
    /// requirements whose markers cannot apply with the requested extras.
    ///
    /// `bundled_imports` maps each bundled import to the extras explicitly requested
    /// for it (e.g. through a `requirements.module-map` entry like `provider[speed]`),
    /// so extras-conditioned dependencies of bundled distributions survive when their
    /// extra was requested. Duplicate declarations of one distribution are merged by
    /// combining their version specifiers.
    ///
    /// Inlining a distribution removes it from `requirements.txt`, so its declared
    /// dependency constraints must be carried over for the external dependencies it
    /// pulls in (including dynamically or conditionally imported ones).
    pub(crate) fn bundled_distribution_requirements(
        &self,
        bundled_imports: &FxIndexMap<String, Vec<pep508_rs::ExtraName>>,
    ) -> Vec<String> {
        use std::str::FromStr;

        let mut bundled_distribution_names: IndexSet<String> = IndexSet::new();
        // Declared entry -> extras requested for the distribution that declared it
        let mut declared_requirements: IndexMap<String, Vec<pep508_rs::ExtraName>> =
            IndexMap::new();

        // Distributions can be installed into the environment's site-packages or
        // supplied through PYTHONPATH/source roots with adjacent dist-info metadata;
        // search the same roots as requirement resolution itself
        let metadata_dirs = self.get_distribution_metadata_search_directories();
        for (import_name, requested_extras) in bundled_imports {
            for site_packages_dir in &metadata_dirs {
                // Aggregate over all owning distributions (shared namespaces can have
                // several claimants; record-exact owners take precedence in the index)
                let found = self.with_distribution_ownership_index(site_packages_dir, |index| {
                    let owners = index.owning_distributions(import_name);
                    if owners.is_empty() {
                        None
                    } else {
                        Some(
                            owners
                                .iter()
                                .map(|dist| (dist.name.clone(), dist.requires_dist.clone()))
                                .collect::<Vec<_>>(),
                        )
                    }
                });
                if let Some(owners) = found {
                    for (name, requires_dist) in owners {
                        if !name.is_empty() {
                            bundled_distribution_names
                                .insert(Self::normalize_distribution_name(&name));
                        }
                        for entry in requires_dist {
                            declared_requirements
                                .entry(entry)
                                .or_default()
                                .extend(requested_extras.iter().cloned());
                        }
                    }
                    break;
                }
            }
        }

        // Deduplicate by distribution name: `pip install -r` rejects files that name
        // the same distribution twice ("Double requirement given")
        let mut merged: IndexMap<String, pep508_rs::Requirement<pep508_rs::VerbatimUrl>> =
            IndexMap::new();
        for (entry, extras) in declared_requirements {
            let Ok(mut parsed) = pep508_rs::Requirement::<pep508_rs::VerbatimUrl>::from_str(&entry)
            else {
                warn!("Skipping unparsable Requires-Dist entry of a bundled distribution: {entry}");
                continue;
            };
            // Keep only declarations that can apply with the requested extras: a pure
            // `extra == ...` condition holds only when that extra was asked for, while
            // mixed markers like `python_version < "3.12" or extra == "fast"` always can
            if !parsed.marker.evaluate_extras(&extras) {
                continue;
            }
            // Requested extras are satisfied by construction; remove them from the
            // marker, because `extra` is unset when pip evaluates a requirements file
            parsed.marker = parsed.marker.clone().simplify_extras(&extras);
            let name = parsed.name.to_string();
            // Requirements satisfied by other bundled distributions are not needed
            if bundled_distribution_names.contains(&name) {
                continue;
            }
            match merged.entry(name) {
                indexmap::map::Entry::Vacant(vacant) => {
                    vacant.insert(parsed);
                }
                indexmap::map::Entry::Occupied(mut occupied) => {
                    Self::merge_requirement_constraints(occupied.get_mut(), parsed);
                }
            }
        }

        let mut requirements: Vec<String> = merged
            .into_values()
            .map(|requirement| requirement.to_string())
            .collect();
        requirements.sort();
        requirements
    }

    /// Merge a duplicate requirement declaration into an existing one by combining
    /// their version specifiers (e.g. `dep<2` + `dep>=1` becomes `dep>=1,<2`).
    ///
    /// URL-based requirements and marker-conditioned declarations cannot be merged
    /// soundly; the existing declaration is kept in those cases.
    fn merge_requirement_constraints(
        existing: &mut pep508_rs::Requirement<pep508_rs::VerbatimUrl>,
        incoming: pep508_rs::Requirement<pep508_rs::VerbatimUrl>,
    ) {
        use pep508_rs::VersionOrUrl;
        if !existing.marker.is_true() || !incoming.marker.is_true() {
            return;
        }
        match (&existing.version_or_url, incoming.version_or_url) {
            (None, incoming_version) => {
                existing.version_or_url = incoming_version;
            }
            (
                Some(VersionOrUrl::VersionSpecifier(current)),
                Some(VersionOrUrl::VersionSpecifier(additional)),
            ) => {
                let combined: pep508_rs::pep440_rs::VersionSpecifiers =
                    current.iter().cloned().chain(additional).collect();
                existing.version_or_url = Some(VersionOrUrl::VersionSpecifier(combined));
            }
            _ => {}
        }
    }

    /// Extract the normalized distribution name of a PEP 508 requirement string.
    pub(crate) fn requirement_distribution_name(requirement: &str) -> Option<String> {
        use std::str::FromStr;
        pep508_rs::Requirement::<pep508_rs::VerbatimUrl>::from_str(requirement)
            .ok()
            .map(|parsed| parsed.name.to_string())
    }

    /// Normalize a distribution name per PEP 503 for set comparisons.
    fn normalize_distribution_name(name: &str) -> String {
        pep508_rs::PackageName::new(name.to_owned())
            .map_or_else(|_| name.to_owned(), |package| package.to_string())
    }

    /// Run a query against the (lazily built, cached) ownership index of a search root.
    fn with_distribution_ownership_index<R>(
        &self,
        site_packages_dir: &Path,
        query: impl FnOnce(&DistributionOwnershipIndex) -> R,
    ) -> R {
        let search_root = self.canonicalize_path(site_packages_dir.to_path_buf());
        if let Some(index) = self.distribution_ownership_cache.borrow().get(&search_root) {
            return query(index);
        }

        let index = Self::build_distribution_ownership_index(&search_root);
        let result = query(&index);
        self.distribution_ownership_cache
            .borrow_mut()
            .insert(search_root, index);
        result
    }

    /// Build distribution ownership data for one search root, covering both modern
    /// `.dist-info` installs and legacy `.egg-info` installs.
    fn build_distribution_ownership_index(site_packages_dir: &Path) -> DistributionOwnershipIndex {
        let mut index = DistributionOwnershipIndex::default();
        let Ok(entries) = std::fs::read_dir(site_packages_dir) else {
            return index;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if dir_name.ends_with(".dist-info") {
                index.distributions.push(Self::index_dist_info(&path));
            } else if dir_name.ends_with(".egg-info") {
                index.distributions.push(Self::index_egg_info(&path));
            }
        }

        index
    }

    /// Index one modern `.dist-info` distribution (METADATA + RECORD).
    fn index_dist_info(dist_info_dir: &Path) -> DistributionInfo {
        // Index each distribution separately so ownership facts carry the
        // distribution's own policy metadata (native artifacts, requirements)
        let mut distribution = DistributionInfo::default();

        let metadata_file = dist_info_dir.join("METADATA");
        if let Ok(metadata) = std::fs::read_to_string(metadata_file) {
            Self::index_distribution_metadata(&metadata, &mut distribution);
        }

        let record_file = dist_info_dir.join("RECORD");
        if record_file.exists()
            && let Ok(file) = std::fs::File::open(&record_file)
        {
            let reader = BufReader::new(file);
            for line in reader.lines().map_while(Result::ok) {
                let path_part = Self::record_line_first_field(&line);
                Self::index_record_path(&path_part, &mut distribution);
                if Path::new(path_part.as_str())
                    .extension()
                    .and_then(OsStr::to_str)
                    .is_some_and(Self::is_native_artifact_extension)
                {
                    distribution.ships_native_artifacts = true;
                }
            }
        }

        distribution
    }

    /// Index one legacy `.egg-info` distribution: `PKG-INFO` uses the same Core
    /// Metadata headers, `top_level.txt` declares import roots, and
    /// `installed-files.txt` (when present) lists installed files.
    fn index_egg_info(egg_info_dir: &Path) -> DistributionInfo {
        let mut distribution = DistributionInfo::default();

        let metadata_file = egg_info_dir.join("PKG-INFO");
        if let Ok(metadata) = std::fs::read_to_string(metadata_file) {
            Self::index_distribution_metadata(&metadata, &mut distribution);
        }

        let top_level_file = egg_info_dir.join("top_level.txt");
        if let Ok(top_level) = std::fs::read_to_string(top_level_file) {
            for line in top_level.lines() {
                let import_root = line.trim();
                if !import_root.is_empty() {
                    distribution
                        .declared_prefixes
                        .insert(import_root.to_owned());
                }
            }
        }

        // installed-files.txt paths are relative to the egg-info directory itself
        // (e.g. "../package/module.py"); resolve them against the site-packages root
        let installed_files = egg_info_dir.join("installed-files.txt");
        if let Ok(file) = std::fs::File::open(installed_files) {
            let reader = BufReader::new(file);
            for line in reader.lines().map_while(Result::ok) {
                let record_path = line.trim();
                let normalized = record_path
                    .strip_prefix("../")
                    .unwrap_or(record_path)
                    .trim_start_matches("./");
                Self::index_record_path(normalized, &mut distribution);
                if Path::new(normalized)
                    .extension()
                    .and_then(OsStr::to_str)
                    .is_some_and(Self::is_native_artifact_extension)
                {
                    distribution.ships_native_artifacts = true;
                }
            }
        }

        distribution
    }

    /// Extract the first CSV field of an installed-files `RECORD` line.
    ///
    /// PEP 376 `RECORD` files are CSV: paths containing commas are double-quoted with
    /// `""` escapes, so a plain `split(',')` would truncate them.
    fn record_line_first_field(line: &str) -> String {
        if !line.starts_with('"') {
            return line.split(',').next().unwrap_or("").to_owned();
        }
        let mut field = String::new();
        let mut chars = line[1..].chars();
        while let Some(character) = chars.next() {
            if character == '"' {
                match chars.next() {
                    // An escaped quote ("") inside a quoted field
                    Some('"') => field.push('"'),
                    // The closing quote: the field ends here
                    _ => break,
                }
            } else {
                field.push(character);
            }
        }
        field
    }

    /// Add Core Metadata declarations (import names, distribution name, requirement
    /// specifiers) to a distribution's ownership record. Parsing stops at the first
    /// empty line: Core Metadata is RFC 822, and the free-text description that follows
    /// must not be parsed as headers.
    fn index_distribution_metadata(metadata: &str, distribution: &mut DistributionInfo) {
        for line in metadata.lines() {
            if line.is_empty() {
                break;
            }
            let Some((header, value)) = line.split_once(':') else {
                continue;
            };
            if header.eq_ignore_ascii_case("Name") {
                value.trim().clone_into(&mut distribution.name);
            } else if header.eq_ignore_ascii_case("Requires-Python") {
                let specifier = value.trim();
                if !specifier.is_empty() {
                    distribution.requires_python = Some(specifier.to_owned());
                }
            } else if header.eq_ignore_ascii_case("Requires-Dist") {
                let requirement = value.trim();
                if !requirement.is_empty() {
                    distribution.requires_dist.push(requirement.to_owned());
                }
            } else if header.eq_ignore_ascii_case("Import-Name")
                || header.eq_ignore_ascii_case("Import-Namespace")
            {
                let prefix = value.split(';').next().unwrap_or("").trim();
                if !prefix.is_empty() {
                    distribution.declared_prefixes.insert(prefix.to_owned());
                }
            }
        }
    }

    /// Add import paths implied by one installed distribution `RECORD` entry.
    fn index_record_path(record_path: &str, distribution: &mut DistributionInfo) {
        let normalized = record_path.cow_replace('\\', "/");
        if normalized.starts_with('/')
            || normalized
                .as_bytes()
                .get(1)
                .is_some_and(|byte| *byte == b':')
        {
            return;
        }

        let mut components: Vec<&str> = normalized.split('/').collect();
        if components.is_empty()
            || components
                .iter()
                .any(|component| component.is_empty() || matches!(*component, "." | ".."))
        {
            return;
        }
        let file_name = components
            .pop()
            .expect("non-empty record path should contain a file name");
        let mut import_parts = Vec::new();
        for directory in components {
            import_parts.push(directory);
            distribution.record_imports.insert(import_parts.join("."));
        }

        if let Some((module_name, _)) = file_name.split_once('.')
            && !module_name.is_empty()
        {
            import_parts.push(module_name);
            distribution.record_imports.insert(import_parts.join("."));
        }
    }

    /// Resolves a relative import to an absolute module name.
    ///
    /// # Arguments
    /// * `level` - The number of leading dots (e.g., 1 for `.`, 2 for `..`). Must be > 0.
    /// * `name` - The module being imported, if any (e.g., `Some("bar")` for `from . import bar`).
    /// * `current_module_path` - The filesystem path of the module performing the import.
    ///
    /// # Returns
    /// An `Option<String>` containing the absolute module name if resolution is successful.
    pub fn resolve_relative_to_absolute_module_name(
        &self,
        level: u32,
        name: Option<&str>,
        current_module_path: &Path,
    ) -> Option<String> {
        // Get the absolute module path parts for the current file
        let module_parts = self.path_to_module_parts(current_module_path)?;

        log::debug!(
            "resolve_relative_to_absolute_module_name: path={}, module_parts={:?}, level={}, \
             name={:?}",
            current_module_path.display(),
            module_parts,
            level,
            name
        );

        // Apply relative import logic
        let mut current_parts = module_parts;

        // Check if this is a package __init__ file
        let is_init = current_module_path.file_stem().is_some_and(|s| {
            s == crate::python::constants::INIT_STEM || s == crate::python::constants::MAIN_STEM
        });

        // For __init__.py, level=1 means the current package (don't pop)
        // For regular modules, level=1 means the parent package (pop once)
        let components_to_remove = if is_init {
            level.saturating_sub(1) as usize
        } else {
            level as usize
        };

        log::debug!(
            "  is_init={}, components_to_remove={}, current_parts.len()={}",
            is_init,
            components_to_remove,
            current_parts.len()
        );

        // Cannot go beyond the root of the project
        if components_to_remove > current_parts.len() {
            log::debug!("  Cannot go beyond root - returning None");
            return None;
        }

        for _ in 0..components_to_remove {
            current_parts.pop();
        }

        log::debug!("  After popping: current_parts={current_parts:?}");

        // If name is provided, split it and append. Trim any leading dots to avoid
        // accidental empty components (e.g., "._types").
        if let Some(raw_name) = name {
            let cleaned = raw_name.trim_start_matches('.');
            if !cleaned.is_empty() {
                current_parts.extend(
                    cleaned
                        .split('.')
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned),
                );
            }
        }

        if current_parts.is_empty() {
            // If we're at the root after applying relative levels, return empty string
            // This will be handled by the caller to construct the full import name
            Some(String::new())
        } else {
            Some(current_parts.join("."))
        }
    }

    /// Resolve a relative import given a module or package name (not path)
    /// This is used when we have a module string like "foo.bar.baz" instead of a file path
    ///
    /// For relative imports:
    /// - level=1 (from . import x): Import from the same package as the current module
    /// - level=2 (from .. import x): Import from the parent package
    /// - level=3 (from ... import x): Import from the grandparent package, etc.
    pub fn resolve_relative_import_from_package_name(
        &self,
        level: u32,
        name: Option<&str>,
        current_module_name: &str,
    ) -> String {
        // Determine if current_module_name is a package (__init__ or namespace)
        let mut parts: Vec<&str> = current_module_name.split('.').collect();
        let current_is_package = self.get_module_id_by_name(current_module_name).map_or_else(
            || parts.len() == 1,
            |id| self.is_package_init(id) || self.is_namespace_package(id),
        );

        // For regular modules, drop the last segment; for packages, keep it
        if !current_is_package && parts.len() > 1 {
            parts.pop();
        }

        for _ in 1..level {
            if parts.is_empty() {
                break;
            }
            parts.pop();
        }

        if let Some(name_part) = name.filter(|s| !s.is_empty()) {
            parts.push(name_part);
        }

        let result = parts.join(".");
        debug!(
            "Resolved relative import: level={level}, name={name:?}, from '{current_module_name}' \
             → '{result}'"
        );
        result
    }

    /// Convert a filesystem path to module path components
    fn path_to_module_parts(&self, file_path: &Path) -> Option<Vec<String>> {
        // Convert file_path to absolute path if it's relative and canonicalize it
        let absolute_file_path = if file_path.is_absolute() {
            self.canonicalize_path(file_path.to_path_buf())
        } else {
            let current_working_dir = std::env::current_dir().ok()?;
            let joined = current_working_dir.join(file_path);
            self.canonicalize_path(joined)
        };

        // Find which root contains this file. Under third-party bundling, site-packages
        // roots participate as well so relative imports inside bundled dependencies
        // resolve to absolute module names; they are consulted first because they may be
        // nested inside a search directory (e.g. a virtualenv beside the entry file),
        // whose prefix would otherwise win and yield a mangled module name.
        let mut search_dirs = Vec::new();
        if self.config.bundle_third_party() {
            search_dirs.extend(self.get_virtualenv_site_packages_search_directories(None));
        }
        search_dirs.extend(self.get_search_directories());
        log::trace!(
            "path_to_module_parts: absolute_file_path={}, search_dirs={:?}",
            absolute_file_path.display(),
            search_dirs
        );
        let relative_path = search_dirs.iter().find_map(|dir| {
            // The search directories are already canonicalized/absolute from get_search_directories
            let result = absolute_file_path.strip_prefix(dir).ok();
            if result.is_some() {
                log::trace!(
                    "  Found in search dir: {}, relative_path={:?}",
                    dir.display(),
                    result
                );
            }
            result
        })?;

        // Convert path to module path components
        let mut parts = Vec::new();

        // Add directory components
        if let Some(parent) = relative_path.parent()
            && parent != Path::new("")
        {
            parts.extend(
                parent
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned()),
            );
        }

        // Add the file name (without extension) if it's not __init__ or __main__
        if let Some(file_stem) = relative_path.file_stem() {
            let stem = file_stem.to_string_lossy();
            if stem != crate::python::constants::INIT_STEM
                && stem != crate::python::constants::MAIN_STEM
            {
                parts.push(stem.into_owned());
            }
        }

        Some(parts)
    }

    /// Register a module, rejecting names that resolve to a different canonical file.
    ///
    /// The entry module gets ID 0 and later modules receive sequential IDs. Registering another
    /// name for an existing canonical path creates an alias for the existing module.
    pub fn register_module(&self, name: &str, path: &Path) -> Result<ModuleId> {
        let canonical = self.canonicalize_path(path.to_path_buf());
        let id = {
            let mut registry = self.registry.lock().expect("Module registry lock poisoned");
            registry.register(name.to_owned(), &canonical)
        }?;
        let is_package = {
            let registry = self.registry.lock().expect("Module registry lock poisoned");
            registry.get_metadata(id).is_some_and(|m| m.is_package)
        };

        if id.is_entry() {
            info!("Registered ENTRY module '{name}' at the origin (ID 0)");
        } else {
            debug!(
                "Registered module '{}' with ID {} (package: {})",
                name,
                id.as_u32(),
                is_package
            );
        }

        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;
    use tempfile::TempDir;

    use super::*;
    use crate::config::Config;

    fn create_test_file(path: &Path, content: &str) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)?;
        Ok(())
    }

    fn create_mixed_distribution(site_packages: &Path) -> Result<()> {
        create_test_file(
            &site_packages.join(format!(
                "mixed_package/{}",
                crate::python::constants::INIT_FILE
            )),
            "",
        )?;
        create_test_file(&site_packages.join("mixed_package/core.py"), "")?;
        create_test_file(
            &site_packages.join("mixed_package/_native.cpython-312-test.so"),
            "",
        )?;
        create_test_file(
            &site_packages.join("mixed_package-1.0.dist-info/RECORD"),
            "mixed_package/__init__.py,,\n\
             mixed_package/core.py,,\n\
             mixed_package/_native.cpython-312-test.so,,\n",
        )?;
        create_test_file(
            &site_packages.join("mixed_package-1.0.dist-info/METADATA"),
            "Name: Mixed-Package\nVersion: 1.0\n",
        )
    }

    #[test]
    fn test_registry_owns_alias_identity() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let module_path = temp_dir.path().join("utils.py");
        create_test_file(&module_path, "")?;
        let resolver = ModuleResolver::new(Config::default())?;

        let primary_id = resolver.register_module("utils", &module_path)?;
        let alias_id = resolver.register_module("src.utils", &module_path)?;

        assert_eq!(primary_id, alias_id);
        assert_eq!(resolver.get_module_id_by_name("utils"), Some(primary_id));
        assert_eq!(
            resolver.get_module_id_by_name("src.utils"),
            Some(primary_id)
        );
        assert_eq!(
            resolver.get_module_id_by_path(&module_path),
            Some(primary_id)
        );
        Ok(())
    }

    #[test]
    fn test_registry_rejects_conflicting_module_name_paths() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let first_path = temp_dir.path().join("first/shared.py");
        let second_path = temp_dir.path().join("second/shared.py");
        create_test_file(&first_path, "SOURCE = 'first'")?;
        create_test_file(&second_path, "SOURCE = 'second'")?;
        let resolver = ModuleResolver::new(Config::default())?;

        let first_id = resolver.register_module("shared", &first_path)?;
        let error = resolver
            .register_module("shared", &second_path)
            .expect_err("one import name must not resolve to multiple files");

        assert_eq!(resolver.get_module_id_by_name("shared"), Some(first_id));
        assert_eq!(resolver.get_module_id_by_path(&first_path), Some(first_id));
        assert_eq!(resolver.get_module_id_by_path(&second_path), None);
        assert_eq!(
            error.to_string(),
            format!(
                "Import name 'shared' resolves to conflicting files: '{}' and '{}'",
                first_path.canonicalize()?.display(),
                second_path.canonicalize()?.display()
            )
        );
        Ok(())
    }

    #[test]
    fn test_module_first_resolution() -> Result<()> {
        // Test that foo/__init__.py is preferred over foo.py
        let temp_dir = TempDir::new()?;
        let root = temp_dir.path();

        // Create both foo/__init__.py and foo.py
        create_test_file(
            &root.join(format!("foo/{}", crate::python::constants::INIT_FILE)),
            "# Package",
        )?;
        create_test_file(&root.join("foo.py"), "# Module")?;

        let config = Config {
            src: vec![root.to_path_buf()],
            ..Default::default()
        };
        let resolver = ModuleResolver::new(config)?;

        // Resolve foo - should prefer foo/__init__.py
        let result = resolver.resolve_module_path("foo")?;
        let expected = root
            .join(format!("foo/{}", crate::python::constants::INIT_FILE))
            .canonicalize()?;
        assert_eq!(
            result.map(|p| p
                .canonicalize()
                .expect("failed to canonicalize resolved path")),
            Some(expected)
        );

        Ok(())
    }

    #[test]
    fn test_entry_dir_first_in_search_path() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let root = temp_dir.path();

        // Create entry file and module in entry dir
        let entry_dir = root.join("src/app");
        let entry_file = entry_dir.join("main.py");
        create_test_file(&entry_file, "# Main")?;
        create_test_file(&entry_dir.join("helper.py"), "# Helper")?;

        // Create a different helper in configured src
        let other_src = root.join("lib");
        create_test_file(&other_src.join("helper.py"), "# Other helper")?;

        let config = Config {
            src: vec![other_src],
            ..Default::default()
        };
        let mut resolver = ModuleResolver::new(config)?;
        resolver.set_entry_file(&entry_file, &entry_file);

        // Resolve helper - should find the one in entry dir, not lib
        let result = resolver.resolve_module_path("helper")?;
        let expected = entry_dir.join("helper.py").canonicalize()?;
        assert_eq!(
            result.map(|p| p
                .canonicalize()
                .expect("failed to canonicalize resolved path")),
            Some(expected)
        );

        // Verify search path order
        let search_dirs = resolver.get_search_directories();
        assert!(!search_dirs.is_empty());
        // First dir should be the entry dir
        assert_eq!(search_dirs[0], entry_dir.canonicalize()?);

        Ok(())
    }

    #[test]
    fn test_package_resolution() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let root = temp_dir.path();

        // Create nested package structure
        create_test_file(
            &root.join(format!("myapp/{}", crate::python::constants::INIT_FILE)),
            "",
        )?;
        create_test_file(
            &root.join(format!(
                "myapp/utils/{}",
                crate::python::constants::INIT_FILE
            )),
            "",
        )?;
        create_test_file(&root.join("myapp/utils/helpers.py"), "")?;

        let config = Config {
            src: vec![root.to_path_buf()],
            ..Default::default()
        };
        let resolver = ModuleResolver::new(config)?;

        // Test various imports
        assert_eq!(
            resolver.resolve_module_path("myapp")?.map(|p| p
                .canonicalize()
                .expect("failed to canonicalize resolved path")),
            Some(
                root.join(format!("myapp/{}", crate::python::constants::INIT_FILE))
                    .canonicalize()?
            )
        );
        assert_eq!(
            resolver.resolve_module_path("myapp.utils")?.map(|p| p
                .canonicalize()
                .expect("failed to canonicalize resolved path")),
            Some(
                root.join(format!(
                    "myapp/utils/{}",
                    crate::python::constants::INIT_FILE
                ))
                .canonicalize()?
            )
        );
        assert_eq!(
            resolver
                .resolve_module_path("myapp.utils.helpers")?
                .map(|p| p
                    .canonicalize()
                    .expect("failed to canonicalize resolved path")),
            Some(root.join("myapp/utils/helpers.py").canonicalize()?)
        );

        Ok(())
    }

    #[test]
    fn test_classification() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let root = temp_dir.path();

        // Create a first-party module
        create_test_file(&root.join("mymodule.py"), "")?;

        let config = Config {
            src: vec![root.to_path_buf()],
            known_first_party: IndexSet::from(["known_first".to_owned()]),
            known_third_party: IndexSet::from(["requests".to_owned()]),
            ..Default::default()
        };
        let resolver = ModuleResolver::new(config)?;

        // Test classifications
        assert_eq!(
            resolver.classify_import("os").origin,
            ImportOrigin::StandardLibrary
        );
        assert_eq!(
            resolver.classify_import("sys").origin,
            ImportOrigin::StandardLibrary
        );
        assert_eq!(
            resolver.classify_import("mymodule").origin,
            ImportOrigin::FirstParty
        );
        assert_eq!(
            resolver.classify_import("known_first").origin,
            ImportOrigin::FirstParty
        );
        assert_eq!(
            resolver.classify_import("requests").origin,
            ImportOrigin::ThirdParty
        );
        assert_eq!(
            resolver.classify_import(".relative").origin,
            ImportOrigin::FirstParty
        );
        assert_eq!(
            resolver.classify_import("unknown_module").origin,
            ImportOrigin::Unknown
        );

        Ok(())
    }

    #[test]
    fn test_classification_respects_target_python_version() -> Result<()> {
        let mut py38_config = Config::default();
        py38_config.set_target_version("py38".to_owned())?;
        let py38_resolver = ModuleResolver::new(py38_config)?;
        let py38_classification = py38_resolver.classify_import("zoneinfo");

        assert_ne!(py38_classification.origin, ImportOrigin::StandardLibrary);

        let py310_resolver = ModuleResolver::new(Config::default())?;
        let py310_classification = py310_resolver.classify_import("zoneinfo");

        assert_eq!(py310_classification.origin, ImportOrigin::StandardLibrary);

        Ok(())
    }

    #[test]
    fn test_invalid_target_python_version_is_rejected() {
        let config = Config {
            target_version: "invalid".to_owned(),
            ..Default::default()
        };

        let error =
            ModuleResolver::new(config).expect_err("invalid target version should be rejected");

        assert_eq!(
            error.to_string(),
            "ModuleResolver requires a validated target Python version"
        );
        assert!(
            error
                .root_cause()
                .to_string()
                .contains("Invalid target version 'invalid'")
        );
    }

    #[test]
    fn test_distribution_origin_is_independent_from_bundle_disposition() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let site_packages = temp_dir.path().join("site-packages");
        create_mixed_distribution(&site_packages)?;

        let pythonpath = site_packages.to_string_lossy();
        let resolver =
            ModuleResolver::new_with_overrides(Config::default(), Some(pythonpath.as_ref()), None)?;

        let package = resolver.classify_import("mixed_package");
        assert_eq!(package.origin, ImportOrigin::ThirdParty);
        assert_eq!(package.source, ImportSource::Python);
        assert_eq!(package.bundle, BundleDisposition::Include);

        let source_submodule = resolver.classify_import("mixed_package.core");
        assert_eq!(source_submodule.origin, ImportOrigin::ThirdParty);
        assert_eq!(source_submodule.source, ImportSource::Python);
        assert_eq!(source_submodule.bundle, BundleDisposition::Include);

        let native_submodule = resolver.classify_import("mixed_package._native");
        assert_eq!(native_submodule.origin, ImportOrigin::ThirdParty);
        assert_eq!(native_submodule.source, ImportSource::NativeExtension);
        assert_eq!(native_submodule.bundle, BundleDisposition::External);
        assert!(
            resolver
                .resolve_module_path("mixed_package._native")?
                .is_none(),
            "native extensions must not be sent to the Python parser"
        );

        Ok(())
    }

    #[test]
    fn test_distribution_metadata_import_headers_are_case_insensitive() {
        let metadata = "\
Metadata-Version: 2.5
name: mixed-case-distribution
import-name: lower_case.module
IMPORT-NAMESPACE: UPPER_CASE
requires-python: >=3.8
requires-dist: helper-dependency>=1
";

        let mut distribution = DistributionInfo::default();
        ModuleResolver::index_distribution_metadata(metadata, &mut distribution);

        assert!(distribution.owns_import("lower_case.module.child"));
        assert!(distribution.owns_import("UPPER_CASE.child"));
        assert!(!distribution.owns_import("unrelated"));
        assert_eq!(distribution.name, "mixed-case-distribution");
        assert_eq!(distribution.requires_python.as_deref(), Some(">=3.8"));
        assert_eq!(distribution.requires_dist, vec!["helper-dependency>=1"]);
    }

    /// Core Metadata is RFC 822: header-like lines in the free-text description after
    /// the first empty line must not be parsed as headers.
    #[test]
    fn test_distribution_metadata_parsing_stops_at_description() {
        let metadata = "\
Metadata-Version: 2.5
Name: real-name
Import-Name: real_module

This README documents the package.
Requires-Python: >=3.13
Requires-Dist: injected-dependency
Name: other-name
Import-Name: injected_module
";

        let mut distribution = DistributionInfo::default();
        ModuleResolver::index_distribution_metadata(metadata, &mut distribution);

        assert_eq!(distribution.name, "real-name");
        assert_eq!(distribution.requires_python, None);
        assert!(distribution.requires_dist.is_empty());
        assert!(distribution.owns_import("real_module"));
        assert!(!distribution.owns_import("injected_module"));
    }

    #[test]
    fn test_distribution_ownership_is_cached_by_search_root() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let site_packages = temp_dir.path().join("site-packages");
        create_mixed_distribution(&site_packages)?;
        let resolver = ModuleResolver::new(Config::default())?;

        assert!(resolver.distribution_owns_import(&site_packages, "mixed_package.core"));
        fs::remove_dir_all(site_packages.join("mixed_package-1.0.dist-info"))?;
        assert!(resolver.distribution_owns_import(&site_packages, "mixed_package._native"));
        assert_eq!(resolver.distribution_ownership_cache.borrow().len(), 1);

        Ok(())
    }

    #[test]
    fn test_native_package_initializers_remain_external() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let root = temp_dir.path();
        create_test_file(&root.join("native_parent/__init__.cpython-312-test.so"), "")?;
        create_test_file(
            &root.join("native_parent/native_child/__init__.cp312-win_amd64.pyd"),
            "",
        )?;

        let resolver = ModuleResolver::new(Config {
            src: vec![root.to_path_buf()],
            ..Default::default()
        })?;

        for module_name in ["native_parent", "native_parent.native_child"] {
            let classification = resolver.classify_import(module_name);
            assert_eq!(classification.source, ImportSource::NativeExtension);
            assert_eq!(classification.bundle, BundleDisposition::External);
            assert!(
                resolver.resolve_module_path(module_name)?.is_none(),
                "native package initializers must not be sent to the Python parser"
            );
        }

        Ok(())
    }

    #[test]
    fn test_virtualenv_python_distribution_remains_external() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        let site_packages = virtualenv.join("lib/python3.12/site-packages");
        create_mixed_distribution(&site_packages)?;

        let virtualenv_path = virtualenv.to_string_lossy();
        let resolver = ModuleResolver::new_with_overrides(
            Config::default(),
            Some(""),
            Some(virtualenv_path.as_ref()),
        )?;

        let classification = resolver.classify_import("mixed_package");
        assert_eq!(classification.origin, ImportOrigin::ThirdParty);
        assert_eq!(classification.source, ImportSource::Python);
        assert_eq!(classification.bundle, BundleDisposition::External);

        Ok(())
    }

    #[test]
    fn test_namespace_package() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let root = temp_dir.path();

        // Create namespace package (directory without __init__.py)
        fs::create_dir_all(root.join("namespace_pkg/subpkg"))?;
        create_test_file(&root.join("namespace_pkg/subpkg/module.py"), "")?;

        let config = Config {
            src: vec![root.to_path_buf()],
            ..Default::default()
        };
        let resolver = ModuleResolver::new(config)?;

        // Namespace packages should be resolved to the directory
        let result = resolver.resolve_module_path("namespace_pkg")?;
        assert!(result.is_some());
        let resolved_path = result.expect("namespace_pkg should resolve to a path");
        assert!(resolved_path.is_dir());
        let expected = root.join("namespace_pkg").canonicalize()?;
        assert_eq!(resolved_path.canonicalize()?, expected);

        // Should be classified as first-party
        assert_eq!(
            resolver.classify_import("namespace_pkg").origin,
            ImportOrigin::FirstParty
        );

        Ok(())
    }

    #[test]
    fn test_non_python_shared_libraries_do_not_shadow_namespace_package() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let root = temp_dir.path();

        create_test_file(&root.join("namespace_pkg/module.py"), "")?;
        create_test_file(&root.join("namespace_pkg.dll"), "")?;
        create_test_file(&root.join("namespace_pkg.dylib"), "")?;

        let resolver = ModuleResolver::new(Config {
            src: vec![root.to_path_buf()],
            ..Default::default()
        })?;

        assert_eq!(
            resolver.resolve_module_path("namespace_pkg")?,
            Some(root.join("namespace_pkg").canonicalize()?)
        );
        assert_eq!(
            resolver.classify_import("namespace_pkg").source,
            ImportSource::NamespacePackage
        );

        Ok(())
    }

    #[test]
    fn test_relative_import_resolution() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let root = temp_dir.path();

        // Create a package structure:
        // mypackage/
        //   __init__.py
        //   module1.py
        //   subpackage/
        //     __init__.py
        //     module2.py
        //     deeper/
        //       __init__.py
        //       module3.py

        fs::create_dir_all(root.join("mypackage/subpackage/deeper"))?;
        create_test_file(
            &root.join(format!("mypackage/{}", crate::python::constants::INIT_FILE)),
            "# Package init",
        )?;
        create_test_file(&root.join("mypackage/module1.py"), "# Module 1")?;
        create_test_file(
            &root.join(format!(
                "mypackage/subpackage/{}",
                crate::python::constants::INIT_FILE
            )),
            "# Subpackage init",
        )?;
        create_test_file(&root.join("mypackage/subpackage/module2.py"), "# Module 2")?;
        create_test_file(
            &root.join(format!(
                "mypackage/subpackage/deeper/{}",
                crate::python::constants::INIT_FILE
            )),
            "# Deeper init",
        )?;
        create_test_file(
            &root.join("mypackage/subpackage/deeper/module3.py"),
            "# Module 3",
        )?;

        let config = Config {
            src: vec![root.to_path_buf()],
            ..Default::default()
        };
        let resolver = ModuleResolver::new(config)?;

        // Test relative import from module3.py
        let module3_path = root.join("mypackage/subpackage/deeper/module3.py");

        // Test "from . import module3" (same directory)
        assert_eq!(
            resolver.resolve_module_path_with_context(".module3", Some(&module3_path))?,
            Some(
                root.join("mypackage/subpackage/deeper/module3.py")
                    .canonicalize()?
            )
        );

        // Test "from .. import module2" (parent directory)
        assert_eq!(
            resolver.resolve_module_path_with_context("..module2", Some(&module3_path))?,
            Some(
                root.join("mypackage/subpackage/module2.py")
                    .canonicalize()?
            )
        );

        // Test "from ... import module1" (grandparent directory)
        assert_eq!(
            resolver.resolve_module_path_with_context("...module1", Some(&module3_path))?,
            Some(root.join("mypackage/module1.py").canonicalize()?)
        );

        // Test "from . import" (current package)
        assert_eq!(
            resolver.resolve_module_path_with_context(".", Some(&module3_path))?,
            Some(
                root.join(format!(
                    "mypackage/subpackage/deeper/{}",
                    crate::python::constants::INIT_FILE
                ))
                .canonicalize()?
            )
        );

        // Test "from .. import" (parent package)
        assert_eq!(
            resolver.resolve_module_path_with_context("..", Some(&module3_path))?,
            Some(
                root.join(format!(
                    "mypackage/subpackage/{}",
                    crate::python::constants::INIT_FILE
                ))
                .canonicalize()?
            )
        );

        // Test relative import from a package __init__.py
        let subpackage_init = root.join(format!(
            "mypackage/subpackage/{}",
            crate::python::constants::INIT_FILE
        ));

        // Test "from . import module2" from __init__.py
        assert_eq!(
            resolver.resolve_module_path_with_context(".module2", Some(&subpackage_init))?,
            Some(
                root.join("mypackage/subpackage/module2.py")
                    .canonicalize()?
            )
        );

        // Test "from .deeper import module3"
        assert_eq!(
            resolver.resolve_module_path_with_context(".deeper.module3", Some(&subpackage_init))?,
            Some(
                root.join("mypackage/subpackage/deeper/module3.py")
                    .canonicalize()?
            )
        );

        // Test error case: too many dots
        let result =
            resolver.resolve_module_path_with_context("....toomanydots", Some(&module3_path));
        assert!(result.is_err() || result.expect("result should be Ok").is_none());

        Ok(())
    }

    #[test]
    fn test_pythonpath_module_discovery() -> Result<()> {
        // Create temporary directories for testing
        let temp_dir = TempDir::new()?;
        let pythonpath_dir = temp_dir.path().join("pythonpath_modules");
        let src_dir = temp_dir.path().join("src");

        // Create directory structures
        fs::create_dir_all(&pythonpath_dir)?;
        fs::create_dir_all(&src_dir)?;

        // Create a module in PYTHONPATH directory
        let pythonpath_module = pythonpath_dir.join("pythonpath_module.py");
        fs::write(
            &pythonpath_module,
            "# This is a PYTHONPATH module\ndef hello():\n    return 'Hello from PYTHONPATH'",
        )?;

        // Create a package in PYTHONPATH directory
        let pythonpath_pkg = pythonpath_dir.join("pythonpath_pkg");
        fs::create_dir_all(&pythonpath_pkg)?;
        let pythonpath_pkg_init = pythonpath_pkg.join(crate::python::constants::INIT_FILE);
        fs::write(&pythonpath_pkg_init, "# PYTHONPATH package")?;
        let pythonpath_pkg_module = pythonpath_pkg.join("submodule.py");
        fs::write(&pythonpath_pkg_module, "# PYTHONPATH submodule")?;

        // Create a module in src directory
        let src_module = src_dir.join("src_module.py");
        fs::write(&src_module, "# This is a src module")?;

        // Set up config with src directory
        let config = Config {
            src: vec![src_dir],
            ..Default::default()
        };

        // Create resolver with PYTHONPATH override
        let pythonpath_str = pythonpath_dir.to_string_lossy();
        let resolver = ModuleResolver::new_with_overrides(config, Some(&pythonpath_str), None)?;

        // Test that modules can be resolved from both src and PYTHONPATH
        assert!(
            resolver.resolve_module_path("src_module")?.is_some(),
            "Should resolve modules from configured src directories"
        );
        assert!(
            resolver.resolve_module_path("pythonpath_module")?.is_some(),
            "Should resolve modules from PYTHONPATH directories"
        );
        assert!(
            resolver.resolve_module_path("pythonpath_pkg")?.is_some(),
            "Should resolve packages from PYTHONPATH directories"
        );
        assert!(
            resolver
                .resolve_module_path("pythonpath_pkg.submodule")?
                .is_some(),
            "Should resolve submodules from PYTHONPATH packages"
        );

        // Also verify classification
        assert_eq!(
            resolver.classify_import("src_module").origin,
            ImportOrigin::FirstParty,
            "Should classify src_module as first-party"
        );
        assert_eq!(
            resolver.classify_import("pythonpath_module").origin,
            ImportOrigin::FirstParty,
            "Should classify pythonpath_module as first-party"
        );
        assert_eq!(
            resolver.classify_import("pythonpath_pkg").origin,
            ImportOrigin::FirstParty,
            "Should classify pythonpath_pkg as first-party"
        );
        assert_eq!(
            resolver.classify_import("pythonpath_pkg.submodule").origin,
            ImportOrigin::FirstParty,
            "Should classify pythonpath_pkg.submodule as first-party"
        );

        Ok(())
    }

    #[test]
    fn test_pythonpath_module_classification() -> Result<()> {
        // Create temporary directories for testing
        let temp_dir = TempDir::new()?;
        let pythonpath_dir = temp_dir.path().join("pythonpath_modules");
        let src_dir = temp_dir.path().join("src");

        // Create directory structures
        fs::create_dir_all(&pythonpath_dir)?;
        fs::create_dir_all(&src_dir)?;

        // Create a module in PYTHONPATH directory
        let pythonpath_module = pythonpath_dir.join("pythonpath_module.py");
        fs::write(&pythonpath_module, "# This is a PYTHONPATH module")?;

        // Set up config
        let config = Config {
            src: vec![src_dir],
            ..Default::default()
        };

        // Create resolver with PYTHONPATH override
        let pythonpath_str = pythonpath_dir.to_string_lossy();
        let resolver = ModuleResolver::new_with_overrides(config, Some(&pythonpath_str), None)?;

        // Test that PYTHONPATH modules are classified as first-party
        assert_eq!(
            resolver.classify_import("pythonpath_module").origin,
            ImportOrigin::FirstParty,
            "PYTHONPATH modules should be classified as first-party"
        );

        // Unknown modules still remain external and produce a fallback requirement.
        assert_eq!(
            resolver.classify_import("unknown_module").origin,
            ImportOrigin::Unknown,
            "Unknown modules should preserve their unknown origin"
        );

        Ok(())
    }

    #[test]
    fn test_pythonpath_multiple_directories() -> Result<()> {
        // Create temporary directories for testing
        let temp_dir = TempDir::new()?;
        let pythonpath_dir1 = temp_dir.path().join("pythonpath1");
        let pythonpath_dir2 = temp_dir.path().join("pythonpath2");
        let src_dir = temp_dir.path().join("src");

        // Create directory structures
        fs::create_dir_all(&pythonpath_dir1)?;
        fs::create_dir_all(&pythonpath_dir2)?;
        fs::create_dir_all(&src_dir)?;

        // Create modules in different PYTHONPATH directories
        let module1 = pythonpath_dir1.join("module1.py");
        fs::write(&module1, "# Module in pythonpath1")?;

        let module2 = pythonpath_dir2.join("module2.py");
        fs::write(&module2, "# Module in pythonpath2")?;

        // Set up config
        let config = Config {
            src: vec![src_dir],
            ..Default::default()
        };

        // Create resolver with PYTHONPATH override (multiple directories separated by
        // platform-appropriate separator)
        let separator = if cfg!(windows) { ';' } else { ':' };
        let pythonpath_str = format!(
            "{}{}{}",
            pythonpath_dir1.to_string_lossy(),
            separator,
            pythonpath_dir2.to_string_lossy()
        );
        let resolver = ModuleResolver::new_with_overrides(config, Some(&pythonpath_str), None)?;

        // Test that modules from both PYTHONPATH directories can be resolved
        assert!(
            resolver.resolve_module_path("module1")?.is_some(),
            "Should resolve modules from first PYTHONPATH directory"
        );
        assert!(
            resolver.resolve_module_path("module2")?.is_some(),
            "Should resolve modules from second PYTHONPATH directory"
        );

        // Also verify classification
        assert_eq!(
            resolver.classify_import("module1").origin,
            ImportOrigin::FirstParty,
            "Should classify module1 as first-party"
        );
        assert_eq!(
            resolver.classify_import("module2").origin,
            ImportOrigin::FirstParty,
            "Should classify module2 as first-party"
        );

        Ok(())
    }

    #[test]
    fn test_pythonpath_empty_or_nonexistent() -> Result<()> {
        // Create a temporary directory for testing
        let temp_dir = TempDir::new()?;
        let src_dir = temp_dir.path().join("src");
        fs::create_dir_all(&src_dir)?;

        // Create a test module
        let test_module = src_dir.join("test_module.py");
        fs::write(&test_module, "# Test module")?;

        let config = Config {
            src: vec![src_dir],
            ..Default::default()
        };

        // Test with empty PYTHONPATH
        let resolver1 = ModuleResolver::new_with_overrides(config.clone(), Some(""), None)?;

        // Should be able to resolve module from src directory
        assert!(
            resolver1.resolve_module_path("test_module")?.is_some(),
            "Should resolve module from src directory with empty PYTHONPATH"
        );

        // Test with no PYTHONPATH
        let resolver2 = ModuleResolver::new_with_overrides(config.clone(), None, None)?;

        // Should be able to resolve module from src directory
        assert!(
            resolver2.resolve_module_path("test_module")?.is_some(),
            "Should resolve module from src directory with no PYTHONPATH"
        );

        // Test with nonexistent directories in PYTHONPATH
        let separator = if cfg!(windows) { ';' } else { ':' };
        let nonexistent_pythonpath = format!("/nonexistent1{separator}/nonexistent2");
        let resolver3 =
            ModuleResolver::new_with_overrides(config, Some(&nonexistent_pythonpath), None)?;

        // Should still be able to resolve module from src directory
        assert!(
            resolver3.resolve_module_path("test_module")?.is_some(),
            "Should resolve module from src directory even with nonexistent PYTHONPATH"
        );

        // Non-existent modules should not be found
        assert!(
            resolver3
                .resolve_module_path("nonexistent_module")?
                .is_none(),
            "Should not find nonexistent modules"
        );

        Ok(())
    }

    #[test]
    fn test_directory_deduplication() -> Result<()> {
        // Create temporary directories for testing
        let temp_dir = TempDir::new()?;
        let src_dir = temp_dir.path().join("src");
        let other_dir = temp_dir.path().join("other");

        // Create directory structures
        fs::create_dir_all(&src_dir)?;
        fs::create_dir_all(&other_dir)?;

        // Create modules
        let src_module = src_dir.join("src_module.py");
        fs::write(&src_module, "# Source module")?;
        let other_module = other_dir.join("other_module.py");
        fs::write(&other_module, "# Other module")?;

        // Set up config with src directory
        let config = Config {
            src: vec![src_dir.clone()],
            ..Default::default()
        };

        // Create resolver with PYTHONPATH override that includes the same src directory plus
        // another directory
        let separator = if cfg!(windows) { ';' } else { ':' };
        let pythonpath_str = format!(
            "{}{}{}",
            src_dir.to_string_lossy(),
            separator,
            other_dir.to_string_lossy()
        );
        let resolver = ModuleResolver::new_with_overrides(config, Some(&pythonpath_str), None)?;

        // Test that deduplication works - both modules should be resolvable
        assert!(
            resolver.resolve_module_path("src_module")?.is_some(),
            "Should resolve src_module"
        );
        assert!(
            resolver.resolve_module_path("other_module")?.is_some(),
            "Should resolve other_module"
        );

        // Both should be classified as first-party
        assert_eq!(
            resolver.classify_import("src_module").origin,
            ImportOrigin::FirstParty,
            "Should classify src_module as first-party"
        );
        assert_eq!(
            resolver.classify_import("other_module").origin,
            ImportOrigin::FirstParty,
            "Should classify other_module as first-party"
        );

        Ok(())
    }

    #[test]
    fn test_path_canonicalization() -> Result<()> {
        // Create temporary directories for testing
        let temp_dir = TempDir::new()?;
        let src_dir = temp_dir.path().join("src");
        fs::create_dir_all(&src_dir)?;

        // Create a module
        let module_file = src_dir.join("test_module.py");
        fs::write(&module_file, "# Test module")?;

        // Set up config with the src directory
        let config = Config {
            src: vec![src_dir.clone()],
            ..Default::default()
        };

        // Create resolver with PYTHONPATH override using a relative path with .. components
        // This creates a different string representation of the same directory
        let parent_dir = src_dir
            .parent()
            .expect("test source directory should have a parent");
        let relative_path = parent_dir.join("src/../src"); // This resolves to the same directory
        let pythonpath_str = relative_path.to_string_lossy();
        let resolver = ModuleResolver::new_with_overrides(config, Some(&pythonpath_str), None)?;

        // Test that the module can be resolved despite path canonicalization differences
        assert!(
            resolver.resolve_module_path("test_module")?.is_some(),
            "Should resolve module even with different path representations"
        );

        // Should be classified as first-party
        assert_eq!(
            resolver.classify_import("test_module").origin,
            ImportOrigin::FirstParty,
            "Should classify test_module as first-party"
        );

        Ok(())
    }

    /// Create a fake virtualenv with a pure-Python distribution and a distribution that
    /// ships native extension artifacts, returning the site-packages directory.
    fn create_bundle_third_party_virtualenv(virtualenv: &Path) -> Result<PathBuf> {
        let site_packages = virtualenv.join("lib/python3.12/site-packages");

        // Pure-Python package whose __init__.py uses a relative import
        create_test_file(
            &site_packages.join(format!(
                "pure_package/{}",
                crate::python::constants::INIT_FILE
            )),
            "from .helpers import helper\nVALUE = 'pure'\n",
        )?;
        create_test_file(
            &site_packages.join("pure_package/helpers.py"),
            "def helper():\n    return 'helper'\n",
        )?;

        // Package with a native extension buried in a subdirectory
        create_test_file(
            &site_packages.join(format!(
                "native_package/{}",
                crate::python::constants::INIT_FILE
            )),
            "",
        )?;
        create_test_file(
            &site_packages.join("native_package/_internals/speedups.cpython-312-test.so"),
            "",
        )?;

        Ok(site_packages)
    }

    /// Build a resolver with third-party bundling enabled against a fake virtualenv.
    fn bundle_third_party_resolver(virtualenv: &Path) -> Result<ModuleResolver> {
        let config = Config {
            bundle_third_party: Some(true),
            ..Default::default()
        };
        let virtualenv_path = virtualenv.to_string_lossy();
        ModuleResolver::new_with_overrides(config, Some(""), Some(virtualenv_path.as_ref()))
    }

    /// Pure-Python site-packages distributions are classified for bundling and resolve
    /// to real site-packages paths when third-party bundling is enabled.
    #[test]
    fn test_bundle_third_party_includes_pure_python_distribution() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        let site_packages = create_bundle_third_party_virtualenv(&virtualenv)?;
        let resolver = bundle_third_party_resolver(&virtualenv)?;

        let classification = resolver.classify_import("pure_package");
        assert_eq!(classification.origin, ImportOrigin::ThirdParty);
        assert_eq!(classification.source, ImportSource::Python);
        assert_eq!(classification.bundle, BundleDisposition::Include);

        let submodule = resolver.classify_import("pure_package.helpers");
        assert_eq!(submodule.bundle, BundleDisposition::Include);

        // Resolution must produce actual site-packages paths for bundled modules
        assert_eq!(
            resolver.resolve_module_path("pure_package")?,
            Some(
                site_packages
                    .join(format!(
                        "pure_package/{}",
                        crate::python::constants::INIT_FILE
                    ))
                    .canonicalize()?
            )
        );
        assert_eq!(
            resolver.resolve_module_path("pure_package.helpers")?,
            Some(
                site_packages
                    .join("pure_package/helpers.py")
                    .canonicalize()?
            )
        );

        // Relative imports inside the bundled package resolve against site-packages
        let package_init = site_packages.join(format!(
            "pure_package/{}",
            crate::python::constants::INIT_FILE
        ));
        assert_eq!(
            resolver.resolve_module_path_with_context(".helpers", Some(&package_init))?,
            Some(
                site_packages
                    .join("pure_package/helpers.py")
                    .canonicalize()?
            ),
            "relative imports inside bundled site-packages modules must resolve"
        );

        Ok(())
    }

    /// A distribution shipping native extension artifacts anywhere inside its package
    /// directory stays external as a whole, even when its `__init__.py` is pure Python.
    #[test]
    fn test_bundle_third_party_keeps_native_distribution_external() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        create_bundle_third_party_virtualenv(&virtualenv)?;
        let resolver = bundle_third_party_resolver(&virtualenv)?;

        // Even though native_package/__init__.py is pure Python, the package ships a
        // native artifact in a nested directory, so the whole package stays external
        let classification = resolver.classify_import("native_package");
        assert_eq!(classification.origin, ImportOrigin::ThirdParty);
        assert_eq!(classification.source, ImportSource::Python);
        assert_eq!(classification.bundle, BundleDisposition::External);
        assert!(
            resolver.resolve_module_path("native_package")?.is_none(),
            "external native-extension packages must not resolve to bundle paths"
        );

        Ok(())
    }

    /// `known_third_party` entries force listed packages external even when pure Python.
    #[test]
    fn test_bundle_third_party_respects_known_third_party_override() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        create_bundle_third_party_virtualenv(&virtualenv)?;

        let config = Config {
            bundle_third_party: Some(true),
            known_third_party: IndexSet::from(["pure_package".to_owned()]),
            ..Default::default()
        };
        let virtualenv_path = virtualenv.to_string_lossy();
        let resolver =
            ModuleResolver::new_with_overrides(config, Some(""), Some(virtualenv_path.as_ref()))?;

        // Explicit known_third_party acts as a manual "keep external" escape hatch
        let classification = resolver.classify_import("pure_package");
        assert_eq!(classification.bundle, BundleDisposition::External);
        assert!(resolver.resolve_module_path("pure_package")?.is_none());

        // The escape hatch covers submodules of the configured package root as well
        let submodule = resolver.classify_import("pure_package.helpers");
        assert_eq!(submodule.origin, ImportOrigin::ThirdParty);
        assert_eq!(submodule.bundle, BundleDisposition::External);
        assert!(
            resolver
                .resolve_module_path("pure_package.helpers")?
                .is_none(),
            "submodules of known_third_party roots must stay external"
        );

        Ok(())
    }

    /// Packages reading their own installed distribution metadata at runtime
    /// (`importlib.metadata`, `pkg_resources`) stay external because that metadata is
    /// unavailable once the source is inlined into a bundle.
    #[test]
    fn test_bundle_third_party_keeps_metadata_dependent_distribution_external() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        let site_packages = virtualenv.join("lib/python3.12/site-packages");
        // Parenthesized multiline import form: detection must be AST-based
        create_test_file(
            &site_packages.join(format!(
                "metadata_package/{}",
                crate::python::constants::INIT_FILE
            )),
            "from importlib import (\n    metadata,\n)\n__version__ = \
             metadata.version('metadata-package')\n",
        )?;
        let resolver = bundle_third_party_resolver(&virtualenv)?;

        let classification = resolver.classify_import("metadata_package");
        assert_eq!(classification.origin, ImportOrigin::ThirdParty);
        assert_eq!(classification.bundle, BundleDisposition::External);
        assert!(
            resolver.resolve_module_path("metadata_package")?.is_none(),
            "metadata-dependent packages must not be inlined"
        );

        Ok(())
    }

    /// The installed-package API import detector recognizes all valid import forms and
    /// ignores non-import mentions of the API names.
    #[test]
    fn test_distribution_metadata_import_detection() {
        // Positive: every import form that makes the installed distribution a runtime
        // dependency (dist-info metadata or adjacent package data files)
        for source in [
            "import importlib.metadata\n",
            "import importlib.metadata as ilm\n",
            "from importlib import metadata\n",
            "from importlib import (\n    metadata,\n)\n",
            "from importlib import util, metadata\n",
            "from importlib.metadata import version\n",
            "import importlib_metadata\n",
            "import pkg_resources\n",
            "from pkg_resources import get_distribution\n",
            "import importlib.resources\n",
            "from importlib import resources\n",
            "from importlib.resources import files\n",
            "import importlib_resources\n",
            "from importlib_resources import files\n",
            "def lazy():\n    from importlib import metadata\n    return metadata\n",
        ] {
            assert!(
                ModuleResolver::python_source_blocks_bundling(source),
                "should detect installed-package dependency in: {source:?}"
            );
        }

        // Negative: importlib usage that does not touch installed-package data
        for source in [
            "import importlib\nimportlib.import_module('json')\n",
            "from importlib import util\n",
            "'''docstring mentioning importlib.metadata and pkg_resources'''\n",
            "# comment: importlib.metadata\nVALUE = 1\n",
            "from .metadata import local_helper\n",
            "from .resources import local_data\n",
            "VALUE = 1\n",
        ] {
            assert!(
                !ModuleResolver::python_source_blocks_bundling(source),
                "should not flag: {source:?}"
            );
        }
    }

    /// Static `importlib.import_module` relative imports inside bundled site-packages
    /// dependencies resolve through the site-packages fallback.
    #[test]
    fn test_bundle_third_party_resolves_importlib_static_relative_import() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        let site_packages = create_bundle_third_party_virtualenv(&virtualenv)?;
        let resolver = bundle_third_party_resolver(&virtualenv)?;

        let (resolved_name, resolved_path) = resolver
            .resolve_importlib_static_with_context(".helpers", Some("pure_package"))
            .expect("importlib static relative import must resolve for bundled packages");
        assert_eq!(resolved_name, "pure_package.helpers");
        assert_eq!(
            resolved_path,
            site_packages
                .join("pure_package/helpers.py")
                .canonicalize()?
        );

        Ok(())
    }

    /// With third-party bundling disabled (the default), pure-Python site-packages
    /// distributions keep their previous external disposition.
    #[test]
    fn test_bundle_third_party_disabled_keeps_pure_distribution_external() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        create_bundle_third_party_virtualenv(&virtualenv)?;

        let virtualenv_path = virtualenv.to_string_lossy();
        let resolver = ModuleResolver::new_with_overrides(
            Config::default(),
            Some(""),
            Some(virtualenv_path.as_ref()),
        )?;

        let classification = resolver.classify_import("pure_package");
        assert_eq!(classification.bundle, BundleDisposition::External);
        assert!(resolver.resolve_module_path("pure_package")?.is_none());

        Ok(())
    }

    /// Directory symlinks inside a site-packages package are never traversed; the
    /// package conservatively stays external because it cannot be inspected safely.
    #[cfg(unix)]
    #[test]
    fn test_bundle_third_party_directory_symlink_keeps_package_external() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        let site_packages = virtualenv.join("lib/python3.12/site-packages");
        create_test_file(
            &site_packages.join(format!(
                "linked_package/{}",
                crate::python::constants::INIT_FILE
            )),
            "",
        )?;
        // Symlink cycle: linked_package/loop -> linked_package
        std::os::unix::fs::symlink(
            site_packages.join("linked_package"),
            site_packages.join("linked_package/loop"),
        )?;
        let resolver = bundle_third_party_resolver(&virtualenv)?;

        let classification = resolver.classify_import("linked_package");
        assert_eq!(
            classification.bundle,
            BundleDisposition::External,
            "packages with directory symlinks must conservatively stay external"
        );
        assert!(resolver.resolve_module_path("linked_package")?.is_none());

        Ok(())
    }

    /// A distribution whose RECORD lists native artifacts outside the import's own
    /// package directory (e.g. a pure `frontend` package with a sibling `_backend.so`)
    /// keeps every import it owns external.
    #[test]
    fn test_bundle_third_party_keeps_distribution_with_sibling_native_module_external() -> Result<()>
    {
        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        let site_packages = virtualenv.join("lib/python3.12/site-packages");
        create_test_file(
            &site_packages.join(format!("frontend/{}", crate::python::constants::INIT_FILE)),
            "VALUE = 'frontend'\n",
        )?;
        create_test_file(&site_packages.join("_backend.cpython-312-test.so"), "")?;
        create_test_file(
            &site_packages.join("split_distribution-1.0.dist-info/METADATA"),
            "Metadata-Version: 2.5\nName: split-distribution\nVersion: 1.0\nImport-Name: \
             frontend\nImport-Name: _backend\n",
        )?;
        create_test_file(
            &site_packages.join("split_distribution-1.0.dist-info/RECORD"),
            "frontend/__init__.py,,\n_backend.cpython-312-test.so,,\n",
        )?;
        let resolver = bundle_third_party_resolver(&virtualenv)?;

        // The frontend package directory itself is pure Python, but its distribution
        // ships a native sibling module, so the whole distribution stays external
        let classification = resolver.classify_import("frontend");
        assert_eq!(classification.origin, ImportOrigin::ThirdParty);
        assert_eq!(
            classification.bundle,
            BundleDisposition::External,
            "imports owned by native-shipping distributions must stay external"
        );
        assert!(resolver.resolve_module_path("frontend")?.is_none());

        Ok(())
    }

    /// Extras requested for a bundled distribution (e.g. through a module-map entry
    /// like `provider[speed]`) activate its extras-conditioned `Requires-Dist`
    /// entries, while unrequested extras stay dropped.
    #[test]
    fn test_bundled_distribution_requirements_respect_requested_extras() -> Result<()> {
        use std::str::FromStr;

        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        let site_packages = virtualenv.join("lib/python3.12/site-packages");
        create_test_file(
            &site_packages.join(format!("provider/{}", crate::python::constants::INIT_FILE)),
            "VALUE = 1\n",
        )?;
        create_test_file(
            &site_packages.join("provider-1.0.dist-info/METADATA"),
            "Metadata-Version: 2.5\nName: provider\nVersion: 1.0\nImport-Name: \
             provider\nRequires-Dist: speed-dep; extra == \"speed\"\nRequires-Dist: docs-tool; \
             extra == \"docs\"\n",
        )?;
        let resolver = bundle_third_party_resolver(&virtualenv)?;

        let mut bundled_imports: FxIndexMap<String, Vec<pep508_rs::ExtraName>> =
            FxIndexMap::default();
        bundled_imports.insert(
            "provider".to_owned(),
            vec![pep508_rs::ExtraName::from_str("speed").expect("valid extra name")],
        );

        let requirements = resolver.bundled_distribution_requirements(&bundled_imports);
        assert_eq!(
            requirements,
            vec!["speed-dep".to_owned()],
            "requested extras must activate their dependencies with the extra marker \
             simplified away (pip leaves `extra` unset in requirements files); unrequested \
             extras must not"
        );

        Ok(())
    }

    /// Shared libraries loaded through ctypes/CFFI (`.dll`/`.dylib`) also keep a
    /// distribution external, not just Python extension modules.
    #[test]
    fn test_bundle_third_party_keeps_shared_library_distribution_external() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        let site_packages = virtualenv.join("lib/python3.12/site-packages");
        create_test_file(
            &site_packages.join(format!(
                "cffi_style_pkg/{}",
                crate::python::constants::INIT_FILE
            )),
            "VALUE = 1\n",
        )?;
        create_test_file(&site_packages.join("cffi_style_pkg/libhelper.dylib"), "")?;
        let resolver = bundle_third_party_resolver(&virtualenv)?;

        let classification = resolver.classify_import("cffi_style_pkg");
        assert_eq!(
            classification.bundle,
            BundleDisposition::External,
            "distributions shipping shared libraries must stay external"
        );
        assert!(resolver.resolve_module_path("cffi_style_pkg")?.is_none());

        Ok(())
    }

    /// `Requires-Dist` metadata of bundled distributions supplied through PYTHONPATH
    /// (dist-info beside the sources) is propagated like virtualenv installs.
    #[test]
    fn test_bundled_distribution_requirements_from_pythonpath_root() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let pythonpath_dir = temp_dir.path().join("vendored");
        create_test_file(
            &pythonpath_dir.join(format!(
                "pythonpath_dist/{}",
                crate::python::constants::INIT_FILE
            )),
            "VALUE = 1\n",
        )?;
        create_test_file(
            &pythonpath_dir.join("pythonpath_dist-1.0.dist-info/METADATA"),
            "Metadata-Version: 2.5\nName: pythonpath-dist\nVersion: 1.0\nImport-Name: \
             pythonpath_dist\nRequires-Dist: pp-extra>=1\n",
        )?;
        let config = Config {
            bundle_third_party: Some(true),
            ..Default::default()
        };
        let pythonpath = pythonpath_dir.to_string_lossy();
        let resolver =
            ModuleResolver::new_with_overrides(config, Some(pythonpath.as_ref()), Some(""))?;

        let mut bundled_imports: FxIndexMap<String, Vec<pep508_rs::ExtraName>> =
            FxIndexMap::default();
        bundled_imports.insert("pythonpath_dist".to_owned(), Vec::new());

        let requirements = resolver.bundled_distribution_requirements(&bundled_imports);
        assert_eq!(
            requirements,
            vec!["pp-extra>=1".to_owned()],
            "dist-info metadata found through PYTHONPATH roots must be propagated"
        );

        Ok(())
    }

    /// `RECORD` files are CSV: quoted paths with embedded commas (and `""` escapes)
    /// must be parsed whole so native artifacts inside them are still detected.
    #[test]
    fn test_record_line_first_field_handles_csv_quoting() {
        assert_eq!(
            ModuleResolver::record_line_first_field("frontend/__init__.py,,"),
            "frontend/__init__.py"
        );
        assert_eq!(
            ModuleResolver::record_line_first_field(
                "\"weird,dir/_backend.cpython-312-test.so\",sha256=abc,123"
            ),
            "weird,dir/_backend.cpython-312-test.so"
        );
        assert_eq!(
            ModuleResolver::record_line_first_field("\"quo\"\"ted\"\"/module.py\",,"),
            "quo\"ted\"/module.py"
        );
        assert_eq!(ModuleResolver::record_line_first_field(""), "");
    }

    /// A native artifact whose RECORD path is CSV-quoted (contains a comma) still
    /// marks the distribution as native-shipping.
    #[test]
    fn test_bundle_third_party_detects_native_artifact_in_quoted_record_path() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        let site_packages = virtualenv.join("lib/python3.12/site-packages");
        create_test_file(
            &site_packages.join(format!(
                "quoted_frontend/{}",
                crate::python::constants::INIT_FILE
            )),
            "VALUE = 'frontend'\n",
        )?;
        create_test_file(
            &site_packages.join("quoted_distribution-1.0.dist-info/METADATA"),
            "Metadata-Version: 2.5\nName: quoted-distribution\nVersion: 1.0\nImport-Name: \
             quoted_frontend\n",
        )?;
        create_test_file(
            &site_packages.join("quoted_distribution-1.0.dist-info/RECORD"),
            "quoted_frontend/__init__.py,,\n\"data,dir/_accel.cpython-312-test.so\",sha256=abc,\
             123\n",
        )?;
        let resolver = bundle_third_party_resolver(&virtualenv)?;

        let classification = resolver.classify_import("quoted_frontend");
        assert_eq!(
            classification.bundle,
            BundleDisposition::External,
            "native artifacts in quoted RECORD paths must keep the distribution external"
        );

        Ok(())
    }

    /// A distribution whose `Requires-Python` is incompatible with the configured
    /// target version stays external so installers can reject the mismatch.
    #[test]
    fn test_bundle_third_party_enforces_requires_python() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        let site_packages = virtualenv.join("lib/python3.12/site-packages");
        for (package, requires_python) in [("strict_pkg", ">=3.13"), ("relaxed_pkg", ">=3.8")] {
            create_test_file(
                &site_packages.join(format!("{package}/{}", crate::python::constants::INIT_FILE)),
                "VALUE = 1\n",
            )?;
            create_test_file(
                &site_packages.join(format!("{package}-1.0.dist-info/METADATA")),
                &format!(
                    "Metadata-Version: 2.5\nName: {package}\nVersion: 1.0\nRequires-Python: \
                     {requires_python}\nImport-Name: {package}\n"
                ),
            )?;
        }
        // Default target version is py310
        let resolver = bundle_third_party_resolver(&virtualenv)?;

        let strict = resolver.classify_import("strict_pkg");
        assert_eq!(
            strict.bundle,
            BundleDisposition::External,
            "Requires-Python >=3.13 must keep the distribution external for a py310 target"
        );
        assert!(resolver.resolve_module_path("strict_pkg")?.is_none());

        let relaxed = resolver.classify_import("relaxed_pkg");
        assert_eq!(
            relaxed.bundle,
            BundleDisposition::Include,
            "compatible Requires-Python must not prevent bundling"
        );

        Ok(())
    }

    /// Packages using dynamic imports with non-literal module names stay external
    /// because their dynamically loaded submodules cannot be discovered statically.
    #[test]
    fn test_bundle_third_party_keeps_dynamic_import_distribution_external() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        let site_packages = virtualenv.join("lib/python3.12/site-packages");
        create_test_file(
            &site_packages.join(format!(
                "dynamic_pkg/{}",
                crate::python::constants::INIT_FILE
            )),
            "import importlib\nbackend = 'json_backend'\nimpl = \
             importlib.import_module(f'.{backend}', __package__)\n",
        )?;
        create_test_file(&site_packages.join("dynamic_pkg/json_backend.py"), "")?;
        let resolver = bundle_third_party_resolver(&virtualenv)?;

        let classification = resolver.classify_import("dynamic_pkg");
        assert_eq!(
            classification.bundle,
            BundleDisposition::External,
            "distributions with unresolved dynamic imports must stay external"
        );
        assert!(resolver.resolve_module_path("dynamic_pkg")?.is_none());

        Ok(())
    }

    /// The dynamic-import detector flags non-literal module names but leaves literal
    /// (statically resolvable) dynamic imports alone.
    #[test]
    fn test_dynamic_import_detection() {
        for source in [
            "import importlib\nimportlib.import_module(f'.{name}', __package__)\n",
            "import importlib\nimportlib.import_module(module_variable)\n",
            "from importlib import import_module\nimport_module(module_variable)\n",
            "from importlib import import_module as load\nload(f'.{name}', __package__)\n",
            "from importlib import (\n    import_module as _load,\n)\n_load(module_variable)\n",
            "def load_backend(name):\n    return load(name)\n\nfrom importlib import \
             import_module as load\n",
            "def load_backend(name):\n    return load(name)\n\ntry:\n    from importlib import \
             import_module as load\nexcept ImportError:\n    load = None\n",
            "import importlib\nload = importlib.import_module\nload(module_variable)\n",
            "def load_backend(name):\n    return load(name)\n\nimport importlib\nload = \
             importlib.import_module\n",
            "import importlib\nimportlib.import_module(name=module_variable)\n",
            "import importlib\nimportlib.import_module(name=f'.{name}', package=__package__)\n",
            "__import__(module_variable)\n",
            "__import__('json')\n",
            "backend = __import__('pkg.backend', fromlist=['KIND'])\n",
        ] {
            assert!(
                ModuleResolver::python_source_blocks_bundling(source),
                "should flag non-literal dynamic import in: {source:?}"
            );
        }

        for source in [
            "import importlib\nimportlib.import_module('json')\n",
            "import importlib\nimportlib.import_module('.helper', 'pkg')\n",
            "import importlib\nimportlib.import_module(name='pkg.module')\n",
            "from importlib import import_module as load\nload('.helper', 'pkg')\n",
            "import importlib\nload = importlib.import_module\nload('json')\n",
        ] {
            assert!(
                !ModuleResolver::python_source_blocks_bundling(source),
                "literal dynamic imports are statically resolvable and must not be flagged: \
                 {source:?}"
            );
        }
    }

    /// `Requires-Dist` entries of bundled distributions are carried over, except those
    /// satisfied by other bundled distributions or conditioned on extras.
    #[test]
    fn test_bundled_distribution_requirements_propagation() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        let site_packages = virtualenv.join("lib/python3.12/site-packages");
        create_test_file(
            &site_packages.join(format!(
                "pure_parent/{}",
                crate::python::constants::INIT_FILE
            )),
            "import pure_child\n",
        )?;
        create_test_file(
            &site_packages.join("pure_parent-1.0.dist-info/METADATA"),
            "Metadata-Version: 2.5\nName: Pure_Parent\nVersion: 1.0\nImport-Name: \
             pure_parent\nRequires-Dist: lazy-extra<2\nRequires-Dist: pure-child>=1\nRequires-Dist: \
             docs-tool; extra == \"docs\"\nRequires-Dist: mixed-dep; python_version < \"3.12\" or \
             extra == \"speed\"\n",
        )?;
        create_test_file(
            &site_packages.join(format!(
                "pure_child/{}",
                crate::python::constants::INIT_FILE
            )),
            "VALUE = 1\n",
        )?;
        create_test_file(
            &site_packages.join("pure_child-1.0.dist-info/METADATA"),
            "Metadata-Version: 2.5\nName: pure-child\nVersion: 1.0\nImport-Name: pure_child\n",
        )?;
        let resolver = bundle_third_party_resolver(&virtualenv)?;

        let mut bundled_imports: FxIndexMap<String, Vec<pep508_rs::ExtraName>> =
            FxIndexMap::default();
        bundled_imports.insert("pure_parent".to_owned(), Vec::new());
        bundled_imports.insert("pure_child".to_owned(), Vec::new());

        let requirements = resolver.bundled_distribution_requirements(&bundled_imports);
        assert_eq!(
            requirements,
            vec![
                "lazy-extra<2".to_owned(),
                // Entries are re-serialized in canonical PEP 508 form
                "mixed-dep ; python_full_version < '3.12' or extra == 'speed'".to_owned(),
            ],
            "constraints for external deps and mixed markers are kept; bundled deps and \
             extras-only markers are dropped"
        );

        Ok(())
    }

    /// Two bundled distributions declaring the same external dependency with different
    /// specifiers produce a single requirements entry (pip rejects duplicates).
    #[test]
    fn test_bundled_distribution_requirements_dedupe_by_name() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        let site_packages = virtualenv.join("lib/python3.12/site-packages");
        for (package, specifier) in [
            ("first_pkg", "lazy-extra<2"),
            ("second_pkg", "lazy_extra>=1"),
        ] {
            create_test_file(
                &site_packages.join(format!("{package}/{}", crate::python::constants::INIT_FILE)),
                "VALUE = 1\n",
            )?;
            create_test_file(
                &site_packages.join(format!("{package}-1.0.dist-info/METADATA")),
                &format!(
                    "Metadata-Version: 2.5\nName: {package}\nVersion: 1.0\nImport-Name: \
                     {package}\nRequires-Dist: {specifier}\n"
                ),
            )?;
        }
        let resolver = bundle_third_party_resolver(&virtualenv)?;

        let mut bundled_imports: FxIndexMap<String, Vec<pep508_rs::ExtraName>> =
            FxIndexMap::default();
        bundled_imports.insert("first_pkg".to_owned(), Vec::new());
        bundled_imports.insert("second_pkg".to_owned(), Vec::new());

        let requirements = resolver.bundled_distribution_requirements(&bundled_imports);
        assert_eq!(
            requirements,
            vec!["lazy-extra>=1,<2".to_owned()],
            "duplicate declarations of one distribution must merge their specifiers"
        );

        Ok(())
    }

    /// Legacy `.egg-info` installs are indexed too: their `PKG-INFO` `Requires-Python`
    /// is enforced like a `.dist-info` `METADATA` one.
    #[test]
    fn test_bundle_third_party_enforces_requires_python_from_egg_info() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        let site_packages = virtualenv.join("lib/python3.12/site-packages");
        create_test_file(
            &site_packages.join(format!(
                "legacy_pkg/{}",
                crate::python::constants::INIT_FILE
            )),
            "VALUE = 1\n",
        )?;
        create_test_file(
            &site_packages.join("legacy_pkg.egg-info/PKG-INFO"),
            "Metadata-Version: 1.2\nName: legacy-pkg\nVersion: 1.0\nRequires-Python: >=3.13\n",
        )?;
        create_test_file(
            &site_packages.join("legacy_pkg.egg-info/top_level.txt"),
            "legacy_pkg\n",
        )?;
        let resolver = bundle_third_party_resolver(&virtualenv)?;

        let classification = resolver.classify_import("legacy_pkg");
        assert_eq!(classification.origin, ImportOrigin::ThirdParty);
        assert_eq!(
            classification.bundle,
            BundleDisposition::External,
            "egg-info Requires-Python must be enforced like dist-info metadata"
        );
        assert!(resolver.resolve_module_path("legacy_pkg")?.is_none());

        Ok(())
    }

    /// In a namespace split across distributions, exact RECORD ownership decides which
    /// distribution's policy applies to each child, not directory iteration order.
    #[test]
    fn test_bundle_third_party_namespace_split_uses_record_ownership() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let virtualenv = temp_dir.path().join("venv");
        let site_packages = virtualenv.join("lib/python3.12/site-packages");
        // PEP 420 namespace package: acme/ has no __init__.py
        for (child, requires_python) in [("plugin_a", ">=3.8"), ("plugin_b", ">=3.13")] {
            create_test_file(
                &site_packages.join(format!(
                    "acme/{child}/{}",
                    crate::python::constants::INIT_FILE
                )),
                "VALUE = 1\n",
            )?;
            create_test_file(
                &site_packages.join(format!("{child}-1.0.dist-info/METADATA")),
                &format!(
                    "Metadata-Version: 2.5\nName: {child}\nVersion: 1.0\nRequires-Python: \
                     {requires_python}\nImport-Namespace: acme\n"
                ),
            )?;
            create_test_file(
                &site_packages.join(format!("{child}-1.0.dist-info/RECORD")),
                &format!("acme/{child}/__init__.py,,\n"),
            )?;
        }
        let resolver = bundle_third_party_resolver(&virtualenv)?;

        // plugin_a is record-owned by a compatible distribution and bundles
        let plugin_a = resolver.classify_import("acme.plugin_a");
        assert_eq!(
            plugin_a.bundle,
            BundleDisposition::Include,
            "record-owned child of a compatible distribution must bundle"
        );

        // plugin_b is record-owned by the incompatible distribution and stays external,
        // regardless of the shared Import-Namespace claims
        let plugin_b = resolver.classify_import("acme.plugin_b");
        assert_eq!(
            plugin_b.bundle,
            BundleDisposition::External,
            "record ownership must override shared namespace-prefix claims"
        );
        assert!(resolver.resolve_module_path("acme.plugin_b")?.is_none());

        Ok(())
    }
}
