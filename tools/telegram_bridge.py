#!/usr/bin/env python3
"""
nAIVE Telegram Bridge — Control a running nAIVE engine via Telegram messages.

Architecture:
    Telegram (BotFather bot) → This script → Claude API (NL interpretation) → nAIVE Unix Socket

Setup:
    1. Create a Telegram bot via @BotFather, get the token
    2. Set environment variables:
       export TELEGRAM_BOT_TOKEN="your-bot-token"
       export ANTHROPIC_API_KEY="your-anthropic-key"
    3. Start nAIVE engine (it creates /tmp/naive-runtime.sock)
    4. Run this script: python3 telegram_bridge.py

Usage:
    Send messages to your bot in Telegram:
    - "make it rain" → spawns blue particles, darkens ambient
    - "make the lights red" → changes all light colors to red
    - "add a spotlight above" → spawns a new light entity
    - "make everything glow" → cranks up emission on all entities
    - "sunrise" → triggers warm golden lighting ramp
    - "chaos mode" → randomizes light colors and intensities
"""

import json
import os
import socket
import sys
import time
import urllib.request
import urllib.error

# ─────────────────────────────────────────────────────────────
# Configuration
# ─────────────────────────────────────────────────────────────

TELEGRAM_TOKEN = os.environ.get("TELEGRAM_BOT_TOKEN", "")
ANTHROPIC_KEY = os.environ.get("ANTHROPIC_API_KEY", "")
NAIVE_SOCKET = os.environ.get("NAIVE_SOCKET", "/tmp/naive-runtime.sock")
TELEGRAM_API = f"https://api.telegram.org/bot{TELEGRAM_TOKEN}"

# ─────────────────────────────────────────────────────────────
# nAIVE Engine Communication
# ─────────────────────────────────────────────────────────────

def send_naive_command(cmd: dict) -> dict:
    """Send a JSON command to nAIVE's Unix domain socket and return the response."""
    try:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(5.0)
        sock.connect(NAIVE_SOCKET)
        payload = json.dumps(cmd) + "\n"
        sock.sendall(payload.encode("utf-8"))
        response = b""
        while True:
            chunk = sock.recv(4096)
            if not chunk:
                break
            response += chunk
            if b"\n" in chunk:
                break
        sock.close()
        return json.loads(response.decode("utf-8").strip())
    except Exception as e:
        return {"status": "error", "message": str(e)}


def list_entities() -> list:
    """Get all entities currently in the scene."""
    result = send_naive_command({"cmd": "list_entities"})
    if result.get("status") == "ok" and result.get("data"):
        return result["data"].get("entities", [])
    return []


def modify_entity(entity_id: str, components: dict) -> dict:
    """Modify an entity's components."""
    return send_naive_command({
        "cmd": "modify_entity",
        "entity_id": entity_id,
        "components": components,
    })


def spawn_entity(entity_id: str, components: dict) -> dict:
    """Spawn a new entity."""
    return send_naive_command({
        "cmd": "spawn_entity",
        "entity_id": entity_id,
        "components": components,
    })


def emit_event(event_type: str, data: dict = None) -> dict:
    """Emit an event on the event bus."""
    return send_naive_command({
        "cmd": "emit_event",
        "event_type": event_type,
        "data": data or {},
    })


# ─────────────────────────────────────────────────────────────
# Claude API — Natural Language → nAIVE Commands
# ─────────────────────────────────────────────────────────────

SYSTEM_PROMPT = """You are a bridge between natural language commands and the nAIVE game engine.
The engine is currently running a scene. You translate user requests into JSON commands.

Available commands:

1. modify_entity — Change an entity's properties
   {"cmd": "modify_entity", "entity_id": "entity_name", "components": {
     "point_light": {"color": [r, g, b], "intensity": float, "range": float},
     "transform": {"position": [x, y, z]},
     "material_override": {"emission": [r, g, b], "roughness": float, "metallic": float, "base_color": [r, g, b]}
   }}

2. spawn_entity — Create a new entity
   {"cmd": "spawn_entity", "entity_id": "unique_name", "components": {
     "transform": {"position": [x, y, z], "scale": [sx, sy, sz]},
     "point_light": {"color": [r, g, b], "intensity": float, "range": float},
     "mesh_renderer": {"mesh": "procedural:sphere", "material": "assets/materials/chrome.yaml"}
   }}

3. destroy_entity — Remove an entity
   {"cmd": "destroy_entity", "entity_id": "entity_name"}

Available materials: chrome, obsidian, dark_mirror, copper_ring, steel_ring, genesis_core,
neon_pink, neon_cyan, neon_purple, neon_gold, neon_blue, neon_green, neon_white, neon_amber.
Material paths: "assets/materials/{name}.yaml"

Meshes: "procedural:sphere", "procedural:cube", "assets/meshes/cube.gltf"

Colors are [r, g, b] floats 0.0-1.0. Emission values can exceed 1.0 for bloom (e.g., [3.0, 0.5, 0.5]).
Light intensity: 0-25. Light range: 1-30. Positions: the scene is centered at origin, radius ~10.

IMPORTANT: Return ONLY a JSON array of commands. No explanation, no markdown. Just the JSON array.
If the request involves multiple changes, return multiple commands in the array.
For "rain", simulate by spawning several blue lights at various heights that flicker.
For color changes, modify the lights and/or material overrides.
Be creative but stay within the available command set.

Current entities in the scene (will be provided per-message)."""


