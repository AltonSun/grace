# Deeply Human: static rebuild for altonsun.com/deeplyhuman

Plain HTML, CSS and JS. Nothing to build or install, and no Lovable code.

## Upload

Your server runs nginx. Copy this folder to the web root as `deeplyhuman/`:

```bash
# from the folder that contains this file
rsync -av --delete ./ YOU@altonsun.com:/var/www/altonsun.com/deeplyhuman/
#                                       ^ use whatever `root` your nginx server block points at
```

Your existing nginx `try_files $uri $uri/ =404` (the default) already serves
`/deeplyhuman/about/` → `about/index.html`. You don't need any config changes.

Optional, for long-lived image caching:

```nginx
location /deeplyhuman/img/ { expires 30d; add_header Cache-Control "public"; }
```

## Pages

| URL | Page |
|---|---|
| /deeplyhuman/ | Home (DEEPLY · MENU · HUMAN) |
| /deeplyhuman/theta-healing/ | ThetaHealing sessions and prices (EN and LT price lists) |
| /deeplyhuman/retreats/ | Upcoming, galleries (21 + 10 + 16 photos), past retreats |
| /deeplyhuman/about/ | Gabrielė: mission, certifications, past retreats |
| /deeplyhuman/poetry/ | „šviesos“, poems, book photos with lightbox |
| /deeplyhuman/private-sessions/ | Redirects to theta-healing/ (the original URL) |

The EN/LT toggle remembers its choice under the same `dh-lang` key as the original.

## To do before it goes live
- **Portrait:** on deeplyhuman.eu the About portrait returns a 404. Put a photo at
  `img/gabriele-portrait.jpg` and replace the `<div class="portrait">` in `about/index.html`
  with `<img src="../img/gabriele-portrait.jpg" alt="Gabrielė Jauniūtė">` inside it.
- Every page has `<link rel="canonical">` pointing at deeplyhuman.eu, so search engines
  don't count this copy as duplicate content. If this copy becomes the main site, remove those tags.
