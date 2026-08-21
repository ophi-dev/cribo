//! Source Map v3 generation for the bundled output.
//!
//! Cribo emits statement/line-level mappings (column 0): each mapped statement in
//! the bundle contributes one token `generated line → (original file, original line)`.
//! That granularity matches Python's line-oriented tracebacks, which are the primary
//! consumer via the injected runtime (see `docs/source-maps.md`).
//!
//! Serialization is delegated to `oxc_sourcemap`, the encoder used by rolldown and
//! rspack, which guarantees spec-compliant VLQ `mappings` output.

use std::borrow::Cow;

use anyhow::Context as _;
use oxc_sourcemap::{SourceMap, Token};
use ruff_python_ast::{HasNodeIndex as _, ModModule, NodeIndex, Stmt};
use ruff_text_size::{Ranged as _, TextSize};

use crate::{ast_indexer::MODULE_INDEX_RANGE, types::FxIndexMap};

/// Identifier of a registered original source file within a [`SourceMapGenerator`].
///
/// Indexes into the emitted `sources` array of the Source Map v3 JSON.
pub(crate) type SourceId = u32;

/// Byte offsets of line starts, for converting a `TextSize` offset to a line number.
#[derive(Debug)]
pub(crate) struct LineIndex {
    /// Byte offset of the start of each line; `line_starts[0] == 0`.
    line_starts: Vec<u32>,
}

impl LineIndex {
    /// Build the index from source text (lines separated by `\n`; sources are
    /// normalized to `\n` line endings when read).
    pub(crate) fn new(source: &str) -> Self {
        let mut line_starts = vec![0_u32];
        for (offset, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset as u32 + 1);
            }
        }
        Self { line_starts }
    }

    /// 0-based line containing byte `offset`.
    pub(crate) fn line_of(&self, offset: TextSize) -> u32 {
        let offset = offset.to_u32();
        // partition_point returns the count of line starts <= offset; the line
        // containing the offset is the last such line.
        (self.line_starts.partition_point(|&start| start <= offset) - 1) as u32
    }
}

/// Provenance data for one bundled module, in module-ordinal order.
#[derive(Debug)]
pub(crate) struct ModuleSourceInfo {
    /// Original filesystem path of the module (as recorded in the `sources` array).
    pub(crate) path: std::path::PathBuf,
    /// The module's original source text (for `sourcesContent` and line lookup).
    pub(crate) source: String,
    /// Line index over `source`.
    pub(crate) line_index: LineIndex,
}

/// Resolves node provenance: which original module and line an AST node came from.
///
/// Module ordinals are assigned by the bundler's AST indexing pass
/// (`Bundler::index_module_asts`): the n-th module receives node indices in
/// `[n * MODULE_INDEX_RANGE, (n + 1) * MODULE_INDEX_RANGE)`. Entries here MUST be
/// registered in that same order. Synthesized nodes carry `AtomicNodeIndex::NONE`
/// (or an index past all module ranges) and resolve to `None`.
#[derive(Debug, Default)]
pub(crate) struct ProvenanceResolver {
    modules: Vec<ModuleSourceInfo>,
}

impl ProvenanceResolver {
    /// Register the next module (ordinal = number of previously registered modules).
    pub(crate) fn push_module(&mut self, path: std::path::PathBuf, source: String) {
        let line_index = LineIndex::new(&source);
        self.modules.push(ModuleSourceInfo {
            path,
            source,
            line_index,
        });
    }

    /// Registered modules, in ordinal order.
    pub(crate) fn modules(&self) -> &[ModuleSourceInfo] {
        &self.modules
    }

    /// Resolve a node to (module ordinal, 0-based original line).
    ///
    /// Returns `None` for synthesized nodes: those with the `NONE` placeholder
    /// index or with an index beyond all module ranges (allocated by
    /// `TransformationContext` after indexing).
    pub(crate) fn resolve(
        &self,
        node_index: NodeIndex,
        range_start: TextSize,
    ) -> Option<(usize, u32)> {
        let index = node_index.as_u32()?;
        let ordinal = (index / MODULE_INDEX_RANGE) as usize;
        let module = self.modules.get(ordinal)?;
        Some((ordinal, module.line_index.line_of(range_start)))
    }
}

/// A single line-level mapping record.
#[derive(Debug, Clone, Copy)]
struct Mapping {
    /// 0-based line in the generated bundle.
    generated_line: u32,
    /// Which original source file this line came from.
    source_id: SourceId,
    /// 0-based line in the original source file.
    original_line: u32,
}

