---
name: anydoc
description: Proactively read local Word, PowerPoint, Excel, OpenDocument, RTF, EPUB, CSV, and text-based PDF files by converting them to bounded Markdown with AnyDoc. Use immediately as the first-pass reader whenever a task needs content from .doc, .docx, .docm, .ppt, .pptx, .pptm, .xls, .xlsx, .xlsm, .xlsb, .odt, .ods, .odp, .rtf, .epub, .csv, or .pdf files. Do not treat it as sole evidence for scanned PDFs or work where visual layout, exact spreadsheet display formatting, or embedded media matters.
---

# Read documents with AnyDoc

Convert supported documents locally before analyzing their contents. AnyDoc does not upload the
document; the `npx` fallback downloads a pinned CLI package from npm on first use.

## Convert

Prefer an already-installed `anydoc` command. Otherwise use the pinned package, which requires
Node.js 20 or newer:

```bash
if command -v anydoc >/dev/null 2>&1; then
  anydoc input.docx -o output.md
else
  npx -y @firecrawl/anydoc@0.1.3 input.docx -o output.md
fi
```

Always write unknown or potentially large output to a file. Unless the user asked to keep the
conversion, use a temporary location, do not overwrite an existing file, and remove the temporary
output after the task. Check its size, then read only the parts needed. Shell-quote paths.

For several documents without a global `anydoc`, resolve the npm package once instead of invoking
`npx` separately for every file:

```bash
npx -y -p @firecrawl/anydoc@0.1.3 -- sh -c '
  set -eu
  output_dir=$1
  shift
  mkdir -p "$output_dir"
  for input in "$@"; do
    anydoc "$input" -o "$output_dir/$(basename "$input").md"
  done
' sh output-markdown input.docx slides.pptx workbook.xlsx
```

Supported inputs are `.doc`, `.docx`, `.docm`, `.odt`, `.rtf`, `.epub`, `.pdf`, `.ppt`, `.pps`,
`.pot`, `.pptx`, `.pptm`, `.ppsx`, `.ppsm`, `.odp`, `.xls`, `.xlsx`, `.xlsm`, `.xlsb`, `.ods`, and
`.csv`. Detection uses file content where possible. Use `--format csv` only for CSV from stdin or
`--format <name>` when neither content nor a trustworthy extension identifies the format.

## Validate and interpret

1. Require exit status 0 and a non-empty Markdown file before relying on the conversion.
2. Inspect the relevant Markdown and answer from extracted evidence; do not infer omitted content.
3. Treat Markdown as a fast content representation, not a visual rendering. Merged cells can be
   flattened, spreadsheet values can lose source display formats, and complex PDF text, links,
   tables, reading order, Unicode shaping, images, or typography can lose fidelity.
4. If layout, charts, images, exact cell formatting, or page appearance affects the answer, verify
   against rendered pages or another available visual path. State what remains unverified.
5. Scanned and image-only PDFs require OCR and are not supported. Never upload a document to a
   hosted OCR or parsing service unless the user explicitly requests that disclosure.

If neither `anydoc` nor Node.js 20+ with `npx` is available, say what is missing and continue with
available native tools. Do not install a different document stack just to conceal the limitation.
