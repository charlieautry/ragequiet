# Ragequiet brand assets

| File | Use |
|---|---|
| logo-dark.svg / .png | README, site, anything on a dark background (white wordmark) |
| logo-light.svg / .png | Light backgrounds (dark wordmark) |
| logo-mono-white.svg, logo-mono-black.svg | Single-color contexts (print, embossing, disabled states) |
| icon.svg | Master square app icon, bars only, transparent |
| icon-mono-*.svg | Single-color icon variants |
| icon-16..512.png | Rasterized app icon sizes |
| ragequiet.ico | Windows executable / installer icon (16 to 256) |
| tray-{quiet,warning,loud,off}-{16,32}.png | System tray states; load as RGBA in tray-icon |
| tray-*.svg | Editable sources for the tray states |

Colors: red #ff3b4a, orange #ff6a3d, amber #ffb020, lime #9ccf3a, green #3ed67a, ink #15171c, calibration dot #ffd23f.

GitHub README snippet (auto-switches with theme):

```html
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg">
  <img src="assets/logo-light.svg" alt="ragequiet" width="360">
</picture>
```