/// Collects line-level mapping records and serializes them as Source Map v3 JSON.
///
/// Lines are 0-based on both the generated and original side, matching the
/// Source Map v3 token encoding (callers converting from 1-based line numbers
/// must subtract one).
#[derive(Debug, Default)]
pub(crate) struct SourceMapGenerator {
    /// Name of the generated file (the bundle), emitted as the `file` field.
    file: String,
    /// Original source path → optional embedded content. Insertion order defines
    /// the `sources` array order; the map index is the [`SourceId`].
    sources: FxIndexMap<String, Option<String>>,
    /// Collected mappings, in insertion order (sorted at serialization time).
    mappings: Vec<Mapping>,
}

impl SourceMapGenerator {
    /// Create a generator for a bundle named `file` (emitted as the `file` field).
    pub(crate) fn new(file: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            sources: FxIndexMap::default(),
            mappings: Vec::new(),
        }
    }

    /// Register an original source file, returning its stable [`SourceId`].
    ///
    /// Paths are deduplicated: registering the same path twice returns the same
    /// id, and the first non-`None` `content` wins.
    pub(crate) fn add_source(&mut self, path: &str, content: Option<String>) -> SourceId {
        if let Some(index) = self.sources.get_index_of(path) {
            let id = index as SourceId;
            if let Some(existing) = self.sources.get_index_mut(index)
                && existing.1.is_none()
            {
                *existing.1 = content;
            }
            return id;
        }
        let id = self.sources.len() as SourceId;
        self.sources.insert(path.to_owned(), content);
        id
    }

    /// Record that 0-based `generated_line` in the bundle originates from
    /// 0-based `original_line` of `source_id`.
    ///
    /// Multiple records for the same generated line are allowed; the first one
    /// added wins at serialization time (statement granularity means the first
    /// statement starting on a line is the authoritative origin).
    pub(crate) fn add_mapping(
        &mut self,
        generated_line: u32,
        source_id: SourceId,
        original_line: u32,
    ) {
        debug_assert!(
            (source_id as usize) < self.sources.len(),
            "add_mapping called with unregistered source_id {source_id}"
        );
        self.mappings.push(Mapping {
            generated_line,
            source_id,
            original_line,
        });
    }

    /// Whether any mappings have been recorded.
    #[cfg(test)]
    pub(crate) const fn is_empty(&self) -> bool {
        self.mappings.is_empty()
    }

    /// Serialize to a Source Map v3 JSON string.
    ///
    /// Tokens are emitted sorted by generated line, one token per line (the
    /// first mapping recorded for a line wins). `sourcesContent` is included iff
    /// at least one source has content (the field is omitted entirely otherwise).
    pub(crate) fn into_json(self) -> String {
        let mut mappings = self.mappings;
        // Stable sort: for duplicate generated lines the earliest insertion stays first.
        mappings.sort_by_key(|m| m.generated_line);
        mappings.dedup_by_key(|m| m.generated_line);

        let tokens: Vec<Token> = mappings
            .iter()
            .map(|m| {
                Token::new(
                    m.generated_line,
                    0,
                    m.original_line,
                    0,
                    Some(m.source_id),
                    None,
                )
            })
            .collect();

        let sources: Vec<Cow<'_, str>> = self
            .sources
            .keys()
            .map(|path| Cow::Borrowed(path.as_str()))
            .collect();
        let source_contents: Vec<Option<Cow<'_, str>>> = self
            .sources
            .values()
            .map(|content| content.as_deref().map(Cow::Borrowed))
            .collect();

        let map = SourceMap::new(
            Some(Cow::Borrowed(self.file.as_str())),
            Vec::new(),
            None,
            sources,
            source_contents,
            tokens.into_boxed_slice(),
            None,
        );
        map.to_json_string()
    }
}

/// One record produced by the parallel statement walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MappingRecord {
    /// 0-based line in the generated bundle text.
    pub(crate) generated_line: u32,
    /// Ordinal of the originating module in the [`ProvenanceResolver`].
    pub(crate) module_ordinal: usize,
    /// 0-based line in the original module source.
    pub(crate) original_line: u32,
}

