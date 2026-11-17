-- tests/test_basic.lua
-- Basic automated tests for the game project.

function test_scene_loads()
    scene.load("scenes/main.yaml")
    wait_for_event("lifecycle.scene_loaded")

    local camera = scene.find("main_camera")
    assert(camera ~= nil, "Main camera must exist")

    local floor = scene.find("floor")
    assert(floor ~= nil, "Floor entity must exist")

    log.info("Scene loads test passed!")
end

function test_welcome_cube_exists()
    scene.load("scenes/main.yaml")
    wait_for_event("lifecycle.scene_loaded")

    local cube = scene.find("welcome_cube")
    assert(cube ~= nil, "Welcome cube must exist")

    -- Wait a moment for scripts to init
    wait_frames(2)

    log.info("Welcome cube test passed!")
end
