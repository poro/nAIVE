# init-0.1.22 — Product Requirements Document

## 1. Overview

**Project:** init-0.1.22
**Engine:** nAIVE (AI-native game engine)
**Version:** 0.1.0

### Vision
<!-- What is this game? One paragraph that captures the core experience. -->

### Target Audience
<!-- Who is this game for? -->

## 2. Core Gameplay

### Game Loop
<!-- Describe the primary gameplay loop: what does the player do repeatedly? -->

### Win/Loss Conditions
<!-- How does the player win? How do they lose? -->

### Controls
<!-- List the key actions and what they do. These map to input/bindings.yaml. -->

| Action | Default Key | Description |
|--------|------------|-------------|
| move_forward | W | Move forward |
| move_backward | S | Move backward |
| move_left | A | Strafe left |
| move_right | D | Strafe right |
| jump | Space | Jump |
| interact | E | Interact with objects |

## 3. Scenes

### Scene List
<!-- List all planned scenes/levels. -->

| Scene | File | Description |
|-------|------|-------------|
| Main | `scenes/main.yaml` | Default starting scene |

## 4. Entities & Components

### Entity Catalog
<!-- List the key entities in the game. -->

| Entity | Components | Description |
|--------|-----------|-------------|
| main_camera | transform, camera | Player camera |
| sun | transform, point_light | Scene lighting |

## 5. Events

### Event Catalog
<!-- List game events. These map to events/schema.yaml. -->

| Event | Fields | Description |
|-------|--------|-------------|
| lifecycle.scene_loaded | — | Scene finished loading |

## 6. Art & Audio

### Visual Style
<!-- Describe the visual direction. -->

### Audio
<!-- List key sounds and music tracks needed. -->

## 7. Milestones

- [ ] **v0.1.0** — Prototype: basic scene loads and runs
- [ ] **v0.2.0** — Core mechanics implemented
- [ ] **v0.3.0** — Content complete
- [ ] **v1.0.0** — Release