/// Extract statement-level mappings by re-parsing the emitted bundle text and
/// walking it in parallel with the bundled AST (which carries node provenance).
///
/// The re-parse doubles as a validity check of the emitted bundle: a parse
/// failure is returned as an error. Structural divergence between the two ASTs
/// (possible where post-generation string patching altered the code shape) is
/// handled defensively: the divergent subtree is skipped with a debug log.
pub(crate) fn extract_statement_mappings(
    bundle_text: &str,
    bundled_ast: &ModModule,
    provenance: &ProvenanceResolver,
) -> anyhow::Result<Vec<MappingRecord>> {
    let reparsed = ruff_python_parser::parse_module(bundle_text)
        .context("bundled output failed to re-parse during source map extraction")?
        .into_syntax();

    let mut walker = ParallelWalker {
        line_index: LineIndex::new(bundle_text),
        provenance,
        records: Vec::new(),
    };
    walker.walk_body(&reparsed.body, &bundled_ast.body);
    Ok(walker.records)
}

/// Statement-only parallel traversal of the re-parsed bundle and the bundled AST.
struct ParallelWalker<'a> {
    /// Line index over the emitted bundle text.
    line_index: LineIndex,
    provenance: &'a ProvenanceResolver,
    records: Vec<MappingRecord>,
}

