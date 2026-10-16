# Texture assets

PBR texture sets under `textures/<category>/<set>/`, each providing three maps with
canonical names consumed by `src/materials.gd`:

- `albedo.jpg` — base color
- `normal.jpg` — OpenGL-style tangent-space normal map
- `roughness.jpg` — roughness (red channel)

Swapping a set = replace the three files in a set directory (or point the registry
entry in `src/materials.gd` at a new directory). 1K JPGs are committed for a lean
repo; drop in 2K/4K variants of the same set for higher fidelity.

| Registry key | Directory | Source |
|---|---|---|
| asphalt | roads/asphalt_02 | PolyHaven "asphalt_02" |
| sidewalk | sidewalks/concrete_floor_02 | PolyHaven "concrete_floor_02" |
| facade | facades/concrete_panels | PolyHaven "concrete_panels" |
| metal | metal/metal_plate | PolyHaven "metal_plate" |

All sets from https://polyhaven.com — CC0 1.0 Universal public domain, no
attribution required (recorded here anyway). Downloaded 2026-10-16 via the
PolyHaven API at 1K JPG quality.
