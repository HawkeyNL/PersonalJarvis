# 3D Scene

## Aanbevolen technologie

Voor de Tauri/Vue-client:

- Three.js;
- bij voorkeur via een dunne eigen Vue-componentlaag;
- geen zware game-engine nodig;
- WebGL, later optioneel WebGPU;
- Canvas 2D fallback voor oudere of mobiele apparaten.

## Scenegraph

```text
Scene
├── JarvisCore
├── DomainOrbitGroup[]
│   ├── AgentNode[]
│   │   └── SubAgentNode[]
├── ToolNode[]
├── MessageParticle[]
├── ConnectionLine[]
├── AlertLayer
└── CameraRig
```

## Positiebepaling

Gebruik geen volledig willekeurige force-layout in 3D. Posities moeten stabiel blijven.

- domein bepaalt orbit;
- agent-ID bepaalt vaste hoek;
- status beïnvloedt alleen kleine animaties;
- tools staan in een buitenste ring;
- externe markten/brokers in een aparte perimeter.

Daardoor leert de gebruiker waar alles staat.

## Performance

- instanced meshes voor agents en particles;
- maximaal zichtbaar particle-budget;
- events aggregeren bij hoge frequentie;
- geen volledige streaming tokens animeren;
- DOM/tickdata niet per event visualiseren;
- requestAnimationFrame;
- quality modes: low, balanced, high;
- iPhone standaard low/balanced.

## Mobiel

Op iPhone:

- simplified 3D;
- minder particles;
- geen zware postprocessing;
- 30 FPS target;
- mogelijkheid om naar 2D graph mode te schakelen;
- batterijbesparingsmodus.
