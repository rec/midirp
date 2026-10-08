use pyo3::create_exception;
use pyo3::exceptions::PyException;

create_exception!(
    midirp.midi,
    MidiError,
    PyException,
    "A native MIDI operation failed."
);
create_exception!(
    midirp.midi,
    InitError,
    MidiError,
    "The MIDI backend could not initialize."
);
create_exception!(
    midirp.midi,
    PortInfoError,
    MidiError,
    "Port information could not be retrieved."
);
create_exception!(
    midirp.midi,
    ConnectError,
    MidiError,
    "A MIDI connection could not be opened."
);
create_exception!(
    midirp.midi,
    SendError,
    MidiError,
    "A MIDI message could not be sent."
);
