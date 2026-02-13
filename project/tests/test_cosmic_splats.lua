-- Cosmic Splats headless tests

function test_scene_loads()
    scene.load("scenes/cosmic_splats.yaml")
    wait_for_event("lifecycle.scene_loaded")
    log.info("Cosmic Splats scene loaded successfully")
end

function test_has_crystal()
    scene.load("scenes/cosmic_splats.yaml")
    wait_for_event("lifecycle.scene_loaded")

    local x, y, z = get_position("crystal")
    assert(y > 0.5, "Crystal floating above pedestal, got y=" .. y)
    log.info("Crystal entity present")
end

function test_camera_orbits()
    scene.load("scenes/cosmic_splats.yaml")
    wait_for_event("lifecycle.scene_loaded")

    local x1, y1, z1 = get_position("camera")
    wait_seconds(2.0)
    local x2, y2, z2 = get_position("camera")
    local moved = math.abs(x2 - x1) + math.abs(z2 - z1)
    assert(moved > 0.1, "Camera should be orbiting, moved: " .. moved)
    log.info("Camera orbit test passed")
end
