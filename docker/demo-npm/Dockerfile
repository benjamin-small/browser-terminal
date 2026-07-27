# Builds and serves the demo against the *published* library, to prove that
# installing from npm is enough.
#
# Every other build in this repo resolves @benjamin-small/browser-terminal
# through a workspace symlink into packages/browser-terminal, so it reads
# whatever is on disk regardless of what was published. That hides a whole
# class of packaging bug, and it means "the demo works" has never actually
# been evidence that a stranger's `npm install` works.
#
# So: neither stage contains Rust, cargo, wasm-bindgen, wasm-opt, or a single
# file from crates/ or packages/browser-terminal. The .wasm in the final image
# can only have arrived inside the npm tarball. If the terminal boots, the
# published package is self-sufficient.
#
#   docker build -f docker/demo-npm/Dockerfile -t bterm-demo-npm .
#   docker run --rm -p 8080:8080 bterm-demo-npm
#
# Point it at any published release:
#   docker build -f docker/demo-npm/Dockerfile --build-arg BTERM_VERSION=0.1.0 -t bterm-demo-npm .

ARG BTERM_VERSION=0.1.0

# ---------------------------------------------------------------- build ----
FROM node:20-alpine AS build
WORKDIR /app
ARG BTERM_VERSION

# The manifest deliberately declares only vite and typescript. @xterm/xterm
# and @xterm/addon-fit are NOT listed: if the terminal renders, they were
# pulled in as the library's own dependencies, which is the part of the
# manifest a workspace build can never exercise.
COPY docker/demo-npm/package.json ./package.json

RUN npm install --no-audit --no-fund --loglevel=error

# Separate from the devDependency install so the version being proven is
# visible in the build log and in `docker history`.
RUN npm install --no-audit --no-fund --loglevel=error \
      "@benjamin-small/browser-terminal@${BTERM_VERSION}"

# Fail here rather than in a browser later. Three things a tarball can get
# wrong that a workspace build would sail past.
RUN set -eu; \
    pkg=node_modules/@benjamin-small/browser-terminal; \
    echo "--- proving the package came from the registry ---"; \
    node -e " \
      const lock = require('./package-lock.json'); \
      const key = 'node_modules/@benjamin-small/browser-terminal'; \
      const entry = lock.packages[key]; \
      if (!entry) { console.error('not in the lockfile at all'); process.exit(1); } \
      console.log('resolved:', entry.resolved); \
      console.log('integrity:', entry.integrity); \
      console.log('version:', entry.version); \
      if (!String(entry.resolved).startsWith('https://registry.npmjs.org/')) { \
        console.error('NOT from the public registry — this proves nothing'); process.exit(1); \
      } \
    "; \
    echo "--- the .wasm must have shipped inside the tarball ---"; \
    test -f "$pkg/dist/wasm/bterm_wasm_bg.wasm" \
      || { echo "no .wasm in the installed package"; exit 1; }; \
    size=$(wc -c < "$pkg/dist/wasm/bterm_wasm_bg.wasm"); \
    echo "wasm: $size bytes"; \
    [ "$size" -gt 100000 ] || { echo "too small to be the real binary"; exit 1; }; \
    echo "--- xterm must have come transitively, not from our manifest ---"; \
    test -d node_modules/@xterm/xterm \
      || { echo "@xterm/xterm absent: the library under-declares its dependencies"; exit 1; }; \
    echo "OK"

# The demo application only — its source imports the library by name and
# otherwise references nothing outside this directory. Note what is absent:
# no crates/, no packages/browser-terminal.
COPY packages/demo/src ./src
COPY packages/demo/index.html ./index.html
COPY docker/demo-npm/vite.config.ts ./vite.config.ts

RUN npx vite build

# A build that emits no .wasm asset means the URL silently resolved to
# nothing — success-shaped right up until a browser asks for the file.
RUN set -eu; \
    found=$(find dist -name '*.wasm' | head -1); \
    [ -n "$found" ] || { echo "the bundle emitted no .wasm asset"; exit 1; }; \
    echo "bundled: $found ($(wc -c < "$found") bytes)"

# --------------------------------------------------------------- serve ----
FROM nginx:1.27-alpine AS serve

COPY --from=build /app/dist /usr/share/nginx/html
COPY docker/demo-npm/nginx.conf /etc/nginx/conf.d/default.conf

# Re-assert in the runtime image: a COPY that silently matched nothing would
# otherwise surface as a blank page.
RUN test -f /usr/share/nginx/html/index.html \
      && find /usr/share/nginx/html -name '*.wasm' | grep -q . \
      || { echo "the served root is missing index.html or the .wasm"; exit 1; }

EXPOSE 8080

HEALTHCHECK --interval=10s --timeout=3s --start-period=2s --retries=3 \
  CMD wget -qO- http://127.0.0.1:8080/healthz >/dev/null || exit 1

# nginx:alpine's default entrypoint handles config templating and signals.
