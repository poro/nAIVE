# naive.dev

The static site for naive.dev, rebuilt in November 2026 from the live pages
(which had no source in this repository) and updated for 0.1.23.

Deploy it to the `naive-dev` Cloudflare Pages project:

```sh
wrangler pages deploy site --project-name naive-dev --branch main
```

That serves https://naive-dev.pages.dev. The naive.dev domain is attached to
the project in the Cloudflare dashboard (Pages → naive-dev → Custom domains).
