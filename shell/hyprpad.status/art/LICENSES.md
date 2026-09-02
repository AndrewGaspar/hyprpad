# Artwork provenance

One file, one source.

## Bundled — `kenney/controller_icon.svg`

Kenney "Input Prompts" 1.5A, <https://kenney.nl/assets/input-prompts>.
**CC0 1.0 (public domain dedication).** The pack's own `License.txt` is copied
verbatim to `kenney/LICENSE.txt`; the operative wording is:

> License: (Creative Commons Zero, CC0)
> <http://creativecommons.org/publicdomain/zero/1.0/>
> You can use this content for personal, educational, and commercial purposes.
> Support by crediting 'Kenney' or 'www.kenney.nl' (this is not a requirement)

Attribution is explicitly optional, so bundling is unencumbered — we credit
anyway, here and in `shell/README.md`.

Byte-for-byte upstream, and byte-for-byte the same file the cheat sheet
vendors at `shell/hyprpad.cheatsheet/art/kenney/controller_icon.svg`, where it
is the Steam button's modifier glyph. It is **copied rather than shared**: each
plugin is installed into its own directory under `~/.config/omarchy/plugins/`,
and Omarchy's manifest validation rejects a plugin folder containing symlinks,
so a bar widget cannot reach into the cheat sheet's copy — nor should it, since
either plugin is meant to be installable without the other.

This is the one Kenney file that is *right* at bar scale. The pack's
`controller_<device>.svg` pictograms are 64×64 solid fills with no separable
controls, which is why the cheat sheet's callout diagram comes from elsewhere
(see `shell/hyprpad.cheatsheet/art/LICENSES.md`) — but a 16 px bar glyph wants
exactly a solid-filled pictogram, so here it is the correct choice rather than
a compromise.

The SVG is authored with a white fill (`#FFFFFF`). `Widget.qml` rewrites that
to the bar's foreground colour at load and hands the result to the `Image` as a
`data:` URL, so the glyph follows the active theme — including a light one —
without a second asset.
