"""Cribo source map runtime (injected prologue).

Remaps tracebacks of uncaught exceptions back to the original source files
using the Source Map v3 emitted at bundle time. Lazy by design: no file I/O,
parsing, or decoding happens at import time; everything is deferred to the
first uncaught exception. Under resource pressure the decoder streams the map
in constant memory and falls back to the default traceback on any failure.
See docs/source-maps.md in the cribo repository.
"""

import binascii as _cribo_binascii
import os as _cribo_os
import sys as _cribo_sys
import threading as _cribo_threading

_CRIBO_SM_MODE = "__CRIBO_SOURCEMAP_MODE__"
_CRIBO_SM_BUNDLE = globals().get("__file__", "<bundle>")
_CRIBO_SM_CHUNK = 8192
_CRIBO_SM_STATE = {"in_hook": False}
_CRIBO_SM_B64 = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"

_cribo_sm_prev_excepthook = _cribo_sys.excepthook
_cribo_sm_prev_unraisablehook = _cribo_sys.unraisablehook
_cribo_sm_prev_threading_hook = _cribo_threading.excepthook


def _cribo_sm_map_location():
    """Resolve the map location per delivery mode, or None when inactive.

    Returns (map_path, map_dir); map_path is None for inline mode (the map
    lives inside the bundle file itself). Called lazily at hook-fire time so
    the happy path never touches the environment or the filesystem.
    """
    env = _cribo_os.environ.get("CRIBO_SOURCE_MAPS", "")
    if env == "0":
        return None
    bundle = _CRIBO_SM_BUNDLE
    if _CRIBO_SM_MODE == "inline":
        return (None, _cribo_os.path.dirname(_cribo_os.path.abspath(bundle)))
    sibling = bundle + ".map"
    if _CRIBO_SM_MODE == "linked":
        if _cribo_os.path.exists(sibling):
            return (sibling, _cribo_os.path.dirname(_cribo_os.path.abspath(sibling)))
        return None
    # external: opt in via CRIBO_SOURCE_MAPS=1 (or a path to the map file)
    if env in ("1", "true", "yes", "on"):
        path = sibling
    elif env:
        path = env
    else:
        return None
    return (path, _cribo_os.path.dirname(_cribo_os.path.abspath(path)))


def _cribo_sm_file_chunks(path):
    """Yield fixed-size chunks of a file (constant memory)."""
    handle = open(path, "rb")
    try:
        while True:
            chunk = handle.read(_CRIBO_SM_CHUNK)
            if not chunk:
                break
            yield chunk
    finally:
        handle.close()


def _cribo_sm_find_inline_payload(handle):
    """Backward-scan the bundle for the last inline map marker.

    Returns the byte offset of the base64 payload, or -1. Only the tail of the
    file is examined; the bundle body is never read.
    """
    marker = b"# sourceMappingURL=data:"
    handle.seek(0, 2)
    position = handle.tell()
    overlap = b""
    found = -1
    while position > 0:
        step = _CRIBO_SM_CHUNK if position >= _CRIBO_SM_CHUNK else position
        position -= step
        handle.seek(position)
        data = handle.read(step) + overlap
        index = data.rfind(marker)
        if index >= 0:
            found = position + index
            break
        overlap = data[: len(marker) - 1]
    if found < 0:
        return -1
    handle.seek(found)
    head = handle.read(192)
    base64_at = head.find(b"base64,")
    if base64_at < 0:
        return -1
    return found + base64_at + len(b"base64,")


def _cribo_sm_inline_chunks(path):
    """Yield decoded chunks of an inline (base64 data URL) source map."""
    handle = open(path, "rb")
    try:
        start = _cribo_sm_find_inline_payload(handle)
        if start < 0:
            return
        handle.seek(start)
        pending = b""
        while True:
            raw = handle.read(_CRIBO_SM_CHUNK)
            if not raw:
                break
            data = pending + raw.translate(None, b"\r\n")
            usable = len(data) - (len(data) % 4)
            pending = data[usable:]
            if usable:
                yield _cribo_binascii.a2b_base64(data[:usable])
        if pending:
            yield _cribo_binascii.a2b_base64(pending + b"=" * (-len(pending) % 4))
    finally:
        handle.close()


class _CriboSmStream(object):
    """Byte-at-a-time reader over an iterator of byte chunks."""

    __slots__ = ("_chunks", "_buf", "_pos")

    def __init__(self, chunks):
        self._chunks = iter(chunks)
        self._buf = b""
        self._pos = 0

    def read_byte(self):
        while self._pos >= len(self._buf):
            try:
                self._buf = next(self._chunks)
            except StopIteration:
                return -1
            self._pos = 0
        value = self._buf[self._pos]
        self._pos += 1
        return value


