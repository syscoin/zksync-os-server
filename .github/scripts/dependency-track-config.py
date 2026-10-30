#!/usr/bin/env python3
"""Select Dependency Track only when its complete configuration is present."""

import os
import sys


def enabled(env):
    fields = ("DT_HOSTNAME", "DT_API_KEY", "DT_PARENT_UUID")
    present = [bool(env.get(name, "")) for name in fields]
    if not any(present):
        return False
    if not all(present):
        raise ValueError("Dependency Track hostname, API key, and parent project UUID must all be configured")
    return True


if __name__ == "__main__":
    try:
        print("true" if enabled(os.environ) else "false")
    except ValueError as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
