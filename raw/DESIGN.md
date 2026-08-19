---
name: Loam View
status: canonical
source: mockups/loam-view.html
extracted_at: 2026-07-16
note: >
  CANONICAL design system for Loam View. Originally extracted from the static mockup
  (mockups/loam-view.html), which is now subordinate reference material and must be
  updated to conform when it disagrees. All implementation work MUST follow this system; the
  Loam View spec (specs/loam-view.md) references it as authoritative. Colors are
  authored in OKLCH (the source of truth); hex values are approximate references
  only. Living in raw/ for now; promote when the design system is formally adopted.
colors:
  # Surfaces (dark, cool)
  surface-app: "oklch(16% 0.004 260)"        # ~#111214 — app background / chrome
  surface-app-raised: "oklch(20% 0.004 260)" # ~#1a1b1e
  surface-app-active: "oklch(25% 0.005 260)" # ~#242529
  surface-canvas: "oklch(16.5% 0.004 260)"      # ~#121316 — workspace background
  surface-card: "oklch(20% 0.004 260)"   # ~#1a1b1e — card / panel fill
  surface-inset: "oklch(23.5% 0.005 260)" # ~#212226 — inset / chip fill
  surface-critical: "oklch(24% 0.05 25)"       # ~#2e1c1a — critical-card tint
  # Ink (text)
  ink-primary: "oklch(96% 0 0)"                # ~#f4f4f5 — headings, values
  ink-secondary: "oklch(70% 0.004 260)"        # ~#a1a1aa — body, descriptions
  ink-tertiary: "oklch(56% 0.004 260)"         # ~#78787f — labels, meta
  ink-chrome: "oklch(94% 0 0)"                 # ~#efeff0 — text on app chrome (topbar, inspector)
  ink-chrome-muted: "oklch(64% 0.004 260)"     # ~#8f8f97 — muted text on chrome
  ink-muted: "oklch(70% 0.006 260)"            # ~#a1a1a8 — quiet labels / links / dates (was accent-copper-dark)
  # Borders / rules (white-alpha)
  border: "oklch(100% 0 0 / 0.09)"         # 1px card borders / dividers
  border-faint: "oklch(100% 0 0 / 0.06)"   # faint dividers / dot-grid
  border-chrome: "oklch(100% 0 0 / 0.09)"  # border on app chrome (topbar, nav, inspector)
  border-critical: "oklch(40% 0.09 25 / 0.5)"    # critical-card border
  # Overlay elevation (Inspector/Reader/Query scrim + shadow; pure black at alpha)
  scrim: "oklch(0% 0 0 / 0.55)"                  # dimming scrim behind overlays
  shadow-overlay: "0 8px 22px oklch(0% 0 0 / 0.55)"  # overlay elevation shadow
  # Severity badge borders without a named token derive from their state color:
  # color-mix(in oklch, var(--state-*) 45%, transparent). Critical keeps border-critical.
  # Accent (single, restrained brand green)
  accent: "oklch(74% 0.15 158)"                # ~#3ecf8e — active nav, links, sparkle, primary status
  accent-bright: "oklch(76% 0.15 158)"         # ~#43d494 — icon-on-fill highlight
  accent-soft: "oklch(34% 0.06 158)"           # ~#1e3b2f — accent tint fill/border
  # Semantic status
  state-healthy: "oklch(74% 0.14 158)"         # ~#3ecf8e green — healthy/ready/sealed
  state-watch: "oklch(80% 0.13 82)"            # ~#e6c35c amber — watch/pending
  state-drift: "oklch(66% 0.18 25)"            # ~#ea5f57 red — critical/drift/missing
  state-muted: "oklch(60% 0.006 260)"          # ~#828289 neutral
  # Graph (Atlas) node + edge kinds
  node-code: "oklch(68% 0.11 244)"             # ~#6ba3e0 slate/blue
  node-concept: "oklch(74% 0.14 158)"          # ~#3ecf8e green
  node-work: "oklch(72% 0.13 55)"              # ~#d89a6a copper/orange
  node-memory: "oklch(74% 0.09 80)"            # ~#d3ba7f amber
  edge-explicit: "oklch(66% 0.09 247)"         # ~#6f9fd6 solid link
  edge-derived: "oklch(72% 0.11 70)"           # ~#cba86a dashed derived

