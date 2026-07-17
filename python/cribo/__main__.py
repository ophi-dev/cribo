"""
Command-line interface for cribo.

This module provides access to the cribo CLI when called as `python -m cribo`.
The main interface is the binary `cribo` command.
"""

import os
import subprocess
import sys


def main() -> None:
    """Main entry point that delegates to the cribo binary."""
    try:
        env = os.environ.copy()
        env.setdefault("CRIBO_PYTHON", sys.executable)
        # Call the cribo binary with the same arguments
        result = subprocess.run(["cribo"] + sys.argv[1:], check=False, env=env)
        sys.exit(result.returncode)
    except FileNotFoundError:
        print(
            "cribo binary not found. Please ensure cribo is properly installed.",
            file=sys.stderr,
        )
        sys.exit(1)


if __name__ == "__main__":
    main()
