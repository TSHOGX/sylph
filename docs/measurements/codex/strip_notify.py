#!/usr/bin/env python3
"""Strip the user's `notify` hook from a copied config.toml.

The real ~/.codex/config.toml runs a computer-use client on `turn-ended`.
A probe must not fire the user's own tooling, so the throwaway CODEX_HOME
gets that one key removed. Everything else is kept, because the model
provider and reasoning effort are part of what we are measuring against.
"""

import re
import sys

path = sys.argv[1]
src = open(path).read()
out = re.sub(r"^notify\s*=.*$", "", src, flags=re.M)
open(path, "w").write(out)
print("notify lines removed:", len(re.findall(r"^notify\s*=", src, flags=re.M)))
