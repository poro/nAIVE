-- The target in scenes/test_swarm.yaml: reports the damage it takes.

function on_damage(amount, source_id)
    events.emit("dummy.damaged", { amount = amount, source = source_id })
end

function on_death()
    events.emit("dummy.died", {})
end
