function init()
    events.on("release.destroyed", function()
        events.emit("release.stale_event", {})
    end)
    events.emit("release.initialized", {})
end

function update(dt)
    entity.set_rotation(_entity_string_id, 0, 30, 0)
end

function on_destroy()
    -- Scene transitions must release the world borrow before callbacks run.
    entity.set_position(_entity_string_id, 0, 1, 0)
    events.emit("release.destroyed", { id = _entity_string_id })
end
