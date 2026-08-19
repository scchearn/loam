// Entry point. Kept separate from app.mjs so the shell can be booted against a
// test DOM without a module-load side effect (and so index.html needs no inline
// script, which the server's CSP forbids).
import { boot } from './app.mjs';
import { initInspector } from './inspector.mjs';
import { state } from './store.mjs';
import { initChronicle } from './views/chronicle.mjs';
import { initWorkStream } from './views/workstream.mjs';

// The Inspector is a top-level overlay, not part of a view, so it wires itself
// to the static markup once and the area views just call openInspector().
initInspector({ getSnapshot: () => state.snapshot });

// Area views mount before boot() so they catch its first `loam:render`.
initWorkStream({});
initChronicle({});

boot();
