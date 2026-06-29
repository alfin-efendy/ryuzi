# Harness Router Brand Assets

Shared brand assets for the router CLI, IDE, web app, mobile app, and documentation live here.

Use these files as the canonical source. Do not import from `outputs/logos/`; that folder is a local exploration artifact and is intentionally ignored by git.

## Defaults

- `wordmark.svg` - default adaptive wordmark. Uses `prefers-color-scheme` and transparent canvas.
- `mark.svg` - default adaptive standalone mark. Uses `prefers-color-scheme` and transparent canvas.
- `icon-512.png` - fixed app icon export for places that require PNG.
- `favicon.ico` - fixed favicon export.

## Explicit Theme Variants

- `wordmark-light.svg` - black wordmark for light backgrounds.
- `wordmark-dark.svg` - white wordmark for dark backgrounds.
- `mark-light.svg` - black standalone mark for light backgrounds.
- `mark-dark.svg` - white standalone mark for dark backgrounds.
- `wordmark-adaptive.svg` - explicit adaptive wordmark alias.
- `mark-adaptive.svg` - explicit adaptive mark alias.
- `mark-solid.svg` - fixed black square with white mark for app icons, avatars, package identity, and high-control placements.

## Usage

Use the adaptive files when the surface follows the user's global light or dark theme:

```html
<img src="/assets/brand/wordmark.svg" alt="Harness Router">
```

For Markdown documentation, prefer explicit light and dark sources. Some renderers do not apply the SVG's internal `prefers-color-scheme` styles when the SVG is loaded through a plain Markdown image.

```html
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="/assets/brand/wordmark-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="/assets/brand/wordmark-light.svg">
  <img src="/assets/brand/wordmark-light.svg" alt="Harness Router" width="560">
</picture>
```

Use explicit variants when the logo is placed on a custom background that may not match the global theme:

```html
<img src="/assets/brand/wordmark-light.svg" alt="Harness Router">
<img src="/assets/brand/wordmark-dark.svg" alt="Harness Router">
```

For CLI text surfaces, use the text identity from `apps/cli/src/cli/brand.ts`: the glyph is `マ`, and the brand name is `Harness Router`.
