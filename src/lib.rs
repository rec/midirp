use pyo3::prelude::*;

mod errors;
mod input;
mod output;
mod state;

/// Python bindings for midir. Importing this module does not open a MIDI client.
#[pymodule]
fn midi(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    let py = module.py();
    module.add("MidiError", py.get_type::<errors::MidiError>())?;
    module.add("InitError", py.get_type::<errors::InitError>())?;
    module.add("PortInfoError", py.get_type::<errors::PortInfoError>())?;
    module.add("ConnectError", py.get_type::<errors::ConnectError>())?;
    module.add("SendError", py.get_type::<errors::SendError>())?;
    module.add_class::<input::MidiInput>()?;
    module.add_class::<input::MidiInputPort>()?;
    module.add_class::<output::MidiOutput>()?;
    module.add_class::<output::MidiOutputPort>()?;
    module.add_class::<output::MidiOutputConnection>()?;
    Ok(())
}
