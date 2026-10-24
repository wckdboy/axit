# axit.sh

Static WCKD site deployed with the standard Cloudflare Wrangler workflow.
No dependency installation is needed for the build or checks; Node.js is sufficient.

```sh
npm --prefix site run check
cd site
npm run dev
# After checking the release links and the preview:
npm run deploy
```

`check` builds from a clean checkout, then verifies local assets, anchors, Axit
branding, the installer mirror and font licensing. `build.mjs` copies the exact
root `install.sh` into the output; update that source rather than the generated copy.
The page links to GitHub release downloads; its install command uses `axit.sh/install.sh`.

`public/fonts/jetbrains-mono-latin.woff2` is a tracked 31,340-byte binary asset,
alongside its SIL Open Font License. Binary-excluding review tools may omit the
font from their context; `check.mjs` still verifies the real file exists in the build.

`wrangler.jsonc` deploys the `axit-site` Worker with static assets and the custom
domains `axit.sh` and `www.axit.sh`. Use the existing Cloudflare account login.
`public/_headers` supplies the content policy and prevents caching the installer.
Keep the website, root README and Axit Plane project current when releases change.
