#!/usr/bin/env python3
"""Build, bundle and exercise a release with a real display/GPU (or Xvfb + Vulkan).

Default: builtin WGSL/no SDK, so the copied bundle must run independently of
vendor/. Pass --slang to exercise a Slang-enabled build as a separate check.
Artifacts and logs are retained in the printed temporary directory on failure.
"""

import argparse
import json
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import tempfile
import time
import zlib


def run(command, cwd, timeout=1800):
    print("+", " ".join(map(str, command)), flush=True)
    subprocess.run(command, cwd=cwd, check=True, timeout=timeout)


def command(socket_path, cmd, **params):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(15)
        stream.connect(str(socket_path))
        stream.sendall((json.dumps({"cmd": cmd, **params}) + "\n").encode())
        with stream.makefile("rb") as reader:
            result = json.loads(reader.readline(16 * 1024 * 1024))
    if result.get("status") != "ok":
        raise RuntimeError(f"{cmd}: {result}")
    return result.get("data") or {}


def verify_png(path):
    """Decode the engine's RGB PNG without third-party Python packages."""
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise AssertionError("capture is not a PNG")
    offset, compressed, dimensions = 8, bytearray(), None
    while offset < len(data):
        size = struct.unpack_from(">I", data, offset)[0]
        kind = data[offset + 4:offset + 8]
        payload = data[offset + 8:offset + 8 + size]
        crc = struct.unpack_from(">I", data, offset + 8 + size)[0]
        if zlib.crc32(kind + payload) & 0xFFFFFFFF != crc:
            raise AssertionError("corrupt PNG chunk")
        if kind == b"IHDR":
            width, height, depth, color, _, _, interlace = struct.unpack(">IIBBBBB", payload)
            if min(width, height) < 64 or depth != 8 or color != 2 or interlace:
                raise AssertionError("unexpected capture format or size")
            dimensions = width, height
        elif kind == b"IDAT":
            compressed.extend(payload)
        offset += size + 12
        if kind == b"IEND":
            break
    if dimensions is None:
        raise AssertionError("PNG has no dimensions")
    width, height = dimensions
    raw = zlib.decompress(compressed)
    stride, previous, pixels, cursor = width * 3, bytearray(width * 3), set(), 0
    if len(raw) != height * (stride + 1):
        raise AssertionError("PNG scanline length is incorrect")
    for _ in range(height):
        mode = raw[cursor]
        cursor += 1
        row = bytearray(raw[cursor:cursor + stride])
        cursor += stride
        for i in range(stride):
            left, above = (row[i - 3] if i >= 3 else 0), previous[i]
            upper_left = previous[i - 3] if i >= 3 else 0
            if mode == 1:
                prediction = left
            elif mode == 2:
                prediction = above
            elif mode == 3:
                prediction = (left + above) // 2
            elif mode == 4:
                p = left + above - upper_left
                candidates = (left, above, upper_left)
                prediction = min(candidates, key=lambda value: abs(p - value))
            elif mode == 0:
                prediction = 0
            else:
                raise AssertionError("unknown PNG scanline filter")
            row[i] = (row[i] + prediction) & 255
        pixels.update(bytes(row[i:i + 3]) for i in range(0, stride, 3))
        previous = row
    # A valid image of a clear/background only is not a rendering success.
    if len(pixels) < 8:
        raise AssertionError(f"capture has only {len(pixels)} colors; scene may be blank")
    print(f"Captured {width}x{height} PNG with {len(pixels)} colors", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--no-build", action="store_true", help="use existing release binaries")
    parser.add_argument("--slang", action="store_true", help="build with Slang instead of builtin WGSL")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    if not args.no_build:
        build = ["cargo", "build", "--release", "--locked", "-p", "naive-runtime", "--bins"]
        if not args.slang:
            build.append("--no-default-features")
        run(build, repo)
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=repo))
    release = Path(metadata["target_directory"]) / "release"
    temporary = Path(tempfile.mkdtemp(prefix="naive-release-"))
    print(f"Release smoke artifacts: {temporary}", flush=True)
    project = temporary / "project"
    shutil.copytree(repo / "scripts" / "release-smoke-fixture", project)
    run([release / "naive", "build"], project)
    bundles = list((project / "dist").iterdir())
    if len(bundles) != 1:
        raise AssertionError("expected exactly one release bundle")
    # Move the bundle out of its source project to detect accidental relative
    # references to unpackaged project content.
    bundle = temporary / "standalone"
    shutil.move(str(bundles[0]), bundle)
    binary = bundle / "naive-runtime"
    result = subprocess.run([binary, "test"], cwd=bundle, capture_output=True,
                            text=True, timeout=60, check=True)
    print(result.stdout, end="", flush=True)
    if "1 passed, 0 failed" not in result.stdout:
        raise AssertionError("packaged Lua test did not execute")
    private = temporary / "control"
    private.mkdir(mode=0o700)
    socket_path = private / "runtime.sock"
    log_path = temporary / "runtime.log"
    with log_path.open("w") as log:
        process = subprocess.Popen(
            [bundle / "launch.sh", "--socket", socket_path],
            cwd=bundle, stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 60
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError(f"runtime exited with {process.returncode}; see {log_path}")
                try:
                    entities = command(socket_path, "list_entities")["entities"]
                    break
                except (OSError, KeyError, RuntimeError):
                    time.sleep(0.1)
            else:
                raise TimeoutError(f"runtime did not become ready; see {log_path}")
            assert "smoke_cube" in {entity["id"] for entity in entities}
            # Exercise MCP stdio -> typed request -> secured socket -> Lua.
            request = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "naive_run_lua", "arguments": {"code":
                    'assert(entity.exists("smoke_cube")); '
                    'entity.set_position("smoke_cube", 0, 1, 0); return "release-ok"'}}}
            bridge = subprocess.run([release / "naive_mcp", socket_path],
                input=json.dumps(request) + "\n", cwd=bundle,
                capture_output=True, text=True, timeout=30, check=True)
            response = json.loads(bridge.stdout)
            assert not response["result"].get("isError"), response
            returned = json.loads(response["result"]["content"][0]["text"])
            assert returned["data"]["result"] == "release-ok", returned
            command(socket_path, "capture_frame", path="captures/release.png")
            verify_png(bundle / "captures" / "release.png")
            # Save confinement must apply in the actual packaged engine too.
            with socket.socket(socket.AF_UNIX) as stream:
                stream.settimeout(10)
                stream.connect(str(socket_path))
                stream.sendall(b'{"cmd":"save_scene","path":"../escaped.yaml"}\n')
                with stream.makefile("rb") as reader:
                    refused = json.loads(reader.readline())
            assert refused["status"] == "error", refused
            assert not (temporary / "escaped.yaml").exists()
        except Exception:
            # CI runners discard their temporary directories. Include the
            # actual engine error rather than only a path to an unavailable log.
            log.flush()
            print("Packaged runtime log:", file=sys.stderr, flush=True)
            print(log_path.read_text(errors="replace")[-64 * 1024:],
                  file=sys.stderr, flush=True)
            raise
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
    print(f"Release smoke passed. Artifacts: {temporary}")


if __name__ == "__main__":
    main()
