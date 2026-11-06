# Web source and deployment checks

This directory is the source of the whole web app: public pages, the waitlist form, account pages
and administration. Cloud enforces authorization and holds account data and secrets. A restricted
page does not need a separate source tree.

`source/inputs.json` lists the inputs needed to build the application. Deployment captures one public
commit and archives those paths from it, alongside the service's own committed inputs. The web and
relay use the same captured public commit. Local edits are never deployment inputs. Tests, build
outputs and operational harnesses are not application build inputs.

From the repository root, compare an existing mirror with the public source:

```sh
make web-source-check WEB_PEER=/path/to/operations WEB_SOURCE_MODE=mirror
```

Both refs default to `HEAD`; set `WEB_SOURCE_REF` and `WEB_PEER_REF` to full commit SHAs for a
deployment. Missing, extra or changed files under a manifest path fail the check, including file
mode changes. Uncommitted edits are ignored. A missing mirror fails rather than silently passing.
After migrating the consumer, use `WEB_SOURCE_MODE=absent` to reject any remaining application
inputs in that checkout. Operational harnesses outside the manifest paths may remain.

The consumer can obtain the archive paths from the captured source commit:

```sh
git -C "$ALONDRA_SOURCE" show "$SOURCE_COMMIT:web/source/check.py" > "$STAGING/web_source.py"
python3 "$STAGING/web_source.py" paths --source "$ALONDRA_SOURCE" --source-ref "$SOURCE_COMMIT"
```

The command prints one validated path per line, suitable for an argument array to `git archive`.
It fails if the manifest is invalid or any source input is missing or is not a regular file.
After removal of a mirror, deployment must use `absent`; it must not fall back to old local inputs.
Cloud image identity and provenance must include both service and public source commits so a web
change alone produces a new image. Rollback restores the previous image and its recorded inputs.

## Browser checks

```sh
cd web
npm ci --include=dev
npm run typecheck
npm run build
npm run test:e2e
```

To verify an already deployed site, first build the exact public commit recorded by deployment,
then run from its `web/` directory:

```sh
PUBLIC_WEB_ORIGIN=https://getalondra.com WEB_SOURCE_COMMIT="$SOURCE_COMMIT" \
  npm run check:public
```

`WEB_BUILD_DIR` optionally selects that commit's compiled `build/client` directory. The probe
requires an explicit origin and source SHA, starts no server, uses a fresh browser context and
blocks requests other than GET and HEAD. It never signs in or submits the waitlist. It compares
loaded scripts and styles with the expected build by SHA-256, checks public routes and mobile menu
contrast and navigation in both color schemes, and records menu screenshots and asset hashes.
Results and source metadata are in `test-results/public-deployment.json`; attachments are beside
the test outputs. A mismatch fails even if the site returns HTTP 200.
