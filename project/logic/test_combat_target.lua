-- Target for the headless combat test (scenes/test_combat.yaml).

function init()
    self.hits = 0
end

function on_damage(amount, source_id)
    self.hits = self.hits + 1
    -- Calling the entity API from a damage callback has to work: the engine
    -- must not keep the scene borrowed while it calls into scripts.
    local current, max = entity.get_health(_entity_string_id)
    events.emit("target.damaged", {
        amount = amount,
        health = current,
        max = max,
        source = source_id,
        hits = self.hits,
    })
end

function on_death()
    events.emit("target.died", { hits = self.hits })
    entity.destroy(_entity_string_id)
end

function on_destroy()
    events.emit("target.destroyed", {})
end
