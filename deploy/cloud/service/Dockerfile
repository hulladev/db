# Build context: deploy/cloud. Supply a tarball containing linux-<arch> assets.
FROM oven/bun:1.4.0 AS build
WORKDIR /app
COPY service/package.json service/hulla-db.tgz ./
RUN bun install --ignore-scripts
COPY next-app/db ./db
COPY next-app/lib/session.ts ./lib/session.ts
COPY next-app/db.config.ts ./db.config.ts
# Generation imports configuration but never connects. These are inert fixtures.
RUN FIXTURE_MODE=isolated DATABASE_NAMESPACE=cloud_fixture_build \
    DATABASE_URL=postgresql://unused@127.0.0.1:1/unused \
    FIXTURE_SESSION_TOKEN=build-only-placeholder-not-a-runtime-session \
    APP_ORIGIN=http://invalid.test NEXT_PUBLIC_HULLA_SERVICE_URL=/db \
    bun --bun node_modules/@hulla/db/dist/cli.js generate

FROM node:22.18.0-bookworm-slim AS runtime
WORKDIR /app
COPY --from=build --chown=node:node /app /app
USER node
ENV SERVICE_HOST=0.0.0.0 SERVICE_PORT=3001
EXPOSE 3001
STOPSIGNAL SIGTERM
CMD ["node", "node_modules/@hulla/db/dist/cli.js", "serve", "db.config.ts"]
