-- Headless tests of the HAVOC prototype (scenes/tier4_havoc.yaml): waves
-- spawn on time, clearing one starts the next, and the horde reaches and
-- hurts a player who stands still. Swarms run on the CPU here.

local function load()
    scene.load("scenes/tier4_havoc.yaml")
    wait_for_event("lifecycle.scene_loaded")
end

function test_the_first_wave_arrives_after_the_countdown()
    load()
    assert(swarm.alive("goblins") == 0, "the arena should start empty")
    assert(wait_for_event("havoc.wave_started", 5), "wave 1 never started")
    assert(get_game_value("wave") == 1, "game.wave should be 1")
    wait_frames(2)
    assert(swarm.alive("goblins") == 1000, "wave 1 should be 1,000 goblins, got " .. swarm.alive("goblins"))
end

function test_clearing_a_wave_rests_then_starts_the_next()
    load()
    assert(wait_for_event("havoc.wave_started", 5), "wave 1 never started")
    wait_frames(2)

    -- Wipe out the wave.
    swarm.damage_sphere("goblins", 0, 0.5, 0, 400, 1000)
    assert(wait_for_event("havoc.wave_cleared", 2), "clearing the swarm should clear the wave")
    assert(get_game_value("kills") == 1000, "1,000 kills expected, got " .. tostring(get_game_value("kills")))

    assert(wait_until(function() return get_game_value("wave") == 2 end, 8), "wave 2 never started")
    wait_frames(2)
    assert(swarm.alive("goblins") == 2500, "wave 2 should be 2,500 goblins, got " .. swarm.alive("goblins"))
end

function test_holding_fire_shoots_the_goblins_coming_at_you()
    load()
    assert(wait_for_event("havoc.wave_started", 5), "wave 1 never started")
    input.inject("attack", "press", nil)
    assert(wait_for_event("swarm.kills", 20), "holding fire never killed a goblin")
    wait_seconds(3)
    input.inject("attack", "release", nil)
    local kills = get_game_value("kills")
    assert(kills > 0, "the director should count the rifle's kills, got " .. tostring(kills))
    log.info("Rifle killed " .. kills .. " goblins")
end

function test_the_slam_clears_the_goblins_around_you()
    load()
    assert(wait_for_event("havoc.wave_started", 5), "wave 1 never started")
    assert(wait_for_event("swarm.hit", 30), "the goblins never reached the player")
    local before = get_game_value("kills")
    input.inject("interact", "press", nil)
    wait_frames(1)
    input.inject("interact", "release", nil)
    wait_frames(3)
    local slain = get_game_value("kills") - before
    assert(slain >= 20, "the slam should kill the goblins packed around the player, killed " .. slain)
    log.info("Slam killed " .. slain .. " goblins")
end

function test_the_horde_hurts_a_player_who_stands_still()
    load()
    assert(wait_for_event("havoc.wave_started", 5), "wave 1 never started")
    assert(wait_for_event("swarm.hit", 30), "the goblins never reached the player")
    local health = scene.find("player"):get("health")
    assert(health.current < 100, "the player should have lost health, has " .. health.current)
end
