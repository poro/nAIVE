function on_collision(other_id)
    -- Play collision sound with slight volume randomization
    local vol = 0.3 + math.random() * 0.4
    audio.play_sfx("hit_" .. tostring(math.random(1000)), "assets/audio/collision.wav", vol)
end
