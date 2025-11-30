#!/usr/bin/env python3
"""
Concat all Rust source files under a given directory into source.txt,
using separators like:

    // <dirname>/relative/path.rs

Removes unit tests by omitting everything from the first occurrence of
'#[cfg(test)]' to the end of the file.
"""

import argparse
from pathlib import Path


def strip_tests(file_path: Path) -> str:
    """Return file contents up to (but not including) #[cfg(test)]."""
    lines = []
    with file_path.open("r", encoding="utf-8") as f:
        for line in f:
            if line.strip().startswith("#[cfg(test)]"):
                break
            lines.append(line)
    return "".join(lines)


def concat_rust_sources(root: Path) -> None:
    root = root.resolve()
    dirname = root.name
    out_file = root / "source.txt"

    # Find and sort all .rs files
    rs_files = sorted(
        (p for p in root.rglob("*.rs") if p.is_file()),
        key=lambda p: p.relative_to(root).as_posix(),
    )

    with out_file.open("w", encoding="utf-8") as out:
        for path in rs_files:
            rel = path.relative_to(root).as_posix()
            display_path = f"{dirname}/{rel}"

            # Write separator header
            out.write("// =============================================\n")
            out.write(f"// {display_path}\n")
            out.write("// =============================================\n\n")

            # Write file content without tests
            contents = strip_tests(path)
            out.write(contents)

            out.write("\n\n")

    print(f"Done. Processed {len(rs_files)} files → {out_file}")


def main():
    parser = argparse.ArgumentParser(description="Concatenate Rust sources")
    parser.add_argument("directory", help="Directory to scan")

    args = parser.parse_args()
    root = Path(args.directory)

    if not root.exists() or not root.is_dir():
        raise SystemExit(f"Error: {root} is not a valid directory")

    concat_rust_sources(root)


if __name__ == "__main__":
    main()
