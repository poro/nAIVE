#!/usr/bin/env python3
"""Check representative demo scenes in the real engine.

For each scene: the entities it must build (including meshes scripts create
with mesh.create), that its frame renders, and that a paused scene captures
the same pixels twice. Run under a display (xvfb-run on Linux CI).

usage: scripts/scene-check.py [--binary PATH]   (default: target/release/naive)
"""

import argparse
import importlib.util
import os
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
_spec = importlib.util.spec_from_file_location("smoke", REPO / "scripts" / "release-smoke.py")
smoke = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(smoke)
command, wait_until = smoke.command, smoke.wait_until


def ids(socket_path):
    return {entity["id"] for entity in command(socket_path, "list_entities")["entities"]}


def mesh_handle(socket_path, entity_id):
    renderer = command(socket_path, "query_entity", entity_id=entity_id,
                       component="mesh_renderer")["mesh_renderer"]
    return renderer["mesh_handle"]


def check_relic(socket_path):
    present = ids(socket_path)
    assert {"player", "floor", "ceiling", "wall_north"} <= present, sorted(present)
    assert len(present) >= 19, len(present)


def check_showcase(socket_path):
    diamonds = [f"diamond_{i}" for i in range(1, 6)]
    wait_until(lambda: set(diamonds) <= ids(socket_path),
               "the showcase did not build its mesh.create diamonds")
    handles = {mesh_handle(socket_path, diamond) for diamond in diamonds}
    assert len(handles) == 1, handles
    # The diamonds are their own mesh, not a stand-in cube.
    assert handles.isdisjoint({mesh_handle(socket_path, "pedestal_cube")}), handles


def check_procedural_world(socket_path):
    def bridges():
        return [entity for entity in ids(socket_path) if entity.startswith("bridge_")]
    wait_until(lambda: len(bridges()) == 1, "the procedural world did not build its bridge",
               timeout=30)
    terrain = sorted(entity for entity in ids(socket_path) if entity.startswith("terrain_"))
    assert len(terrain) == 1089, len(terrain)
    assert mesh_handle(socket_path, bridges()[0]) != mesh_handle(socket_path, terrain[0])


SCENES = [
    ("scenes/relic.yaml", check_relic),
    ("scenes/tier3_showcase.yaml", check_showcase),
    ("scenes/procedural_world.yaml", check_procedural_world),
    ("scenes/pbr_gallery.yaml", None),
]


def check_scene(binary, project, scene, check, temporary):
    stem = Path(scene).stem
    socket_path = temporary / f"{stem}.sock"
    log_path = temporary / f"{stem}.log"
    with log_path.open("w") as log:
        process = subprocess.Popen([binary, "--socket", socket_path, "edit", "--scene", scene],
                                   cwd=project, stdout=log, stderr=log)
        try:
            smoke.wait_for_runtime(process, socket_path, log_path)
            if check:
                check(socket_path)
            log.flush()
            text = log_path.read_text(errors="replace")
            assert "Pipeline compilation failed" not in text, "pipeline failed"
            assert "Entity spawn failed" not in text, "an entity failed to spawn"
            command(socket_path, "runtime_control", action="pause")
            command(socket_path, "set_camera", position=[0.0, 6.0, 14.0], look_at=[0.0, 1.0, 0.0])
            first, second = (f"captures/scene-check-{stem}-{n}.png" for n in (1, 2))
            command(socket_path, "capture_frame", path=first)
            command(socket_path, "capture_frame", path=second)
            smoke.verify_png(project / first)
            assert (project / first).read_bytes() == (project / second).read_bytes(), \
                "a paused scene captured different pixels twice"
            print(f"{scene}: entities, render and repeatable capture OK", flush=True)
        except Exception:
            log.flush()
            print(f"{scene} log:", file=sys.stderr)
            print(log_path.read_text(errors="replace")[-16 * 1024:], file=sys.stderr, flush=True)
            raise
        finally:
            smoke.stop_process(process)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=REPO / "target" / "release" / "naive")
    args = parser.parse_args()
    project = REPO / "project"
    # Short socket paths: Unix sockets allow about 100 bytes.
    temporary = Path(tempfile.mkdtemp(prefix="naive-scenes-", dir="/tmp" if os.path.isdir("/tmp") else None))
    for scene, check in SCENES:
        check_scene(args.binary.resolve(), project, scene, check, temporary)
    print("Scene check passed.")


if __name__ == "__main__":
    main()
