#!/usr/bin/env python3
"""Build, bundle and exercise a release with a real display/GPU (or Xvfb + Vulkan).

Default: use checked-in WGSL without the SDK. Pass --slang to require fresh
compilation of the fixture's custom pass. Either way the bundle ships
naive-player, built without Slang. --bundle verifies an existing release app
after copying it away from its source project.
Artifacts and logs are retained in the printed temporary directory on failure.
"""

import argparse
import json
import os
import queue
import plistlib
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import threading
import zlib


def run(command, cwd, timeout=1800):
    print("+", " ".join(map(str, command)), flush=True)
    subprocess.run(command, cwd=cwd, check=True, timeout=timeout)


def raw_command(socket_path, cmd, **params):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(15)
        stream.connect(str(socket_path))
        stream.sendall((json.dumps({"cmd": cmd, **params}) + "\n").encode())
        with stream.makefile("rb") as reader:
            result = json.loads(reader.readline(16 * 1024 * 1024))
    return result


def command(socket_path, cmd, **params):
    result = raw_command(socket_path, cmd, **params)
    if result.get("status") != "ok":
        raise RuntimeError(f"{cmd}: {result}")
    return result.get("data") or {}


def wait_until(predicate, description, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.02)
    raise AssertionError(description)


CONSTRUCTION_ENTITIES = ("initial_controller", "initial_trimesh")


def construction_counts(socket_path):
    return {identifier: command(socket_path, "query_events",
        filter=f"release.construction_checked.{identifier}")["count"]
        for identifier in CONSTRUCTION_ENTITIES}


def verify_construction(socket_path, expected_counts=None):
    """Real game hooks must observe the completed authored ECS and physics."""
    expected_counts = expected_counts or dict.fromkeys(CONSTRUCTION_ENTITIES, 1)
    wait_until(lambda: all(construction_counts(socket_path)[identifier] >= expected
                          for identifier, expected in expected_counts.items()),
               f"construction hooks did not finish: expected {expected_counts}")
    counts = construction_counts(socket_path)
    assert counts == expected_counts, (counts, expected_counts)
    player = command(socket_path, "query_entity", entity_id="initial_controller",
                     component="player")["player"]
    assert abs(player["height"] - 1.8) < 0.0001, player
    assert abs(player["radius"] - 0.3) < 0.0001, player
    light = command(socket_path, "query_entity", entity_id="initial_trimesh",
                    component="point_light")["point_light"]
    assert light["intensity"] == 3, light
    command(socket_path, "run_lua", code='''
        assert(entity.get_health("initial_controller") == 55)
        assert(entity.get_health("initial_trimesh") == 33)
        local x, y, z = entity.get_position("initial_trimesh")
        local hit, distance = physics.raycast(x + 1.5, y + 3, z, 0, -1, 0, 5)
        assert(hit and math.abs(distance - 3) < 0.01, "scaled trimesh ray missed")
        local hit, id, distance = physics.hitscan(x + 1.5, y + 3, z, 0, -1, 0, 5)
        assert(hit and id == "initial_trimesh" and math.abs(distance - 3) < 0.01)
    ''')
    verify_runtime_mesh(socket_path)
    return counts


RUNTIME_MESHES = iter(range(1000))


