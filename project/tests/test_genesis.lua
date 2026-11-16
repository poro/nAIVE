-- GENESIS demo headless test: validates scene loads correctly

function test_genesis_loads()
    scene.load("scenes/genesis.yaml")
    wait_for_event("lifecycle.scene_loaded")

    -- Run a couple of frames so every entity script gets an update.
    wait_frames(2)

    assert(scene.find("camera"), "GENESIS should have a camera entity")
    assert(scene.find("spark_core"), "GENESIS should have its spark_core entity")
    log.info("GENESIS scene loaded")
end