impl ParallelWalker<'_> {
    /// Walk two statement lists in lockstep; skip entirely on length divergence.
    fn walk_body(&mut self, generated: &[Stmt], original: &[Stmt]) {
        if generated.len() != original.len() {
            log::debug!(
                "source map: skipping diverged body (generated {} statements, bundled AST {})",
                generated.len(),
                original.len()
            );
            return;
        }
        for (generated_stmt, original_stmt) in generated.iter().zip(original) {
            self.walk_stmt(generated_stmt, original_stmt);
        }
    }

    /// Record a mapping for one aligned statement pair and recurse into nested bodies.
    fn walk_stmt(&mut self, generated: &Stmt, original: &Stmt) {
        if std::mem::discriminant(generated) != std::mem::discriminant(original) {
            log::debug!("source map: skipping diverged statement pair");
            return;
        }

        let stmt_provenance = self
            .provenance
            .resolve(original.node_index().load(), original.range().start());
        if let Some((module_ordinal, original_line)) = stmt_provenance {
            self.records.push(MappingRecord {
                generated_line: self.line_index.line_of(generated.range().start()),
                module_ordinal,
                original_line,
            });
        }
        // Note on multiline statements: ruff's generator emits every statement
        // on a single physical line — multiline string/f-string literals are
        // rendered with `\n` escapes, and docstrings likewise. There are
        // therefore no interior physical lines to map; a raising expression
        // inside such a literal is attributed to the statement's single
        // generated line, which the record above already covers.

        // Evaluating a decorator can raise on its own `@...` line; give every
        // decorator its own mapping (the statement mapping above only covers
        // the `def`/`class` header).
        let decorator_pairs = match (generated, original) {
            (Stmt::FunctionDef(g), Stmt::FunctionDef(o)) => {
                Some((&g.decorator_list, &o.decorator_list))
            }
            (Stmt::ClassDef(g), Stmt::ClassDef(o)) => Some((&g.decorator_list, &o.decorator_list)),
            _ => None,
        };
        if let Some((gen_decorators, orig_decorators)) = decorator_pairs
            && gen_decorators.len() == orig_decorators.len()
        {
            for (gen_decorator, orig_decorator) in gen_decorators.iter().zip(orig_decorators) {
                if let Some((module_ordinal, original_line)) = self.provenance.resolve(
                    orig_decorator.node_index.load(),
                    orig_decorator.range().start(),
                ) {
                    self.records.push(MappingRecord {
                        generated_line: self.line_index.line_of(gen_decorator.range().start()),
                        module_ordinal,
                        original_line,
                    });
                }
            }
            // With decorators present, the statement range starts at the first
            // `@...` line, so the mapping recorded above covers that decorator
            // — the actual `def`/`class` header still needs its own record.
            // The name identifier sits on the header line on both sides, but
            // symbol renaming can regenerate the original identifier with a
            // synthetic (default) range; only ranges inside the statement are
            // trusted, with the parameter list as a fallback anchor.
            let header_anchor = match (generated, original) {
                (Stmt::FunctionDef(g), Stmt::FunctionDef(o)) => {
                    let orig_anchor = Some(o.name.range())
                        .filter(|range| o.range().contains(range.start()))
                        .or_else(|| {
                            Some(o.parameters.range())
                                .filter(|range| o.range().contains(range.start()))
                        });
                    orig_anchor.map(|range| (g.name.range(), range, o.node_index().load()))
                }
                (Stmt::ClassDef(g), Stmt::ClassDef(o)) => {
                    // The inliner regenerates inlined class names with a
                    // default range, so fall back to the base-class /
                    // metaclass argument list, which also sits on the header.
                    let orig_anchor = Some(o.name.range())
                        .filter(|range| o.range().contains(range.start()))
                        .or_else(|| {
                            o.arguments
                                .as_deref()
                                .map(ruff_text_size::Ranged::range)
                                .filter(|range| o.range().contains(range.start()))
                        });
                    orig_anchor.map(|range| (g.name.range(), range, o.node_index().load()))
                }
                _ => None,
            };
            if !gen_decorators.is_empty()
                && let Some((gen_anchor, orig_anchor, orig_index)) = header_anchor
                && let Some((module_ordinal, original_line)) =
                    self.provenance.resolve(orig_index, orig_anchor.start())
            {
                self.records.push(MappingRecord {
                    generated_line: self.line_index.line_of(gen_anchor.start()),
                    module_ordinal,
                    original_line,
                });
            }
        }

        // Recurse into nested statement bodies even when the statement itself is
        // synthesized: wrapper-module init functions are synthesized `def`s whose
        // bodies contain original module statements.
        match (generated, original) {
            (Stmt::FunctionDef(g), Stmt::FunctionDef(o)) => self.walk_body(&g.body, &o.body),
            (Stmt::ClassDef(g), Stmt::ClassDef(o)) => self.walk_body(&g.body, &o.body),
            (Stmt::If(g), Stmt::If(o)) => {
                self.walk_body(&g.body, &o.body);
                if g.elif_else_clauses.len() == o.elif_else_clauses.len() {
                    for (gen_clause, orig_clause) in
                        g.elif_else_clauses.iter().zip(&o.elif_else_clauses)
                    {
                        // An exception raised while evaluating an `elif`
                        // condition reports the clause header line, so the
                        // header needs its own mapping (provenance comes from
                        // the condition expression, which carries a node index).
                        if let (Some(_gen_test), Some(orig_test)) =
                            (&gen_clause.test, &orig_clause.test)
                            && let Some((module_ordinal, original_line)) = self
                                .provenance
                                .resolve(orig_test.node_index().load(), orig_clause.range().start())
                        {
                            self.records.push(MappingRecord {
                                generated_line: self.line_index.line_of(gen_clause.range().start()),
                                module_ordinal,
                                original_line,
                            });
                        }
                        self.walk_body(&gen_clause.body, &orig_clause.body);
                    }
                }
            }
            (Stmt::While(g), Stmt::While(o)) => {
                self.walk_body(&g.body, &o.body);
                self.walk_body(&g.orelse, &o.orelse);
            }
            (Stmt::For(g), Stmt::For(o)) => {
                self.walk_body(&g.body, &o.body);
                self.walk_body(&g.orelse, &o.orelse);
            }
            (Stmt::With(g), Stmt::With(o)) => self.walk_body(&g.body, &o.body),
            (Stmt::Try(g), Stmt::Try(o)) => {
                self.walk_body(&g.body, &o.body);
                if g.handlers.len() == o.handlers.len() {
                    for (gen_handler, orig_handler) in g.handlers.iter().zip(&o.handlers) {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(gen_handler) =
                            gen_handler;
                        let ruff_python_ast::ExceptHandler::ExceptHandler(orig_handler) =
                            orig_handler;
                        // Evaluating an exception matcher can itself raise, and
                        // Python reports the `except` header line; give the
                        // header its own mapping via the matcher expression's
                        // provenance.
                        if let (Some(_), Some(orig_type)) =
                            (&gen_handler.type_, &orig_handler.type_)
                            && let Some((module_ordinal, original_line)) = self.provenance.resolve(
                                orig_type.node_index().load(),
                                orig_handler.range().start(),
                            )
                        {
                            self.records.push(MappingRecord {
                                generated_line: self
                                    .line_index
                                    .line_of(gen_handler.range().start()),
                                module_ordinal,
                                original_line,
                            });
                        }
                        self.walk_body(&gen_handler.body, &orig_handler.body);
                    }
                }
                self.walk_body(&g.orelse, &o.orelse);
                self.walk_body(&g.finalbody, &o.finalbody);
            }
            (Stmt::Match(g), Stmt::Match(o)) if g.cases.len() == o.cases.len() => {
                for (gen_case, orig_case) in g.cases.iter().zip(&o.cases) {
                    // A raising pattern operation or guard is attributed to the
                    // `case` header line; map it via the case's provenance.
                    if let Some((module_ordinal, original_line)) = self
                        .provenance
                        .resolve(orig_case.node_index.load(), orig_case.range().start())
                    {
                        self.records.push(MappingRecord {
                            generated_line: self.line_index.line_of(gen_case.range().start()),
                            module_ordinal,
                            original_line,
                        });
                    }
                    self.walk_body(&gen_case.body, &orig_case.body);
                }
            }
            _ => {}
        }
    }
}

