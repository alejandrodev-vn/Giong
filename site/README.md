# giong.run

The project's website: one static page in English.
No framework, no build tool, no external requests: fonts and logos are served from this
folder.

```
site/
  build.py        generates public/index.html
  public/         the folder to deploy
    styles.css    every style, light and dark from prefers-color-scheme
    main.js       copy button, code tabs, the event-loop animation (optional)
    fonts/        Inter and JetBrains Mono (SIL Open Font License), latin subsets
    assets/       the mark, favicon and social preview, copied from ../assets/
    CNAME         giong.run, for GitHub Pages
```

## Changing the text or a number

Edit `build.py` (the `CHARTS`, `CODE` and `T` tables), then run

```bash
python3 site/build.py
```

Python 3.8+ with nothing installed. Commit the regenerated HTML with the change.

Every number on the page comes from the README of release 0.2 and `bench/results/`. When a
benchmark is run again, update the README first and then the page, so the two never
disagree. Keep the caveats next to the numbers they qualify.

## Looking at it locally

```bash
python3 -m http.server -d site/public 8000    # then open http://localhost:8000
```

## Deploying

giong.run is served by GitHub Pages from the
[giong-run/giong-run.github.io](https://github.com/giong-run/giong-run.github.io)
repository, which holds a copy of `site/public/`. To publish a change, rebuild here, copy
`site/public/` over the files in that repository (keep its `README.md` and `.nojekyll`),
and push. Pages rebuilds within a minute.

The domain's DNS is on Cloudflare, set to "DNS only" so GitHub can issue the certificate:
four `A` and four `AAAA` records for the apex pointing at GitHub Pages, `www` as a `CNAME`
to `giong-run.github.io`, and the `_github-pages-challenge-giong-run` TXT record that
verifies the domain for the organisation. HTTPS is enforced.

The links to `bench/*.sh`, `ROADMAP.md` and `CHANGELOG.md` point at the `main` branch on
GitHub, so they resolve once release 0.2 is merged there.
