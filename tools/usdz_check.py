#!/usr/bin/env python3
"""Check a USDZ against a real USD implementation, including Apple's AR rules.

The `usd-core` wheel ships Pixar's library but not the `usdchecker` command, so
this calls the same API that command wraps. macOS has `usdchecker` itself; this
is what runs everywhere else, and it is the check CI performs.

    python3 tools/usdz_check.py file.usdz
"""

import sys

from pxr import Usd, UsdValidation


def check(path: str) -> bool:
    """Every validator USD registers, run over the stage."""
    stage = Usd.Stage.Open(path)
    if stage is None:
        print(f"  the stage would not open at all")
        return False

    registry = UsdValidation.ValidationRegistry()
    # Everything registered, which includes the USDZ package rules and the
    # UsdShade ones that catch a normal map without its scale and bias.
    names = [m.name for m in registry.GetAllValidatorMetadata() if not m.isSuite]
    context = UsdValidation.ValidationContext(
        [registry.GetOrLoadValidatorByName(n) for n in names]
    )

    errors = list(context.Validate(stage))
    for error in errors:
        print(f"  {error.GetMessage()}")
    print(f"  {len(names)} validators, {len(errors)} errors")
    return not errors


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    path = sys.argv[1]
    print(f"{path}")
    return 0 if check(path) else 1


if __name__ == "__main__":
    sys.exit(main())
