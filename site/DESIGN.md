---
version: alpha
name: Plonix
description: The design system of plonix.io. A cool, quiet bench for looking closely at traffic. Light by default with a graphite dark twin, one indigo accent taken from the logo, heavy grotesque headlines, mono for anything a researcher would type, and real app screenshots instead of illustrations.

colors:
  accent: "#4b55d6"
  accent-dark-mode: "#8b93ff"
  on-accent: "#ffffff"
  canvas: "#f5f5fa"
  surface: "#ffffff"
  surface-sunk: "#eceef6"
  hairline: "#e0e2ee"
  ink: "#15162b"
  muted: "#565a78"
  faint: "#8b8ea8"
  spotted: "#f7c64f"
  ok: "#11a36a"
  danger: "#d33b4a"
  canvas-dark: "#0d0e13"
  surface-dark: "#15161e"
  surface-sunk-dark: "#1d1f2a"
  hairline-dark: "#292b39"
  ink-dark: "#ecedf4"
  muted-dark: "#a2a5bb"
  terminal: "#13141f"

typography:
  display-xl:
    fontFamily: Bricolage Grotesque, Avenir Next, system-ui, sans-serif
    fontSize: clamp(2.4rem, 4.4vw, 3.7rem)
    fontWeight: 800
    lineHeight: 1.04
    letterSpacing: -0.02em
  display-lg:
    fontFamily: Bricolage Grotesque, Avenir Next, system-ui, sans-serif
    fontSize: clamp(2rem, 4.4vw, 3.4rem)
    fontWeight: 800
    lineHeight: 1.04
    letterSpacing: -0.02em
  title:
    fontFamily: Bricolage Grotesque, Avenir Next, system-ui, sans-serif
    fontSize: 1.3rem
    fontWeight: 600
    lineHeight: 1.2
    letterSpacing: -0.015em
  body:
    fontFamily: Hanken Grotesk, -apple-system, Helvetica Neue, system-ui, sans-serif
    fontSize: 17px
    fontWeight: 400
    lineHeight: 1.6
  lede:
    fontFamily: Hanken Grotesk, -apple-system, system-ui, sans-serif
    fontSize: 1.12rem
    fontWeight: 400
    lineHeight: 1.6
  label:
    fontFamily: JetBrains Mono, ui-monospace, SF Mono, Menlo, monospace
    fontSize: 0.78rem
    fontWeight: 500
    letterSpacing: 0.12em
    textTransform: uppercase
  code:
    fontFamily: JetBrains Mono, ui-monospace, SF Mono, Menlo, monospace
    fontSize: 0.84rem
    fontWeight: 400
    lineHeight: 1.8

rounded:
  sm: 7px
  md: 12px
  lg: 14px
  xl: 18px
  pill: 999px

spacing:
  gutter: 24px
  gutter-mobile: 16px
  wrap: 1180px
  section: 110px
  section-mobile: 72px
  grid-gap: 20px

components:
  button-primary:
    backgroundColor: "{colors.accent}"
    textColor: "{colors.on-accent}"
    typography: "600 0.98rem {typography.body}"
    rounded: "{rounded.md}"
    height: 46px
  button-secondary:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.ink}"
    borderColor: "{colors.hairline}"
    rounded: "{rounded.md}"
    height: 46px
  tile:
    backgroundColor: "{colors.surface}"
    borderColor: "{colors.hairline}"
    rounded: "{rounded.xl}"
    padding: 14px
  tile-picture:
    backgroundColor: "{colors.surface-sunk}"
    rounded: "{rounded.md}"
  screenshot-frame:
    backgroundColor: "{colors.surface}"
    borderColor: "{colors.hairline}"
    rounded: "{rounded.lg}"
  terminal:
    backgroundColor: "{colors.terminal}"
    textColor: "#cfd1ea"
    typography: "{typography.code}"
    rounded: "{rounded.lg}"
  chip:
    backgroundColor: "{colors.surface}"
    borderColor: "{colors.hairline}"
    textColor: "{colors.muted}"
    typography: "0.8rem {typography.code}"
    rounded: "{rounded.pill}"
---

# Plonix design system

This file describes how plonix.io looks and why, so that anyone (a person or a coding agent) adding a
section keeps it consistent. `site/index.html` is the source of truth for exact values; this file is the
reasoning. Update both together.

## Overview

Plonix is a tool for looking closely at web traffic, and the site should feel like a well-lit workbench:
calm, precise, a little technical, with the product itself doing the showing. The page leads with a live
Lens that decodes a request under the cursor, then shows real screenshots of the app. There are no stock
photos, no illustrations and no invented dashboards built from boxes.

**Key characteristics**
- One accent: the logo's indigo `#4b55d6` (lighter `#8b93ff` for text and rings in dark mode).
- Light by default (cool paper `#f5f5fa`), with a graphite dark twin that follows the system setting and a
  visible switch in the nav. The whole page is one theme; sections never flip.
- Heavy Bricolage Grotesque headlines, Hanken Grotesk body, JetBrains Mono for queries, commands, hosts and
  labels.
- Hairline borders and surface contrast carry elevation. Shadows are soft, long and tinted toward the
  page's hue, never pure black on light.
- A barely visible film grain over the page and a single soft indigo wash behind the hero. No second hue.

## Colors

### Accent
- **Indigo** (`{colors.accent}`): primary buttons, links in tiles, the Lens ring, focus rings, selected
  states. Used on every section the same way.

