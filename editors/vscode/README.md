# HolyC syntax highlighting

A VS Code extension providing syntax highlighting for [HolyC](../../README.md)
source files (`.hc`), the language implemented by **solomon**.

The grammar is a standard TextMate grammar
([`syntaxes/holyc.tmLanguage.json`](syntaxes/holyc.tmLanguage.json),
scope `source.holyc`), so it is reusable by any TextMate-compatible editor
(Sublime Text, Atom, the `vscode-textmate` library, `bat`/`delta`, etc.) — not
just VS Code.

## What it highlights

The token classes are kept in sync with solomon's lexer
(`src/token.rs`) and builtin registry (`src/builtins.rs`):

- **Types** — `U0 I8 U8 I16 U16 I32 U32 I64 U64 F64 Bool`
- **Control flow** — `if else while do for switch case default break continue
  return goto try catch throw start end`
- **Declarations / other keywords** — `class union typedef public extern import
  reg noreg lastclass sizeof offset no_warn asm`
- **Language constants** — `NULL TRUE FALSE`
- **Builtin functions** — `Print`, `StrPrint`, `MAlloc`, `Sqrt`, … (the full
  `builtins.rs` set)
- **Literals** — decimal / `0x` hex / `0b` binary integers, `F64` floats,
  `"strings"` and `'char'` constants with escapes
- **Comments** — `//` line and `/* */` block
- **Preprocessor** — `#include "file"`, `#define`, `#undef`, `#ifdef`,
  `#ifndef`, `#else`, `#endif`
- Operators and function-call names

## Install (local / dev)

The fastest way is a symlink into your VS Code extensions folder:

```sh
ln -s "$(pwd)/editors/vscode" ~/.vscode/extensions/solomon-holyc-0.1.0
```

Then reload VS Code (Cmd/Ctrl-Shift-P → *Developer: Reload Window*) and open any
`.hc` file.

## Package as a `.vsix`

```sh
npm install -g @vscode/vsce
cd editors/vscode
vsce package          # produces holyc-0.1.0.vsix
code --install-extension solomon-holyc-0.1.0.vsix
```

## Tweaking colors

The grammar emits conventional TextMate scopes (`storage.type`,
`keyword.control`, `support.function.builtin`, `constant.numeric.*`, …). To
recolor a class without editing the grammar, add a
`editor.tokenColorCustomizations` block to your VS Code `settings.json` keyed on
those scopes.
