-- Headless swarm tests: spawning, damage volumes, kills, attacks on the
-- target and the swarm events. The test runner steps swarms on the CPU with
-- the same maths as the GPU shaders, and reports counts on the same frame.
--
-- scenes/test_swarm.yaml: "horde" holds 300 members with 10 health each,
-- none alive at the start; they chase "dummy" (100 health) and hit it for 2
-- every half second from within 1.8 m.

local function load()
    scene.load("scenes/test_swarm.yaml")
    wait_for_event("lifecycle.scene_loaded")
end

function test_spawning_brings_members_to_life()
    load()
    assert(swarm.capacity("horde") == 300, "capacity should be 300, got " .. swarm.capacity("horde"))
    assert(swarm.alive("horde") == 0, "no member should be alive at the start")

    swarm.spawn("horde", 120, 30, 0, 0, 6)
    assert(swarm.alive("horde") == 120, "a requested spawn counts at once, got " .. swarm.alive("horde"))
    wait_frames(2)
    assert(swarm.alive("horde") == 120, "120 should be alive after spawning, got " .. swarm.alive("horde"))

    -- Asking for more than the pool holds fills it.
    swarm.spawn("horde", 1000, -30, 0, 0, 10)
    wait_frames(2)
    assert(swarm.alive("horde") == 300, "the pool should be full, got " .. swarm.alive("horde"))
end

function test_damage_wounds_then_kills_and_reports()
    load()
    swarm.spawn("horde", 50, 30, 0, 0, 4)
    wait_frames(2)

    -- Half their health: wounded, nobody dies.
    swarm.damage_sphere("horde", 30, 0.5, 0, 6, 5)
    wait_frames(2)
    assert(not event_occurred("swarm.kills"), "5 damage should not kill members with 10 health")
    assert(swarm.alive("horde") == 50)

    -- The same again finishes those it covered.
    swarm.damage_sphere("horde", 30, 0.5, 0, 12, 5)
    assert(wait_for_event("swarm.kills", 1), "no swarm.kills event")
    assert(event_occurred("swarm.kills", { swarm = "horde" }))
    wait_frames(1)
    assert(swarm.alive("horde") == 0, "all 50 should be dead, " .. swarm.alive("horde") .. " alive")
    assert(swarm.kills("horde") == 50, "50 kills expected, got " .. swarm.kills("horde"))
    assert(event_occurred("swarm.cleared", { swarm = "horde" }), "no swarm.cleared event")
end

function test_a_line_kills_only_along_its_length()
    load()
    swarm.spawn("horde", 60, 40, 0, 0, 3)
    wait_frames(2)
    -- A shot across the group's middle, from well outside it.
    swarm.damage_line("horde", 40, 0.5, -20, 40, 0.5, 20, 0.5, 50)
    assert(wait_for_event("swarm.kills", 1), "the line killed nothing")
    wait_frames(1)
    local alive = swarm.alive("horde")
    assert(alive > 0 and alive < 60, "a 1 m wide line through a 6 m group should kill some, not all: " .. alive .. " alive")
end

function test_members_attack_their_target()
    load()
    swarm.spawn("horde", 40, 12, 0, 0, 3)
    assert(wait_for_event("dummy.damaged", 6), "the horde never hurt the dummy")
    assert(event_occurred("swarm.hit", { swarm = "horde", target = "dummy" }), "no swarm.hit event naming the dummy")
    local health = scene.find("dummy"):get("health")
    assert(health.current < 100, "the dummy should have lost health, has " .. health.current)
    assert(wait_for_event("dummy.died", 20), "the horde never killed the dummy")
end

function test_a_new_target_draws_the_horde_away()
    load()
    swarm.spawn("horde", 40, 12, 0, 0, 3)
    assert(wait_for_event("dummy.damaged", 6), "the horde never reached the dummy")

    swarm.set_target("horde", 50, 0, 50)
    wait_seconds(3)
    local before = scene.find("dummy"):get("health").current
    wait_seconds(2)
    local after = scene.find("dummy"):get("health").current
    assert(after == before, "the dummy kept taking damage after the horde left: " .. before .. " -> " .. after)

    swarm.follow("horde", "dummy")
    wait_seconds(12)
    assert(scene.find("dummy"):get("health").current < after, "following the dummy again should hurt it")
end

function test_unknown_swarms_are_errors()
    load()
    local ok, err = pcall(swarm.spawn, "goblins", 10, 0, 0, 0, 5)
    assert(not ok and tostring(err):find("no swarm with id 'goblins'"), "spawning into a missing swarm should fail: " .. tostring(err))
    ok, err = pcall(swarm.follow, "horde", "nobody")
    assert(not ok and tostring(err):find("no entity with id 'nobody'"), "following a missing entity should fail: " .. tostring(err))
end
