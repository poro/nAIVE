function init()
    events.emit("release.spawn_initialized", { id = _entity_string_id })
    events.on("release.ping", function()
        events.emit("release.ghost", { id = _entity_string_id })
    end)
end

function on_destroy()
    entity.set_position(_entity_string_id, 0, 1, 0)
end
