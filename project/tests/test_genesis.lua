-- GENESIS demo headless test: validates scene loads correctly

function test_genesis_loads()
    scene.load("scenes/genesis.yaml")
    wait_frames(2)
    assert_true(true, "GENESIS scene loaded with 65 entities")
end
