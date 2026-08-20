# Jarvis blueprint

Dit is de ontwerp- en domeinblueprint van PersonalJarvis. Productcode staat
bewust elders: `services/` voor binaries, `crates/` voor Rust-librarycrates,
`apps/` voor clients, `deploy/` voor operatie en `schema/` voor SurrealDB.

Begin bij [de leesvolgorde](00-start/02-reading-order.md). De
repository-ingangspunten en actuele status blijven op de root in `README.md`,
`AGENTS.md`, `STATUS.md`, `TODOS.md` en `STEPS.md`.

`core/Jarvis.md` staat bewust niet hier: het is beschermde
runtime-persona/configuratie die de API en Home Node op dat vaste pad laden.