def _cribo_sm_skip_ws(stream, byte):
    while byte in (32, 9, 10, 13):
        byte = stream.read_byte()
    return byte


def _cribo_sm_read_string(stream, collect):
    """Consume a JSON string whose opening quote was already read.

    Returns the decoded text when collect is true, else None (contents are
    discarded byte-by-byte). Escaped quotes and \\uXXXX sequences are handled,
    so string *values* containing text like '"mappings":' cannot confuse the
    key scanner.
    """
    buf = bytearray() if collect else None
    while True:
        byte = stream.read_byte()
        if byte < 0:
            raise ValueError("unterminated JSON string")
        if byte == 34:  # '"'
            return buf.decode("utf-8", "replace") if collect else None
        if byte != 92:  # '\\'
            if buf is not None:
                buf.append(byte)
            continue
        escape = stream.read_byte()
        if escape < 0:
            raise ValueError("unterminated JSON escape")
        if escape == 117:  # 'u'
            code = 0
            for _ in range(4):
                digit = stream.read_byte()
                if digit < 0:
                    raise ValueError("unterminated unicode escape")
                code = code * 16 + int(chr(digit), 16)
            if buf is not None:
                buf.extend(chr(code).encode("utf-8", "surrogatepass"))
        elif buf is not None:
            table = {98: 8, 102: 12, 110: 10, 114: 13, 116: 9}
            buf.append(table.get(escape, escape))


def _cribo_sm_skip_value(stream, byte):
    """Skip one JSON value; return the first byte after it (or -1)."""
    if byte == 34:  # string
        _cribo_sm_read_string(stream, False)
        return stream.read_byte()
    if byte in (123, 91):  # object / array
        depth = 1
        while depth > 0:
            byte = stream.read_byte()
            if byte < 0:
                raise ValueError("unterminated JSON container")
            if byte == 34:
                _cribo_sm_read_string(stream, False)
            elif byte in (123, 91):
                depth += 1
            elif byte in (125, 93):
                depth -= 1
        return stream.read_byte()
    # number / true / false / null: consume until a delimiter
    while byte >= 0 and byte not in (44, 125, 93):  # ',' '}' ']'
        byte = stream.read_byte()
    return byte


def _cribo_sm_read_string_array(stream, byte):
    """Read a JSON array of strings/nulls; return (list, byte after array)."""
    if byte != 91:  # '['
        raise ValueError("expected array")
    items = []
    byte = _cribo_sm_skip_ws(stream, stream.read_byte())
    if byte == 93:  # ']'
        return items, stream.read_byte()
    while True:
        if byte == 34:
            items.append(_cribo_sm_read_string(stream, True))
            byte = _cribo_sm_skip_ws(stream, stream.read_byte())
        else:
            items.append(None)
            byte = _cribo_sm_skip_ws(stream, _cribo_sm_skip_value(stream, byte))
        if byte == 93:
            return items, stream.read_byte()
        if byte != 44:  # ','
            raise ValueError("malformed array")
        byte = _cribo_sm_skip_ws(stream, stream.read_byte())


def _cribo_sm_decode_vlq(stream, needed, max_needed):
    """Streaming VLQ state machine over the raw bytes of the mappings string.

    Constant state: line/segment counters plus running deltas. Records the
    first segment per needed generated line; exits as soon as every needed
    line is resolved or the max needed line is passed. Consumes up to and
    including the closing quote (or stops early).
    """
    lut = {}
    for index in range(64):
        lut[_CRIBO_SM_B64[index]] = index
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
            return result
        if byte == 59:  # ';'
            end_segment()
            gen_line += 1
            field = 0
            if gen_line > max_needed or len(result) == len(needed):
                return result
            continue
        if byte == 44:  # ','
            end_segment()
            field = 0
            continue
        value = lut.get(byte)
        if value is None:
            raise ValueError("unexpected byte in mappings")
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


def _cribo_sm_scan(chunks, needed, max_needed):
    """Scan the map's top-level object; return (sources, line table)."""
    stream = _CriboSmStream(chunks)
    byte = _cribo_sm_skip_ws(stream, stream.read_byte())
    if byte != 123:  # '{'
        raise ValueError("not a JSON object")
    sources = []
    table = {}
    saw_mappings = False
    byte = _cribo_sm_skip_ws(stream, stream.read_byte())
    while byte == 34:  # '"' starting a key
        key = _cribo_sm_read_string(stream, True)
        byte = _cribo_sm_skip_ws(stream, stream.read_byte())
        if byte != 58:  # ':'
            raise ValueError("malformed object")
        byte = _cribo_sm_skip_ws(stream, stream.read_byte())
        if key == "sources":
            sources, byte = _cribo_sm_read_string_array(stream, byte)
        elif key == "mappings":
            if byte != 34:
                raise ValueError("mappings is not a string")
            table = _cribo_sm_decode_vlq(stream, needed, max_needed)
            saw_mappings = True
            if sources:
                break  # both fields consumed; ignore the rest of the map
            byte = stream.read_byte()
        else:
            byte = _cribo_sm_skip_value(stream, byte)
        byte = _cribo_sm_skip_ws(stream, byte)
        if byte == 44:  # ','
            byte = _cribo_sm_skip_ws(stream, stream.read_byte())
    if not saw_mappings:
        raise ValueError("no mappings field")
    return sources, table


