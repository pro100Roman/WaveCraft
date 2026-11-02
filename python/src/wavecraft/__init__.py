"""First-party Python client for WaveCraft automation."""

from .client import (
    WaveCraftClient,
    WaveCraftError,
    WaveCraftProcessError,
    WaveCraftProtocolError,
)

__all__ = [
    "WaveCraftClient",
    "WaveCraftError",
    "WaveCraftProcessError",
    "WaveCraftProtocolError",
]
