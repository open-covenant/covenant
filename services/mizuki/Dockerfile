ARG SOURCE_DATE_EPOCH=0

FROM --platform=linux/amd64 node:22.22.0-bookworm-slim@sha256:7cc56ef285a8568121537d17b05e72128f01b89c54607b51acf084a50ef483f3 AS build

ENV PNPM_HOME=/pnpm
ENV PATH=$PNPM_HOME:$PATH

WORKDIR /workspace

RUN corepack enable \
  && corepack prepare pnpm@10.31.0 --activate \
  && pnpm --version | grep -Fx 10.31.0

COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY services/mizuki/package.json services/mizuki/package.json

RUN pnpm install --frozen-lockfile --ignore-scripts --filter @covenant/mizuki...

COPY services/mizuki/tsconfig.json services/mizuki/tsconfig.json
COPY services/mizuki/src services/mizuki/src

RUN pnpm --filter @covenant/mizuki build \
  && pnpm --filter @covenant/mizuki deploy --prod --legacy /release \
  && test -f /release/dist/server.js \
  && test ! -d /release/node_modules/typescript \
  && test ! -d /release/node_modules/vitest \
  && rm -r /release/src \
  && rm /release/tsconfig.json

FROM --platform=linux/amd64 node:22.22.0-bookworm-slim@sha256:7cc56ef285a8568121537d17b05e72128f01b89c54607b51acf084a50ef483f3

ENV NODE_ENV=production
ENV MIZUKI_HOST=0.0.0.0
ENV MIZUKI_PORT=10000

WORKDIR /app

RUN groupadd --gid 10001 mizuki \
  && useradd --uid 10001 --gid mizuki --no-create-home --shell /usr/sbin/nologin mizuki

COPY --from=build --chown=0:0 /release ./

USER 10001:10001

EXPOSE 10000

CMD ["node", "dist/server.js"]
