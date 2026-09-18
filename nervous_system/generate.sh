#!/usr/bin/env bash
# Generate the Python side from the schema. Rust generates itself at build time; Python has no
# build step, so its modules are generated here and committed.
#
#   ./nervous_system/generate.sh
set -euo pipefail
cd "$(dirname "$0")/.."
command -v protoc >/dev/null || { echo "protoc is needed to regenerate the protocol"; exit 1; }

out=groow/proto
mkdir -p "$out"
protoc --proto_path=nervous_system/proto --python_out="$out" nervous_system/proto/*.proto

# protoc writes flat imports between generated modules, which only resolve if their directory
# is on the path. They live inside a package here, so the imports are made relative.
for f in "$out"/*_pb2.py; do
  sed -i -E 's/^import ([a-z_]+)_pb2 as /from . import \1_pb2 as /' "$f"
done
cat > "$out/__init__.py" <<'PY'
"""The protocol, generated from nervous_system/proto.

Nothing in here is written by hand. To change a message, change the schema and run
`./nervous_system/generate.sh`; the Rust side regenerates itself when it builds.

The encoding is canonical protobuf JSON, which both languages produce identically. Use
`to_json` and `from_json` rather than `json.dumps` on these, so field names and enum spellings
come from one place.
"""
from google.protobuf import json_format

from . import brain_pb2 as brain
from . import records_pb2 as records
from . import turn_pb2 as turn
from . import wire_pb2 as wire


def to_json(message) -> str:
    """One line, field names as the schema writes them."""
    return json_format.MessageToJson(
        message, preserving_proto_field_name=True, indent=0).replace("\n", "")


def to_dict(message, full: bool = False) -> dict:
    """The message as a dictionary.

    `full` keeps fields that are at their default. Protobuf leaves those out, which is right on
    a wire and wrong in a status report, where a person reading it wants to see that the queue
    is zero rather than wonder where the field went.
    """
    return json_format.MessageToDict(
        message, preserving_proto_field_name=True, always_print_fields_with_no_presence=full)


def from_json(text: str, message):
    """Parse into `message`, ignoring fields it does not know.

    Unknown fields are ignored on purpose: a record written by a newer version must still be
    readable by an older one.
    """
    return json_format.Parse(text, message, ignore_unknown_fields=True)


def from_dict(d: dict, message):
    return json_format.ParseDict(d, message, ignore_unknown_fields=True)


__all__ = ["wire", "turn", "brain", "records", "to_json", "to_dict", "from_json", "from_dict"]
PY
echo "generated $(ls "$out"/*_pb2.py | wc -l) modules into $out"