fonts:
  sans: '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif'  # display + body
  mono: '"SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace'                              # labels, evidence, paths, counts

type_scale:
  view-title:   { size: "1.5rem",                          weight: 600, tracking: "-0.02em",  line: 1.2,  font: sans }
  h1:           { size: "clamp(1.2rem, 1.5vw, 1.45rem)",   weight: 600, tracking: "-0.008em", line: 1.25, font: sans }
  h2-card:      { size: "1rem–1.15rem",                    weight: 600, line: 1.25,           font: sans }
  metric-value: { size: "clamp(1.5rem, 1.9vw, 1.9rem)",    weight: 600, line: 1,   numeric: "tabular-nums", font: sans }  # data is the hero
  body:         { size: "0.82rem–0.92rem",                 weight: 400, line: 1.5,            font: sans, color: ink-secondary }
  label:        { size: "0.6rem–0.66rem",                  weight: "500–600", line: 1, transform: uppercase, tracking: "0.05em–0.1em", font: mono, color: ink-tertiary }
  badge:        { size: "0.66rem",                         weight: 600, line: 1, transform: uppercase, tracking: "0.05em", font: mono }
  code-chip:    { size: "0.78em",                          font: mono }

spacing:            # 0.25rem base grid
  space-1: "0.25rem"
  space-2: "0.5rem"
  space-3: "0.75rem"
  space-4: "1rem"
  space-6: "1.5rem"
  space-8: "2rem"
  space-12: "3rem"
  space-16: "4rem"

radii:
  sm: "7px"    # buttons, chips, tiles, sparkle button
  md: "10px"   # cards
  lg: "14px"   # large panels, atlas stage, dialog

borders:
  hairline-width: "1px"
  hairline: "oklch(100% 0 0 / 0.09)"        # standard card/panel border + divider
  hairline-faint: "oklch(100% 0 0 / 0.06)"  # faint dividers + dot-grid texture

layout:
  nav-width: "3.5rem"          # icon rail
  topbar-height: "3.5rem"
  inspector-width: "24rem"     # right modal side sheet
  content-max-width: "1440px"  # .view caps + centers. Chrome (topbar) stays full-bleed; Reader exempt (own surface)
  content-block-padding: "1.5rem"                       # var(--space-6)
  content-inline-padding: "clamp(1.25rem, 2.5vw, 2.25rem)"
  band-gap: "2rem"             # var(--space-8) between bands
  card-row-min-col: "16rem"    # grid-auto-columns minmax(16rem, 1fr), horizontal scroll
  dot-grid: "radial 22px"      # overview box + atlas stage texture

breakpoints:
  tablet: "72rem"   # rail narrows
  mobile: "48rem"   # rail -> bottom tab bar; overlays full-width
  small: "40rem"    # tiles 2-col, issue grid 1-col

motion:
  fast: "140ms"
  easing: "linear / ease"
  reduced-motion: respected

focus_ring: "2px solid accent, 3px offset"
color_scheme: dark   # dark theme only

