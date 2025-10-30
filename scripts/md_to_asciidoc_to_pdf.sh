#!/usr/bin/env bash
# md2adoc.sh — clean numbered headers, convert to AsciiDoc, build PDF (TOC depth 2)
# Usage: ./md2adoc.sh "Document Title" /path/to/src [/path/to/dest]
# Defaults: dest=./_asciidoc

set -euo pipefail

TITLE="${1:-WhisperCMS Architecture}"
SRC="${2:-../docs/arc42}"
DST="${3:-../docs/.asciidoc}"

# 1) Mirror the directory structure (folders only)
rsync -a -f"+ */" -f"- *" "$SRC"/ "$DST"/

# 2) Copy .md files and sanitize ALL headings (#..######), also remove U+2028
find "$SRC" -type f -name "*.md" \
  -not -path "*/.git/*" \
  -not -path "*/node_modules/*" \
  -not -path "*/vendor/*" \
  -print0 |
while IFS= read -r -d '' f; do
  rel="${f#"$SRC"/}"
  out="$DST/$rel"
  mkdir -p "$(dirname "$out")"

  # Strip numeric prefixes from headings like "## 01.2- A Title" → "## A Title"
  perl -CSD -0777 -pe 's/^\s*(#{1,6}\s+)(?:\d+(?:[.\-:_]\d+)*)(?:[.)\-\:_]*)\s+/\1/gm; s/\x{2028}/\n/g' \
    "$f" > "$out"
done

# 3) Convert Markdown → AsciiDoc (preserve tables, no line wrapping)
find "$DST" -type f -name "*.md" -print0 |
while IFS= read -r -d '' f; do
  pandoc "$f" -f gfm -t asciidoc --wrap=none --columns=200 -o "${f%.md}.adoc"
done

# 4) Build master.adoc with TOC depth 2, blank lines between includes
MASTER="$DST/master.adoc"
{
  echo "= $TITLE"
  echo ":doctype: book"
  echo ":toc: left"
  echo ":toclevels: 2"
  echo ":sectnums:"
  echo

  # Include all .adoc (except master.adoc), natural sort; add a blank line after each include
  find "$DST" -type f -name "*.adoc" ! -name "master.adoc" -print0 \
    | sort -z -V \
    | while IFS= read -r -d '' ad; do
        rel_ad="${ad#"$DST/"}"
        printf 'include::%s[]\n\n' "$rel_ad"
      done
} > "$MASTER"

# 5) Generate the final PDF
asciidoctor-pdf -a toc=left -a toclevels=2 -a sectnums "$MASTER" -o "$SRC/../$TITLE.pdf"

echo "✅ Done. PDF: $SRC/../$TITLE.pdf"
