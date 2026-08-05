Search and inspect live internet sources.

Use this tool when the user asks to browse (and do not use it when they ask not
to), when information may have changed (>10% chance) or is uncertain, for
recommendations involving substantial time or money or for high-stakes
accuracy, when exact links or quotations are needed, or when a referenced page,
paper, dataset, PDF, or site was not provided. For news, compare publication
dates with event dates.

For OpenAI-product questions, inspect local code first and otherwise use only
official OpenAI sites unless the user requests different sources. For technical
questions, use primary sources such as official documentation and research
papers. Prefer authoritative sources generally and label inferences.

Batch independent operations in one call. Omit unused fields and nulls.
`search_query` accepts at most four queries; more than three requires
`response_length` of `medium` or `long`.

Cite web-supported claims with direct, descriptive Markdown links placed next
to the claim. Do not cite search-result pages, use bare URLs as citations, or
expose internal result IDs such as `turn2search5`. Every citation must directly
support its claim; use multiple source domains when useful.

Respect each source's `[wordlim N]`. Quote at most 25 words from each non-lyrical
source and at most 10 words of song lyrics. Reddit may be quoted at greater
length only in a linked Markdown blockquote. Do not reproduce full works or
long passages; otherwise summarize or paraphrase.
