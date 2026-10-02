# What was verified here

- `cargo test`: 155 tests passed (query inheritance, capture names, language detection, indent, textobjects, tags, folds, rainbow depth, grammar path helpers, rust highlighter reparse).
- `s0 --grammar list`: prints 303 grammar sources, none fetched until you ask.
- `s0 --grammar status rust`: reports Missing for the dynamic copy because rust is statically linked (static wins).
- 39 `folds.scm` files copied from Helix.

# What you should run

1. `cargo test`
2. Open `tests/test.rs` and confirm keywords, strings, and comments are colored. Press Enter inside a function and confirm the new line is indented. Press `=` on a mis-indented line.
3. In a Rust file, `:af` and `:if` should select the function. `za` should fold it.
4. With rust-analyzer on PATH, `gd`, `gD`, `gy`, `gi`, `gH`, `:ws`, and `:fmt-range` against a real project.
5. Install one extra grammar, not all of them:
   `s0 --grammar install kotlin`
   Needs `git` and `cc`/`gcc`/`clang`. Then open a `.kt` file and confirm highlighting.
6. `s0 --grammar remove kotlin` should delete that one library only.
