-- PBR Material Gallery headless tests
--
-- The scene is a ball-drop demo: fifteen PBR balls start in the air above a
-- walled floor and fall under gravity.

function test_scene_loads()
    scene.load("scenes/pbr_gallery.yaml")
    wait_for_event("lifecycle.scene_loaded")
    log.info("PBR Gallery scene loaded successfully")
end

function test_has_balls()
    scene.load("scenes/pbr_gallery.yaml")
    wait_for_event("lifecycle.scene_loaded")

    local x, y, z = get_position("ball_01")
    assert(y > 10, "ball_01 should start high above the floor, got y=" .. y)
    local x, y, z = get_position("ball_15")
    assert(y > 1, "ball_15 should start above the floor, got y=" .. y)
    log.info("Ball grid exists")
end

function test_balls_fall_and_land()
    scene.load("scenes/pbr_gallery.yaml")
    wait_for_event("lifecycle.scene_loaded")

    -- Every ball is in free fall for the first half second: the lowest one
    -- needs about 0.7 s to reach the floor.
    local _, top_start = get_position("ball_01")
    wait_seconds(0.5)
    local _, top_now = get_position("ball_01")
    local dropped = top_start - top_now
    assert(dropped > 1.0 and dropped < 1.5,
        "ball_01 should fall about 1.2 m in 0.5 s, fell " .. dropped)

    -- By one second the lowest ball has reached the floor (top surface at
    -- y = -0.7) and must have bounced off it rather than passed through.
    wait_seconds(0.5)
    local _, low = get_position("ball_15")
    assert(low > -0.7, "ball_15 fell through the floor, y=" .. low)
    assert(low < 2.0, "ball_15 should be below its start height, y=" .. low)
    log.info("Ball physics test passed")
end
