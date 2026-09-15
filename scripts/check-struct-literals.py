#!/usr/bin/env python3
"""Check that every literal of a shared struct sets every field it requires.

Adding a field to a widely-constructed struct breaks a lot of call sites at once, and the obvious way
to find them — compile, follow the errors — is blind to code the host platform does not compile. That
bit this repository four times in one stretch:

  * `BodyData.grpc` and `HttpResponse.trailers` broke `benches/`, which `cargo test` does not build;
  * `NetworkInfo.server_host` broke a literal inside `#[cfg(target_os = "linux")]` in `udp.rs`, which
    macOS never compiles — so CI failed on a change that was green locally;
  * `UdpSession` gained Linux-only fields and a test literal outside the gate stopped building.

Each time the fix was mechanical; each time it reached CI first. This is that mechanical check, done
by reading the source rather than by asking one platform's compiler.

A literal that uses `..Default::default()` is considered satisfied.

Usage:
    python3 scripts/check-struct-literals.py NetworkInfo relay-core-api/src/flow.rs
    python3 scripts/check-struct-literals.py --all    # every struct registered below
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Structs that are constructed in many places, with the file that defines their fields.
REGISTERED = [
    ("NetworkInfo", "relay-core-api/src/flow.rs"),
    ("BodyData", "relay-core-api/src/flow.rs"),
    ("HttpRequest", "relay-core-api/src/flow.rs"),
    ("HttpResponse", "relay-core-api/src/flow.rs"),
]

# Fields with a serde default are still required in a Rust literal, so nothing is excluded here.


def struct_fields(source_file: str, struct_name: str) -> list[str]:
    """Field names of `struct_name`, skipping anything inside a nested block (closures, fn bodies)."""
    text = (ROOT / source_file).read_text()
    match = re.search(rf"pub struct {re.escape(struct_name)}\s*\{{", text)
    if not match:
        raise SystemExit(f"struct {struct_name} not found in {source_file}")

    fields: list[str] = []
    depth = 1
    for line in text[match.end():].split("\n"):
        stripped = line.strip()
        if stripped.startswith("//"):
            continue
        depth += stripped.count("{") - stripped.count("}")
        if depth <= 0:
            break
        field = re.match(r"pub ([a-z_][a-z0-9_]*)\s*:", stripped)
        if field:
            fields.append(field.group(1))
    if not fields:
        raise SystemExit(f"no fields parsed for {struct_name}")
    return fields


def rust_sources() -> list[Path]:
    files: list[Path] = []
    for path in ROOT.rglob("*.rs"):
        parts = path.relative_to(ROOT).parts
        if "target" in parts or ".git" in parts:
            continue
        files.append(path)
    return files


def literal_body(text: str, open_brace: int) -> str:
    """Text between a literal's braces, by brace depth rather than by line heuristics.

    A line-window heuristic produced false positives: it could run past the end of one literal into the
    next, and it missed short literals entirely. Braces are counted instead, with line comments skipped.
    """
    depth = 0
    for index in range(open_brace, len(text)):
        char = text[index]
        if char == "\n":
            comment = text.find("//", index)
            newline = text.find("\n", index)
            if comment != -1 and comment < newline:
                index = comment
                continue
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return text[open_brace + 1 : index]
    return text[open_brace + 1 :]


def sets_field(body: str, field: str) -> bool:
    """`field: value` or the shorthand `field,` both initialise it."""
    return (
        re.search(rf"(^|[\s{{(,]){re.escape(field)}\s*:", body) is not None
        or re.search(rf"(^|[\s{{(,]){re.escape(field)}\s*,", body) is not None
    )


def check(struct_name: str, source_file: str) -> list[str]:
    """Return `path:line` for every literal that does not set every field."""
    required = struct_fields(source_file, struct_name)
    problems: list[str] = []

    for path in rust_sources():
        text = path.read_text()
        # `-> BodyData {` is a function signature, not a literal: the brace opens the body. Same for a
        # type in a `where` clause or a `:` annotation.
        pattern = rf"(?<!-> )(?<!: )\b{re.escape(struct_name)}\s*\{{"
        for match in re.finditer(pattern, text):
            body = literal_body(text, match.end() - 1)
            # A struct-update expression (`..other`) supplies the rest on purpose.
            if "..Default::default()" in body or re.search(r"(^|\s)\.\.", body):
                continue
            absent = [field for field in required if not sets_field(body, field)]
            if absent:
                line = text[: match.start()].count("\n") + 1
                problems.append(f"{path.relative_to(ROOT)}:{line} missing {', '.join(absent)}")
    return problems


def main(argv: list[str]) -> int:
    if len(argv) == 2 and argv[1] == "--all":
        targets = REGISTERED
    elif len(argv) == 3:
        targets = [(argv[1], argv[2])]
    else:
        print(__doc__)
        return 2

    failures = 0
    for struct_name, source_file in targets:
        problems = check(struct_name, source_file)
        if problems:
            failures += len(problems)
            print(f"{struct_name}: {len(problems)} incomplete literal(s)")
            for problem in problems:
                print(f"  {problem}")
        else:
            print(f"{struct_name}: ok")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
