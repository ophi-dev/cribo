"""Cribo source map runtime (injected prologue).

Remaps tracebacks of uncaught exceptions back to the original source files
using the Source Map v3 emitted at bundle time. Lazy by design: no file I/O,
parsing, or decoding happens at import time; everything is deferred to the
first uncaught exception. Under resource pressure the decoder streams the map
in constant memory and falls back to the default traceback on any failure.
See docs/source-maps.md in the cribo repository.

Note: this leading docstring is stripped at injection time so the bundle's
``__doc__`` is not affected.
"""

import sys as _cribo_sys


def _cribo_sm_import(name, *, _list=list, _import=__import__):
    """Import a stdlib module immune to script-directory shadowing.

    The bundle's own directory is `sys.path[0]` (or `''` for `-c`/stdin), and
    `PYTHONPATH=.` can expose the same directory again at later indices — so a
    project file named e.g. `threading.py` sitting next to the bundle would
    otherwise shadow the stdlib for this runtime. Already-imported modules are
    taken from `sys.modules`; otherwise the import runs with every path entry
    resolving to the script directory removed. (`sys` itself is a builtin and
    can never be shadowed.)
    """
    module = _cribo_sys.modules.get(name)
    if module is not None:
        return module
    saved_path = _cribo_sys.path
    filtered = _list(saved_path[1:])
    os_mod = _cribo_sys.modules.get("os")
    if os_mod is not None and saved_path:
        script_dir = os_mod.path.normcase(os_mod.path.abspath(saved_path[0] or "."))
        filtered = [
            entry
            for entry in filtered
            if not os_mod.path.normcase(os_mod.path.abspath(entry or ".")) == script_dir
        ]
    _cribo_sys.path = filtered
    try:
        return _import(name)
    finally:
        _cribo_sys.path = saved_path


class _CriboSmStream(object):
    """Byte-at-a-time reader over an iterator of byte chunks.

    Keyword-only defaults snapshot the builtins at definition time (before any
    bundled user code runs), so later shadowing of e.g. `len` cannot break the
    reader — the same idiom cribo's generated module proxies use.
    """

    __slots__ = ("_chunks", "_buf", "_pos")

    def __init__(self, chunks, *, _iter=iter):
        self._chunks = _iter(chunks)
        self._buf = b""
        self._pos = 0

    def read_byte(self, *, _len=len, _next=next, _stop=StopIteration):
        while self._pos >= _len(self._buf):
            try:
                self._buf = _next(self._chunks)
            except _stop:
                return -1
            self._pos = 0
        value = self._buf[self._pos]
        self._pos += 1
        return value