def ask_claude(user_message: str, entities: list) -> list:
    """Ask Claude to interpret a natural language command into nAIVE commands."""
    entity_summary = ", ".join([e.get("id", "?") for e in entities[:40]])

    request_body = json.dumps({
        "model": "claude-sonnet-4-5-20250929",
        "max_tokens": 2048,
        "system": SYSTEM_PROMPT,
        "messages": [
            {
                "role": "user",
                "content": f"Scene entities: [{entity_summary}]\n\nUser request: {user_message}"
            }
        ],
    }).encode("utf-8")

    req = urllib.request.Request(
        "https://api.anthropic.com/v1/messages",
        data=request_body,
        headers={
            "Content-Type": "application/json",
            "x-api-key": ANTHROPIC_KEY,
            "anthropic-version": "2023-06-01",
        },
    )

    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            data = json.loads(resp.read().decode("utf-8"))
            text = data["content"][0]["text"].strip()
            # Parse the JSON array of commands
            commands = json.loads(text)
            if isinstance(commands, dict):
                commands = [commands]
            return commands
    except Exception as e:
        print(f"  [Claude API error] {e}")
        return []


# ─────────────────────────────────────────────────────────────
# Telegram Bot Polling
# ─────────────────────────────────────────────────────────────

def telegram_request(method: str, params: dict = None) -> dict:
    """Make a request to the Telegram Bot API."""
    url = f"{TELEGRAM_API}/{method}"
    if params:
        data = json.dumps(params).encode("utf-8")
        req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"})
    else:
        req = urllib.request.Request(url)
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8") if e.fp else ""
        print(f"  [Telegram API error] {e.code}: {body}")
        return {"ok": False}
    except Exception as e:
        print(f"  [Telegram error] {e}")
        return {"ok": False}


def send_telegram_message(chat_id: int, text: str):
    """Send a message back to the Telegram chat."""
    telegram_request("sendMessage", {"chat_id": chat_id, "text": text})


def process_message(chat_id: int, text: str):
    """Process an incoming Telegram message: interpret via Claude, execute on nAIVE."""
    print(f"  [Telegram] Received: {text!r}")

    # Check nAIVE is running
    entities = list_entities()
    if not entities:
        send_telegram_message(chat_id, "nAIVE engine is not running or no scene loaded.")
        return

    send_telegram_message(chat_id, f"Processing: \"{text}\"...")

    # Ask Claude to interpret
    commands = ask_claude(text, entities)
    if not commands:
        send_telegram_message(chat_id, "Could not interpret that command. Try something like \"make the lights blue\" or \"add a spotlight\".")
        return

    # Execute each command
    results = []
    for cmd in commands:
        result = send_naive_command(cmd)
        status = result.get("status", "unknown")
        cmd_type = cmd.get("cmd", "?")
        entity = cmd.get("entity_id", "?")
        results.append(f"  {cmd_type} {entity}: {status}")
        print(f"    -> {cmd_type} {entity}: {status}")

    summary = "\n".join(results)
    send_telegram_message(
        chat_id,
        f"Executed {len(commands)} command(s) on nAIVE:\n{summary}"
    )


def main():
    if not TELEGRAM_TOKEN:
        print("ERROR: Set TELEGRAM_BOT_TOKEN environment variable")
        print("  1. Message @BotFather on Telegram")
        print("  2. Send /newbot and follow prompts")
        print("  3. Copy the token and set it:")
        print("     export TELEGRAM_BOT_TOKEN='your-token-here'")
        sys.exit(1)

    if not ANTHROPIC_KEY:
        print("ERROR: Set ANTHROPIC_API_KEY environment variable")
        sys.exit(1)

    # Verify bot token
    me = telegram_request("getMe")
    if not me.get("ok"):
        print("ERROR: Invalid Telegram bot token")
        sys.exit(1)

    bot_name = me["result"]["username"]
    print(f"nAIVE Telegram Bridge")
    print(f"  Bot: @{bot_name}")
    print(f"  Socket: {NAIVE_SOCKET}")
    print(f"  Send messages to @{bot_name} on Telegram to control nAIVE!")
    print()

    # Long polling loop
    offset = 0
    while True:
        try:
            updates = telegram_request("getUpdates", {
                "offset": offset,
                "timeout": 30,
                "allowed_updates": ["message"],
            })

            if updates.get("ok") and updates.get("result"):
                for update in updates["result"]:
                    offset = update["update_id"] + 1
                    msg = update.get("message", {})
                    text = msg.get("text", "").strip()
                    chat_id = msg.get("chat", {}).get("id")

                    if not text or not chat_id:
                        continue

                    if text.startswith("/start"):
                        send_telegram_message(
                            chat_id,
                            f"nAIVE Engine Control\n\n"
                            f"Send me natural language commands and I'll control the running nAIVE engine in real-time.\n\n"
                            f"Examples:\n"
                            f"  \"make it rain\"\n"
                            f"  \"turn all lights red\"\n"
                            f"  \"add a giant glowing sphere\"\n"
                            f"  \"make everything dark\"\n"
                            f"  \"sunrise\"\n"
                            f"  \"chaos mode\"\n"
                            f"  \"spawn a neon cube at the center\"\n"
                        )
                        continue

                    if text.startswith("/entities"):
                        entities = list_entities()
                        names = [e.get("id", "?") for e in entities]
                        send_telegram_message(chat_id, f"Scene entities ({len(names)}):\n" + "\n".join(names))
                        continue

                    process_message(chat_id, text)

        except KeyboardInterrupt:
            print("\nShutting down.")
            break
        except Exception as e:
            print(f"  [Poll error] {e}")
            time.sleep(2)


if __name__ == "__main__":
    main()
