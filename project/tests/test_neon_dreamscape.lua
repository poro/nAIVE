-- Neon Dreamscape headless tests

function test_scene_loads()
    scene.load("scenes/neon_dreamscape.yaml")
    wait_for_event("lifecycle.scene_loaded")
    log.info("Neon Dreamscape loaded successfully")
end

function test_has_emissive_cubes()
    scene.load("scenes/neon_dreamscape.yaml")
    wait_for_event("lifecycle.scene_loaded")

    local x, y, z = get_position("emissive_cube_1")
    assert(math.abs(x) < 1, "Emissive cube 1 near center x, got x=" .. x)
    log.info("Emissive cubes present")
end

function test_lights_and_scripts()
    scene.load("scenes/neon_dreamscape.yaml")
    wait_for_event("lifecycle.scene_loaded")

    -- Let scripts run for a bit
    wait_seconds(1.0)
    log.info("Lights and scripts running without errors")
end

function test_camera_orbits()
    scene.load("scenes/neon_dreamscape.yaml")
    wait_for_event("lifecycle.scene_loaded")

    local x1, y1, z1 = get_position("camera")
    wait_seconds(2.0)
    local x2, y2, z2 = get_position("camera")
    local moved = math.abs(x2 - x1) + math.abs(z2 - z1)
    assert(moved > 0.1, "Camera should be orbiting, moved: " .. moved)
    log.info("Camera orbit test passed")
end
