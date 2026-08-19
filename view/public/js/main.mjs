// Entry point. Kept separate from app.mjs so the shell can be booted against a
// test DOM without a module-load side effect (and so index.html needs no inline
// script, which the server's CSP forbids).
import { boot } from './app.mjs';

boot();