def verify_runtime_mesh(socket_path):
    """A mesh created and spawned in one frame is drawn and collides as itself.

    The wedge's slanted face (height 0.75 under the ray) is no stock shape, so
    a stand-in cube or a missing body fails the ray.
    """
    n = next(RUNTIME_MESHES)
    x = 40 + 10 * n
    command(socket_path, "run_lua", code=f'''
        mesh.create("release_wedge", {{{{0,0,0}},{{2,0,0}},{{0,0,2}},{{0,1,0}}}},
            {{0,2,1, 0,1,3, 0,3,2, 1,2,3}}, {{}})
        entity.spawn_ex({{id = "release_wedge_{n}", mesh = "runtime:release_wedge",
            material = "procedural:default", position = {{{x}, 0, 0}},
            collider = {{shape = "trimesh"}}}})
    ''')

    def wedge_collides():
        try:
            command(socket_path, "run_lua", code=f'''
                local hit, id, distance = physics.hitscan({x} + 0.25, 5, 0.25, 0, -1, 0, 10)
                assert(hit and id == "release_wedge_{n}" and math.abs(distance - 4.25) < 0.05,
                    tostring(id) .. " " .. tostring(distance))
            ''')
            return True
        except RuntimeError:
            return False

    wait_until(wedge_collides, "a runtime mesh spawned in its creation frame did not collide as itself")
    # Scene snapshots (undo, save) cannot represent runtime meshes; leave the
    # scene as the later checks expect it.
    command(socket_path, "destroy_entity", entity_id=f"release_wedge_{n}")


def wait_for_runtime(process, socket_path, log_path, timeout=60):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"runtime exited with {process.returncode}; see {log_path}")
        try:
            return command(socket_path, "list_entities")["entities"]
        except (OSError, KeyError, RuntimeError):
            time.sleep(0.1)
    raise TimeoutError(f"runtime did not become ready; see {log_path}")


