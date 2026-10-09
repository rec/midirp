use pyo3::prelude::*;

mod errors;
mod ignore;
mod input;
mod output;
mod state;

mod callback;
mod lifecycle;

/// Python bindings for midir. Importing this module does not open a MIDI client.
#[pymodule]
fn midi(module: &Bound<'_, PyModule>) -> PyResult<()> {
    lifecycle::initialize(module.py())?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    let py = module.py();
    module.add("MidiError", py.get_type::<errors::MidiError>())?;
    module.add("InitError", py.get_type::<errors::InitError>())?;
    module.add("PortInfoError", py.get_type::<errors::PortInfoError>())?;
    module.add("ConnectError", py.get_type::<errors::ConnectError>())?;
    module.add("SendError", py.get_type::<errors::SendError>())?;
    module.add("StateError", py.get_type::<errors::StateError>())?;
    module.add(
        "CallbackThreadError",
        py.get_type::<errors::CallbackThreadError>(),
    )?;
    module.add(
        "NativePanicError",
        py.get_type::<errors::NativePanicError>(),
    )?;
    module.add("ResourceError", py.get_type::<errors::ResourceError>())?;
    module.add("WorkerError", py.get_type::<errors::WorkerError>())?;
    module.add(
        "WorkerTimeoutError",
        py.get_type::<errors::WorkerTimeoutError>(),
    )?;
    module.add_class::<ignore::Ignore>()?;
    module.add_class::<input::MidiInput>()?;
    module.add_class::<input::MidiInputPort>()?;
    module.add_class::<input::MidiInputConnection>()?;
    module.add_class::<output::MidiOutput>()?;
    module.add_class::<output::MidiOutputPort>()?;
    module.add_class::<output::MidiOutputConnection>()?;
    let native = PyModule::new(py, "midirp._native")?;
    for name in [
        "MidiError",
        "InitError",
        "PortInfoError",
        "ConnectError",
        "SendError",
        "StateError",
        "CallbackThreadError",
        "NativePanicError",
        "ResourceError",
        "WorkerError",
        "WorkerTimeoutError",
        "Ignore",
        "MidiInput",
        "MidiInputPort",
        "MidiInputConnection",
        "MidiOutput",
        "MidiOutputPort",
        "MidiOutputConnection",
    ] {
        native.add(name, module.getattr(name)?)?;
    }
    native.add_class::<callback::CallbackBridge>()?;
    native.add_function(wrap_pyfunction!(callback::check_thread, &native)?)?;
    PyModule::import(py, "sys")?
        .getattr("modules")?
        .set_item("midirp._native", native)?;
    let clients = PyModule::import(py, "midirp._clients")?;
    for name in [
        "MidiInput",
        "MidiInputPort",
        "MidiInputConnection",
        "MidiOutput",
        "MidiOutputPort",
        "MidiOutputConnection",
    ] {
        let class = clients.getattr(name)?;
        class.setattr("__module__", "midirp.midi")?;
        module.add(name, class)?;
    }
    Ok(())
}
