# Terminal UI

Read this before changing `src/tui/`, terminal lifecycle, transcript rendering,
composer behavior, completion, popups, or shortcuts.

`README.md` records current operator-visible behavior. The source and rendered
tests remain authoritative when prose has drifted.

Test terminal changes through rendered output or terminal behavior, not only
the data used to produce it.

`preserve/chatbox-and-statusline.md` and `preserve/assets/` record the accepted
legacy terminal design and canonical visual references. Use them when the task
touches that design. They are reference material, not an instruction to restore
the old Pi implementation wholesale.