### Surfaces
- **Canvas** (`{colors.canvas}`): the page.
- **Surface** (`{colors.surface}`): tiles, screenshot frames, inputs.
- **Surface sunk** (`{colors.surface-sunk}`): the picture area inside a tile.
- **Hairline** (`{colors.hairline}`): every border and divider.
- **Terminal** (`{colors.terminal}`): command examples, in both themes.

### Text
- **Ink** for headings and strong text, **muted** for body copy, **faint** for captions and notes.

### Meaning (never decoration)
- **Spotted amber** (`{colors.spotted}`): a value the Lens decoded or highlighted.
- **Ok green** (`{colors.ok}`): something succeeded or is in scope.
- **Danger red** (`{colors.danger}`): reject, exclude, hide.

These three appear only where they mean something in the product. No colored dots in front of list items,
pills or nav links.

## Typography

| Role | Face | Size | Weight |
|---|---|---|---|
| Hero headline | Bricolage Grotesque | clamp(2.4rem, 4.4vw, 3.7rem) | 800 |
| Section headline | Bricolage Grotesque | clamp(2rem, 4.4vw, 3.4rem) | 800 |
| Tile / step title | Bricolage Grotesque | 1.2-1.35rem | 600 |
| Body | Hanken Grotesk | 17px / 1.6 | 400 |
| Lede | Hanken Grotesk | 1.12rem, max 60ch | 400 |
| Label, query, command, host | JetBrains Mono | 0.75-0.9rem | 400-500 |

**Rules**
- Sentence case for every heading and button. Headlines end with a period.
- `text-wrap: balance` on headings; body copy stays under about 65 characters per line.
- No em-dashes or en-dashes anywhere in visible copy. Use a period, a comma or a colon.
- Plain, specific verbs. Describe what Plonix does in its own terms and never compare it to another product.

## Layout

- Content sits in a 1180px wrap with 24px gutters (16px under 520px).
- Sections are 110px apart (72px on phones). Anchored sections have `scroll-margin-top` so the sticky nav
  never covers a heading.
- Each section uses a different layout family. Never more than two text-and-picture rows in a row: break
  the run with a stacked row (copy on top, wide screenshot below) or a grid.
- The feature tiles are a bento on a 6-column grid with rows of 4+2, 3+3 and 2+4. Wide tiles put the words
  first and the picture beside them; one tile carries a light indigo tint so the grid is not uniform.
- Under 980px the bento becomes two columns (wide tiles span both); under 620px it is one column.

### Hero
At most four text elements: one label, the headline (two lines), a lede of about 20 words, and two buttons.
Facts, badges and trust strips go below the hero, never inside it.

### Small uppercase labels
The mono uppercase label (with the ring mark) is rationed to about one per three sections. Today: the hero,
Adaptive scope, AI-native and Get started. A heading alone is enough everywhere else.

## Elevation and depth

| Level | Treatment | Use |
|---|---|---|
| Flat | No border, no shadow | Section bands |
| Hairline | 1px `{colors.hairline}` on `{colors.surface}` | Tiles, the flow strip, the guard strip |
| Lifted | Hairline plus a long soft shadow tinted to the page | Screenshots, the Lens demo |

## Shapes

| Token | Value | Use |
|---|---|---|
| `{rounded.sm}` | 7px | Inline tokens, small buttons inside pictures |
| `{rounded.md}` | 12px | Buttons, pictures inside tiles |
| `{rounded.lg}` | 14px | Screenshot frames, terminals |
| `{rounded.xl}` | 18px | Tiles, the Lens demo, the flow strip |
| `{rounded.pill}` | 999px | Chips and filter tokens only |

Inner elements always have a smaller radius than their container.

## Components

- **Buttons**: one primary (indigo fill) and one secondary (surface with hairline) per group. Hover lifts
  1px; press settles with `scale(.98)`. Labels fit on one line. One label per intent across the page:
  downloading is always "Download for Mac".
- **Tiles**: surface, hairline, `{rounded.xl}`, a sunk picture area that shows a small piece of the real UI,
  then a title, one or two sentences, and a mono link.
- **Screenshot frame**: a real app screenshot with explicit width and height, `loading="lazy"` below the
  fold. The hero window gets `fetchpriority="high"`.
- **Terminal**: dark in both themes, prompt `$` in indigo, success `✓` in green, comments faint.
- **Chips**: mono text in a pill, no colored dots.
- **Lens demo**: the signature component. A ring follows the pointer and reveals the decoded layer. It drifts
  on its own only while on screen and holds still under reduced motion.

## Motion

- Reveal on scroll: fade plus an 18px rise, once, via IntersectionObserver. Nothing is hidden before
  JavaScript knows it is below the fold.
- Only `transform` and `opacity` animate. Every animation stops under `prefers-reduced-motion`.
- No scroll listeners and no animation loops running off screen.

## Accessibility

- Skip link to the content, visible focus rings (2px indigo, offset 3px), keyboard arrows in every tab list.
- Status changes (copy confirmation, query explanations) are announced with `aria-live="polite"`.
- Body text meets WCAG AA in both themes.

## Do and don't

### Do
- Show the real product. New sections use a real screenshot or a small working demo.
- Keep one accent and one theme per page.
- Write short, concrete copy: a headline of about 8 words, a paragraph of about 25.

### Don't
- Don't add a second accent color, a purple glow, or gradient text.
- Don't add rows of identical cards or a label above every heading.
- Don't put facts, badges or version numbers in the hero.
- Don't use emoji as icons or draw decorative illustrations.
- Don't mention other products or compare Plonix to them.
