"""Opt-in WinMM checks that do not connect to a hardware port."""

import sys

import pytest
from midirp import midi


def test_windows_virtual_creation_preserves_available_client_state() -> None:
    if sys.platform != "win32":
        pytest.skip("WinMM unsupported-path check requires Windows")
    source = midi.MidiInput("midirp Windows input")
    target = midi.MidiOutput("midirp Windows output")
    source.ignore(midi.Ignore.SYSEX)
    before_input = source.ports()
    before_output = target.ports()
    with pytest.raises(NotImplementedError):
        source.create_virtual("unsupported", receive)
    with pytest.raises(NotImplementedError):
        target.create_virtual("unsupported")
    assert source.ports() == before_input
    assert target.ports() == before_output
    source.ignore(midi.Ignore.NONE)


def receive(timestamp: int, message: bytes) -> None:
    raise AssertionError("unsupported virtual input dispatched a callback")