def stop_process(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def replace_scene_text(path, contents):
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(contents)
    temporary.replace(path)


def verify_editor_construction(binary, game, temporary):
    """Exercise actual editor startup and both unchanged/structural reloads."""
    socket_path = temporary / "control" / "editor.sock"
    log_path = temporary / "editor.log"
    with log_path.open("w") as log:
        # Edit discovers naive.yaml in cwd; the original project was removed.
        process = subprocess.Popen([binary, "--socket", socket_path, "edit",
                                    "--scene", "scenes/main.yaml"],
                                   cwd=game, stdout=log, stderr=log)
        try:
            wait_for_runtime(process, socket_path, log_path)
            counts = verify_construction(socket_path)
            command(socket_path, "runtime_control", action="pause")
            command(socket_path, "run_lua", code='physics.set_velocity("initial_trimesh", 3, 0, 0)')
            before = command(socket_path, "query_entity", entity_id="initial_trimesh",
                             component="transform")["transform"]["position"]
            scene_path = game / "scenes" / "main.yaml"
            source = scene_path.read_text()
            updated = source + "\n  - id: reload_marker\n    components:\n      transform: {position: [50, 0, 0]}\n"
            replace_scene_text(scene_path, updated)
            wait_until(lambda: command(socket_path, "run_lua",
                code='return entity.exists("reload_marker")')["result"] == "true",
                "editor did not reload the marker entity", timeout=20)
            after = command(socket_path, "query_entity", entity_id="initial_trimesh",
                            component="transform")["transform"]["position"]
            assert all(abs(a - b) < 0.000001 for a, b in zip(before, after)), (before, after)
            command(socket_path, "run_lua", code='''
                local vx, vy, vz = physics.get_velocity("initial_trimesh")
                assert(math.abs(vx - 3) < 0.0001 and vy == 0 and vz == 0,
                    "unrelated reload rebuilt the live trimesh body")
            ''')
            assert construction_counts(socket_path) == counts, "unrelated reload reinitialized scripts"

            # A real physics transform/scale change must replace only that body.
            moved = updated.replace("position: [15, 0, 0]", "position: [25, 0, 0]", 1)
            moved = moved.replace("scale: [4, 1, 4]", "scale: [6, 1, 6]", 1)
            assert moved != updated and "position: [25, 0, 0]" in moved
            replace_scene_text(scene_path, moved)
            wait_until(lambda: abs(command(socket_path, "query_entity",
                entity_id="initial_trimesh", component="transform")["transform"]["position"][0] - 25) < 0.0001,
                "editor did not replace the moved trimesh", timeout=20)
            command(socket_path, "runtime_control", action="resume")
            expected = {**counts, "initial_trimesh": counts["initial_trimesh"] + 1}
            verify_construction(socket_path, expected)
            command(socket_path, "runtime_control", action="pause")
            command(socket_path, "run_lua", code='''
                local old_hit = physics.raycast(15, 3, 0, 0, -1, 0, 5)
                assert(not old_hit, "structural reload left the old trimesh collider")
                local hit, id, distance = physics.hitscan(27.5, 3, 0, 0, -1, 0, 5)
                assert(hit and id == "initial_trimesh" and math.abs(distance - 3) < 0.01,
                    "structural reload did not recook the moved/scaled geometry")
            ''')
            assert construction_counts(socket_path) == expected, "controller or trimesh initialized twice"
        except Exception:
            log.flush()
            print("Packaged editor log:", file=sys.stderr, flush=True)
            print(log_path.read_text(errors="replace")[-64 * 1024:], file=sys.stderr, flush=True)
            raise
        finally:
            stop_process(process)
    print("Packaged editor construction and reload checks passed", flush=True)


def verify_editor_startup_recovery(binary, game, temporary, reload_timeout=20):
    """An invalid initial scene must not hide valid actors on its next reload."""
    source = (game / "scenes" / "main.yaml").read_text()
    expected_ids = {line[len("  - id: "):].strip()
                    for line in source.splitlines() if line.startswith("  - id: ")}
    assert set(CONSTRUCTION_ENTITIES) <= expected_ids
    scene_path = game / "scenes" / "startup-recovery.yaml"
    invalid = source + ("\n  - id: late_invalid_asset\n    components:\n"
        "      mesh_renderer:\n        mesh: assets/missing-editor-startup.glb\n"
        "        material: procedural:default\n")
    replace_scene_text(scene_path, invalid)
    socket_path = temporary / "control" / "recovery.sock"
    log_path = temporary / "editor-recovery.log"
    with log_path.open("w") as log:
        process = subprocess.Popen([binary, "--socket", socket_path, "edit",
                                    "--scene", "scenes/startup-recovery.yaml"],
                                   cwd=game, stdout=log, stderr=log)
        try:
            entities = wait_for_runtime(process, socket_path, log_path)
            assert not entities, "failed initial preflight left partial live entities"
            assert construction_counts(socket_path) == dict.fromkeys(CONSTRUCTION_ENTITIES, 0)
            replace_scene_text(scene_path, source)
            wait_until(lambda: expected_ids == {entity["id"] for entity in
                command(socket_path, "list_entities")["entities"]},
                "corrected editor scene did not materialize all unchanged actor IDs",
                timeout=reload_timeout)
            verify_construction(socket_path)
        except Exception:
            log.flush()
            print("Packaged editor startup recovery log:", file=sys.stderr, flush=True)
            print(log_path.read_text(errors="replace")[-64 * 1024:], file=sys.stderr, flush=True)
            raise
        finally:
            stop_process(process)
    print("Packaged editor startup recovery check passed", flush=True)


def check_mcp_test_execution(release, socket_path, game):
    """The same bridge must answer ordinary calls while a test is in flight."""
    bridge = subprocess.Popen([release / "naive_mcp", socket_path], cwd=game,
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, text=True, bufsize=1)
    replies = queue.Queue()

    def read_replies():
        try:
            for line in bridge.stdout:
                replies.put(json.loads(line))
        except Exception as error:
            replies.put(error)

    threading.Thread(target=read_replies, daemon=True).start()

    def send(identifier, tool, arguments):
        bridge.stdin.write(json.dumps({"jsonrpc": "2.0", "id": identifier,
            "method": "tools/call", "params": {"name": tool, "arguments": arguments}}) + "\n")
        bridge.stdin.flush()

    try:
        # Six seconds also proves neither transport abandons the reply at five.
        send(101, "naive_run_tests", {"file": "tests/bounded.lua", "timeout_seconds": 6})
        wait_until(lambda: command(socket_path, "editor_status")["tests_running"],
                   "test worker did not start", timeout=3)
        duplicate = raw_command(socket_path, "run_tests", file="tests/test_release.lua")
        assert duplicate["status"] == "error" and "already running" in duplicate["message"], duplicate
        send(102, "naive_editor_status", {})
        send(103, "naive_run_tests", {"file": "tests/test_release.lua"})
        early = [replies.get(timeout=3), replies.get(timeout=3)]
        assert {reply["id"] for reply in early} == {102, 103}, early
        assert not next(reply for reply in early if reply["id"] == 102)["result"].get("isError"), early
        assert next(reply for reply in early if reply["id"] == 103)["result"].get("isError"), early
        timed_out = replies.get(timeout=9)
        assert timed_out["id"] == 101 and timed_out["result"].get("isError"), timed_out
        assert "Test suite timed out" in timed_out["result"]["content"][0]["text"], timed_out
        send(104, "naive_run_tests", {"file": "tests/test_release.lua"})
        recovered = replies.get(timeout=10)
        assert recovered["id"] == 104 and not recovered["result"].get("isError"), recovered
        data = json.loads(recovered["result"]["content"][0]["text"])["data"]
        assert data["passed"] == 1 and data["failed"] == 0, data
        bridge.stdin.close()
        assert bridge.wait(timeout=10) == 0
    finally:
        if bridge.poll() is None:
            bridge.terminate()
            bridge.wait(timeout=5)

    # EOF after one asynchronous request must still deliver that response.
    request = {"jsonrpc": "2.0", "id": 105, "method": "tools/call", "params": {
        "name": "naive_run_tests", "arguments": {"file": "tests/test_release.lua"}}}
    response = subprocess.run([release / "naive_mcp", socket_path], cwd=game,
        input=json.dumps(request) + "\n", capture_output=True, text=True, timeout=10, check=True)
    assert not json.loads(response.stdout)["result"].get("isError"), response.stdout


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
    decoded = bytearray()
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
        decoded.extend(row)
        previous = row
    # A valid image of a clear/background only is not a rendering success.
    if len(pixels) < 8:
        raise AssertionError(f"capture has only {len(pixels)} colors; scene may be blank")
    print(f"Captured {width}x{height} PNG with {len(pixels)} colors", flush=True)
    return width, height, decoded


def verify_custom_shader(path):
    """These edge colors are emitted only by the fixture's custom shader."""
    width, height, pixels = verify_png(path)
    for x_fraction, expected in [(0.1, (0, 255, 255)), (0.9, (255, 0, 255))]:
        for y_fraction in [0.25, 0.5, 0.75]:
            x, y = int(width * x_fraction), int(height * y_fraction)
            offset = (y * width + x) * 3
            actual = tuple(pixels[offset:offset + 3])
            if any(abs(a - e) > 2 for a, e in zip(actual, expected)):
                raise AssertionError(f"custom shader marker missing at ({x}, {y}): {actual}, expected {expected}")
    print("Custom packaged shader rendered cyan and magenta markers", flush=True)


def bundle_paths(bundle):
    if bundle.suffix == ".app":
        with (bundle / "Contents" / "Info.plist").open("rb") as plist:
            executable = plistlib.load(plist)["CFBundleExecutable"]
        return (bundle / "Contents" / "MacOS" / executable,
                bundle / "Contents" / "Resources" / "game")
    binaries = [path for path in bundle.iterdir() if path.is_file() and path.stat().st_mode & 0o111]
    if len(binaries) != 1:
        raise AssertionError(f"expected one player executable in {bundle}")
    return binaries[0], bundle / "game"


def verify_player_libraries(binary):
    linker = ["otool", "-L"] if sys.platform == "darwin" else ["ldd"]
    libraries = subprocess.check_output(linker + [str(binary)], text=True)
    if "slang" in libraries.lower():
        raise AssertionError(f"shipped player links Slang:\n{libraries}")


def verify_existing_bundle(source):
    """Verify the actual release app, retaining its signed source for packaging."""
    temporary = Path(tempfile.mkdtemp(prefix="naive-demo-release-"))
    bundle = temporary / source.name
    shutil.copytree(source, bundle)
    binary, game = bundle_paths(bundle)
    verify_player_libraries(binary)
    control = temporary / "control"
    control.mkdir(mode=0o700)
    socket_path = control / "runtime.sock"
    log_path = temporary / "runtime.log"
    with log_path.open("w") as log:
        # No run subcommand, no source project cwd: exercise app game discovery.
        environment = os.environ.copy()
        environment["RUST_LOG"] = "info"
        process = subprocess.Popen([binary, "--socket", socket_path],
                                   cwd=temporary, stdout=log, stderr=log, env=environment)
        try:
            deadline = time.monotonic() + 90
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError(f"release app exited with {process.returncode}")
                try:
                    entities = command(socket_path, "list_entities")["entities"]
                    if not entities:
                        raise AssertionError("release demo has no entities")
                    break
                except (OSError, KeyError, RuntimeError):
                    time.sleep(0.1)
            else:
                raise TimeoutError("relocated release app did not become ready")
            command(socket_path, "capture_frame", path="captures/release-verification.png")
            verify_png(game / "captures" / "release-verification.png")
            log.flush()
            runtime_log = log_path.read_text(errors="replace")
            if "Render pipeline compiled successfully" not in runtime_log:
                raise AssertionError("release app did not load its packaged render pipeline")
        except Exception:
            log.flush()
            print(log_path.read_text(errors="replace")[-64 * 1024:], file=sys.stderr, flush=True)
            raise
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
    print(f"Relocated release app passed. Artifacts: {temporary}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--no-build", action="store_true", help="use existing release binaries")
    parser.add_argument("--slang", action="store_true", help="require fresh Slang compilation of the custom pass")
    parser.add_argument("--bundle", type=Path, help="verify an existing release app or Linux bundle")
    args = parser.parse_args()
    if args.bundle:
        verify_existing_bundle(args.bundle.resolve(strict=True))
        return
    repo = Path(__file__).resolve().parent.parent
    if not args.no_build:
        build = ["cargo", "build", "--release", "--locked", "-p", "naive-runtime", "--bins"]
        if not args.slang:
            build.append("--no-default-features")
        run(build, repo)
        # Built on its own so feature unification cannot link Slang into it.
        run(["cargo", "build", "--release", "--locked", "-p", "naive-player"], repo)
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=repo))
    release = Path(metadata["target_directory"]) / "release"
    temporary = Path(tempfile.mkdtemp(prefix="naive-release-"))
    print(f"Release smoke artifacts: {temporary}", flush=True)
    project = temporary / "project"
    shutil.copytree(repo / "scripts" / "release-smoke-fixture", project)
    marker = Path("assets/effects/release_marker.slang.wgsl")
    if args.slang:
        # A checked-in artifact must not conceal a compiler or discovery bug.
        (project / marker).unlink()
    # The standalone player also exposes build. Use it to guarantee the
    # checked-in-artifact path really runs without Slang, even with --no-build
    # after a Slang-enabled CLI was built for the other smoke mode.
    builder = release / ("naive" if args.slang else "naive-player")
    if not args.slang:
        verify_player_libraries(builder)
    run([builder, "build"], project)
    bundles = list((project / "dist").iterdir())
    if len(bundles) != 1:
        raise AssertionError("expected exactly one release bundle")
    # Move the bundle out of its source project to detect accidental relative
    # references to unpackaged project content.
    bundle = temporary / bundles[0].name
    shutil.move(str(bundles[0]), bundle)
    # Remove the source fixture so no source-only content can mask omissions.
    shutil.rmtree(project)
    binary, game = bundle_paths(bundle)
    artifact = game / marker
    if not artifact.is_file() or artifact.stat().st_size == 0:
        raise AssertionError("custom pass has no packaged WGSL artifact")
    verify_player_libraries(binary)
    result = subprocess.run([binary, "test"], cwd=game, capture_output=True,
                            text=True, timeout=60, check=True)
    print(result.stdout, end="", flush=True)
    if "1 passed, 0 failed" not in result.stdout:
        raise AssertionError("packaged Lua test did not execute")
    private = temporary / "control"
    private.mkdir(mode=0o700)
    socket_path = private / "runtime.sock"
    log_path = temporary / "runtime.log"
    with log_path.open("w") as log:
        # Started from elsewhere with no subcommand, the player finds the game
        # packaged beside it, as when the app is opened.
        process = subprocess.Popen(
            [binary, "--socket", socket_path],
            cwd=temporary, stdout=log, stderr=log)
        try:
            entities = wait_for_runtime(process, socket_path, log_path)
            assert "smoke_cube" in {entity["id"] for entity in entities}
            verify_construction(socket_path)
            # Exercise MCP stdio -> typed request -> secured socket -> Lua.
            request = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                "name": "naive_run_lua", "arguments": {"code":
                    'assert(entity.exists("smoke_cube")); '
                    'entity.set_position("smoke_cube", 0, 1, 0); return "release-ok"'}}}
            bridge = subprocess.run([release / "naive_mcp", socket_path],
                input=json.dumps(request) + "\n", cwd=game,
                capture_output=True, text=True, timeout=30, check=True)
            response = json.loads(bridge.stdout)
            assert not response["result"].get("isError"), response
            returned = json.loads(response["result"]["content"][0]["text"])
            assert returned["data"]["result"] == "release-ok", returned
            command(socket_path, "capture_frame", path="captures/release.png")
            verify_custom_shader(game / "captures" / "release.png")

            # A failing live command may have edited the scene and must be undoable.
            command(socket_path, "runtime_control", action="pause")
            failed = raw_command(socket_path, "run_lua", code='entity.set_position("smoke_sphere", 9, 1, 0); error("partial edit")')
            assert failed["status"] == "error", failed
            assert command(socket_path, "query_entity", entity_id="smoke_sphere", component="transform")["transform"]["position"][0] == 9
            assert command(socket_path, "undo")["undone"] == "run_lua"
            assert command(socket_path, "query_entity", entity_id="smoke_sphere", component="transform")["transform"]["position"][0] == 2
            command(socket_path, "runtime_control", action="resume")
            check_mcp_test_execution(release, socket_path, game)

            # Rejected scene assets preserve the live scene and its undo history.
            (game / "scenes" / "invalid-construction.yaml").write_text(
                "name: Invalid construction\nentities:\n  - id: invalid_construction\n"
                "    components:\n      mesh_renderer:\n"
                "        mesh: assets/missing.glb\n        material: procedural:default\n")
            log_offset = log_path.stat().st_size
            command(socket_path, "run_lua", code='scene.load("scenes/invalid-construction.yaml")')
            wait_until(lambda: b"Scene replacement failed:" in log_path.read_bytes()[log_offset:]
                       and b"invalid_construction" in log_path.read_bytes()[log_offset:],
                       "invalid scene replacement did not report its preflight failure", timeout=20)
            command(socket_path, "query_entity", entity_id="initial_controller", component="player")
            assert command(socket_path, "undo")["undone"] == "run_lua", "failed scene load erased undo history"

            # Authoring round trip: preserve script, health, physics and flags.
            initialized_before_save = command(socket_path, "query_events", filter="release.initialized")["count"]
            command(socket_path, "run_lua", code='entity.set_health("smoke_cube", 73, 100)')
            command(socket_path, "save_scene", path="scenes/roundtrip.yaml")
            roundtrip = subprocess.run([binary, "test", "tests/roundtrip.lua"], cwd=game,
                capture_output=True, text=True, timeout=60)
            if roundtrip.returncode:
                raise AssertionError(roundtrip.stdout + roundtrip.stderr)
            assert "All 1 tests passed." in roundtrip.stdout, roundtrip.stdout

            # MCP construction must apply components together and initialize once.
            command(socket_path, "spawn_entity", entity_id="mcp_actor", components={
                "transform": {"position": [4, 2, 0]},
                "mesh_renderer": {"mesh": "procedural:cube", "material": "procedural:default"},
                "point_light": {"intensity": 7}, "health": {"max": 60},
                "rigid_body": {"type": "static"}, "collider": {"shape": "box"},
                "script": {"source": "logic/spawned.lua"}})
            light = command(socket_path, "query_entity", entity_id="mcp_actor", component="point_light")
            assert light["point_light"]["intensity"] == 7, light
            assert float(command(socket_path, "run_lua", code='return entity.get_health("mcp_actor")')["result"]) == 60
            command(socket_path, "run_lua", code='entity.spawn_ex({id="lua_actor", mesh="procedural:cube", material="procedural:default", script="logic/spawned.lua", position={6,2,0}})')
            wait_until(lambda: command(socket_path, "query_events", filter="release.spawn_initialized")["count"] >= 2,
                       "spawned scripts did not initialize")
            initialized = command(socket_path, "query_events", filter="release.spawn_initialized")["events"]
            assert sorted(event["data"]["id"] for event in initialized) == ["lua_actor", "mcp_actor"], initialized
            assert command(socket_path, "query_events", filter="release.initialized")["count"] == initialized_before_save, "saving another document reloaded the active scene"

            # A failed script load must roll back the whole MCP entity, including physics.
            try:
                command(socket_path, "spawn_entity", entity_id="failed_actor", components={
                    "transform": {"position": [8, 2, 0]},
                    "rigid_body": {"type": "static"}, "collider": {"shape": "box"},
                    "script": {"source": "logic/missing.lua"}})
            except RuntimeError as error:
                assert "Failed to load script" in str(error), str(error)
            else:
                raise AssertionError("MCP accepted a missing script")
            assert command(socket_path, "run_lua", code='return entity.exists("failed_actor")')["result"] == "false"
            assert command(socket_path, "run_lua", code='return physics.raycast(8, 5, 0, 0, -1, 0, 4)')["result"] == "false"

            # Destroying an entity removes both the physical object and callbacks.
            ray = 'return physics.raycast(4, 5, 0, 0, -1, 0, 4)'
            # Spawn/init can complete before the next fixed physics step.
            wait_until(lambda: command(socket_path, "run_lua", code=ray)["result"] == "true",
                       "MCP collider never became available to physics queries")
            command(socket_path, "destroy_entity", entity_id="mcp_actor")
            assert command(socket_path, "run_lua", code='return entity.exists("mcp_actor")')["result"] == "false"
            assert command(socket_path, "run_lua", code=ray)["result"] == "false", "destroy left a collider"
            command(socket_path, "destroy_entity", entity_id="lua_actor")
            command(socket_path, "emit_event", event_type="release.ping", data={})
            command(socket_path, "benchmark", frames=2)
            assert command(socket_path, "query_events", filter="release.ghost")["count"] == 0, "destroyed listener still ran"

            # Reopening a saved scene exercises on_destroy setters and physics reload.
            # The bus preserves event history across transitions.
            stale_before = command(socket_path, "query_events", filter="release.stale_event")["count"]
            construction_before = construction_counts(socket_path)
            command(socket_path, "spawn_entity", entity_id="transition_marker", components={"transform": {}})
            command(socket_path, "run_lua", code='scene.load("scenes/roundtrip.yaml")')
            wait_until(lambda: command(socket_path, "run_lua", code='return entity.exists("transition_marker")')["result"] == "false",
                       "scene transition did not complete")
            assert process.poll() is None, "on_destroy crashed scene transition"
            assert float(command(socket_path, "run_lua", code='return entity.get_health("smoke_cube")')["result"]) == 73
            wait_until(lambda: command(socket_path, "run_lua", code='return physics.raycast(0, 3, 0, 0, -1, 0, 2)')["result"] == "true",
                       "saved collider never became available to physics queries")
            command(socket_path, "benchmark", frames=2)
            stale_after = command(socket_path, "query_events", filter="release.stale_event")["count"]
            assert stale_after == stale_before, f"old scene event reached new scene listeners: {stale_before} -> {stale_after}"
            verify_construction(socket_path, {identifier: count + 1
                for identifier, count in construction_before.items()})
            # Save confinement must apply in the actual packaged engine too.
            with socket.socket(socket.AF_UNIX) as stream:
                stream.settimeout(10)
                stream.connect(str(socket_path))
                stream.sendall(b'{"cmd":"save_scene","path":"../escaped.yaml"}\n')
                with stream.makefile("rb") as reader:
                    refused = json.loads(reader.readline())
            assert refused["status"] == "error", refused
            assert not (game.parent / "escaped.yaml").exists()
        except Exception:
            # CI runners discard their temporary directories. Include the
            # actual engine error rather than only a path to an unavailable log.
            log.flush()
            print("Packaged runtime log:", file=sys.stderr, flush=True)
            print(log_path.read_text(errors="replace")[-64 * 1024:],
                  file=sys.stderr, flush=True)
            raise
        finally:
            stop_process(process)
    verify_editor_construction(binary, game, temporary)
    verify_editor_startup_recovery(binary, game, temporary)
    verify_legacy_project(repo, builder, temporary)
    print(f"Release smoke passed. Artifacts: {temporary}")


