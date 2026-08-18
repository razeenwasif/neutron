# Neutron Progress

## UI Redesign: Depth Through Translucency (Aero-Style Glassmorphism)

### Status: Complete & Verified

#### 1. Visual Architecture
- **3-Layer Depth Model**:
  1. **Ground**: Near-black deep violet `#0c0714` / `#05020a` with radial overhead light source.
  2. **Ambient Drifting Colour Fields**: Two large slow-drifting colour fields on sinusoidal ease-in-out loops, each near window-sized so what shows is the gradient where they overlap rather than a recognisable circle:
     - **Field A (41s loop)**: Vibrant purple (`#9333ea`), top-left, drifting `+7vw, +6vh`, scaling `1.0 → 1.10`.
     - **Field B (57s loop)**: Electric indigo / violet (`#6366f1`), bottom-right, drifting `-8vw, -5vh`, scaling `1.08 → 0.98`.
     - Periods share no common factor, so the pair never settles into a visible repeat. Each field is a three-layer gaussian falloff fan — two layers still left a discernible rim at this size.
  3. **Translucent Glass Panels**: Floating glass cards at `rgba(36, 27, 56, 0.44)` in dark mode and `rgba(252, 250, 255, 0.62)` in light, with soft glass shadow, subtle glass stroke, a top-edge highlight that fades down the upper sides, and a darker bottom edge so the panel has thickness.
     - The dark fill is *lighter* than the ground, not darker. At this transparency a near-ground fill composites to the same colour and the panel edge disappears in the unlit corners.

#### 2. Disciplined Purple Palette & WCAG AA Compliance
- Primary purple brand ramp (`#c084fc` / `#a855f7` / `#9333ea`).
- Strictly reserved amber (`#fbbf24`) and rose (`#fb7185`) accents for warning and error states.
- Dark theme set as default (`ThemeMode::Dark`).
- All text surfaces tested and verified to meet WCAG AA contrast (>= 4.5:1 for primary text, >= 3.0:1 for muted text).
- Contrast is measured at **both** extremes of the lit ground — a card over the deepest unlit corner *and* over the brightest colour field at its peak. Checking only the deep ground is the best case for light text on a dark card, and would pass however washed-out the lit areas became.

#### 3. Component Refinements
- **Sidebar**:
  - Wordmark brand icon with purple gradient glow and glass highlight.
  - Active navigation item with glowing vertical accent indicator on the left rim (matching Aero's `.nav-item.active::before`).
  - Segmented storage capacity bar in violet-to-purple ramp.
- **Panes & Tab Strip**:
  - Translucent glass card background refracts moving orbs underneath.
  - Inset top-edge glass refraction highlight.
  - Active tab indicator underline in purple accent.
  - Focus is carried by the active tab's accent underline. The pane itself keeps one hairline in both states: an accent ring plus a glow around the largest element on screen read as a selection box, and framed the whole window when only one pane was open.
- **Header & Filter Pill**:
  - Filter search pill with purple focus ring and glowing search glyph when active.
  - Breadcrumbs with responsive ellipsis truncation.
- **Finder & Command Palette Overlay**:
  - Elevated floating card with glass highlight and deep shadow.
  - Purple mode chip and fuzzy match character highlighting.
- **File List**:
  - Selection row pill with purple highlight and border.
  - Focus cursor indicator.

#### 4. Grid View
- `Ctrl+Shift+L`, a header toggle, and a command palette entry switch between list and grid.
- Tiles are virtualized a row at a time, so a 500k-entry folder costs the same per frame as a 50-entry one.
- Arrow keys move by a whole row in the grid; the layout reports its column count back to the tab, because the pane is drawn from a clone of the view state and the count would otherwise be discarded every frame.

#### 5. Selection & Context Menu
- **Rubber-band select**: press and drag anywhere in the list or grid to sweep a
  selection; Ctrl adds to what is already selected. The band is tracked in
  *content* coordinates so it stays glued to the rows while the list
  auto-scrolls under it. Its origin lives in egui memory rather than in
  `FileListState`, because the pane is drawn from a clone of that state and
  anything written during paint is discarded when the frame ends.
- **Themed context menu**: the shell still builds the menu — every installed
  extension (WinRAR, 7-Zip, Git, PowerRename, IntelliJ) is present — but Neutron
  reads the `HMENU` into plain data and draws the rows itself, so the menu is
  the same glass as the rest of the window instead of a system-grey rectangle.
  Submenus populate lazily through a forwarded `WM_INITMENUPOPUP`, the default
  verb is accent-tinted, and keyboard navigation works.
  - Not carried over: per-item icons supplied as `HBITMAP`. A fully owner-drawn
    item with no menu string falls back to its verb.

#### 6. Known Cost
- The drifting fields require a continuous repaint. Measured idle CPU: **17.7%** of one core at 60fps, **8.4%** at 30fps (current setting), against **0.3%** for a static ground. The loops are 41s and 57s long, so the lower rate is visually identical.

#### 7. Test Suite
- Pure-logic unit tests (`neutron-core`, `neutron-ui`, `neutron-fuzzy`, `neutron-index`) passing on Linux.
- Full suite on the Windows target: 305 tests, clippy clean.
