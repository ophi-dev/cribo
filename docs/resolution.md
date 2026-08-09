# Module Resolution Algorithm

This document describes how Cribo resolves Python imports during the bundling process. The algorithm follows Python's import resolution semantics while adapting them for static analysis.

## Overview

Cribo records three independent module-resolution facts for each import:

- **Origin**: First-party, third-party distribution, standard library, or unknown
- **Source kind**: Python source, namespace package, native extension, or unresolved
- **Bundle disposition**: Included in the bundle or preserved as an external import

Requirement generation is a separate step that maps runtime imports to installed distributions.
This keeps bundling policy independent from distribution-name inference.

## Resolution Process

### 1. Import Discovery

When analyzing a Python file, Cribo extracts all import statements:

- `import module`
- `import package.submodule`
- `from package import module`
- `from . import relative`
- `from ..parent import module`

### 2. Search Path Construction

For bundling purposes, the "current directory" is **the directory containing the entry file**, not the directory where Cribo is executed.

For example, if the entry file is `/project/src/app/main.py`, the search path is:

```
1. /project/src/app/          # Directory containing the entry file
2. [PYTHONPATH directories]   # From PYTHONPATH environment variable
3. [Configured src dirs]      # From cribo.toml or defaults
```

This matches Python's behavior where `sys.path[0]` is the directory containing the script being run.

#### Example with Entry File

Command:

```bash
cribo --entry /home/user/myproject/src/main.py --output bundle.py
```

Search path for imports in `main.py`:

```
1. /home/user/myproject/src/  # Entry file's directory
2. /home/user/libs/           # From PYTHONPATH (if set)
3. [configured directories]    # From cribo.toml
```

**Note**: This behavior is currently not configurable. The entry file's directory is always the first in the search path.

### 3. Module Location Algorithm

For each import (e.g., `import tada`), Cribo searches each directory in the search path:

#### Step 1: Check for Package Initializer

```
Look for: <search_dir>/tada/__init__.py or __init__.<platform-extension>
If Python source is found: Load as a package module
If a native initializer is found: Preserve as an external import
```

#### Step 2: Check for File Module

```
Look for: <search_dir>/tada.py
If found: Load as file module
```

#### Step 3: Check for Native Extension

```
Look for: <search_dir>/tada.<platform-extension>
Examples: tada.cpython-312-x86_64-linux-gnu.so, tada.pyd
If found: Preserve as an external import
```

#### Step 4: Check for Namespace Package (PEP 420)

```
Look for: <search_dir>/tada/ (directory)
If found: Continue searching other paths
Only use if no Python or native package initializer exists anywhere
```

**First match wins** - the search stops as soon as a module is found.

### 4. Relative Import Resolution

Relative imports are resolved within the current package structure:

#### Single Dot (`.`)

```python
# In /project/src/utils/helper.py
from . import tada
```

- Searches only in `/project/src/utils/`
- Does not fall back to other search paths

#### Multiple Dots (`..`)

```python
# In /project/src/utils/deep/helper.py
from ...data import tada
```

- Goes up two levels: `/project/src/`
- Looks for `/project/src/data/` (must be a package)
- Then resolves `tada` within that package

### 5. Import Classification

After locating a module, Cribo derives each classification fact independently:

1. **Origin**
   - Explicit `known_first_party` and `known_third_party` entries take precedence.
   - Distribution metadata (`*.dist-info/RECORD` and `METADATA`) identifies installed packages.
   - Source without distribution metadata in an entry, PYTHONPATH, or configured source directory
     is first-party.
   - Standard-library and unresolved imports retain their own origins.

2. **Source kind**
   - `.py` files and package `__init__.py` files are Python source.
   - PEP 420 directories are namespace packages.
   - Platform extension files such as `.so` and `.pyd`, including native package initializers, are
     native extensions.

3. **Bundle disposition**
   - Python source and namespace packages found through bundle search paths are included unless
     explicitly configured as third-party.
   - Modules found only through a virtual environment remain external by default. With the opt-in
     `bundle-third-party` mode enabled, pure-Python distributions found in a virtual environment
     are included, while any package that ships native extension artifacts (`.so`/`.pyd` anywhere
     inside its top-level package directory) remains external as a whole.
   - Native extensions remain external.
   - Unresolved imports remain external unless explicitly first-party, in which case bundling
     reports the missing source.

