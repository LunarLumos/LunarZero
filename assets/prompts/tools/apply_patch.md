Apply a multi-file patch in the `*** Begin Patch` / `*** End Patch` format:

*** Begin Patch
*** Update File: path/to/file
@@ optional context line
-removed line
+added line
*** Add File: path/to/new
+content
*** Delete File: path/to/old
*** End Patch

Context lines start with a space, removed with `-`, added with `+`; `*** Move to: new/path` after `Update File` renames. Every hunk must match the current file exactly.
