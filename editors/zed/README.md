# HolyC support for Zed

A [Zed](https://zed.dev) extension giving HolyC (`.hc`) files — the language
implemented by **solomon** — syntax highlighting, bracket matching, smart
indentation, and an outline.

Unlike the VS Code extension (which is a TextMate grammar), Zed is built on
**Tree-sitter**, so this directory ships a real Tree-sitter grammar plus
`.scm` query files.

## Layout

```
editors/zed/
  extension.toml                  # Zed extension manifest
  grammars/holyc/                 # the Tree-sitter grammar
    grammar.js                    #   the grammar definition (source of truth)
    src/parser.c                  #   generated parser (Zed compiles this)
    tree-sitter.json
  languages/holyc/
    config.toml                   # language config (comments, brackets, …)
    highlights.scm                # syntax highlighting queries
    brackets.scm                  # bracket matching
    indents.scm                   # auto-indentation
    outline.scm                   # symbol outline / breadcrumbs
```

The grammar is kept in sync with solomon's front end: types and keywords mirror
`src/token.rs`, builtin functions (`Print`, `MAlloc`, …) mirror `src/builtins.rs`,
and the statement/expression set tracks `src/parser.rs`. All 18 `examples/*.hc`
programs parse with **zero** errors.

## Install as a dev extension

Zed compiles the grammar from git at the revision named in `extension.toml`, so
the grammar must be pushed first:

1. Commit and push `editors/zed/grammars/holyc/` (including the generated
   `src/parser.c`).
2. Edit [`extension.toml`](extension.toml) and set `[grammars.holyc].rev` to that
   commit's SHA.
3. In Zed: `cmd-shift-p` → **zed: install dev extension** → choose this
   `editors/zed/` directory.
4. Open any `.hc` file.

After changing the grammar, run `tree-sitter generate` in `grammars/holyc/` to
refresh `src/parser.c`, commit, bump `rev`, and reload the extension in Zed
(**zed: reload extensions**).

## Working on the grammar

```sh
cd editors/zed/grammars/holyc
npm install                       # pulls tree-sitter-cli (dev only)
npx tree-sitter generate          # regenerate src/parser.c from grammar.js
npx tree-sitter parse ../../../../examples/classes.hc   # inspect a parse tree

# Check the whole corpus parses cleanly:
for f in ../../../../examples/*.hc; do
  npx tree-sitter parse "$f" | grep -q ERROR && echo "ERROR in $f"
done

# Exercise the highlight queries:
npx tree-sitter query ../../languages/holyc/highlights.scm ../../../../examples/json.hc
```
