-- Headless combat tests: collision damage, on_damage, death, and destruction.
--
-- scenes/test_combat.yaml drops two weights onto a target with 30 health.
-- The light one deals 10 damage after about 0.6 s; the heavy one deals 100
-- after about 1.5 s.

function test_collision_damage_hurts_the_target()
    scene.load("scenes/test_combat.yaml")
    wait_for_event("lifecycle.scene_loaded")

    local health = scene.find("target"):get("health")
    assert(health.current == 30 and health.max == 30,
        "target should start at 30 of 30 health, got " .. health.current .. " of " .. health.max)

    assert(wait_for_event("target.damaged", 2), "the light weight never damaged the target")

    health = scene.find("target"):get("health")
    assert(health.current == 20, "one 10-damage hit should leave 20 health, got " .. health.current)
    assert(not health.dead, "the target should survive the first hit")
    assert(not event_occurred("target.died"), "the target should not have died yet")
    log.info("Collision damage applied and on_damage ran")
end

function test_destroy_on_hit_removes_the_damager()
    scene.load("scenes/test_combat.yaml")
    wait_for_event("lifecycle.scene_loaded")

    assert(scene.find("light_weight"), "the light weight should exist at the start")
    assert(wait_for_event("target.damaged", 2), "the light weight never damaged the target")
    wait_frames(1)

    assert(scene.find("light_weight") == nil, "a destroy_on_hit damager should be gone after it hits")
    assert(scene.find("heavy_weight"), "the heavy weight should still be falling")
    log.info("destroy_on_hit removed the damager")
end

function test_lethal_damage_kills_and_destroys()
    scene.load("scenes/test_combat.yaml")
    wait_for_event("lifecycle.scene_loaded")

    assert(wait_for_event("target.died", 4), "the heavy weight never killed the target")
    wait_frames(1)

    -- on_death destroys the target, which runs on_destroy and removes it.
    assert(event_occurred("target.destroyed"), "on_destroy should have run for the target")
    assert(scene.find("target") == nil, "the destroyed target should be gone")
    assert(scene.find("heavy_weight") == nil, "the heavy weight should be gone after its hit")
    log.info("Lethal damage ran on_death, and the entity was destroyed")
end