/// Options for assembling a complete source map for a bundle.
#[derive(Debug)]
pub(crate) struct SourceMapOptions<'a> {
    /// Value of the `file` field (the bundle's file name, or `<stdout>`).
    pub(crate) file: &'a str,
    /// Whether to embed original source text as `sourcesContent`.
    pub(crate) include_contents: bool,
    /// Directory the map will live in; source paths are recorded relative to it.
    /// When `None`, paths are recorded as given.
    pub(crate) base_dir: Option<&'a std::path::Path>,
}

/// Compute a relative path from `base` to `target` using lexical components
/// (no filesystem access). Falls back to `target` as-is when the two share no
/// common prefix that allows a relative form (e.g., different roots).
fn relative_path(base: &std::path::Path, target: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;

    let mut base_components = base.components().peekable();
    let mut target_components = target.components().peekable();

    // Drop the shared prefix.
    while let (Some(b), Some(t)) = (base_components.peek(), target_components.peek()) {
        if b == t {
            base_components.next();
            target_components.next();
        } else {
            break;
        }
    }

    let mut result = std::path::PathBuf::new();
    for component in base_components {
        match component {
            Component::Normal(_) => result.push(".."),
            // A remaining root/prefix component means the paths have no common
            // ancestor expressible relatively; a `..` component cannot be
            // inverted lexically. In both cases keep the target as-is.
            Component::RootDir | Component::Prefix(_) | Component::ParentDir => {
                return target.to_path_buf();
            }
            Component::CurDir => {}
        }
    }
    result.extend(target_components);
    result
}

/// Comment linking the bundle to an adjacent source map file.
///
/// The `sourceMappingURL` convention is borrowed from the JS ecosystem; Python
/// treats the line as a plain comment. The file name is percent-encoded so a
/// hostile or accidental control character (e.g. a newline in a Unix filename)
/// cannot terminate the comment and inject executable text into the bundle.
pub(crate) fn linked_source_mapping_comment(map_file_name: &str) -> String {
    let mut encoded = String::with_capacity(map_file_name.len());
    for character in map_file_name.chars() {
        // Control characters would break out of the comment; '#', '?', and
        // other URL delimiters would truncate the reference for
        // standards-compliant URL consumers. Non-ASCII Unicode passes through
        // verbatim.
        if character < '\u{21}'
            || character == '\u{7F}'
            || matches!(
                character,
                '%' | '#' | '?' | '"' | '<' | '>' | '\\' | '^' | '`' | '|'
            )
        {
            let _ =
                std::fmt::Write::write_fmt(&mut encoded, format_args!("%{:02X}", character as u32));
        } else {
            encoded.push(character);
        }
    }
    format!("# sourceMappingURL={encoded}\n")
}

