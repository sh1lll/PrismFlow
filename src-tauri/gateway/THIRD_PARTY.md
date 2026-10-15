# Third-party components — embedded gateway runtime

Generated from `package-lock.json` on 2026-10-14. Regenerate on every upgrade
(ADR-005 amendment: pin, review, re-test).

**Engine:** `@the-next-ai/ai-gateway` 1.0.21. MIT License, Copyright (c) 2026
musistudio. Tarball sha256 `21732fa5d077390ae1f92a42d7cce9cfe94500f8ea9c2a2be15df62ad7ef5990`.
The published source map embeds all 100 first-party TypeScript sources, so the code can be reviewed.

**PrismFlow's use:** run unmodified as a supervised child process (`node --require
prismflow-preload.cjs …/bin/next-ai-gateway.js`). No ai-gateway code is copied
into PrismFlow's sources.

License summary (60 packages): MIT ×46, BSD-3-Clause ×5, ISC ×3, BlueOak-1.0.0 ×5, Apache-2.0 ×1. All are compatible with PrismFlow's MIT license.
`openai` (Apache-2.0) is a declared dependency that `dist/index.js` never requires.

| Package | Version | License |
|---|---|---|
| @fastify/ajv-compiler | 4.0.7 | MIT |
| @fastify/deepmerge | 3.2.2 | MIT |
| @fastify/error | 4.2.1 | MIT |
| @fastify/fast-json-stringify-compiler | 5.1.1 | MIT |
| @fastify/forwarded | 3.1.1 | MIT |
| @fastify/merge-json-schemas | 0.2.2 | MIT |
| @fastify/proxy-addr | 5.1.2 | MIT |
| @pinojs/redact | 0.4.0 | MIT |
| @the-next-ai/ai-gateway | 1.0.21 | MIT |
| abstract-logging | 2.0.1 | MIT |
| ajv | 8.20.0 | MIT |
| ajv-formats | 3.0.1 | MIT |
| fast-uri | 3.1.8 | BSD-3-Clause |
| atomic-sleep | 1.0.0 | MIT |
| avvio | 9.3.1 | MIT |
| balanced-match | 4.0.4 | MIT |
| brace-expansion | 5.0.12 | MIT |
| cookie | 2.0.1 | MIT |
| dequal | 2.0.3 | MIT |
| diff | 8.0.4 | BSD-3-Clause |
| fast-decode-uri-component | 1.0.1 | MIT |
| fast-deep-equal | 3.1.3 | MIT |
| fast-json-stringify | 7.1.0 | MIT |
| fast-querystring | 1.1.2 | MIT |
| fast-uri | 4.2.2 | BSD-3-Clause |
| fastify | 5.12.7 | MIT |
| fastq | 1.20.3 | ISC |
| find-my-way | 9.10.0 | MIT |
| glob | 13.0.6 | BlueOak-1.0.0 |
| ipaddr.js | 2.5.0 | MIT |
| json-schema-ref-resolver | 3.0.1 | MIT |
| json-schema-traverse | 1.0.0 | MIT |
| light-my-request | 6.8.0 | BSD-3-Clause |
| lru-cache | 11.5.3 | BlueOak-1.0.0 |
| minimatch | 10.2.6 | BlueOak-1.0.0 |
| minipass | 7.1.3 | BlueOak-1.0.0 |
| on-exit-leak-free | 2.1.2 | MIT |
| openai | 6.49.0 | Apache-2.0 |
| path-scurry | 2.0.2 | BlueOak-1.0.0 |
| pino | 10.4.0 | MIT |
| pino-abstract-transport | 3.0.0 | MIT |
| pino-std-serializers | 7.1.0 | MIT |
| process-warning | 5.1.1 | MIT |
| quick-format-unescaped | 4.0.4 | MIT |
| real-require | 1.0.0 | MIT |
| require-from-string | 2.0.2 | MIT |
| ret | 0.5.0 | MIT |
| reusify | 1.1.0 | MIT |
| rfdc | 1.4.1 | MIT |
| safe-regex2 | 5.1.2 | MIT |
| safe-stable-stringify | 2.5.0 | MIT |
| secure-json-parse | 4.1.1 | BSD-3-Clause |
| semver | 7.8.5 | ISC |
| set-cookie-parser | 3.1.3 | MIT |
| sonic-boom | 4.2.1 | MIT |
| split2 | 4.2.0 | ISC |
| thread-stream | 4.2.0 | MIT |
| toad-cache | 3.7.4 | MIT |
| undici | 6.29.0 | MIT |
| ws | 8.22.0 | MIT |
