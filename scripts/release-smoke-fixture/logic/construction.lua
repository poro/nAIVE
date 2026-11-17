-- The same hooks run in the packaged headless test, game, and editor.
local checked = false

function init()
    local expected_health = _entity_string_id == "initial_controller" and 55 or 33
    assert(entity.get_health(_entity_string_id) == expected_health,
        "construction omitted authored health")
    if _entity_string_id == "initial_trimesh" then
        physics.set_velocity(_entity_string_id, 0.25, 0, 0)
        local vx, vy, vz = physics.get_velocity(_entity_string_id)
        assert(math.abs(vx - 0.25) < 0.0001 and vy == 0 and vz == 0,
            "on_init ran before the rigid body existed")
        physics.set_velocity(_entity_string_id, 0, 0, 0)
    end
end

function update(dt)
    if checked then return end
    local x, y, z = entity.get_position("initial_trimesh")
    local hit, distance = physics.raycast(x + 1.5, y + 3, z, 0, -1, 0, 5)
    assert(hit and math.abs(distance - 3) < 0.01,
        "scaled trimesh was unavailable to the first update")
    local named_hit, id, named_distance = physics.hitscan(x + 1.5, y + 3, z, 0, -1, 0, 5)
    assert(named_hit and id == "initial_trimesh" and math.abs(named_distance - 3) < 0.01,
        "scaled trimesh hit the wrong entity")
    -- Position-based character bodies intentionally ignore set_velocity.
    local cx, cy, cz = entity.get_position("initial_controller")
    local controller_hit, controller_id, controller_distance =
        physics.hitscan(cx + 1, cy, cz, -1, 0, 0, 2)
    assert(controller_hit and controller_id == "initial_controller"
        and math.abs(controller_distance - 0.7) < 0.01,
        "character capsule was unavailable to the first update")
    events.emit("release.construction_checked." .. _entity_string_id,
        {id = _entity_string_id})
    checked = true
end
