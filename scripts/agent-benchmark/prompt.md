Build a small original 3D arcade game called "Orb Harvest" in this nAIVE
project (the current directory, created with `naive init`).

The game:
- The player moves a small craft with W, A, S and D on a flat, walled arena.
- Glowing orbs appear at random places. Touching one scores a point and the
  orb reappears somewhere else.
- A few red hazards drift around the arena. Touching one costs a life; the
  player has three.
- The HUD shows the score and the lives left. When no lives are left it
  shows "GAME OVER" and the final score.

Requirements:
- `scenes/main.yaml` is the default scene and the gameplay is in Lua under
  `logic/`.
- At least three Lua tests in `tests/` check the gameplay (scoring, losing a
  life, game over), and `naive test` passes.
- Start the engine and look at a captured frame to check that the game
  renders sensibly, then stop the engine.

The `naive` command is on PATH. nAIVE's game development guide is at
NAIVE_GUIDE. The engine can be driven over MCP: start it with
`naive --socket NAIVE_SOCKET run &` and the `naive_*` tools (spawn, query,
run_lua, capture_frame, run_tests, screenshot_diff, undo and others) talk to
it. Finish when the tests pass and the game renders.
