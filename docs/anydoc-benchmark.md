# AnyDoc evaluation

BetterCodex bundles AnyDoc as a proactive first-pass document reader because it won the
end-to-end extraction workload described here. It does not replace visual inspection.

## Setup

The comparison ran on 2026-08-05 on Linux 6.12, an AMD Ryzen 7 5700X, and 8 GiB RAM.
Both agents used `gpt-5.6-sol` at `max` reasoning effort:

- native Codex CLI 0.146.0; and
- BetterCodex commit `dbe64dd03c909bd7254b9cee8f1236567c204951` with the unmodified
  [upstream AnyDoc skill](https://github.com/firecrawl/anydoc/blob/cad7ef2dd6130f59e1c2ff2bdc10e23099d7cd4b/skills/convert-documents-to-markdown/SKILL.md).

The 13 inputs came from AnyDoc commit
[`cad7ef2`](https://github.com/firecrawl/anydoc/tree/cad7ef2dd6130f59e1c2ff2bdc10e23099d7cd4b/tests/fixtures)
and covered DOC, DOCX, PPT, PPTX, XLS, XLSX, ODT, ODS, ODP, RTF, EPUB, CSV, and a
text PDF. Each agent had to create one Markdown file per input and answer 17 exact facts
covering Unicode text, numbering, speaker notes, spreadsheet values and durations, merged
tables, links, and PDF text.

The native run measured the actual out-of-box host path. No PDF or office-document skill was
installed; `pandoc`, LibreOffice, Poppler, and `unzip` were absent. Codex could use existing
commands and the Python standard library but could not download or install a converter. The
BetterCodex run discovered the AnyDoc skill implicitly; npm resolved `@firecrawl/anydoc`
0.1.3. Wall time includes the complete agent turn and validation. This was one controlled
agent run per path, not a statistical converter-quality study, and using upstream fixtures can
favor AnyDoc.

## End-to-end result

| Measure | Native Codex | BetterCodex + AnyDoc | Difference |
| --- | ---: | ---: | ---: |
| Non-empty Markdown outputs | 13/13 | 13/13 | tie |
| Exact fact answers | 15/17 | 17/17 | AnyDoc +2 |
| Semantically correct facts | 16/17 | 17/17 | AnyDoc +1 |
| Wall time | 719.031 s | 136.965 s | AnyDoc 5.25x faster |
| Cumulative input tokens | 1,438,181 | 173,596 | AnyDoc 8.28x lower |
| Uncached input tokens | 118,245 | 44,572 | AnyDoc 2.65x lower |
| Output tokens | 31,213 | 5,843 | AnyDoc 5.34x lower |
| Reasoning output tokens | 16,972 | 2,513 | AnyDoc 6.75x lower |

Native Codex issued 25 shell commands and implemented ZIP/XML, OLE/CFB, BIFF, RTF, and PDF
parsing during the turn. Its two exact mismatches were harmless punctuation around an ODT list
number and, materially, the source ISO duration `-PT1H5M` instead of the displayed ODS value
`-1:05:00`. BetterCodex used the converter output and returned every expected value.

On these small fixtures, a cold pinned `npx` conversion took 1.453 s including package fetch.
Warm separate CLI calls took 304–399 ms per document and 4.227 s for all 13. Resolving the npm
package once and running the 13 CLI conversions in one shell took 582 ms, which is why the
bundled skill includes a batch path. Seven warm in-process Node conversions per file produced a
0.112 ms median of per-document medians; the PDF was 2.670 ms. These core timings exclude Node
startup and are not representative of large real-world documents.

## Fidelity comparison

AnyDoc won content retrieval, latency, and model cost, but native Codex's bespoke reconstruction
was often more layout-faithful after spending much more time and context:

- Native output preserved merged spreadsheet/RTF/EPUB cells with HTML spans; AnyDoc flattened
  them into rectangular GFM tables.
- Native output reconstructed ODT and EPUB list semantics. AnyDoc preserved the text and source
  numbers but represented some non-decimal or restarted lists as bullets containing numbers.
- Native XLS output retained percentage, currency, and thousands display formats. AnyDoc emitted
  the underlying numeric values. Conversely, AnyDoc correctly rendered ODS durations that the
  native path left as ISO source values.
- The strongest native advantage was PDF. It reconstructed lists, a merged table, links,
  footnotes, and Unicode joiners. AnyDoc retained the queried numeric text but flattened the
  table and list structure, dropped links and joiners, and changed one Persian run's character
  order.
- AnyDoc does not OCR scanned or image-only PDFs.

The product decision is therefore narrow: invoke AnyDoc proactively to get local document
content into bounded Markdown quickly, then verify through a rendered or otherwise visual path
whenever layout, exact display formatting, charts, embedded media, PDF reading order, or Unicode
shaping can affect the answer. Never silently upload a document to a hosted OCR service.
