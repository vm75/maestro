# Linux UI baseline

The Linux UI is based on Server Man commit `c1a9f32f5898146ae7ac339043b910b8e2a2c438` (Svelte 5 / TypeScript). Reuse upstream components wherever possible. Preserve Maestro session setup, module discovery, per-operation capabilities and the existing router UI. Do not import the ASUS theme or modify `web/`.

Shared Nginx, certificates and Ban Shield should follow the corresponding router page information layout but use the Server Man panel, table, dialog and theme primitives. Existing shared REST routes require bearer authentication, while Linux browser sessions use separate origin-checked session authorization. Integrate shared Linux pages only through explicitly scoped authenticated routes with capability enforcement; do not copy router token handling into Linux or grant browser sessions unrestricted access to router mutation routes.

Validate with `npm ci && npm run check && npm run build` in `linux-web`, followed by Linux integration tests against the live Rust API. Compare the original UI's tool arguments/results with Maestro contracts and test both helper-present and helper-absent modes.