components:
  icon:
    set: "Phosphor (regular)"
    delivery: "inline SVG <symbol> sprite (no external dependency)"
    color: currentColor
    sizing: "em relative to label; all-caps chips size icon to cap height"
  badge:
    shape: "pill (radius 999px)"
    typography: "0.66rem mono uppercase, 0.05em tracking"
    padding: "0.34rem 0.75rem 0.26rem"     # slight optical bias for all-caps
    height: "~22px"
    variants: [critical, watch, healthy, neutral]   # colored text + translucent border
  sparkle-button:            # copy-prompt: paste-ready agent prompt
    shape: "rounded square, radius sm"
    fill: surface-inset
    icon-color: accent
    size-in-card: "1.4rem (matches badge height)"
    size-beside-text-button: "1.9rem (matches ghost button height)"
    success-state: "swaps to green seal (i-seal), .copied green, tooltip 'Copied'"
    tooltip: "styled ::after, dark rounded, below-right, on hover/focus"
  ghost-button:
    shape: "rounded rect, radius sm"
    border: "1px hairline"
    fill: surface-card
    typography: "0.78rem sans, ink-primary"
    hover: "fill -> surface-inset"
  pill:
    shape: "rounded rect, radius sm"
    border: "1px hairline"
    fill: surface-card
    typography: "0.75rem sans, ink-secondary"
  card:
    fill: surface-card
    border: "1px hairline"
    radius: "md (10px) or lg (14px)"
    padding: "space-4 to space-6"
    shadow: "none in flow; elevation reserved for overlays"
  band:
    structure: "header (drag-handle + title + optional pill/action) over card-row or card-grid"
    gap: "space-8 between bands"
  metric-card:
    min-height: "9rem"
    content: "mono label + tabular metric value + colored-dot sub-stats"
  advisor-card:
    header: "category (icon+label) left; [severity badge + sparkle] right"
    body: "optional metric + title + description with inline code chips"
    critical-variant: "surface-critical fill + border-critical border"
  tile:
    shape: "rounded rect, radius md"
    icon-box: "2rem rounded-sm, surface-inset fill"
    content: "icon + mono label + sans value"
    container: "grouped inside one dotted-background overview box"
  nav-rail:
    width: "3.5rem"
    items: "icon-only Phosphor glyphs in rounded squares"
    active: "surface-app-active backing + green glyph"
    mobile: "bottom tab bar with text labels"
  topbar:
    height: "3.5rem"
    content: "brand, workspace/branch breadcrumb, freshness, Cmd+K search"
  inspector:
    type: "full-height right-side modal side sheet"
    width: "24rem"
    backdrop: "dimming scrim over app"
    close: "close control / Escape / outside-click"
    chrome: "shell surfaces, ink-chrome text, green section headings"
  reader:
    type: "full-screen in-app document surface"
    controls: "own Back; sticky 'On this page' outline; collapsible front matter"
    links: "resolvable wikilinks green, broken red; paths as code chips"
---

# Design System: Loam View

> **Canonical.** This document is the authoritative design system for Loam View,
> extracted from the approved mockup `mockups/loam-view.html`. Implementation work
> defined by `specs/loam-view.md` must conform to these tokens, components, and
> principles. When the mockup and this document disagree, this document wins:
> update the mockup to conform. The mockup is reference implementation evidence
> only and never overrides this design system.

## 1. Visual Theme & Atmosphere

Loam View is a **calm, dark, operational knowledge dashboard** — closer to a
developer-tooling console (Supabase / Linear / Vercel family) than to a website,
a graph toy, or an art-directed brochure. The mood is precise and evidence-first:
a near-black cool-grey canvas, content organized into quiet bordered cards and
labeled "bands," a single restrained green accent, and small monospace labels that
read like instrument markings. Warmth comes from calibrated dark tones and the
green accent rather than from color or paper — an earlier warm-paper "Living
Archive" direction was deliberately retired in favor of this.

Density is **daily-app balanced**: information is grouped and scannable, never a
wall of widgets and never a hero-driven landing page. Every screen keeps evidence
and next actions above decoration; provenance is always one interaction away
(the Inspector) and the interface itself is never the main event. Typography is
sans for reading and mono for evidence — no serif, no oversized hero type; view
titles read as compact section headers, and the largest type is reserved for the
data itself (metric numbers).

## 2. Color Palette & Roles

