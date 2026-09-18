"""Gateway: the daemon (`groow start`) and the protocol any UI speaks to it.

    protocol  event/command shapes, newline-delimited JSON over a Unix socket
    daemon    Daemon: owns the App and the Mind, broadcasts events, accepts commands
    client    Client: connect, send, iterate events (line client and Textual UI use it)
"""
from .protocol import event, encode, decode, MOODS
from .client import Client, default_socket
from .daemon import Daemon

__all__ = ["event", "encode", "decode", "MOODS", "Client", "default_socket", "Daemon"]