def _cribo_sm_load(needed_lines):
    """Load (table, sources, map_dir) for 1-based bundle line numbers.

    Returns None when the runtime is inactive for the current mode. The
    returned table is keyed by 1-based bundle lines mapping to
    (source_index, 1-based original line).
    """
    location = _cribo_sm_map_location()
    if location is None:
        return None
    map_path, map_dir = location
    needed0 = set(line - 1 for line in needed_lines)
    max_needed = max(needed0)
    if map_path is None:
        chunks = _cribo_sm_inline_chunks(_CRIBO_SM_BUNDLE)
    else:
        chunks = _cribo_sm_file_chunks(map_path)
    sources, table0 = _cribo_sm_scan(chunks, needed0, max_needed)
    table = {}
    for line0, (src_idx, src_line0) in table0.items():
        table[line0 + 1] = (src_idx, src_line0 + 1)
    return (table, sources, map_dir)


def _cribo_sm_load_json_fallback(needed_lines):
    """Fallback: full json.loads parse, reusing the VLQ machine on the result."""
    location = _cribo_sm_map_location()
    if location is None:
        return None
    map_path, map_dir = location
    import json

    if map_path is None:
        raw = b"".join(_cribo_sm_inline_chunks(_CRIBO_SM_BUNDLE))
    else:
        handle = open(map_path, "rb")
        try:
            raw = handle.read()
        finally:
            handle.close()
    data = json.loads(raw.decode("utf-8"))
    sources = data.get("sources") or []
    mappings = data.get("mappings") or ""
    needed0 = set(line - 1 for line in needed_lines)
    stream = _CriboSmStream([mappings.encode("ascii"), b'"'])
    table0 = _cribo_sm_decode_vlq(stream, needed0, max(needed0))
    table = {}
    for line0, (src_idx, src_line0) in table0.items():
        table[line0 + 1] = (src_idx, src_line0 + 1)
    return (table, sources, map_dir)


def _cribo_sm_collect_needed(exc_value, traceback_obj):
    """1-based bundle line numbers referenced by the traceback (and its chain)."""
    needed = set()

    def add(tb):
        while tb is not None:
            if tb.tb_frame.f_code.co_filename == _CRIBO_SM_BUNDLE:
                needed.add(tb.tb_lineno)
            tb = tb.tb_next

    add(traceback_obj)
    exc = exc_value
    seen = set()
    depth = 0
    while exc is not None and id(exc) not in seen and depth < 16:
        seen.add(id(exc))
        depth += 1
        add(getattr(exc, "__traceback__", None))
        cause = getattr(exc, "__cause__", None)
        context = getattr(exc, "__context__", None)
        exc = cause if cause is not None else context
    return needed


def _cribo_sm_source_line(path, lineno):
    """Read a single 1-based line from a file without caching it."""
    try:
        handle = open(path, "rb")
    except OSError:
        return None
    try:
        current = 0
        for raw in handle:
            current += 1
            if current == lineno:
                return raw.decode("utf-8", "replace").strip()
            if current > lineno:
                break
    except OSError:
        return None
    finally:
        handle.close()
    return None


def _cribo_sm_write_frames(traceback_obj, table, sources, map_dir, write):
    """Write remapped frame lines, collapsing repeated frames like CPython.

    Consecutive identical frames (recursion) print at most 3 times followed by
    a "[Previous line repeated N more times]" marker; source line text is
    cached per (file, line) within one rendering to avoid re-reading files.
    """
    cache = {}
    last = None
    repeats = 0

    def emit(entry):
        write('  File "%s", line %d, in %s\n' % entry)
        key = (entry[0], entry[1])
        if key not in cache:
            cache[key] = _cribo_sm_source_line(entry[0], entry[1])
        if cache[key]:
            write("    %s\n" % cache[key])

    while traceback_obj is not None:
        frame = traceback_obj.tb_frame
        filename = frame.f_code.co_filename
        lineno = traceback_obj.tb_lineno
        name = frame.f_code.co_name
        if filename == _CRIBO_SM_BUNDLE:
            mapped = table.get(lineno)
            if mapped is not None and 0 <= mapped[0] < len(sources) and sources[mapped[0]]:
                source = sources[mapped[0]]
                if not _cribo_os.path.isabs(source):
                    source = _cribo_os.path.normpath(_cribo_os.path.join(map_dir, source))
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