class _CriboSourceMapRuntime(object):
    """Traceback-remapping runtime.

    All collaborators (modules, the stream class, previous hooks) are bound to
    the instance at construction time, and every method snapshots the builtins
    it needs via keyword-only defaults evaluated at class-definition time — so
    the installed hooks keep working even if bundled user code later rebinds
    any module-level name this template introduced, or common builtins such as
    `open`, `len`, or `max`.
    """

    _B64 = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
    _CHUNK = 8192

    def __init__(self, mode, bundle_file, os_mod, binascii_mod, threading_mod, traceback_mod):
        self._mode = mode
        # As-given path for frame matching (co_filename uses the invocation
        # spelling), plus a startup-anchored absolute path for file I/O so a
        # later os.chdir() in bundled code cannot orphan a relative path.
        self._bundle = bundle_file
        if bundle_file == "<stdin>":
            self._bundle_anchor = bundle_file
        else:
            self._bundle_anchor = os_mod.path.abspath(bundle_file)
        # Marks this instance so another cribo runtime chained behind it can
        # recognize it (see _notify_custom_hook).
        self._cribo_sm_runtime_marker = True
        # Startup working directory, for anchoring a relative
        # CRIBO_SOURCE_MAPS override before user code can chdir away.
        try:
            self._startup_cwd = os_mod.getcwd()
        except OSError:
            self._startup_cwd = "."
        self._os = os_mod
        self._sys = _cribo_sys
        self._binascii = binascii_mod
        self._threading = threading_mod
        self._stream_cls = _CriboSmStream
        self._import = _cribo_sm_import
        # Captured at construction (before any bundled user code runs) so a
        # first-party module registering sys.modules["traceback"] later cannot
        # degrade exception formatting.
        self._traceback = traceback_mod
        # Re-entrancy guard; thread-local so a hook firing on one thread never
        # disables remapping on another.
        self._local = threading_mod.local()
        self._prev_excepthook = _cribo_sys.excepthook
        self._prev_unraisablehook = _cribo_sys.unraisablehook
        self._prev_threading_hook = threading_mod.excepthook
        # Interpreter defaults, snapshotted now so user code rebinding e.g.
        # sys.__excepthook__ later cannot make the captured previous hook look
        # custom (which would double-print) or raise from the hook.
        self._default_excepthook = _cribo_sys.__excepthook__
        self._default_unraisablehook = _cribo_sys.__unraisablehook__
        self._default_threading_hook = getattr(threading_mod, "__excepthook__", None)
        try:
            self._group_type = BaseExceptionGroup
        except NameError:  # Python < 3.11
            self._group_type = None

    def install(self):
        """Install the three hooks; the previous hooks stay chained."""
        self._sys.excepthook = self.excepthook
        self._sys.unraisablehook = self.unraisablehook
        self._threading.excepthook = self.threading_hook

    @classmethod
    def _bootstrap(cls, mode, bundle_file):
        """Import dependencies safely, construct, and install — fail-open.

        Any failure (however exotic the host environment) leaves the program
        running without remapping instead of aborting it at startup.
        """
        try:
            runtime = cls(
                mode,
                bundle_file,
                _cribo_sm_import("os"),
                _cribo_sm_import("binascii"),
                _cribo_sm_import("threading"),
                _cribo_sm_import("traceback"),
            )
            runtime.install()
        except BaseException:
            pass

    # -- map location and raw chunk access ---------------------------------

    def _map_location(self):
        """Resolve the map location per delivery mode, or None when inactive.

        Returns (map_path, map_dir); map_path is None for inline mode (the map
        lives inside the bundle file itself). Called lazily at hook-fire time
        so the happy path never touches the environment or the filesystem.
        """
        env = self._os.environ.get("CRIBO_SOURCE_MAPS", "")
        if env == "0":
            return None
        # An explicit path wins for every mode. This is also the only way to
        # supply a map to a bundle executed via `python -` (stdin), whose
        # source cannot be re-read at hook time. A relative override is
        # anchored to the startup working directory, immune to later chdir.
        if env not in ("", "1", "true", "yes", "on"):
            path = env
            if not self._os.path.isabs(path):
                path = self._os.path.join(self._startup_cwd, path)
            return (path, self._os.path.dirname(self._os.path.abspath(path)))
        bundle = self._bundle_anchor
        if self._mode == "inline":
            if bundle == "<stdin>":
                return None  # stdin cannot be re-opened; use CRIBO_SOURCE_MAPS=<path>
            return (None, self._os.path.dirname(bundle))
        sibling = bundle + ".map"
        if self._mode == "linked":
            if self._os.path.exists(sibling):
                return (sibling, self._os.path.dirname(sibling))
            return None
        # external: opt in via CRIBO_SOURCE_MAPS=1 (a path was handled above)
        if env in ("1", "true", "yes", "on"):
            return (sibling, self._os.path.dirname(sibling))
        return None

    def _file_chunks(self, path, *, _open=open):
        """Yield fixed-size chunks of a file (constant memory)."""
        handle = _open(path, "rb")
        try:
            while True:
                chunk = handle.read(self._CHUNK)
                if not chunk:
                    break
                yield chunk
        finally:
            handle.close()

    def _find_inline_payload(self, handle, *, _len=len):
        """Backward-scan the bundle for the last inline map marker.

        Returns the byte offset of the base64 payload, or -1. Only the tail of
        the file is examined; the bundle body is never read.
        """
        marker = b"# sourceMappingURL=data:"
        handle.seek(0, 2)
        position = handle.tell()
        overlap = b""
        found = -1
        while position > 0:
            step = self._CHUNK if position >= self._CHUNK else position
            position -= step
            handle.seek(position)
            data = handle.read(step) + overlap
            index = data.rfind(marker)
            if index >= 0:
                found = position + index
                break
            overlap = data[: _len(marker) - 1]
        if found < 0:
            return -1
        handle.seek(found)
        head = handle.read(192)
        base64_at = head.find(b"base64,")
        if base64_at < 0:
            return -1
        return found + base64_at + _len(b"base64,")

    def _inline_chunks(self, path, *, _open=open, _len=len):
        """Yield decoded chunks of an inline (base64 data URL) source map."""
        handle = _open(path, "rb")
        try:
            start = self._find_inline_payload(handle)
            if start < 0:
                return
            handle.seek(start)
            pending = b""
            while True:
                raw = handle.read(self._CHUNK)
                if not raw:
                    break
                data = pending + raw.translate(None, b"\r\n")
                usable = _len(data) - (_len(data) % 4)
                pending = data[usable:]
                if usable:
                    yield self._binascii.a2b_base64(data[:usable])
            if pending:
                yield self._binascii.a2b_base64(pending + b"=" * (-_len(pending) % 4))
        finally:
            handle.close()

    # -- streaming JSON field scanner ---------------------------------------

    def _skip_ws(self, stream, byte):
        while byte in (32, 9, 10, 13):
            byte = stream.read_byte()
        return byte

    def _read_hex4(self, stream, *, _int=int, _chr=chr, _range=range, _error=ValueError):
        """Read the four hex digits of a \\uXXXX escape; return the code unit."""
        code = 0
        for _ in _range(4):
            digit = stream.read_byte()
            if digit < 0:
                raise _error("unterminated unicode escape")
            code = code * 16 + _int(_chr(digit), 16)
        return code

    def _read_string(
        self,
        stream,
        collect,
        *,
        _bytearray=bytearray,
        _chr=chr,
        _error=ValueError,
    ):
        """Consume a JSON string whose opening quote was already read.

        Returns the decoded text when collect is true, else None (contents are
        discarded byte-by-byte). Escaped quotes, \\uXXXX sequences, and UTF-16
        surrogate pairs (non-BMP characters) are handled, so string *values*
        containing text like '"mappings":' cannot confuse the key scanner and
        emoji-bearing paths survive intact.
        """
        table = {98: 8, 102: 12, 110: 10, 114: 13, 116: 9}
        buf = _bytearray() if collect else None
        pending = None  # one byte of lookahead pushed back by surrogate handling
        while True:
            if pending is None:
                byte = stream.read_byte()
            else:
                byte, pending = pending, None
            if byte < 0:
                raise _error("unterminated JSON string")
            if byte == 34:  # '"'
                return buf.decode("utf-8", "replace") if collect else None
            if not byte == 92:  # '\\'
                if buf is not None:
                    buf.append(byte)
                continue
            escape = stream.read_byte()
            if escape < 0:
                raise _error("unterminated JSON escape")
            if not escape == 117:  # 'u'
                if buf is not None:
                    buf.append(table.get(escape, escape))
                continue
            code = self._read_hex4(stream)
            if buf is None:
                continue
            if 0xD800 <= code <= 0xDBFF:
                # High surrogate: a following \uXXXX low surrogate combines
                # into one non-BMP code point.
                nxt = stream.read_byte()
                if nxt == 92:
                    escape2 = stream.read_byte()
                    if escape2 == 117:
                        low = self._read_hex4(stream)
                        if 0xDC00 <= low <= 0xDFFF:
                            code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00)
                            buf.extend(_chr(code).encode("utf-8"))
                        else:
                            buf.extend(_chr(code).encode("utf-8", "surrogatepass"))
                            buf.extend(_chr(low).encode("utf-8", "surrogatepass"))
                        continue
                    if escape2 < 0:
                        raise _error("unterminated JSON escape")
                    buf.extend(_chr(code).encode("utf-8", "surrogatepass"))
                    buf.append(table.get(escape2, escape2))
                    continue
                buf.extend(_chr(code).encode("utf-8", "surrogatepass"))
                pending = nxt  # includes EOF/quote; the main loop handles both
                continue
            buf.extend(_chr(code).encode("utf-8", "surrogatepass"))

    def _skip_value(self, stream, byte, *, _error=ValueError):
        """Skip one JSON value; return the first byte after it (or -1)."""
        if byte == 34:  # string
            self._read_string(stream, False)
            return stream.read_byte()
        if byte in (123, 91):  # object / array
            depth = 1
            while depth > 0:
                byte = stream.read_byte()
                if byte < 0:
                    raise _error("unterminated JSON container")
                if byte == 34:
                    self._read_string(stream, False)
                elif byte in (123, 91):
                    depth += 1
                elif byte in (125, 93):
                    depth -= 1
            return stream.read_byte()
        # number / true / false / null: consume until a delimiter
        while byte >= 0 and byte not in (44, 125, 93):  # ',' '}' ']'
            byte = stream.read_byte()
        return byte

    def _decode_vlq(
        self, stream, needed, max_needed, *, _range=range, _len=len, _error=ValueError
    ):
        """Streaming VLQ state machine over the raw bytes of the mappings string.

        Constant state: line/segment counters plus running deltas. Records the
        first segment per needed generated line; exits as soon as every needed
        line is resolved or the max needed line is passed. Returns
        ``(table, terminated)`` where ``terminated`` says whether the closing
        quote was consumed (early exits leave the stream inside the string).
        """
        lut = {}
        for index in _range(64):
            lut[self._B64[index]] = index
        result = {}
        gen_line = 0
        src_idx = 0
        src_line = 0
        field = 0
        vlq_value = 0
        vlq_shift = 0

        def end_segment():
            if field >= 4 and gen_line in needed and gen_line not in result:
                result[gen_line] = (src_idx, src_line)

        while True:
            byte = stream.read_byte()
            if byte < 0 or byte == 34:  # EOF or closing '"'
                end_segment()
                return result, True
            if byte == 59:  # ';'
                end_segment()
                gen_line += 1
                field = 0
                if gen_line > max_needed or _len(result) == _len(needed):
                    return result, False
                continue
            if byte == 44:  # ','
                end_segment()
                field = 0
                continue
            value = lut.get(byte)
            if value is None:
                raise _error("unexpected byte in mappings")
            vlq_value += (value & 31) << vlq_shift
            if value & 32:
                vlq_shift += 5
                continue
            signed = -(vlq_value >> 1) if vlq_value & 1 else vlq_value >> 1
            if field == 1:
                src_idx += signed
            elif field == 2:
                src_line += signed
            field += 1
            vlq_value = 0
            vlq_shift = 0

    def _scan(self, chunks_factory, needed, max_needed, *, _set=set, _error=ValueError):
        """Two-pass scan of the map; return (sources dict, line table).

        Pass 1 decodes only `mappings` (skipping the sources array entirely);
        pass 2 re-streams the map and collects only the source paths actually
        referenced by the decoded lines. Memory therefore scales with the
        traceback's needed frames, not with the bundle's full source table —
        the property the whole streaming decoder exists for. `chunks_factory`
        is a zero-argument callable producing a fresh chunk iterator per pass.
        """
        table = self._scan_mappings(self._stream_cls(chunks_factory()), needed, max_needed)
        wanted = _set(src_idx for (src_idx, _line) in table.values())
        sources = {}
        if wanted:
            sources = self._scan_sources(self._stream_cls(chunks_factory()), wanted)
        return sources, table

    def _scan_mappings(self, stream, needed, max_needed, *, _error=ValueError):
        """Pass 1: decode the `mappings` field; every other value is skipped."""
        byte = self._skip_ws(stream, stream.read_byte())
        if not byte == 123:  # '{'
            raise _error("not a JSON object")
        byte = self._skip_ws(stream, stream.read_byte())
        while byte == 34:  # '"' starting a key
            key = self._read_string(stream, True)
            byte = self._skip_ws(stream, stream.read_byte())
            if not byte == 58:  # ':'
                raise _error("malformed object")
            byte = self._skip_ws(stream, stream.read_byte())
            if key == "mappings":
                if not byte == 34:
                    raise _error("mappings is not a string")
                table, _terminated = self._decode_vlq(stream, needed, max_needed)
                return table
            byte = self._skip_ws(stream, self._skip_value(stream, byte))
            if byte == 44:  # ','
                byte = self._skip_ws(stream, stream.read_byte())
        raise _error("no mappings field")

    def _scan_sources(self, stream, wanted, *, _error=ValueError):
        """Pass 2: collect only the `sources` entries whose index is wanted."""
        byte = self._skip_ws(stream, stream.read_byte())
        if not byte == 123:  # '{'
            raise _error("not a JSON object")
        byte = self._skip_ws(stream, stream.read_byte())
        while byte == 34:  # '"' starting a key
            key = self._read_string(stream, True)
            byte = self._skip_ws(stream, stream.read_byte())
            if not byte == 58:  # ':'
                raise _error("malformed object")
            byte = self._skip_ws(stream, stream.read_byte())
            if key == "sources":
                if not byte == 91:  # '['
                    raise _error("sources is not an array")
                return self._read_wanted_array_items(stream, wanted)
            byte = self._skip_ws(stream, self._skip_value(stream, byte))
            if byte == 44:  # ','
                byte = self._skip_ws(stream, stream.read_byte())
        return {}

    def _read_wanted_array_items(self, stream, wanted, *, _len=len, _error=ValueError):
        """Read a JSON array, collecting only string items at wanted indices."""
        items = {}
        index = 0
        byte = self._skip_ws(stream, stream.read_byte())
        if byte == 93:  # ']'
            return items
        while True:
            if byte == 34 and index in wanted:
                items[index] = self._read_string(stream, True)
                byte = self._skip_ws(stream, stream.read_byte())
            else:
                byte = self._skip_ws(stream, self._skip_value(stream, byte))
            index += 1
            if byte == 93 or _len(items) == _len(wanted):
                return items
            if not byte == 44:  # ','
                raise _error("malformed array")
            byte = self._skip_ws(stream, stream.read_byte())

    # -- loading -------------------------------------------------------------

    def _load(self, needed_lines, *, _set=set, _max=max):
        """Load (table, sources, map_dir) for 1-based bundle line numbers.

        Returns None when the runtime is inactive for the current mode. The
        returned table is keyed by 1-based bundle lines mapping to
        (source_index, 1-based original line).
        """
        location = self._map_location()
        if location is None:
            return None
        map_path, map_dir = location
        needed0 = _set(line - 1 for line in needed_lines)
        max_needed = _max(needed0)
        if map_path is None:

            def chunks_factory():
                return self._inline_chunks(self._bundle_anchor)

        else:

            def chunks_factory():
                return self._file_chunks(map_path)

        sources, table0 = self._scan(chunks_factory, needed0, max_needed)
        table = {}
        for line0, (src_idx, src_line0) in table0.items():
            table[line0 + 1] = (src_idx, src_line0 + 1)
        return (table, sources, map_dir)

    def _load_json_fallback(
        self, needed_lines, *, _open=open, _set=set, _max=max, _enumerate=enumerate
    ):
        """Fallback: full json.loads parse, reusing the VLQ machine on the result."""
        location = self._map_location()
        if location is None:
            return None
        map_path, map_dir = location
        json = self._import("json")

        if map_path is None:
            raw = b"".join(self._inline_chunks(self._bundle_anchor))
        else:
            handle = _open(map_path, "rb")
            try:
                raw = handle.read()
            finally:
                handle.close()
        data = json.loads(raw.decode("utf-8"))
        sources = {}
        for index, source in _enumerate(data.get("sources") or []):
            sources[index] = source
        mappings = data.get("mappings") or ""
        needed0 = _set(line - 1 for line in needed_lines)
        stream = self._stream_cls([mappings.encode("ascii"), b'"'])
        table0, _terminated = self._decode_vlq(stream, needed0, _max(needed0))
        table = {}
        for line0, (src_idx, src_line0) in table0.items():
            table[line0 + 1] = (src_idx, src_line0 + 1)
        return (table, sources, map_dir)

    # -- traceback collection and rendering ----------------------------------

    def _collect_needed(
        self, exc_value, traceback_obj, *, _set=set, _id=id, _getattr=getattr
    ):
        """1-based bundle lines referenced by the traceback (and its chain)."""
        needed = _set()

        def add(tb):
            while tb is not None:
                if tb.tb_frame.f_code.co_filename == self._bundle:
                    needed.add(tb.tb_lineno)
                tb = tb.tb_next

        add(traceback_obj)
        exc = exc_value
        seen = _set()
        while exc is not None and _id(exc) not in seen:
            seen.add(_id(exc))
            add(_getattr(exc, "__traceback__", None))
            cause = _getattr(exc, "__cause__", None)
            context = _getattr(exc, "__context__", None)
            exc = cause if cause is not None else context
        return needed

    def _chain_has_group(
        self, exc_value, *, _set=set, _id=id, _getattr=getattr, _isinstance=isinstance
    ):
        """Whether the exception chain contains a BaseExceptionGroup.

        CPython renders groups with a dedicated nested layout; rather than
        losing the nested tracebacks, the runtime defers group rendering
        entirely to the previous hook (unremapped but complete).
        """
        if self._group_type is None:
            return False
        exc = exc_value
        seen = _set()
        while exc is not None and _id(exc) not in seen:
            seen.add(_id(exc))
            if _isinstance(exc, self._group_type):
                return True
            cause = _getattr(exc, "__cause__", None)
            if cause is not None:
                exc = cause
                continue
            # A suppressed context (`raise ... from None`) is never rendered,
            # so a group hidden there must not force the unremapped fallback.
            if _getattr(exc, "__suppress_context__", False):
                return False
            exc = _getattr(exc, "__context__", None)
        return False

    def _source_line(self, path, lineno, *, _open=open, _os_error=OSError):
        """Read a single 1-based line from a file without caching it."""
        try:
            handle = _open(path, "rb")
        except _os_error:
            return None
        try:
            current = 0
            for raw in handle:
                current += 1
                if current == lineno:
                    return raw.decode("utf-8", "replace").strip()
                if current > lineno:
                    break
        except _os_error:
            return None
        finally:
            handle.close()
        return None

    def _effective_tb_limit(self, *, _getattr=getattr, _isinstance=isinstance, _int=int):
        """The application's `sys.tracebacklimit`, or None when unset/invalid."""
        limit = _getattr(self._sys, "tracebacklimit", None)
        return limit if _isinstance(limit, _int) else None

    def _write_frames(self, traceback_obj, table, sources, map_dir, write, limit):
        """Write remapped frame lines, collapsing repeated frames like CPython.

        Consecutive identical frames (recursion) print at most 3 times followed
        by a "[Previous line repeated N more times]" marker; source line text
        is cached per (file, line) within one rendering to avoid re-reading
        files. A positive `limit` keeps only the last `limit` frames, matching
        the interpreter's `sys.tracebacklimit` handling in the default hook.
        """
        if limit is not None:
            total = 0
            probe = traceback_obj
            while probe is not None:
                total += 1
                probe = probe.tb_next
            skip = total - limit
            while skip > 0 and traceback_obj is not None:
                traceback_obj = traceback_obj.tb_next
                skip -= 1

        cache = {}
        last = None
        repeats = 0

        def emit(entry):
            write('  File "%s", line %d, in %s\n' % entry)
            key = (entry[0], entry[1])
            if key not in cache:
                cache[key] = self._source_line(entry[0], entry[1])
            if cache[key]:
                write("    %s\n" % cache[key])

        while traceback_obj is not None:
            frame = traceback_obj.tb_frame
            filename = frame.f_code.co_filename
            lineno = traceback_obj.tb_lineno
            name = frame.f_code.co_name
            if filename == self._bundle:
                mapped = table.get(lineno)
                if mapped is not None:
                    source = sources.get(mapped[0])
                    if source:
                        if not self._os.path.isabs(source):
                            source = self._os.path.normpath(
                                self._os.path.join(map_dir, source)
                            )
                        filename, lineno = source, mapped[1]
            entry = (filename, lineno, name)
            if entry == last:
                repeats += 1
                if repeats <= 3:
                    emit(entry)
            else:
                if repeats > 3:
                    write("  [Previous line repeated %d more times]\n" % (repeats - 3))
                last = entry
                repeats = 1
                emit(entry)
            traceback_obj = traceback_obj.tb_next
        if repeats > 3:
            write("  [Previous line repeated %d more times]\n" % (repeats - 3))

    def _exception_line(
        self, exc_value, *, _type=type, _getattr=getattr, _str=str, _bex=BaseException
    ):
        """Minimal `Type: message` line, the fallback formatter."""
        exc_type = _type(exc_value)
        name = _getattr(exc_type, "__qualname__", exc_type.__name__)
        module = _getattr(exc_type, "__module__", None)
        if module not in (None, "builtins", "__main__"):
            name = "%s.%s" % (module, name)
        try:
            text = _str(exc_value)
        except _bex:
            text = "<exception str() failed>"
        return "%s: %s\n" % (name, text) if text else "%s\n" % name

    def _write_exception_only(
        self, exc, write, *, _type=type, _getattr=getattr, _bex=BaseException
    ):
        """Write the exception line(s) with full standard-library fidelity.

        `traceback.format_exception_only` supplies the interpreter's
        specialized rendering — SyntaxError source line and caret, NameError /
        AttributeError "Did you mean" suggestions, and `__notes__` — so the
        remapped output matches the default hook. Falls back to the minimal
        line (plus notes) when the traceback module is unavailable.
        """
        try:
            te = self._traceback.TracebackException(
                _type(exc),
                exc,
                _getattr(exc, "__traceback__", None),
                lookup_lines=False,
            )
            for line in te.format_exception_only():
                write(line)
            return
        except _bex:
            pass
        write(self._exception_line(exc))
        notes = _getattr(exc, "__notes__", None)
        if notes:
            try:
                for note in notes:
                    write("%s\n" % (note,))
            except _bex:
                pass

    def _render(
        self,
        exc_value,
        table,
        sources,
        map_dir,
        write,
        *,
        _getattr=getattr,
        _set=set,
        _id=id,
        _list=list,
        _reversed=reversed,
        _enumerate=enumerate,
    ):
        """Render the exception (with its cause/context chain) like CPython."""
        chain = []
        exc = exc_value
        seen = _set()
        while exc is not None and _id(exc) not in seen:
            seen.add(_id(exc))
            cause = _getattr(exc, "__cause__", None)
            context = _getattr(exc, "__context__", None)
            suppress = _getattr(exc, "__suppress_context__", False)
            if cause is not None:
                chain.append((exc, "cause"))
                exc = cause
            elif context is not None and not suppress:
                chain.append((exc, "context"))
                exc = context
            else:
                chain.append((exc, None))
                exc = None
        # Print innermost first, like CPython. The link stored on an exception
        # describes its relation to its own inner exception — which is exactly
        # the one printed immediately before it.
        ordered = _list(_reversed(chain))
        for index, (exc, link) in _enumerate(ordered):
            if index > 0:
                if link == "cause":
                    write(
                        "\nThe above exception was the direct cause of the following "
                        "exception:\n\n"
                    )
                else:
                    write(
                        "\nDuring handling of the above exception, another exception "
                        "occurred:\n\n"
                    )
            tb = _getattr(exc, "__traceback__", None)
            limit = self._effective_tb_limit()
            if tb is not None and (limit is None or limit > 0):
                write("Traceback (most recent call last):\n")
                self._write_frames(tb, table, sources, map_dir, write, limit)
            self._write_exception_only(exc, write)

    def _try_render(
        self, exc_value, traceback_obj, prefix, *, _getattr=getattr, _bex=BaseException
    ):
        """Attempt a remapped rendering to stderr; True on success.

        Never raises and never masks the original exception: any failure in
        this runtime returns False so callers can delegate to the previous
        hook.
        """
        if _getattr(self._local, "in_hook", False) or exc_value is None:
            return False
        self._local.in_hook = True
        old_limit = None
        try:
            if self._chain_has_group(exc_value):
                return False
            needed = self._collect_needed(exc_value, traceback_obj)
            if not needed:
                return False
            loaded = None
            try:
                loaded = self._load(needed)
            except _bex:
                try:
                    loaded = self._load_json_fallback(needed)
                except _bex:
                    loaded = None
            if not loaded:
                return False
            table, sources, map_dir = loaded
            if not table:
                return False
            try:
                old_limit = self._sys.getrecursionlimit()
                self._sys.setrecursionlimit(old_limit + 64)
            except _bex:
                old_limit = None
            # Buffer the rendering so a mid-render failure produces no partial
            # output before the previous hook prints the standard traceback.
            parts = []
            if prefix:
                parts.append(prefix)
            self._render(exc_value, table, sources, map_dir, parts.append)
            stderr = self._sys.stderr
            stderr.write("".join(parts))
            try:
                stderr.flush()
            except _bex:
                pass
            return True
        except _bex:
            return False
        finally:
            if old_limit is not None:
                try:
                    self._sys.setrecursionlimit(old_limit)
                except _bex:
                    pass
            self._local.in_hook = False

    def _notify_custom_hook(self, prev, default, call, *, _bex=BaseException, _getattr=getattr):
        """Invoke a chained hook after a successful remap when it is custom.

        A successful remap replaces the *default* printer, but preinstalled
        custom hooks (error reporters, sitecustomize) must still observe the
        exception; their own output is theirs to manage. Two exclusions: when
        the interpreter default is unavailable for comparison (e.g.
        `threading.__excepthook__` before Python 3.10) no notification happens
        — better to skip a custom hook than to double-print via the default
        one; and an earlier cribo runtime's hook is skipped, since it would
        find no frames for its own bundle and delegate to the default printer,
        duplicating the traceback.
        """
        if default is None or prev is None or prev is default:
            return
        bound_to = _getattr(prev, "__self__", None)
        if bound_to is not None and _getattr(bound_to, "_cribo_sm_runtime_marker", False):
            return
        try:
            call(prev)
        except _bex:
            pass

    # -- installed hooks ------------------------------------------------------

    def excepthook(self, exc_type, exc_value, traceback_obj):
        if self._try_render(exc_value, traceback_obj, None):
            self._notify_custom_hook(
                self._prev_excepthook,
                self._default_excepthook,
                lambda hook: hook(exc_type, exc_value, traceback_obj),
            )
            return
        self._prev_excepthook(exc_type, exc_value, traceback_obj)

    def threading_hook(
        self, args, *, _getattr=getattr, _issubclass=issubclass, _system_exit=SystemExit
    ):
        # The default threading hook deliberately ignores SystemExit (normal
        # sys.exit() in a worker thread); preserve that by delegating.
        if args.exc_type is not None and _issubclass(args.exc_type, _system_exit):
            self._prev_threading_hook(args)
            return
        thread = _getattr(args, "thread", None)
        name = _getattr(thread, "name", None) or "Thread"
        prefix = "Exception in thread %s:\n" % name
        if self._try_render(args.exc_value, args.exc_traceback, prefix):
            self._notify_custom_hook(
                self._prev_threading_hook,
                self._default_threading_hook,
                lambda hook: hook(args),
            )
            return
        self._prev_threading_hook(args)

    def unraisablehook(self, unraisable, *, _getattr=getattr, _bex=BaseException):
        message = _getattr(unraisable, "err_msg", None) or "Exception ignored in"
        try:
            prefix = "%s: %r\n" % (message, unraisable.object)
        except _bex:
            prefix = "%s\n" % message
        if self._try_render(unraisable.exc_value, unraisable.exc_traceback, prefix):
            self._notify_custom_hook(
                self._prev_unraisablehook,
                self._default_unraisablehook,
                lambda hook: hook(unraisable),
            )
            return
        self._prev_unraisablehook(unraisable)


_CriboSourceMapRuntime._bootstrap(
    "__CRIBO_SOURCEMAP_MODE__", globals().get("__file__", "<stdin>")
)
