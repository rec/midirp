use pyo3::prelude::*;

/// Python bindings for midir. Importing this module does not open a MIDI client.
#[pymodule]
fn midi(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