def verify_legacy_project(repo, builder, temporary):
    """A project made by the 0.1.22 `naive init` packages, launches and renders
    through its deferred pipeline (version 1 pipeline mapped to engine shaders)."""
    project = temporary / "legacy"
    shutil.copytree(repo / "scripts" / "fixtures" / "init-0.1.22", project)
    run([builder, "build"], project)
    bundles = list((project / "dist").iterdir())
    if len(bundles) != 1:
        raise AssertionError("expected exactly one bundle for the 0.1.22 project")
    bundle = temporary / ("legacy-" + bundles[0].name)
    shutil.move(str(bundles[0]), bundle)
    shutil.rmtree(project)
    binary, game = bundle_paths(bundle)
    socket_path = temporary / "control" / "legacy.sock"
    log_path = temporary / "legacy.log"
    with log_path.open("w") as log:
        process = subprocess.Popen([binary, "--socket", socket_path], cwd=temporary,
                                   stdout=log, stderr=log)
        try:
            entities = wait_for_runtime(process, socket_path, log_path)
            assert entities, "the 0.1.22 project has no entities"
            log.flush()
            text = log_path.read_text(errors="replace")
            assert "is a version 1 pipeline" in text, "version 1 pipeline was not migrated"
            assert "Render pipeline compiled successfully" in text, \
                "the 0.1.22 project lost its deferred pipeline"
            assert "Pipeline compilation failed" not in text
            command(socket_path, "capture_frame", path="captures/legacy.png")
            verify_png(game / "captures" / "legacy.png")
            print("0.1.22 project packaged, launched and rendered its pipeline", flush=True)
        except Exception:
            log.flush()
            print("0.1.22 project runtime log:", file=sys.stderr, flush=True)
            print(log_path.read_text(errors="replace")[-32 * 1024:], file=sys.stderr, flush=True)
            raise
        finally:
            stop_process(process)


if __name__ == "__main__":
    main()
