# App icons

These icons are part of the æther 0.7 app packages. The kernel draws them in
the dock; App Store shows an installed app's package icon next to its name.

| File | Source |
|---|---|
| `browser.png`, `browser-128.png` | Original æther artwork (clean-room, no Chromium assets). |
| `settings.png`, `installer.png` | Original æther artwork for the Settings and App Store packages. |
| `craft/*.png` | The Craft apps' own icons, taken from their `github.com/storytold/<app>` source trees. |

The Craft apps and their icons are published by the ArtCraft team under the
Apache License 2.0. `craft/NOTICE` lists the exact repository paths. The icons
are included here only to build the apps' own untrusted `.app` packages; they
are not compiled into App Store or any other trusted system app. App Store
reads an installed app's icon from its package through `SYS_APP_ICON` and
shows a coloured initial for apps that are not installed.

`browser.png` is the 256x256 source. `browser-128.png` is the copy packaged
with the browser (RGBA icons are limited to 256x256; 128 keeps the package
small). The craft PNGs are 128x128.

The packaging tool (`tools/mkapp.py --icon`) stores an icon as RGBA8888 at the
end of the `.app` package. The header records its size, offset and SHA-256, so
a damaged or modified icon is refused along with a damaged program. Each
Craft icon travels inside that app's own package, not in the OS image's
trusted apps.