Colors are authored in **OKLCH** (the canonical source); hex values below are
approximate references. The system is nearly monochrome — dark cool-grey surfaces
and greyscale ink — with a single green accent used sparingly and a three-color
severity set.

### Primary Foundation
- **App Charcoal** `oklch(16% 0.004 260)` (~#111214) — application background and chrome (topbar, nav rail).
- **Workspace Charcoal** `oklch(16.5% 0.004 260)` (~#121316) — the scrolling workspace background.
- **Card Slate** `oklch(20% 0.004 260)` (~#1a1b1e) — card, panel, and tile fill (also raised chrome).
- **Inset Slate** `oklch(23.5% 0.005 260)` (~#212226) — insets, code chips, icon-button fill.
- **Critical Wash** `oklch(24% 0.05 25)` (~#2e1c1a) — tint fill for critical-severity cards.

### Accent & Interactive
- **Loam Green** `oklch(74% 0.15 158)` (~#3ecf8e) — the single brand accent: active nav icon, links, the "sparkle" copy-prompt action, primary/healthy status, focus ring. Used sparingly.
- **Green Highlight** `oklch(76% 0.15 158)` (~#43d494) — icon color when sitting on a green-soft fill.
- **Green Soft** `oklch(34% 0.06 158)` (~#1e3b2f) — accent tint fill / soft borders (badges, panel icon backing).

### Typography & Text Hierarchy
- **Ink Primary** `oklch(96% 0 0)` (~#f4f4f5) — headings, view titles, metric values, strong labels.
- **Ink Secondary** `oklch(70% 0.004 260)` (~#a1a1aa) — body copy, card descriptions, band titles.
- **Ink Tertiary** `oklch(56% 0.004 260)` (~#78787f) — small uppercase labels, metadata, muted meta.

### Functional States (severity)
- **Healthy Green** `oklch(74% 0.14 158)` — healthy / ready / sealed / success (shares the accent hue).
- **Watch Amber** `oklch(80% 0.13 82)` (~#e6c35c) — watch / pending.
- **Critical Red** `oklch(66% 0.18 25)` (~#ea5f57) — critical / drift / missing / gap.
- **Muted Neutral** `oklch(60% 0.006 260)` — inactive / not-applicable dots.

### Borders & Rules
- **Hairline** `oklch(100% 0 0 / 0.09)` — 1px card/panel borders and dividers (white at 9% alpha).
- **Hairline Faint** `oklch(100% 0 0 / 0.06)` — faint dividers and the dot-grid texture.

### Graph Palette (Atlas)
Node kinds carry distinct hues on dark node cards: **code** slate/blue `oklch(68% 0.11 244)`, **concept** green `oklch(74% 0.14 158)`, **work** copper/orange `oklch(72% 0.13 55)`, **memory** amber `oklch(74% 0.09 80)`. Edges: **explicit** solid blue `oklch(66% 0.09 247)`; **derived** dashed gold `oklch(72% 0.11 70)`. Stale/drifted nodes dim or edge in the critical red.

## 3. Typography Rules

### Font Families
- **Sans (display + body)** — system stack: `-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif`. Neutral, legible, operational. Used for everything readable: titles, body, metric numbers. **No serif anywhere.**
- **Mono (labels)** — system stack: `"SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace`. Used for evidence: eyebrow/section labels, paths, counts sub-stats, badges, timestamps, code chips, provenance. This is the "instrument marking" voice.

### Hierarchy & Weights
- **Overview / view title** — 1.5rem, 600, letter-spacing −0.02em. Compact section-header scale (not a hero).
- **h1 (in-content)** — clamp(1.2rem → 1.45rem)/1.25, 600, −0.008em. Small, weight-driven.
- **h2 / card titles** — ~1–1.15rem, 600.
- **Metric value** — clamp(1.5rem → 1.9rem), 600, tabular-nums. The largest, most prominent type — the data is the hero.
- **Body / description** — 0.82–0.92rem/1.5, 400, Ink Secondary.
- **Mono label (eyebrow/tile/band)** — 0.6–0.66rem, 500–600, uppercase, letter-spacing 0.05–0.08em, Ink Tertiary.
- **Badge** — 0.66rem, 600, mono, uppercase, letter-spacing 0.05em.

### Spacing Principles
- Numeric values use `font-variant-numeric: tabular-nums`.
- Uppercase mono labels carry positive letter-spacing (0.05–0.1em) for legibility at small sizes; body sans does not.
- Line-height is relaxed for body (1.5–1.6) and tight (1–1.25) for titles, labels, and numbers.

## 4. Component Stylings

### Buttons
- **Ghost button** (`.ghost-btn`) — inline-flex, 1px hairline border, `--radius-sm` (7px), Card-Slate fill, Ink-Primary label at 0.78rem; hover lifts to Inset-Slate. Icon (if present) ~1.05em, centered.
- **Sparkle / copy-prompt** (`.copy-prompt`) — icon-only rounded square (`--radius-sm`), Inset-Slate fill, **Loam Green** sparkle icon; hover brightens border to accent. Sized to its neighbor (≈ badge height ~1.4rem in cards, ≈ button height ~1.9rem beside a text button). On success it swaps to a green seal (✓) and shows a "Copied" tooltip. Carries a styled hover/focus **tooltip** (dark rounded, `::after`, positioned below-right).
- **Pill** (`.pill`) — inline-flex, hairline border, `--radius-sm`, Card-Slate, 0.75rem Ink-Secondary; used for dropdown-style meta ("Indexed 15 Jul ▾") and counts.
- **Filter button** (`.filter-button`) — small, uppercase mono; active state uses green-soft fill + accent border.
- All interactive elements get a 2px green focus ring at 3px offset.

### Cards, Panels & Bands
- **Card / panel** — Card-Slate fill, 1px Hairline border, `--radius` (10px) or `--radius-lg` (14px). No heavy shadows in flow; elevation reserved for overlays. Internal padding `--space-4` to `--space-6`.
- **Band** — the core layout module: a section header (`.band-head`) with a drag handle (⠿), a title (mono or sans), and an optional right-side pill/action, over a card row or card grid.
- **Metric card** — label (mono, tertiary) + big tabular value (Ink Primary) + colored-dot sub-stats; min-height ~9rem.
- **Advisor / issue card** — header row with a category (icon + uppercase label) on the left and, on the right, a group of the **severity badge + sparkle action**; then an optional big metric, a title, and a description with inline `code` chips. Critical cards use the Critical Wash fill + Critical Red border.
- **Stat tile** — icon in a rounded Inset-Slate square + label (mono) + value (sans); grouped inside a single dotted-background overview box.
- **Dot-grid panel** — overview box and the Atlas stage use a faint radial-dot background (`--border-faint`, 22px grid) for the "field/desk" texture.

### Badges
Fully-rounded pills (999px), mono uppercase 0.66rem, colored by severity with a matching translucent border: **critical** (red text/border on faint red), **watch** (amber), **healthy/ok** (green, green-soft border), **neutral** (grey on Inset-Slate). Vertically centered (flex, line-height 1, slight optical bias for all-caps).

### Navigation & Chrome
- **Icon rail** — thin left rail (`--nav-width` 3.5rem), icon-only (Phosphor glyphs in rounded squares); active item gets an Active-Slate backing and a green glyph. Collapses to a bottom tab bar (with text labels) on mobile.
- **Topbar** — dark breadcrumb strip (`--topbar-height` 3.5rem): brand mark, workspace/branch context, freshness ("Indexed … · qmd ready"), and a `⌘K` search trigger. Spans the full viewport width — chrome is deliberately **not** capped to `--content-max-width`, so on wide screens it does not align with the content below it.
- **Inspector** — full-height right-side **modal** side sheet (`--inspector-width` 24rem), slides in over a dimming scrim; closes on ×, `Escape`, or outside-click. Dark chrome (shell surfaces), Ink-Shell text, green section headings.
- **Reader** — **full-screen** in-app document surface with its own Back control; light-on-dark rendered Markdown, a collapsible front-matter block, a sticky "On this page" outline, green resolvable wikilinks and red broken ones, and code chips for paths.

### Inputs & Forms
- **Search field** (in the `⌘K` dialog) — full-width, borderless with a bottom hairline, Shell-Raised fill, 1.25rem mono. Focus states use the green ring. Corner radii match buttons.

### Icons
- **Phosphor** (regular weight), inlined as a self-contained SVG `<symbol>` sprite (no external icon dependency). Icons inherit `currentColor` and are sized in `em` relative to their label so they sit level with text; all-caps chips size the icon to cap height.

## 5. Layout Principles

### Grid & Structure
- **App shell** — CSS grid: `[nav-rail] [workspace]`, with a full-width topbar row. Inspector and Reader are top-level overlays above the shell (not grid columns).
- **Bands & card rows** — each view is a stack of bands; card rows use `grid-auto-flow: column; grid-auto-columns: minmax(16rem, 1fr)` with horizontal scroll; card grids use `repeat(2/3, minmax(0,1fr))`.
- **Radii** — `--radius-sm` 7px (buttons, chips, tiles), `--radius` 10px (cards), `--radius-lg` 14px (large panels, stage, dialog).

### Whitespace Strategy
- **Spacing scale** — 0.25rem base: `--space-1 .25` / `-2 .5` / `-3 .75` / `-4 1` / `-6 1.5` / `-8 2` / `-12 3` / `-16 4` rem.
- View padding `--space-6` block / `clamp(1.25rem, 2.5vw, 2.25rem)` inline; bands separated by `--space-8`; compact, operational rhythm (no brochure margins).

### Alignment & Visual Balance
- Left-aligned, evidence-first. No centered heroes. The heaviest visual weight is the metric numbers and the single green accent; everything else is quiet greyscale.
- Each fact appears once (no duplicated counts/freshness across header, band, and topbar).

### Responsive Behavior & Touch
- Dark theme only (`color-scheme: dark`). Breakpoints ~72rem and ~48rem/40rem.
- Icon rail → bottom tab bar with text labels on mobile; overview tiles → 2-col; issue grid → 1-col; card rows scroll horizontally. Inspector and Reader become full-width. `prefers-reduced-motion` respected.

## 6. Design System Notes for Stitch Generation

### Language to Use
"Calm dark developer-console dashboard. Near-black cool-grey surfaces, quiet 1px-bordered cards grouped into labeled bands, a single restrained green accent, sans body + monospace evidence labels, no serif, no hero type. Data (metric numbers) is the largest element. Amber = watch, red = critical, green = healthy."

### Color References
Loam Green `#3ecf8e` (accent), App Charcoal `#111214` (bg), Card Slate `#1a1b1e` (cards), Ink Primary `#f4f4f5`, Ink Secondary `#a1a1aa`, Watch Amber `#e6c35c`, Critical Red `#ea5f57`. (OKLCH values in the frontmatter are canonical.)

### Component Prompts
- "A metrics band: a section header with a drag handle and a right-aligned pill, above a horizontally-scrolling row of dark bordered metric cards — each with a small uppercase mono label, a large tabular white number, and two colored-dot sub-stats."
- "An advisor issue card: dark card with a category icon + uppercase label top-left, a severity badge and a small green sparkle icon-button top-right, then a title and a muted description with inline monospace code chips. Critical variant tinted deep red."
- "A full-height right-side inspector modal over a dimming scrim: dark panel, green uppercase section headings, source path and evidence rows in monospace."

### Incremental Iteration
Keep the accent green rare — if more than a few green elements appear per screen, pull back. Prefer hairline borders and negative space over shadows. Reach for mono only for evidence (labels, paths, counts); everything readable stays sans.
