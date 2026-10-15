"""A stdio runtime child that answers ``command`` with the SIMFORGE_* names it inherited."""

import json
import os
import sys

for line in sys.stdin:
    request = json.loads(line)
    base = {"schema": "simforge.runtime/v1", "id": request["id"], "op": request["op"], "ok": True}
    if request["op"] == "handshake":
        result = {"runtimeProtocol": 1, "rendererProtocol": 5, "runtimeVersion": "probe", "engineSemver": "0", "operations": ["handshake", "command"], "contractSha256": "0" * 64}
    else:
        result = {"value": sorted(k for k in os.environ if k.startswith("SIMFORGE_")), "exitCode": 0}
    sys.stdout.write(json.dumps({**base, "result": result}) + "\n")
    sys.stdout.flush()
