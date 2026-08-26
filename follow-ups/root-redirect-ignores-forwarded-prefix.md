# Root redirect escapes the proxy prefix

`root_redirect` (pipeline.rs) answers `/` with a hardcoded
`Location: /cpanel/`. Behind a stripping proxy (labdev's `/apps/`),
`GET /apps/` reaches the runtime as `/` and the 302 sends the browser to
`/cpanel/` on the PUBLIC host — outside the prefix, landing on whatever
owns that path out there (the platform's Front Manager, in labdev).

Fix is one line: redirect to the RELATIVE `cpanel/` (RFC 7231 allows
relative Location) or compose with the charset-checked
`X-Forwarded-Prefix` like base_href already does (0.2.1). Ship with the
next release; not worth its own tag.
