function init()
    events.emit("release.initialized", {})
end

function update(dt)
    entity.set_rotation(_entity_string_id, 0, 30, 0)
end