/// Comment recording the SHA-256 of the sibling map at build time.
///
/// The runtime refuses a sibling map whose digest does not match, so no
/// interleaving of concurrent builds (or manual file shuffling) can pair a
/// bundle with another build's mappings — the digest travels inside the bundle
/// itself, which is always internally consistent. Emitted for both linked and
/// external modes (the sibling-map pairing problem is identical); an explicit
/// `CRIBO_SOURCE_MAPS=<path>` override skips verification.
pub(crate) fn linked_map_digest_comment(map_json: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(map_json.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    format!("# cribo-sourcemap-sha256={hex}\n")
}

/// Comment embedding the source map as a base64 data URL.
pub(crate) fn inline_source_mapping_comment(map_json: &str) -> String {
    let encoded = base64_simd::STANDARD.encode_to_string(map_json.as_bytes());
    format!("# sourceMappingURL=data:application/json;base64,{encoded}\n")
}

/// The traceback-remapping runtime injected into bundles built with source maps.
///
/// Lazy and duress-tolerant by design; see `docs/source-maps.md`.
const RUNTIME_TEMPLATE: &str = include_str!("python/sourcemap_runtime.py");

/// Placeholder in the runtime template replaced with the delivery mode.
const RUNTIME_MODE_PLACEHOLDER: &str = "__CRIBO_SOURCEMAP_MODE__";

/// Inject the traceback-remapping runtime prologue into the bundled AST.
///
/// Statements are inserted after any leading `from __future__` imports (which
/// must stay first) and before all other code, so the hooks install before any
/// user code runs. The parsed statements carry no node provenance, so the
/// mapping walk transparently skips them while staying structurally aligned.
///
/// A template parse failure is a cribo bug; it degrades gracefully (warning,
/// no runtime) rather than failing the bundle.
pub(crate) fn inject_runtime_prologue(
    bundled_ast: &mut ModModule,
    mode: crate::config::SourceMapMode,
) {
    use cow_utils::CowUtils as _;

    let mode_str = match mode {
        crate::config::SourceMapMode::Linked => "linked",
        crate::config::SourceMapMode::Inline => "inline",
        crate::config::SourceMapMode::External => "external",
    };
    let source = RUNTIME_TEMPLATE.cow_replace(RUNTIME_MODE_PLACEHOLDER, mode_str);
    match ruff_python_parser::parse_module(&source) {
        Ok(parsed) => {
            let mut statements = parsed.into_syntax().body;
            // Drop the template's leading docstring: injected near position
            // zero it could otherwise become the bundle's module docstring and
            // change the program's observable `__doc__`.
            if statements.first().is_some_and(is_docstring) {
                statements.remove(0);
            }
            // Insert after the bundle's own docstring (which must stay first to
            // remain `__doc__`) and after any `from __future__` imports (which
            // must precede all other code).
            let mut insert_at = usize::from(bundled_ast.body.first().is_some_and(is_docstring));
            insert_at += bundled_ast.body[insert_at..]
                .iter()
                .take_while(|stmt| is_future_import(stmt))
                .count();
            bundled_ast.body.splice(insert_at..insert_at, statements);
        }
        Err(err) => log::warn!(
            "source map runtime template failed to parse; traceback remapping disabled: {err}"
        ),
    }
}

/// Whether a statement is a `from __future__ import ...`.
fn is_future_import(stmt: &Stmt) -> bool {
    matches!(
        stmt,
        Stmt::ImportFrom(import)
            if import.module.as_ref().is_some_and(|module| module.as_str() == "__future__")
    )
}

/// Whether a statement is a bare string-literal expression (a docstring when leading).
fn is_docstring(stmt: &Stmt) -> bool {
    matches!(stmt, Stmt::Expr(expr) if expr.value.is_string_literal_expr())
}

/// Percent-encode a source path for the `sources` array.
///
/// Source Map v3 consumers resolve `sources` entries as URL references, so a
/// raw `#` or `?` in a filesystem name would be read as fragment/query
/// delimiters and point at the wrong resource. Only URL-breaking characters
/// are escaped; everything else (including non-ASCII) passes through. The
/// injected runtime applies the inverse decoding before filesystem access.
fn percent_encode_source(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for character in path.chars() {
        if character < '\u{20}' || matches!(character, '\u{7F}' | '%' | '#' | '?') {
            let _ =
                std::fmt::Write::write_fmt(&mut encoded, format_args!("%{:02X}", character as u32));
        } else {
            encoded.push(character);
        }
    }
    encoded
}

/// Build the complete Source Map v3 JSON for an emitted bundle.
///
/// Re-parses `bundle_text`, extracts statement mappings against `bundled_ast`,
/// and serializes them. Only modules that actually contributed mappings appear
/// in the `sources` array.
pub(crate) fn build_source_map(
    bundle_text: &str,
    bundled_ast: &ModModule,
    provenance: &ProvenanceResolver,
    options: &SourceMapOptions<'_>,
) -> anyhow::Result<String> {
    let records = extract_statement_mappings(bundle_text, bundled_ast, provenance)?;

    let mut generator = SourceMapGenerator::new(options.file);
    // None = not yet seen; Some(None) = seen but unmappable (non-UTF-8 path).
    let mut ordinal_to_source: Vec<Option<Option<SourceId>>> =
        vec![None; provenance.modules().len()];

    for record in records {
        let module = &provenance.modules()[record.module_ordinal];
        let source_id = ordinal_to_source[record.module_ordinal].unwrap_or_else(|| {
            let display_path = options.base_dir.map_or_else(
                || module.path.clone(),
                |base| relative_path(base, &module.path),
            );
            // Source Map v3 is JSON: a path that is not valid UTF-8 has no
            // lossless representation. A lossy rendering would display a
            // nonexistent replacement-character path and could collide with a
            // *different* non-UTF-8 path, mispairing mappings and
            // sourcesContent — skip such modules instead (their frames stay on
            // bundle coordinates).
            let resolved = display_path.to_str().map_or_else(
                || {
                    log::debug!(
                        "source map: skipping module with non-UTF-8 path: {}",
                        display_path.display()
                    );
                    None
                },
                |display| {
                    let content = options.include_contents.then(|| module.source.clone());
                    Some(generator.add_source(&percent_encode_source(display), content))
                },
            );
            ordinal_to_source[record.module_ordinal] = Some(resolved);
            resolved
        });
        if let Some(source_id) = source_id {
            generator.add_mapping(record.generated_line, source_id, record.original_line);
        }
    }

    Ok(generator.into_json())
}

#[cfg(test)]
mod tests {
    use oxc_sourcemap::SourceMap;
    use ruff_python_ast::Stmt;

    use super::*;

    #[test]
    fn empty_map_is_valid_v3() {
        let generator = SourceMapGenerator::new("bundle.py");
        assert!(generator.is_empty());
        let json = generator.into_json();

        let parsed = SourceMap::from_json_string(&json).expect("valid source map JSON");
        assert_eq!(parsed.get_file(), Some("bundle.py"));
        assert_eq!(parsed.get_tokens().count(), 0);
        assert!(json.contains("\"version\":3"));
        assert!(!json.contains("sourcesContent"));
    }

    #[test]
    fn mappings_round_trip_through_vlq() {
        let mut generator = SourceMapGenerator::new("bundle.py");
        let main = generator.add_source("main.py", None);
        let utils = generator.add_source("utils.py", None);
        // Insert out of order to exercise the sort.
        generator.add_mapping(10, utils, 3);
        generator.add_mapping(4, main, 0);
        generator.add_mapping(7, utils, 1);
        let json = generator.into_json();

        let parsed = SourceMap::from_json_string(&json).expect("valid source map JSON");
        let lookup = parsed.generate_lookup_table();

        let token = parsed
            .lookup_token(&lookup, 4, 0)
            .expect("mapping for line 4");
        assert_eq!(
            parsed.get_source(token.get_source_id().expect("source id")),
            Some("main.py")
        );
        assert_eq!(token.get_src_line(), 0);

        let token = parsed
            .lookup_token(&lookup, 10, 0)
            .expect("mapping for line 10");
        assert_eq!(
            parsed.get_source(token.get_source_id().expect("source id")),
            Some("utils.py")
        );
        assert_eq!(token.get_src_line(), 3);

        // An unmapped line before the first token has no mapping.
        assert!(parsed.lookup_token(&lookup, 0, 0).is_none());
    }

    #[test]
    fn source_dedup_returns_same_id_and_first_content_wins() {
        let mut generator = SourceMapGenerator::new("bundle.py");
        let first = generator.add_source("pkg/mod.py", Some("x = 1\n".to_owned()));
        let second = generator.add_source("pkg/mod.py", Some("ignored".to_owned()));
        assert_eq!(first, second);

        generator.add_mapping(0, first, 0);
        let json = generator.into_json();
        let parsed = SourceMap::from_json_string(&json).expect("valid source map JSON");
        assert_eq!(parsed.get_sources().count(), 1);
        assert_eq!(parsed.get_source_content(first), Some("x = 1\n"));
    }

    #[test]
    fn content_backfills_when_first_registration_had_none() {
        let mut generator = SourceMapGenerator::new("bundle.py");
        let id = generator.add_source("mod.py", None);
        let same = generator.add_source("mod.py", Some("y = 2\n".to_owned()));
        assert_eq!(id, same);

        generator.add_mapping(0, id, 0);
        let json = generator.into_json();
        let parsed = SourceMap::from_json_string(&json).expect("valid source map JSON");
        assert_eq!(parsed.get_source_content(id), Some("y = 2\n"));
    }

    #[test]
    fn sources_content_present_iff_any_content() {
        let mut with_content = SourceMapGenerator::new("bundle.py");
        let id = with_content.add_source("a.py", Some("a = 1\n".to_owned()));
        with_content.add_source("b.py", None);
        with_content.add_mapping(0, id, 0);
        let json = with_content.into_json();
        assert!(json.contains("sourcesContent"));
        assert!(json.contains("null"), "missing content must encode as null");

        let mut without_content = SourceMapGenerator::new("bundle.py");
        let id = without_content.add_source("a.py", None);
        without_content.add_mapping(0, id, 0);
        assert!(!without_content.into_json().contains("sourcesContent"));
    }

    #[test]
    fn first_mapping_per_generated_line_wins() {
        let mut generator = SourceMapGenerator::new("bundle.py");
        let first = generator.add_source("first.py", None);
        let second = generator.add_source("second.py", None);
        generator.add_mapping(5, first, 11);
        generator.add_mapping(5, second, 99);
        let json = generator.into_json();

        let parsed = SourceMap::from_json_string(&json).expect("valid source map JSON");
        assert_eq!(parsed.get_tokens().count(), 1);
        let lookup = parsed.generate_lookup_table();
        let token = parsed
            .lookup_token(&lookup, 5, 0)
            .expect("mapping for line 5");
        assert_eq!(
            parsed.get_source(token.get_source_id().expect("source id")),
            Some("first.py")
        );
        assert_eq!(token.get_src_line(), 11);
    }

    #[test]
    fn line_index_maps_offsets_to_lines() {
        let index = LineIndex::new("a = 1\nb = 2\n\nc = 3\n");
        assert_eq!(index.line_of(TextSize::from(0)), 0); // 'a'
        assert_eq!(index.line_of(TextSize::from(5)), 0); // the '\n' itself
        assert_eq!(index.line_of(TextSize::from(6)), 1); // 'b'
        assert_eq!(index.line_of(TextSize::from(12)), 2); // empty line
        assert_eq!(index.line_of(TextSize::from(13)), 3); // 'c'
    }

    #[test]
    fn line_index_handles_source_without_trailing_newline() {
        let index = LineIndex::new("x = 1");
        assert_eq!(index.line_of(TextSize::from(0)), 0);
        assert_eq!(index.line_of(TextSize::from(4)), 0);
    }

    #[test]
    fn provenance_resolves_nodes_across_modules() {
        use ruff_text_size::Ranged as _;

        let source_a = "a1 = 1\na2 = 2\n";
        let source_b = "def f():\n    return 3\n\nb2 = f()\n";
        let mut ast_a = ruff_python_parser::parse_module(source_a)
            .expect("parse module a")
            .into_syntax();
        let mut ast_b = ruff_python_parser::parse_module(source_b)
            .expect("parse module b")
            .into_syntax();
        crate::ast_indexer::index_module_with_id(&mut ast_a, 0);
        crate::ast_indexer::index_module_with_id(&mut ast_b, 1);

        let mut resolver = ProvenanceResolver::default();
        resolver.push_module(std::path::PathBuf::from("a.py"), source_a.to_owned());
        resolver.push_module(std::path::PathBuf::from("b.py"), source_b.to_owned());

        // Second statement of module a: `a2 = 2` on line 1.
        let stmt = &ast_a.body[1];
        let Stmt::Assign(assign) = stmt else {
            panic!("expected assign statement");
        };
        let resolved = resolver.resolve(assign.node_index.load(), stmt.range().start());
        assert_eq!(resolved, Some((0, 1)));

        // The `return 3` statement nested inside `def f()` of module b: line 1.
        let Stmt::FunctionDef(func) = &ast_b.body[0] else {
            panic!("expected function def");
        };
        let ret = &func.body[0];
        let Stmt::Return(ret_stmt) = ret else {
            panic!("expected return statement");
        };
        let resolved = resolver.resolve(ret_stmt.node_index.load(), ret.range().start());
        assert_eq!(resolved, Some((1, 1)));

        // Last statement of module b: `b2 = f()` on line 3.
        let Stmt::Assign(assign) = &ast_b.body[1] else {
            panic!("expected assign statement");
        };
        let resolved = resolver.resolve(assign.node_index.load(), ast_b.body[1].range().start());
        assert_eq!(resolved, Some((1, 3)));
    }

    #[test]
    fn provenance_rejects_synthesized_nodes() {
        use ruff_python_ast::AtomicNodeIndex;
        use ruff_text_size::TextRange;

        let mut resolver = ProvenanceResolver::default();
        resolver.push_module(std::path::PathBuf::from("a.py"), "x = 1\n".to_owned());

        // A node built by ast_builder carries the NONE placeholder index.
        let synthesized = crate::ast_builder::statements::pass();
        let Stmt::Pass(pass) = &synthesized else {
            panic!("expected pass statement");
        };
        assert_eq!(
            resolver.resolve(pass.node_index.load(), TextRange::default().start()),
            None
        );

        // A node index past all module ranges (TransformationContext territory)
        // also resolves to None.
        let index = AtomicNodeIndex::default();
        index.set(NodeIndex::from(MODULE_INDEX_RANGE));
        assert_eq!(resolver.resolve(index.load(), TextSize::from(0)), None);
    }
}
