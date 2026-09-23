/**
 * Click-and-drag horizontal scrolling for overflow-x strips (`.card-row`).
 *
 * A native overflow container only pans via wheel/trackpad; a mouse user has no
 * visible way to move it. This lets them grab the strip and drag it. Delegation
 * on the document means it covers strips created by any later render without
 * per-element wiring.
 *
 * Mouse only: touch and trackpad already pan natively, so we leave those alone.
 * A small movement threshold keeps ordinary clicks on cards and links working;
 * only a real drag swallows the trailing click.
 */
const THRESHOLD = 5;

export function initDragScroll({ root = document, selector = '.card-row' } = {}) {
  let drag = null;

  root.addEventListener('pointerdown', (event) => {
    if (event.pointerType !== 'mouse' || event.button !== 0) return;
    const strip = event.target.closest?.(selector);
    if (!strip || strip.scrollWidth <= strip.clientWidth) return;
    drag = { strip, startX: event.clientX, startLeft: strip.scrollLeft, moved: false };
  });

  root.addEventListener('pointermove', (event) => {
    if (!drag) return;
    const dx = event.clientX - drag.startX;
    if (!drag.moved && Math.abs(dx) < THRESHOLD) return;
    if (!drag.moved) {
      drag.moved = true;
      drag.strip.classList.add('is-dragging');
    }
    drag.strip.scrollLeft = drag.startLeft - dx;
    event.preventDefault();
  });

  const end = () => {
    if (!drag) return;
    const { strip, moved } = drag;
    drag = null;
    strip.classList.remove('is-dragging');
    if (!moved) return;
    // A real drag ends on a `click` we must not let a link/button under the
    // pointer act on. Swallow exactly that one click, then clean up.
    const swallow = (event) => { event.stopPropagation(); event.preventDefault(); };
    strip.addEventListener('click', swallow, { capture: true, once: true });
    setTimeout(() => strip.removeEventListener('click', swallow, { capture: true }), 0);
  };
  root.addEventListener('pointerup', end);
  root.addEventListener('pointercancel', end);
}
