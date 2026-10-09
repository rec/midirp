"""Exercise unsupported-build rejection in a fresh interpreter, without MIDI."""

import sys
from unittest.mock import patch


def main() -> None:
    with (
        patch("sysconfig.get_config_var", return_value=1),
        patch("atexit.register", side_effect=AssertionError("cleanup was initialized")),
    ):
        try:
            from midirp import midi
        except RuntimeError as error:
            assert "free-threaded builds are unsupported" in str(error)
        else:
            raise AssertionError("free-threaded build was accepted")
        assert "midirp._native" not in sys.modules

    # Rejection leaves no initialized runtime or private module behind.
    from midirp import midi

    assert midi.__version__


if __name__ == "__main__":
    main()
