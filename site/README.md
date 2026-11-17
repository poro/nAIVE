# naive.dev

The static site for naive.dev: plain HTML and one stylesheet, no build step.
It was rebuilt in November 2026 from the live pages and updated for 0.1.23.

To publish a change, copy the `.html` files and `css/` to the folder the
naive.dev server serves. A preview copy can be deployed to Cloudflare Pages:

```sh
wrangler pages deploy site --project-name naive-dev --branch main
```
