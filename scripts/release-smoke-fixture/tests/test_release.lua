function test_packaged_scene_and_script()
    scene.load("scenes/main.yaml")
    assert(wait_for_event("lifecycle.scene_loaded"), "packaged scene did not load")
    assert(scene.find("smoke_cube"), "packaged entity missing")
    assert(event_occurred("release.initialized"), "packaged init script did not run")
    local x, y, z = get_position("smoke_cube")
    assert(x == 0 and y == 1 and z == 0, "packaged transform changed")
end
