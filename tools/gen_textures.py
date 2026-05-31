#!/usr/bin/env python3
"""Generate 5 procedural 64x64 textures as PNG files using only Python stdlib."""

import struct
import zlib
import os
import random

OUTPUT_DIR = os.path.join(os.path.dirname(__file__), "..", "project", "assets", "textures")
SIZE = 64

def make_png(width, height, pixels):
    """Create a PNG file from a list of (r, g, b) tuples, row-major order."""
    def chunk(chunk_type, data):
        c = chunk_type + data
        crc = struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)
        return struct.pack(">I", len(data)) + c + crc

    header = b"\x89PNG\r\n\x1a\n"
    ihdr = chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))

    raw = b""
    for y in range(height):
        raw += b"\x00"  # filter: none
        for x in range(width):
            r, g, b = pixels[y * width + x]
            raw += struct.pack("BBB", clamp(r), clamp(g), clamp(b))

    idat = chunk(b"IDAT", zlib.compress(raw))
    iend = chunk(b"IEND", b"")
    return header + ihdr + idat + iend


def clamp(v):
    return max(0, min(255, int(v)))


def gen_grass():
    random.seed(42)
    pixels = []
    for y in range(SIZE):
        for x in range(SIZE):
            base_g = 120 + random.randint(-20, 20)
            base_r = 50 + random.randint(-15, 15)
            base_b = 30 + random.randint(-10, 10)
            # Occasional darker patch
            if random.random() < 0.15:
                base_g -= 30
                base_r -= 10
            pixels.append((base_r, base_g, base_b))
    return pixels


def gen_stone():
    random.seed(43)
    pixels = []
    for y in range(SIZE):
        for x in range(SIZE):
            base = 140 + random.randint(-15, 15)
            # Darker speckles
            if random.random() < 0.2:
                base -= 40 + random.randint(0, 20)
            # Lighter spots
            if random.random() < 0.08:
                base += 30
            pixels.append((base, base, base + random.randint(-5, 5)))
    return pixels


def gen_wood():
    random.seed(44)
    pixels = []
    for y in range(SIZE):
        # Horizontal grain: base color varies by row
        grain = 10 * (((y * 7) % 13) - 6) // 6
        for x in range(SIZE):
            r = 140 + grain + random.randint(-8, 8)
            g = 90 + grain + random.randint(-8, 8)
            b = 50 + grain // 2 + random.randint(-5, 5)
            # Occasional dark grain line
            if (y + (x // 16)) % 8 == 0:
                r -= 25
                g -= 20
                b -= 10
            pixels.append((r, g, b))
    return pixels


def gen_water():
    random.seed(45)
    import math
    pixels = []
    for y in range(SIZE):
        for x in range(SIZE):
            wave = math.sin(x * 0.3 + y * 0.15) * 20 + math.sin(y * 0.5) * 10
            r = 30 + int(wave * 0.3) + random.randint(-5, 5)
            g = 80 + int(wave * 0.5) + random.randint(-5, 5)
            b = 180 + int(wave) + random.randint(-8, 8)
            pixels.append((r, g, b))
    return pixels


def gen_snow():
    random.seed(46)
    pixels = []
    for y in range(SIZE):
        for x in range(SIZE):
            noise = random.randint(-12, 12)
            r = 235 + noise
            g = 240 + noise
            b = 250 + noise // 2
            # Subtle blue-ish shadows
            if random.random() < 0.1:
                r -= 15
                g -= 10
            pixels.append((r, g, b))
    return pixels


def main():
    os.makedirs(OUTPUT_DIR, exist_ok=True)
    textures = {
        "grass.png": gen_grass,
        "stone.png": gen_stone,
        "wood.png": gen_wood,
        "water.png": gen_water,
        "snow.png": gen_snow,
    }
    for name, gen_fn in textures.items():
        pixels = gen_fn()
        png_data = make_png(SIZE, SIZE, pixels)
        path = os.path.join(OUTPUT_DIR, name)
        with open(path, "wb") as f:
            f.write(png_data)
        print(f"  wrote {path} ({len(png_data)} bytes)")
    print("Done.")


if __name__ == "__main__":
    main()
