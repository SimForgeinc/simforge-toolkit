from __future__ import annotations

import math
import struct

import numpy as np
import pytest

from simforge_rl import wire
from simforge_rl.wire import ProtocolError


def test_array_round_trips_little_endian_bytes() -> None:
    values = np.array([[1.5, -2.0], [3.25, 4.0]], dtype="<f4")
    tail = b"\x00" * 8 + values.tobytes()
    out = wire.array(tail, {"offset": 8, "length": 16, "dtype": "<f4", "shape": [2, 2]})
    assert out.dtype == np.dtype("<f4")
    np.testing.assert_array_equal(out, values)
    out[0, 0] = 99.0
    assert wire.array(tail, {"offset": 8, "length": 16, "dtype": "<f4", "shape": [2, 2]})[0, 0] == 1.5


@pytest.mark.parametrize(
    "ref",
    [
        {"offset": 8, "length": 16, "dtype": "<f8", "shape": [2]},
        {"offset": 0, "length": 32, "dtype": "<f8", "shape": [2]},
        {"offset": 0, "length": 8, "dtype": "<i8", "shape": [1]},
        {"offset": -8, "length": 8, "dtype": "<f8", "shape": [1]},
        {"offset": 0, "length": 8, "dtype": "<f8"},
        {"offset": 4, "length": 4, "dtype": "<f4", "shape": [1]},
        {"offset": 0.0, "length": 8, "dtype": "<f8", "shape": [1]},
        {"offset": 0, "length": 8.9, "dtype": "<f8", "shape": [1]},
        {"offset": 0, "length": 8, "dtype": "<f8", "shape": [True]},
        {"offset": 0, "length": 8, "dtype": ">f8", "shape": [1]},
    ],
)
def test_array_rejects_descriptors_outside_the_contract(ref: dict) -> None:
    with pytest.raises(ProtocolError):
        wire.array(b"\x00" * 16, ref)


def test_decode_rejects_header_longer_than_message() -> None:
    with pytest.raises(ProtocolError):
        wire.decode_body(struct.pack("<I", 100) + b"{}")
    with pytest.raises(ProtocolError):
        wire.decode_body(struct.pack("<I", 2) + b"[]")


def test_encode_decode_round_trip_with_tail() -> None:
    message = wire.encode({"op": "step", "i": 3}, b"\x01\x02")
    (total,) = struct.unpack("<I", message[:4])
    assert total == len(message) - 4
    header, tail = wire.decode_body(message[4:])
    assert header == {"op": "step", "i": 3}
    assert tail == b"\x01\x02"


def test_action_row_marks_unset_and_refuses_infinity() -> None:
    assert wire.action_row([1.0, math.nan, None]) == [1.0, None, None]
    assert wire.action_row(None) is None
    with pytest.raises(ValueError):
        wire.action_row([math.inf])
