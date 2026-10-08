use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyInt;

/// The upstream input filters, including all eight valid combinations.
#[pyclass(frozen, eq, module = "midirp.midi")]
#[derive(PartialEq, Eq)]
pub struct Ignore {
    pub native: midir::Ignore,
}

#[pymethods]
impl Ignore {
    #[new]
    fn new(bits: &Bound<'_, PyInt>) -> PyResult<Self> {
        let bits = bits
            .extract::<u8>()
            .map_err(|_| PyValueError::new_err("Unknown MIDI ignore bits"))?;
        if bits & !(midir::Ignore::All as u8) != 0 {
            return Err(PyValueError::new_err("Unknown MIDI ignore bits"));
        }
        let mut native = midir::Ignore::None;
        for flag in [
            midir::Ignore::Sysex,
            midir::Ignore::Time,
            midir::Ignore::ActiveSense,
        ] {
            if bits & flag as u8 != 0 {
                native = native | flag;
            }
        }
        Ok(Self { native })
    }

    fn __or__(&self, other: &Self) -> Self {
        Self {
            native: self.native | other.native,
        }
    }

    fn __int__(&self) -> u8 {
        self.native as u8
    }

    fn __repr__(&self) -> String {
        format!("Ignore({})", self.native as u8)
    }

    #[classattr]
    const NONE: Self = Self {
        native: midir::Ignore::None,
    };
    #[classattr]
    const SYSEX: Self = Self {
        native: midir::Ignore::Sysex,
    };
    #[classattr]
    const TIME: Self = Self {
        native: midir::Ignore::Time,
    };
    #[classattr]
    const ACTIVE_SENSE: Self = Self {
        native: midir::Ignore::ActiveSense,
    };
    #[classattr]
    const ALL: Self = Self {
        native: midir::Ignore::All,
    };
}
