-- Explicitly executed after the native engine has saved roundtrip.yaml.
function test_saved_gameplay_components_reload()
    scene.load("scenes/roundtrip.yaml")
    local actor = assert(scene.find("smoke_cube"), "saved entity missing")
    local health = assert(actor:get("health"), "saved health missing")
    assert(health.current == 73 and health.max == 100, "saved live health changed")
    assert(event_occurred("release.initialized"), "saved script did not initialize")
end
