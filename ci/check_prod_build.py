#!/usr/bin/env python3
"""Fail CI if the deployable optimizer build is not feature-clean.

The canonical ``artifacts/<crate>.wasm`` file (factory) that
deploy tooling loads are produced by the optimizer build named ``prod``. That
build MUST have an empty feature set: a ``mock`` or ``integration_short_timing``
wasm must never become the deployable artifact (see
``SECURITY_REVIEW_PREAUDIT.md`` finding H-1).

This gate asserts, for each crate, that a
``[[package.metadata.optimizer.builds]]`` entry named ``prod`` exists and does
not enable any test-only feature.
"""
import sys

try:
    import tomllib  # Python 3.11+ (GitHub ubuntu-latest ships 3.12)
except ModuleNotFoundError:  # pragma: no cover
    print("::error::python >= 3.11 required for tomllib", file=sys.stderr)
    sys.exit(2)

CRATES = ("factory/Cargo.toml",)
FORBIDDEN = {"mock", "integration_short_timing"}


def main() -> int:
    overall_ok = True
    for path in CRATES:
        with open(path, "rb") as f:
            data = tomllib.load(f)
        builds = (
            data.get("package", {})
            .get("metadata", {})
            .get("optimizer", {})
            .get("builds", [])
        )
        prod = [b for b in builds if b.get("name") == "prod"]
        crate_ok = True
        if not prod:
            print(
                f"::error file={path}::missing optimizer build named 'prod' "
                f"(the deployable, no-features artifact)"
            )
            crate_ok = False
        for b in prod:
            feats = b.get("features", [])
            forbidden = sorted(set(feats) & FORBIDDEN)
            if forbidden:
                print(
                    f"::error file={path}::'prod' optimizer build enables "
                    f"test-only feature(s) {forbidden}; these must never reach "
                    f"the deployable artifact"
                )
                crate_ok = False
            elif feats:
                print(
                    f"::error file={path}::'prod' optimizer build must have "
                    f"empty features; found {feats}"
                )
                crate_ok = False
        if crate_ok:
            print(f"ok: {path} 'prod' optimizer build is feature-clean")
        overall_ok = overall_ok and crate_ok
    return 0 if overall_ok else 1


if __name__ == "__main__":
    sys.exit(main())