4. **Requirement**
   - Explicit `requirements.module-map` entries use longest-prefix matching.
   - Core Metadata 2.5 `Import-Name` and `Import-Namespace` fields take precedence.
   - Installed Python and native files provide compatibility evidence for older distributions.
   - Multiple equally strong providers produce an actionable ambiguity error.
   - Unknown external imports use a valid top-level import name as a fallback.
   - Imports without a valid fallback requirement name are skipped with a warning.
   - First-party and standard-library imports do not produce requirements.

## Configuration

### Source Directories

Configure which directories contain first-party code:

```toml
# cribo.toml
src = ["src", "lib", "app"]
```

Default configuration:

```toml
src = ["src", "."] # Note: "." can cause performance issues
```

### Known Modules

Explicitly classify modules:

```toml
# cribo.toml
known_first_party = ["mycompany", "internal_lib"]
known_third_party = ["requests", "numpy"]

[requirements]
python = ".venv/bin/python"
module-map = { sklearn = "scikit-learn" }
```

### Third-Party Bundling (Opt-In)

By default, third-party dependencies stay external and are listed in `requirements.txt` (with
`--emit-requirements`). The opt-in `bundle-third-party` mode inlines pure-Python third-party
dependencies into the bundle, similar to how JavaScript bundlers such as esbuild handle
`node_modules`:

```toml
# cribo.toml
bundle-third-party = true
```

Or via CLI / environment variable:

```bash
cribo --entry main.py --output bundle.py --bundle-third-party
CRIBO_BUNDLE_THIRD_PARTY=1 cribo --entry main.py --output bundle.py
```

Behavior in this mode:

- Pure-Python distributions found in the virtual environment are bundled and omitted from
  `requirements.txt`.
- Any package that ships native extension artifacts (`.so`/`.pyd`) anywhere inside its top-level
  package directory is automatically kept external as a whole and still emitted into
  `requirements.txt` — the automatic equivalent of esbuild's `external` option.
- `known_third_party` entries act as a manual escape hatch: listed packages always stay external,
  even when they are pure Python.

### Environment Variables

- `PYTHONPATH`: Additional directories to search for first-party modules
- `CRIBO_SRC`: Override source directories (comma-separated)
- `CRIBO_PYTHON`: Interpreter used to inspect installed distribution metadata
- `CRIBO_BUNDLE_THIRD_PARTY`: Enable opt-in third-party bundling (`true`/`1`)

## Examples

### Example 1: Simple Import

Entry file: `/project/src/main.py`

```python
import helper
import requests
```

Resolution (search path starts from `/project/src/`):

1. `helper`:
   - Check `/project/src/helper/__init__.py` ❌
   - Check `/project/src/helper.py` ✅
   - Found → First-party

2. `requests`:
   - Check `/project/src/requests/__init__.py` ❌
   - Check `/project/src/requests.py` ❌
   - Not in PYTHONPATH ❌
   - Not in standard library ❌
   - → Third-party (not bundled)

### Example 2: Package Import

Entry file: `/project/app/main.py`
Content of another file `/project/app/views.py`:

```python
from utils.database import connect
```

Resolution (search path starts from `/project/app/`):

1. Look for `utils` package:
   - Check `/project/app/utils/__init__.py` ✅

2. Within `utils` package, find `database`:
   - Check `/project/app/utils/database/__init__.py` ✅

3. Import `connect` from that module

### Example 3: Relative Import

Entry file: `/project/src/main.py`
In file `/project/src/utils/helpers/string.py`:

```python
from ..database import Connection
from . import formatters
```

Resolution:

1. `from ..database`:
   - Parent package: `/project/src/utils/`
   - Check `/project/src/utils/database/__init__.py` ✅
   - Import `Connection` from that module

2. `from . import formatters`:
   - Current package: `/project/src/utils/helpers/`
   - Check `/project/src/utils/helpers/formatters/__init__.py` ❌
   - Check `/project/src/utils/helpers/formatters.py` ✅

## Differences from Python Runtime

1. **No dynamic imports**: All imports must be statically analyzable
2. **No sys.path modifications**: Search paths are fixed at bundle time
3. **Explicit bundle roots**: Python source is bundled only when selected by the configured search
   policy; native extensions remain external
4. **Entry-relative paths**: First search path is always the entry file's directory
