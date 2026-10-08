import operator

import pytest
from midirp import midi


@pytest.mark.parametrize(
    "bits, flags",
    [
        (0, midi.Ignore.NONE),
        (1, midi.Ignore.SYSEX),
        (2, midi.Ignore.TIME),
        (3, midi.Ignore.SYSEX | midi.Ignore.TIME),
        (4, midi.Ignore.ACTIVE_SENSE),
        (5, midi.Ignore.SYSEX | midi.Ignore.ACTIVE_SENSE),
        (6, midi.Ignore.TIME | midi.Ignore.ACTIVE_SENSE),
        (7, midi.Ignore.ALL),
    ],
)
def test_filter_masks_preserve_upstream_bits(bits: int, flags: midi.Ignore) -> None:
    assert int(flags) == bits
    assert midi.Ignore(bits) == flags


@pytest.mark.parametrize("bits", [-1, 8, 255, 256, 2**100])
def test_unknown_filter_bits_raise_value_error(bits: int) -> None:
    with pytest.raises(ValueError, match="Unknown MIDI ignore bits"):
        midi.Ignore(bits)


@pytest.mark.parametrize("bits", [None, "1", 1.0, b"1"])
def test_filter_masks_reject_non_integer_arguments(bits: object) -> None:
    with pytest.raises(TypeError):
        midi.Ignore(bits)  # ty: ignore[invalid-argument-type]


def test_combining_filters_does_not_mutate_the_operands() -> None:
    sysex = midi.Ignore.SYSEX
    combined = sysex | midi.Ignore.TIME | midi.Ignore.ACTIVE_SENSE
    assert combined == midi.Ignore.ALL
    assert sysex == midi.Ignore(1)
    assert combined | midi.Ignore.NONE == combined


@pytest.mark.parametrize("other", [1, None, "TIME"])
def test_filter_or_requires_another_filter_mask(other: object) -> None:
    with pytest.raises(TypeError):
        operator.or_(midi.Ignore.TIME, other)
