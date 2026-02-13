-- Test script demonstrating lifecycle hooks

self.tick_count = 0
self.total_time = 0

function init()
    log("Test script initialized!")
    self.tick_count = 0
    self.total_time = 0
end

function update(dt)
    self.tick_count = self.tick_count + 1
    self.total_time = self.total_time + dt

    -- Log every 5 seconds
    if self.tick_count % 300 == 0 then
        log(string.format("Script update: %d ticks, %.1fs elapsed", self.tick_count, self.total_time))
    end
end

function on_collision(other_id)
    log("Collision with: " .. tostring(other_id))
end

function on_trigger_enter(other_id)
    log("Entered trigger: " .. tostring(other_id))
end

function on_destroy()
    log("Test script destroyed after " .. tostring(self.tick_count) .. " ticks")
end

function on_reload()
    log("Script hot-reloaded! State preserved: tick_count=" .. tostring(self.tick_count))
end
