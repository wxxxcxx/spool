# ADR 0003: The Bar Leaves Through the Screen Edge Behind a Handle

- Date: 2026-09-12
- Status: Accepted for implementation; desktop acceptance pending

## Context

The Bar is the menu-bar band, and collapsing it used to keep the panel exactly
where it was: only the chrome drawn inside it changed, into a black capsule
merged with the Notch or a small top-centred tab. Two chevron handles at the far
ends of the band, revealed once the pointer was anywhere on the Bar, were the
only way to collapse it, and nothing in the Bar ever left the band.

That shape was chosen to keep the transition smooth — moving and resizing a
blurred window every frame makes the window server re-blur it every frame, which
is what made an even earlier floating-Bar transition stutter — but it also meant
the collapsed Bar was a shape *inside* the menu bar rather than a piece of the
Bar that remains, and the collapse control lived at the display's edges, where it
had to be revealed before it could be clicked.

## Decision

One small **Bar Handle** at the display's centre is the Bar's only collapse
control, and it toggles in one click in both directions. Its square top edge is
glued to the Bar's bottom edge, so while expanded it hangs 10pt below the band,
over the desktop.

Collapsing slides the whole Bar — band, content and glass — up out of the screen
through the display's top edge, over the same 240ms ease-out the content uses.
What stays is the handle: flush with the screen top on a display without a Notch,
and on a notched display a collar around the Notch, which does not move at all
because the Notch is a hole in the hardware and nothing drawn in the middle of
the band is visible.

The panel window frame still never moves. The window is the band plus the
handle's overhang, what slides is what is drawn inside it, and the blur is masked
to the moving chrome rather than faded, so the glass leaves with the Bar. The
panel is interactive only where the pointer is on the Bar's own chrome, which is
what keeps the menu bar under a collapsed Bar — and the strip beside the handle —
working.

## Considered Options

- **Keep the panel fixed and morph the chrome** (what the Bar did before). It
  cannot express "the Bar has gone": the menu bar underneath is handed back by a
  shape change, so the Bar can never look like it left, only like it shrank.
- **Move the window frame up**, the literal reading of "the Bar slides away".
  Rejected: re-blurring a moving window every frame is the cost this Bar was
  built to avoid, and a borderless panel positioned above the screen top is at
  the mercy of the window server's frame constraints.
- **Keep the end chevrons as well**, so the handle is one of three controls.
  Rejected: the handle is always visible, so a second, edge-hugging affordance
  only reintroduces the edge hunting it replaced.
- **Let the handle idle at the band's bottom edge once collapsed** on a plain
  display, instead of riding to the screen top. Rejected: it would float 24pt
  below the top edge, detached from the Bar it belongs to.

## Consequences

- The Bar's window is `placement::window_overhang()` taller than the menu-bar
  band on every display — the handle at rest plus the room it grows into under
  the pointer, because a view clips its own drawing — so the panel's rect is no
  longer exactly the band's. Anything that assumed the panel and the band are
  the same rect has to say which it means.
- The panel is interactive only while the pointer is on the Bar's chrome, which
  the Bar's own frame loop already tracks. A click that arrives in the strip
  beside the handle before the next frame is the desktop's, not the Bar's.
- The Notch capsule, the plain-display tab, the end chevrons and the
  `CAPSULE_*` / `PLAIN_TAB_*` / `SHOULDER` / `MORPH` constants are gone. The
  Bar's now has one collapse curve, `motion::ease_out` over `motion::DURATION`.
- `bar.corner_radius` still rounds the band's own bottom corners. The handle's
  bottom corners are a fixed 5pt, and its geometry has no configuration keys.
- Hover feedback on the handle is a small growth (`HANDLE_HOVER_GROWTH`), not a
  colour change or an outline: anything drawn along its top edge would cut the
  join that makes it read as part of the Bar.
