# Python bindings for the midir Rust MIDI library

This project is being implemented according to [plan/plan.md](plan/plan.md).
It targets standard, GIL-enabled CPython 3.11 and later. Native MIDI operation
on each platform will be validated separately before claiming support.

## Development

The build uses midir 0.11.0, PyO3 0.29.3, maturin 1.15.0, and Rust 1.87 or
later. Cargo owns the package version; maturin uses it for Python metadata.
Python runtime dependencies are empty.

```sh
uv sync
uv run pytest
uv run maturin build --release --out dist
```

The canonical native module is `midirp.midi`. Importing it does not initialize
the MIDI backend. The package includes type stubs and a `py.typed` marker.

Native unit tests link to Python without the extension-module build setting:

```sh
PYO3_PYTHON="$PWD/.venv/bin/python" cargo test
PYO3_PYTHON="$PWD/.venv/bin/python" cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

maturin selects the extension-module build setting for wheel builds.
Linux source builds also require ALSA development libraries and `pkg-config`.
No MIDI clients or devices are opened by the unit tests.
