"""Gateway: the daemon (`groow start`) and the protocol any UI speaks to it.

    protocol  event shapes; HTTP routes, SSE framing, WebSocket messages
    daemon    Daemon: owns the App and the Mind, serves HTTP/SSE/WS, fans events out
    client    Client: hello/status/ask/say, SSE and WebSocket streams
"""
from .protocol import event, encode, decode, sse, MOODS
from .client import Client, base_url
from .daemon import Daemon

__all__ = ["event", "encode", "decode", "sse", "MOODS", "Client", "base_url", "Daemon"]
