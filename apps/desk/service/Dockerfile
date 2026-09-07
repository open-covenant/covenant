# Covenant Desk in a container.
#
# Build from the repository root so the shared TypeScript settings are in scope:
#
#   docker build -f apps/desk/service/Dockerfile -t covenant-desk .
#
# The desk listens on 127.0.0.1 inside the container. Give it the same network
# namespace as the agent that calls it, and mount a directory for the config
# file, the key file, and the database:
#
#   docker run --rm -v ~/.config/covenant-desk:/desk covenant-desk status
#
# Keys are read from /desk/keys.env at start. They are never written to an image
# layer and never leave the container.

FROM node:24-bookworm-slim AS build
ENV PNPM_HOME=/pnpm
ENV PATH=$PNPM_HOME:$PATH
RUN corepack enable

WORKDIR /build
COPY tsconfig.base.json ./tsconfig.base.json
COPY apps/desk/package.json apps/desk/tsconfig.json ./apps/desk/

WORKDIR /build/apps/desk
# The package has no workspace dependencies, so it installs on its own.
RUN pnpm install --ignore-workspace --no-frozen-lockfile

COPY apps/desk/src ./src
COPY apps/desk/data ./data
COPY apps/desk/service ./service
RUN pnpm run build && pnpm prune --prod

FROM node:24-bookworm-slim AS runtime
ENV NODE_ENV=production
ENV DESK_HOME=/desk

WORKDIR /app
COPY --from=build /build/apps/desk/package.json ./package.json
COPY --from=build /build/apps/desk/node_modules ./node_modules
COPY --from=build /build/apps/desk/dist ./dist
COPY --from=build /build/apps/desk/data ./data
COPY --from=build /build/apps/desk/service ./service

RUN mkdir -p /desk && chown -R node:node /desk /app
USER node
VOLUME ["/desk"]
EXPOSE 46631

ENTRYPOINT ["node", "/app/dist/surfaces/cli/main.js"]
CMD ["start", "--foreground"]