def _cribo_sm_exception_line(exc_value):
    exc_type = type(exc_value)
    name = getattr(exc_type, "__qualname__", exc_type.__name__)
    module = getattr(exc_type, "__module__", None)
    if module not in (None, "builtins", "__main__"):
        name = "%s.%s" % (module, name)
    try:
        text = str(exc_value)
    except BaseException:
        text = "<exception str() failed>"
    return "%s: %s\n" % (name, text) if text else "%s\n" % name


def _cribo_sm_render(exc_value, table, sources, map_dir, write):
    """Render the exception (with its cause/context chain) like CPython does."""
    chain = []
    exc = exc_value
    seen = set()
    while exc is not None and id(exc) not in seen and len(chain) < 16:
        seen.add(id(exc))
        cause = getattr(exc, "__cause__", None)
        context = getattr(exc, "__context__", None)
        suppress = getattr(exc, "__suppress_context__", False)
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
    ordered = list(reversed(chain))
    for index, (exc, link) in enumerate(ordered):
        if index > 0:
            if link == "cause":
                write(
                    "\nThe above exception was the direct cause of the following exception:\n\n"
                )
            else:
                write(
                    "\nDuring handling of the above exception, another exception occurred:\n\n"
                )
        tb = getattr(exc, "__traceback__", None)
        if tb is not None:
            write("Traceback (most recent call last):\n")
            _cribo_sm_write_frames(tb, table, sources, map_dir, write)
        write(_cribo_sm_exception_line(exc))


def _cribo_sm_try_render(exc_value, traceback_obj, prefix):
    """Attempt a remapped rendering to stderr; True on success.

    Never raises and never masks the original exception: any failure in this
    runtime returns False so callers can delegate to the previous hook.
    """
    if _CRIBO_SM_STATE["in_hook"] or exc_value is None:
        return False
    _CRIBO_SM_STATE["in_hook"] = True
    old_limit = None
    try:
        needed = _cribo_sm_collect_needed(exc_value, traceback_obj)
        if not needed:
            return False
        loaded = None
        try:
            loaded = _cribo_sm_load(needed)
        except BaseException:
            try:
                loaded = _cribo_sm_load_json_fallback(needed)
            except BaseException:
                loaded = None
        if not loaded:
            return False
        table, sources, map_dir = loaded
        if not table:
            return False
        try:
            old_limit = _cribo_sys.getrecursionlimit()
            _cribo_sys.setrecursionlimit(old_limit + 64)
        except BaseException:
            old_limit = None
        # Buffer the rendering so a mid-render failure produces no partial
        # output before the previous hook prints the standard traceback.
        parts = []
        if prefix:
            parts.append(prefix)
        _cribo_sm_render(exc_value, table, sources, map_dir, parts.append)
        stderr = _cribo_sys.stderr
        stderr.write("".join(parts))
        try:
            stderr.flush()
        except BaseException:
            pass
        return True
    except BaseException:
        return False
    finally:
        if old_limit is not None:
            try:
                _cribo_sys.setrecursionlimit(old_limit)
            except BaseException:
                pass
        _CRIBO_SM_STATE["in_hook"] = False


def _cribo_sm_excepthook(exc_type, exc_value, traceback_obj):
    if not _cribo_sm_try_render(exc_value, traceback_obj, None):
        _cribo_sm_prev_excepthook(exc_type, exc_value, traceback_obj)


def _cribo_sm_threading_hook(args):
    thread = getattr(args, "thread", None)
    name = getattr(thread, "name", None) or "Thread"
    prefix = "Exception in thread %s:\n" % name
    if not _cribo_sm_try_render(args.exc_value, args.exc_traceback, prefix):
        _cribo_sm_prev_threading_hook(args)


def _cribo_sm_unraisablehook(unraisable):
    message = getattr(unraisable, "err_msg", None) or "Exception ignored in"
    try:
        prefix = "%s: %r\n" % (message, unraisable.object)
    except BaseException:
        prefix = "%s\n" % message
    if not _cribo_sm_try_render(unraisable.exc_value, unraisable.exc_traceback, prefix):
        _cribo_sm_prev_unraisablehook(unraisable)


_cribo_sys.excepthook = _cribo_sm_excepthook
_cribo_sys.unraisablehook = _cribo_sm_unraisablehook
_cribo_threading.excepthook = _cribo_sm_threading_hook
