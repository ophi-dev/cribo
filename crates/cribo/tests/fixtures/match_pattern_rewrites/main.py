from inline_first import (
    Event as FirstEvent,
    get_last_result as get_first_result,
    match_class as match_first_class,
    match_mapping as match_first_mapping,
    match_sequence as match_first_sequence,
    record_result as record_first_result,
)
from inline_second import (
    Event as SecondEvent,
    get_last_result as get_second_result,
    match_class as match_second_class,
    match_mapping as match_second_mapping,
    match_sequence as match_second_sequence,
    record_result as record_second_result,
)
from wrapped_matcher import (
    MODULE_MATCH,
    WrappedEvent,
    get_global_capture,
    match_capture,
    match_global_capture,
    match_import_capture,
    match_wrapped,
    match_wrapped_global,
    value as module_value,
)

print(match_first_class(FirstEvent("first-ready")))
print(match_first_mapping({"kind": "first-ready"}))
print(match_first_sequence(["first-pending"]))
record_first_result("first-ready")
print(get_first_result())

print(match_second_class(SecondEvent("second-ready")))
print(match_second_mapping({"kind": "second-ready"}))
print(match_second_sequence(["second-pending"]))
record_second_result("second-ready")
print(get_second_result())

print(match_wrapped(WrappedEvent("wrapped-ready")))
print(match_wrapped_global(WrappedEvent("wrapped-ready")))
print(MODULE_MATCH)
print(match_capture(["captured-value"]))
print(module_value)
print(match_import_capture(["captured-import"]))
print(match_global_capture(["captured-global"]))
print(get_global_capture())
