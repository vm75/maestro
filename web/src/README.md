# Offline ASUS UI source

The checked-in `web/index.html` is the **generated single-file payload**
embedded by Rust and extracted into an ASUS ASP page. Do not link to these
source fragments at runtime or add a frontend package manager.

Edit `head.html` for CSS, `shell.html` for navigation/layout,
`views/` for the eight existing feature panels, `dialogs.html` for modals,
and `js/` for global functions. Files are concatenated **in the order in
`scripts/build-web-ui.py` into one `<script>`**, preserving inline
`onclick` globals and ASUS firmware integration.

Regenerate and validate before committing:

```sh
python3 scripts/build-web-ui.py
python3 scripts/build-web-ui.py --check
node scripts/test-web-ui.mjs
```

Keep old `data-view`, `view-*` IDs and `?tab=` links working when
changing navigation. Optional capabilities come from protected `/api/modules`
with a static fallback for initial/offline rendering.
