# Control Center Theme

This document describes the theme visible in the provided Qt screenshot and how to apply the same visual language in `picky`.

## Character

The screenshot uses a soft neon control-center look:

- deep blue-violet dark surfaces instead of neutral black or gray
- large rounded containers with inset panel layering
- thin, cool-toned borders instead of heavy separators
- high-contrast text with restrained saturation
- multiple accent roles instead of a single brand color

It reads as polished desktop UI, not terminal-dark and not gamer-neon. The saturation is controlled and the spacing is generous.

## Palette

These colors are inferred from the screenshot and are intended as implementation targets, not exact sampled values.

- Window backdrop: `#151A2D`
- Outer shell: `#1B2135`
- Sidebar surface: `#242A40`
- Main panel surface: `#222840`
- Inset card surface: `#272E47`
- Hover surface: `#2D3550`
- Border: `#3B4465`
- Primary text: `#E5EAFE`
- Secondary text: `#A2ABC9`
- Muted text: `#7B84A7`
- Blue accent: `#86A8FF`
- Blue accent strong: `#6D8FF5`
- Lime accent: `#A8D469`
- Purple accent: `#C3A2FF`
- Error accent: `#E18497`

## Layout

- Use a framed outer shell with a large radius, visible border, and subtle shadow.
- Split the layout into a narrow left rail and a larger content panel.
- Keep gutters spacious: roughly `18-24px`.
- Panels should feel inset from the outer shell, not flush to the window edge.

## Shape Language

- Outer shell radius: `26-30px`
- Primary panels radius: `20-24px`
- Cards and inputs radius: `14-18px`
- Pills and chips radius: fully rounded or `999px`
- Borders should stay thin: `1px`, occasionally `2px` on the outer frame

## Typography

The screenshot uses clear hierarchy rather than dramatic font contrast.

- Main page titles: semibold, bright, slightly accent-tinted
- Section titles: accent-colored, usually purple
- Navigation labels: medium weight
- Body text: muted blue-gray
- Captions and helper text: smaller and lower contrast

For the current app, match this through size, weight, color, and spacing. The exact Qt font family is not recoverable from the screenshot alone.

## Accent Roles

The screenshot uses different accents for different jobs.

- Lime marks the currently selected high-level navigation item.
- Blue marks the current page/header and active interactive state.
- Purple marks subsection headings.
- Red/pink is reserved for warnings or destructive states.

This multi-accent split is a big part of the theme. Avoid collapsing everything into one color.

## Component Treatment

### Navigation items

- Rounded pill-like rows
- Muted by default
- Selected state uses a filled accent background
- Icons and labels sit inside a generous horizontal padding

### Content panels

- Slightly lighter than the window backdrop
- Rounded corners
- Fine border with low-contrast edge definition

### Inputs

- Inset dark field
- Thin border
- Bright focus ring in the blue accent
- Placeholder text should be visibly muted, not gray-on-gray

### Result rows / list items

- Independent cards inside a darker list surface
- Soft hover state
- Selected row uses the blue accent with a brighter border
- Secondary metadata should remain readable over both idle and selected states

### Badges and chips

- Rounded pills
- Dark surface by default
- Accent tint only when they carry state emphasis

## Contrast and Density

- Keep the interface calm: avoid heavy gradients or large glow effects.
- Preserve readability with strong text contrast.
- Use spacing and rounded surfaces to create depth instead of extra ornament.

## Scope of Inference

The screenshot only reveals static appearance. Motion, hover timing, disabled-state behavior, and exact font family are not observable. Those parts should be implemented in a way that supports the visual system above without pretending the screenshot proved them.
