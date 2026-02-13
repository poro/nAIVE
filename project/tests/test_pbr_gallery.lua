-- PBR Material Gallery headless tests

function test_scene_loads()
    scene.load("scenes/pbr_gallery.yaml")
    wait_for_event("lifecycle.scene_loaded")
    log.info("PBR Gallery scene loaded successfully")
end

function test_has_spheres()
    scene.load("scenes/pbr_gallery.yaml")
    wait_for_event("lifecycle.scene_loaded")

    local x, y, z = get_position("sphere_m0_r0")
    assert(math.abs(y) < 1, "First sphere near ground, got y=" .. y)
    log.info("Sphere grid exists")
end

function test_camera_orbits()
    scene.load("scenes/pbr_gallery.yaml")
    wait_for_event("lifecycle.scene_loaded")

    local x1, y1, z1 = get_position("camera")
    wait_seconds(2.0)
    local x2, y2, z2 = get_position("camera")
    local moved = math.abs(x2 - x1) + math.abs(z2 - z1)
    assert(moved > 0.1, "Camera should be orbiting, moved: " .. moved)
    log.info("Camera orbit test passed")
end
